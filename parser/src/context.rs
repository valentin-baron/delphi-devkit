use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

use dproj_rs::{Dproj, dproj::DprojError};

use crate::unit_cache::UnitCache;

/// Interned identifier. Two tracks share one interner:
///
/// - **Display track** ([`ProjectContext::intern`]): text exactly as written.
///   The interner stays case-preserving so it can also hold case-sensitive
///   content (string literals, paths) without corruption.
/// - **Lookup track** ([`ProjectContext::intern_key`]): case-folded. Delphi
///   identifiers are case-insensitive, so every comparison domain — defines,
///   unit names, aliases, cache keys, symbol tables — must key on this track,
///   never on the display track.
///
/// A newtype over `lasso::Spur` (a foreign type, so a bare alias could not
/// carry the transparent serde impls). It `Deref`s to `Spur`, so
/// `interner.resolve(&*id)` and hashing/equality behave exactly as before.
///
/// Serde is transparent through the process-global interner
/// ([`crate::globals`]): serialize resolves the `Spur` to its exact interned
/// string; deserialize re-interns that string with `get_or_intern`, yielding
/// a fresh-process `Spur` for the SAME string. This is what keeps the
/// dual-track invariant across a save/load: each `Identifier` round-trips its
/// OWN string ("TFoo" display vs "TFOO" key), and the two remain independent
/// interner entries — no code may assume they relate. Disk therefore holds
/// strings, never raw `Spur` integers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Identifier(lasso::Spur);

impl Identifier {
    #[inline]
    pub fn spur(self) -> lasso::Spur {
        self.0
    }
}

impl From<lasso::Spur> for Identifier {
    #[inline]
    fn from(spur: lasso::Spur) -> Self {
        Self(spur)
    }
}

impl std::ops::Deref for Identifier {
    type Target = lasso::Spur;
    #[inline]
    fn deref(&self) -> &lasso::Spur {
        &self.0
    }
}

impl serde::Serialize for Identifier {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // Non-panicking, mirroring the `FileId` contract (meta.rs): a `Spur`
        // the process-global interner never issued (foreign interner, or a stale
        // Spur from a previous `reset_for_tests` generation) is a serde error,
        // not a panic. lasso's `resolve` panics on a foreign Spur; `try_resolve`
        // returns `None` instead. Otherwise ONE bad id would abort the whole
        // `UnitCache::save`. The load side already deserializes to an error and
        // counts the entry; this mirrors it on the save side (M2, #21/#25).
        let text = crate::globals::interner()
            .try_resolve(&self.spur())
            .ok_or_else(|| {
                serde::ser::Error::custom("identifier not in current interner")
            })?;
        serializer.serialize_str(text)
    }
}

impl<'de> serde::Deserialize<'de> for Identifier {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Re-intern the string in THIS process's interner. `get_or_intern`
        // reproduces the correct Spur for the exact string, so a display
        // spelling and its folded key deserialize to their own distinct
        // entries independently.
        let text = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
        Ok(Self(crate::globals::interner().get_or_intern(text.as_ref())))
    }
}

/// `ThreadedRodeo` interns through `&self` — required because the context is
/// shared immutably across unit parses while interning continues.
pub type Interner = lasso::ThreadedRodeo;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetPlatform {
    Win32,
    Win64,
    Unknown, // todo for later?
}

impl TargetPlatform {
    pub fn from_dproj_name(name: &str) -> Self {
        match name.to_ascii_lowercase().as_str() {
            "win32" => Self::Win32,
            "win64" | "win64x" => Self::Win64,
            _ => Self::Unknown,
        }
    }

    pub fn pointer_size(self) -> u8 {
        match self {
            Self::Win32 => 4,
            Self::Win64 => 8,
            Self::Unknown => 8,
        }
    }
}

/// Compiler-specific facts supplied by the integrator (delphi-devkit expands
/// its `compilers.ron` entry into this). Nothing here is hardcoded in the
/// parser: which symbols a compiler auto-defines (VERxxx `condition`,
/// MSWINDOWS, UNICODE, CPUX86, ...) varies per version and target, so the
/// full list arrives from config.
#[derive(Debug, Clone, Default)]
pub struct CompilerProfile {
    /// Value of the `CompilerVersion` constant in `{$IF}` expressions
    /// (compilers.ron `compiler_version`, e.g. 36.0 for Delphi 12).
    pub compiler_version: f64,
    /// Value of the `RTLVersion` constant. `None` means "same as
    /// `compiler_version`" — the correct default for every modern Delphi
    /// (RTLVersion == CompilerVersion == 36 for Delphi 12); the two diverged
    /// only in very old releases, where an integrator supplies the real value.
    /// Kept a distinct field so `{$IF RTLVersion = …}` and `{$IF CompilerVersion
    /// = …}` evaluate independently rather than being hard-aliased.
    pub rtl_version: Option<f64>,
    /// Auto-defined symbols for this compiler + target platform, including
    /// the VERxxx condition define.
    pub defines: Vec<String>,
}

/// Set of active conditional symbols. Keys are case-folded [`Identifier`]s, so
/// a clone (one per unit parse — `{$DEFINE}` is unit-local) copies only u32s.
#[derive(Debug, Clone, Default)]
pub struct DefineSet(HashSet<Identifier>);

impl DefineSet {
    pub fn define(&mut self, symbol: Identifier) {
        self.0.insert(symbol);
    }

    pub fn undef(&mut self, symbol: Identifier) {
        self.0.remove(&symbol);
    }

    pub fn contains(&self, symbol: Identifier) -> bool {
        self.0.contains(&symbol)
    }
}

bitflags::bitflags! {
    /// Boolean compiler switches, one bit per `{$X+}`/`{$X-}` letter.
    /// Testable via `{$IFOPT}`; value switches ($A, $Z) live as fields on
    /// [`SwitchState`] instead.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct SwitchFlags: u32 {
        const BOOL_EVAL         = 1 << 0;  // $B
        const ASSERTIONS        = 1 << 1;  // $C
        const DEBUG_INFO        = 1 << 2;  // $D
        const LONG_STRINGS      = 1 << 3;  // $H
        const IO_CHECKS         = 1 << 4;  // $I
        const WRITEABLE_CONSTS  = 1 << 5;  // $J
        const LOCAL_SYMBOLS     = 1 << 6;  // $L
        const TYPE_INFO         = 1 << 7;  // $M
        const OPTIMIZATION      = 1 << 8;  // $O
        const OPEN_STRINGS      = 1 << 9;  // $P
        const OVERFLOW_CHECKS   = 1 << 10; // $Q
        const RANGE_CHECKS      = 1 << 11; // $R
        const TYPED_ADDRESS     = 1 << 12; // $T
        const SAFE_DIVIDE       = 1 << 13; // $U
        const VAR_STRING_CHECKS = 1 << 14; // $V
        const STACK_FRAMES      = 1 << 15; // $W
        const EXTENDED_SYNTAX   = 1 << 16; // $X
        const REFERENCE_INFO    = 1 << 17; // $Y
    }
}

/// Snapshot of switch-directive state. `Copy`: each unit parse starts from
/// [`ProjectContext::default_switches`] and mutates its own copy ($A/$H/...
/// are unit-local, like defines). `align` and `min_enum_size` feed `SizeOf`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SwitchState {
    pub flags: SwitchFlags,
    /// `{$A}` / `{$ALIGN}`: 1, 2, 4, 8 or 16. `$A-` = 1, `$A+` = 8.
    pub align: u8,
    /// `{$Z}` / `{$MINENUMSIZE}`: 1, 2 or 4.
    pub min_enum_size: u8,
}

impl Default for SwitchState {
    fn default() -> Self {
        Self {
            flags: SwitchFlags::ASSERTIONS
                | SwitchFlags::DEBUG_INFO
                | SwitchFlags::LONG_STRINGS
                | SwitchFlags::IO_CHECKS
                | SwitchFlags::LOCAL_SYMBOLS
                | SwitchFlags::OPTIMIZATION
                | SwitchFlags::OPEN_STRINGS
                | SwitchFlags::VAR_STRING_CHECKS
                | SwitchFlags::EXTENDED_SYNTAX
                | SwitchFlags::REFERENCE_INFO,
            align: 8,
            min_enum_size: 1,
        }
    }
}

#[derive(Debug)]
pub struct ContextError(pub String);

impl From<DprojError> for ContextError {
    fn from(value: DprojError) -> Self {
        Self(value.message)
    }
}

/// Everything shared by all unit parses of one (project, build config,
/// platform) combination. Immutable after construction except for the
/// interior-mutable `unit_cache`. A different config or platform is a
/// different `ProjectContext` — never mutate to switch.
#[derive(Debug)]
pub struct ProjectContext {
    /// Resolved build configuration name ("Debug", "Release") — part of the
    /// cache-snapshot identity.
    pub configuration: String,
    /// Resolved platform name as spelled in the dproj ("Win32", "Win64").
    pub platform_name: String,
    pub platform: TargetPlatform,
    /// `CompilerVersion` constant for `{$IF}` expressions (36.0 = Delphi 12).
    pub compiler_version: f64,
    /// `RTLVersion` constant for `{$IF}` expressions. Defaults to
    /// `compiler_version` (they coincide for every modern Delphi) unless the
    /// integrator's [`CompilerProfile`] supplies a divergent value.
    pub rtl_version: f64,
    /// DPROJ `DCC_Define` + [`CompilerProfile::defines`]. Unit parses clone
    /// this — `{$DEFINE}` in one unit never leaks into another.
    pub base_defines: DefineSet,
    /// `DCC_UnitSearchPath`, resolved relative to the dproj directory.
    pub search_paths: Vec<PathBuf>,
    /// `DCC_IncludePath`, resolved relative to the dproj directory. Delphi
    /// resolves `{$I}` against these (distinct from the unit search path).
    pub include_paths: Vec<PathBuf>,
    /// `DCC_Namespace` prefixes, in resolution order.
    pub namespaces: Vec<String>,
    /// `DCC_UnitAlias`: old name → actual unit.
    pub unit_aliases: HashMap<Identifier, Identifier>,
    /// Project-level $A/$H/... defaults; each unit parse copies then mutates.
    pub default_switches: SwitchState,
    pub unit_cache: UnitCache,
}

impl ProjectContext {
    /// Build from a `.dproj`. `config`/`platform` of `None` use the file's
    /// active defaults (e.g. `Debug`/`Win32`). `compiler` comes from the
    /// integrator's compiler config (delphi-devkit `compilers.ron`).
    pub fn from_dproj(
        path: impl AsRef<Path>,
        config: Option<&str>,
        platform: Option<&str>,
        compiler: &CompilerProfile,
    ) -> Result<Self, ContextError> {
        let dproj = Dproj::from_file(path)?;
        let config = match config {
            Some(c) => c.to_string(),
            None => dproj.active_configuration()?,
        };
        let platform_name = match platform {
            Some(p) => p.to_string(),
            None => dproj.active_platform()?,
        };
        let group = dproj.active_property_group_for(&config, &platform_name)?;

        let mut context = Self {
            platform: TargetPlatform::from_dproj_name(&platform_name),
            configuration: config.clone(),
            platform_name: platform_name.clone(),
            compiler_version: compiler.compiler_version,
            rtl_version: compiler.rtl_version.unwrap_or(compiler.compiler_version),
            base_defines: DefineSet::default(),
            search_paths: Vec::new(),
            include_paths: Vec::new(),
            namespaces: Vec::new(),
            unit_aliases: HashMap::new(),
            default_switches: SwitchState::default(),
            unit_cache: UnitCache::default(),
        };

        for define in &compiler.defines {
            let symbol = context.intern_key(define);
            context.base_defines.define(symbol);
        }

        if let Some(defines) = &group.dcc_options.define {
            for define in split_list(defines) {
                let symbol = context.intern_key(define);
                context.base_defines.define(symbol);
            }
        }

        if let Some(paths) = &group.dcc_options.unit_search_path {
            let base_dir = dproj.directory();
            for path in split_list(paths) {
                let path = PathBuf::from(path);
                context.search_paths.push(match base_dir {
                    Some(dir) if path.is_relative() => dir.join(path),
                    _ => path,
                });
            }
        }

        if let Some(paths) = &group.dcc_options.include_path {
            let base_dir = dproj.directory();
            for path in split_list(paths) {
                let path = PathBuf::from(path);
                context.include_paths.push(match base_dir {
                    Some(dir) if path.is_relative() => dir.join(path),
                    _ => path,
                });
            }
        }

        if let Some(namespaces) = &group.dcc_options.namespace {
            context.namespaces = split_list(namespaces).map(String::from).collect();
        }

        if let Some(aliases) = &group.dcc_options.unit_alias {
            for alias in split_list(aliases) {
                if let Some((old, actual)) = alias.split_once('=') {
                    let old = context.intern_key(old.trim());
                    let actual = context.intern_key(actual.trim());
                    context.unit_aliases.insert(old, actual);
                }
            }
        }

        // Project switch options → the switch defaults each unit parse copies,
        // so `{$IFOPT R+}` etc. reflect the actual build, not just compiler
        // defaults. Each option is a `"true"`/`"false"` string; absent options
        // keep the compiler default.
        {
            let dcc = &group.dcc_options;
            let switches = &mut context.default_switches;
            let toggles: [(&Option<String>, SwitchFlags); 18] = [
                (&dcc.range_checking, SwitchFlags::RANGE_CHECKS),
                (&dcc.integer_overflow_check, SwitchFlags::OVERFLOW_CHECKS),
                (&dcc.io_checking, SwitchFlags::IO_CHECKS),
                (&dcc.assertions_at_runtime, SwitchFlags::ASSERTIONS),
                (&dcc.full_boolean_evaluations, SwitchFlags::BOOL_EVAL),
                (&dcc.debug_information, SwitchFlags::DEBUG_INFO),
                (&dcc.long_strings, SwitchFlags::LONG_STRINGS),
                (&dcc.writeable_constants, SwitchFlags::WRITEABLE_CONSTS),
                (&dcc.local_debug_symbols, SwitchFlags::LOCAL_SYMBOLS),
                (&dcc.run_time_type_info, SwitchFlags::TYPE_INFO),
                (&dcc.optimize, SwitchFlags::OPTIMIZATION),
                (&dcc.open_string_params, SwitchFlags::OPEN_STRINGS),
                (&dcc.typed_at_parameter, SwitchFlags::TYPED_ADDRESS),
                (&dcc.pentium_safe_divide, SwitchFlags::SAFE_DIVIDE),
                (&dcc.strict_var_strings, SwitchFlags::VAR_STRING_CHECKS),
                (&dcc.generate_stack_frames, SwitchFlags::STACK_FRAMES),
                (&dcc.extended_syntax, SwitchFlags::EXTENDED_SYNTAX),
                (&dcc.symbol_reference_info, SwitchFlags::REFERENCE_INFO),
            ];
            for (option, flag) in toggles {
                if let Some(enabled) = parse_bool_option(option) {
                    switches.flags.set(flag, enabled);
                }
            }
            if let Some(alignment) = dcc.alignment.as_deref().and_then(|v| v.parse::<u8>().ok()) {
                switches.align = alignment;
            }
            if let Some(size) = dcc
                .minimum_enum_size
                .as_deref()
                .and_then(|v| v.parse::<u8>().ok())
            {
                switches.min_enum_size = size;
            }
        }

        Ok(context)
    }

    /// Display-track intern: text exactly as written. Never use the result
    /// as a comparison key — Delphi identifiers are case-insensitive.
    /// Delegates to the process-global interner ([`crate::globals`]).
    pub fn intern(&self, text: &str) -> Identifier {
        crate::globals::intern(text)
    }

    /// Lookup-track intern: case-folded key for all identifier comparisons
    /// (defines, unit names, aliases, cache, symbol tables). Delegates to the
    /// process-global interner ([`crate::globals`]).
    pub fn intern_key(&self, identifier: &str) -> Identifier {
        crate::globals::intern_key(identifier)
    }

    /// Resolve an identifier through the process-global interner.
    pub fn resolve(&self, identifier: Identifier) -> &'static str {
        crate::globals::resolve(identifier)
    }

    pub fn is_defined(&self, name: &str) -> bool {
        // Fold through the ONE identifier fold (`fold_identifier`), the same one
        // `intern_key` uses, so a define stored via `intern_key` is found here
        // for a non-ASCII name too (a Unicode `to_uppercase` here would diverge).
        crate::globals::interner()
            .get(crate::globals::fold_identifier(name))
            .is_some_and(|symbol| self.base_defines.contains(Identifier::from(symbol)))
    }

    pub fn pointer_size(&self) -> u8 {
        self.platform.pointer_size()
    }
}

/// A dproj boolean switch option: `"true"`/`"false"` (case-insensitive).
/// `None` when the option is absent (keep the compiler default).
fn parse_bool_option(value: &Option<String>) -> Option<bool> {
    value.as_deref().map(|text| text.trim().eq_ignore_ascii_case("true"))
}

/// `;`-separated dproj list. Drops empties and unexpanded `$(Var)` leftovers.
fn split_list(list: &str) -> impl Iterator<Item = &str> {
    list.split(';')
        .map(str::trim)
        .filter(|part| !part.is_empty() && !part.starts_with("$("))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Machine-specific smoke test: build the context for the real be.dproj
    /// and print it readably. Run with:
    ///   cargo test --features local-tests be_dproj -- --nocapture
    #[cfg(feature = "local-tests")]
    #[test]
    fn be_dproj_context() {
        let profile = CompilerProfile {
            compiler_version: 36.0,
            rtl_version: None,
            defines: [
                "VER360", "MSWINDOWS", "WIN32", "CPU386", "CPUX86", "CPU32BITS",
                "UNICODE", "CONDITIONALEXPRESSIONS", "ASSEMBLER",
            ]
            .map(String::from)
            .to_vec(),
        };

        let context = ProjectContext::from_dproj(
            r"C:\Delphi\VSS\Intern\be\D12\be.dproj",
            None,
            None,
            &profile,
        )
        .expect("failed to build context from be.dproj");

        println!("platform:         {:?}", context.platform);
        println!("compiler_version: {}", context.compiler_version);

        let mut defines: Vec<&str> = context.base_defines.0.iter()
            .map(|s| crate::globals::resolve(*s))
            .collect();
        defines.sort_unstable();
        println!("defines ({}):     {:?}", defines.len(), defines);

        println!("namespaces:       {:?}", context.namespaces);
        println!("search_paths ({}):", context.search_paths.len());
        for path in &context.search_paths {
            println!("  {}", path.display());
        }
        for (old, actual) in &context.unit_aliases {
            println!("alias: {} => {}", crate::globals::resolve(*old), crate::globals::resolve(*actual));
        }

        assert_eq!(context.platform, TargetPlatform::Win32);
        assert!(context.is_defined("VER360"));
    }
}
