//! Case-insensitive `$(NAME)` variable expansion.
//!
//! Lives with the compiler code because every source it aggregates is tied to
//! a concrete Delphi installation: `rsvars.bat` variables, the IDE's registry
//! environment-variable overrides, and the per-build `Config`/`Platform`
//! context — unlike `dproj-rs`, which evaluates a `.dproj` in isolation.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// What seeds the macro map of one Delphi installation: the variables its
/// `rsvars.bat` sets, the IDE's own *Environment Variables* overrides, and the
/// per-user data directories the IDE derives (`$(BDSUSERDIR)`,
/// `$(BDSCOMMONDIR)`). Gathered once by [`IdeEnvironment::read`] — the only
/// place that touches disk and registry — so that everything downstream, and
/// every test, can work from a value.
#[derive(Debug, Clone, Default)]
pub struct IdeEnvironment {
    /// The variables `bin\rsvars.bat` exports (`BDS`, `BDSLIB`, `BDSCOMMONDIR`, …).
    pub rsvars: HashMap<String, String>,
    /// The IDE's *Environment Variables* overrides (Tools > Options), which the
    /// IDE injects into its own process and thus into IDE-run MSBuild.
    pub ide_variables: Vec<(String, String)>,
    /// `<Documents>\Embarcadero\Studio\<version>`, the IDE's `$(BDSUSERDIR)`.
    pub user_dir: Option<String>,
    /// The Public Documents counterpart, the IDE's `$(BDSCOMMONDIR)`; only a
    /// fallback, since `rsvars.bat` normally defines it.
    pub common_dir: Option<String>,
}

impl IdeEnvironment {
    /// Reads the environment of the installation at `installation` for the
    /// BDS version `bds_version` (`"23.0"`). Fails when `rsvars.bat` is
    /// missing or unparsable: without it no `$(BDS)`-relative path resolves.
    pub fn read(installation: &Path, bds_version: &str) -> Result<Self> {
        let rsvars_path = installation.join("bin").join("rsvars.bat");
        if !rsvars_path.exists() {
            bail!("rsvars.bat not found in compiler installation: {}", rsvars_path.display());
        }
        let rsvars = dproj_rs::rsvars::parse_rsvars_file(&rsvars_path)
            .with_context(|| format!("Failed to parse {}", rsvars_path.display()))?;
        Ok(IdeEnvironment {
            rsvars,
            ide_variables: crate::delphilsp::registry::read_ide_environment_variables(bds_version),
            user_dir: bds_user_dir(bds_version),
            common_dir: bds_common_dir(bds_version),
        })
    }

    /// The macro map an IDE-launched build effectively sees, in the IDE's
    /// order of precedence: `rsvars.bat` first, the derived `$(BDS…)`
    /// defaults only where nothing defined them, the IDE's own overrides
    /// last (they win over `rsvars.bat`). `PLATFORM` is dropped because
    /// `rsvars.bat` deliberately blanks it while the IDE's library paths use
    /// it; the caller sets `Platform` together with `Config` once it knows
    /// them.
    pub fn macros(&self, installation: &Path) -> MacroMap {
        let mut macros = MacroMap::new();
        macros.extend(self.rsvars.iter().map(|(k, v)| (k.clone(), v.clone())));
        macros.remove("PLATFORM");
        let bds = macros
            .get("BDS")
            .cloned()
            .unwrap_or_else(|| installation.to_string_lossy().to_string());
        macros.set_default("BDS", bds.clone());
        macros.set_default("BDSLIB", format!("{bds}\\lib"));
        macros.set_default("BDSINCLUDE", format!("{bds}\\include"));
        macros.set_default("BDSBIN", format!("{bds}\\bin"));
        if let Some(user_dir) = &self.user_dir {
            macros.set_default("BDSUSERDIR", user_dir.clone());
        }
        if let Some(common_dir) = &self.common_dir {
            macros.set_default("BDSCOMMONDIR", common_dir.clone());
        }
        macros.extend(self.ide_variables.iter().map(|(k, v)| (k.clone(), v.clone())));
        macros
    }
}

/// The IDE data folder name under a Documents root: `RAD Studio` for the
/// D2007–D2010 era (BDS 5.0–7.0), `Embarcadero\Studio` from XE (8.0) on.
fn bds_documents_subpath(bds_version: &str) -> PathBuf {
    if crate::delphilsp::registry::bds_major(bds_version) <= 7 {
        PathBuf::from("RAD Studio")
    } else {
        Path::new("Embarcadero").join("Studio")
    }
}

/// `<Documents>\Embarcadero\Studio\<version>` (or `<Documents>\RAD Studio\<version>`
/// for pre-XE versions) — the IDE's `$(BDSUSERDIR)`.
fn bds_user_dir(bds_version: &str) -> Option<String> {
    let documents = dirs::document_dir()?;
    Some(
        documents
            .join(bds_documents_subpath(bds_version))
            .join(bds_version)
            .to_string_lossy()
            .to_string(),
    )
}

/// The Public Documents counterpart of [`bds_user_dir`] — the IDE's
/// `$(BDSCOMMONDIR)`.
fn bds_common_dir(bds_version: &str) -> Option<String> {
    let public = std::env::var_os("PUBLIC")?;
    Some(
        Path::new(&public)
            .join("Documents")
            .join(bds_documents_subpath(bds_version))
            .join(bds_version)
            .to_string_lossy()
            .to_string(),
    )
}

/// A case-insensitive `$(NAME)` variable map (Windows environment semantics).
///
/// Names are also kept in their original spelling because `dproj-rs` resolves
/// `$(NAME)` through a **case-sensitive** map: seeding it needs the exact
/// casing the `.dproj` files use (`DCC_UnitSearchPath`, not `DCC_UNITSEARCHPATH`).
#[derive(Debug, Clone, Default)]
pub struct MacroMap {
    /// Upper-cased keys — the lookup used by [`MacroMap::expand`].
    vars: HashMap<String, String>,
    /// The same entries under their original spelling.
    original_case: HashMap<String, String>,
}

impl MacroMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a variable, overwriting any previous value.
    pub fn set(&mut self, key: impl AsRef<str>, value: impl Into<String>) {
        let key = key.as_ref();
        let value = value.into();
        self.vars.insert(key.to_ascii_uppercase(), value.clone());
        self.original_case.insert(key.to_string(), value);
    }

    /// Insert a variable only when that name is not already defined.
    pub fn set_default(&mut self, key: impl AsRef<str>, value: impl Into<String>) {
        if self.get(key.as_ref()).is_none() {
            self.set(key, value);
        }
    }

    /// Forget a variable, whatever casing it was defined with.
    pub fn remove(&mut self, key: &str) {
        let upper = key.to_ascii_uppercase();
        self.vars.remove(&upper);
        self.original_case.retain(|k, _| k.to_ascii_uppercase() != upper);
    }

    pub fn get(&self, key: &str) -> Option<&String> {
        self.vars.get(&key.to_ascii_uppercase())
    }

    pub fn extend<I, K, V>(&mut self, entries: I)
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: Into<String>,
    {
        for (k, v) in entries {
            self.set(k, v);
        }
    }

    /// Expand every `$(NAME)` reference. Unknown names are left **verbatim**
    /// so callers can detect (and report) unresolved macros — this is the one
    /// behavioural difference from MSBuild, which expands them to nothing.
    ///
    /// Expansion is iterative (a resolved value may itself contain macros) and
    /// bounded so a self-referential definition cannot loop forever.
    pub fn expand(&self, value: &str) -> String {
        const MAX_PASSES: usize = 8;
        let mut current = value.to_string();
        for _ in 0..MAX_PASSES {
            if !current.contains("$(") {
                break;
            }
            let next = self.expand_once(&current);
            if next == current {
                break;
            }
            current = next;
        }
        current
    }

    fn expand_once(&self, value: &str) -> String {
        let mut out = String::with_capacity(value.len());
        let bytes: Vec<char> = value.chars().collect();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == '$' && i + 1 < bytes.len() && bytes[i + 1] == '(' {
                if let Some(close) = (i + 2..bytes.len()).find(|&j| bytes[j] == ')') {
                    let name: String = bytes[i + 2..close].iter().collect();
                    match self.get(&name) {
                        Some(resolved) => out.push_str(resolved),
                        // Unknown: keep the token so the caller can warn.
                        _ => out.push_str(&format!("$({name})")),
                    }
                    i = close + 1;
                    continue;
                }
            }
            out.push(bytes[i]);
            i += 1;
        }
        out
    }

    /// Seed environment for `dproj-rs` property-group evaluation. Every entry
    /// appears both upper-cased and in its original spelling, because that
    /// lookup is case-sensitive.
    pub fn as_env(&self) -> HashMap<String, String> {
        let mut env = self.vars.clone();
        env.extend(self.original_case.iter().map(|(k, v)| (k.clone(), v.clone())));
        env
    }
}

#[cfg(test)]
mod ide_environment_tests {
    use super::*;

    fn environment(rsvars: &[(&str, &str)], ide_variables: &[(&str, &str)]) -> IdeEnvironment {
        IdeEnvironment {
            rsvars: rsvars.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            ide_variables: ide_variables.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            user_dir: Some(r"C:\Users\me\Documents\Embarcadero\Studio\23.0".into()),
            common_dir: Some(r"C:\Users\Public\Documents\Embarcadero\Studio\23.0".into()),
        }
    }

    #[test]
    fn derives_bds_defaults_from_the_installation_when_rsvars_is_silent() {
        let macros = environment(&[], &[]).macros(Path::new(r"C:\Delphi\23.0"));
        assert_eq!(macros.expand(r"$(BDS)\bin"), r"C:\Delphi\23.0\bin");
        assert_eq!(macros.expand("$(BDSLIB)"), r"C:\Delphi\23.0\lib");
        assert_eq!(macros.expand("$(BDSINCLUDE)"), r"C:\Delphi\23.0\include");
        assert_eq!(macros.expand("$(BDSBIN)"), r"C:\Delphi\23.0\bin");
        assert_eq!(macros.expand("$(BDSUSERDIR)"), r"C:\Users\me\Documents\Embarcadero\Studio\23.0");
        assert_eq!(macros.expand("$(BDSCOMMONDIR)"), r"C:\Users\Public\Documents\Embarcadero\Studio\23.0");
    }

    #[test]
    fn rsvars_wins_over_derived_defaults_and_ide_variables_win_over_rsvars() {
        let macros = environment(
            &[("BDS", r"D:\RAD\23.0"), ("BDSLIB", r"D:\custom\lib"), ("VEGADIR", r"C:\from-rsvars")],
            &[("VEGADIR", r"C:\Athens\hydra_2"), ("VEGA-DIR", r"C:\dashed")],
        )
        .macros(Path::new(r"C:\Delphi\23.0"));
        assert_eq!(macros.expand("$(BDS)"), r"D:\RAD\23.0");
        assert_eq!(macros.expand("$(bdslib)"), r"D:\custom\lib");
        assert_eq!(macros.expand("$(VEGADIR)"), r"C:\Athens\hydra_2");
        // A name with a dash is a valid IDE variable and must expand too.
        assert_eq!(macros.expand(r"$(VEGA-DIR)\x"), r"C:\dashed\x");
    }

    #[test]
    fn drops_the_blank_platform_rsvars_exports() {
        let macros = environment(&[("PLATFORM", "")], &[]).macros(Path::new(r"C:\Delphi\23.0"));
        assert_eq!(macros.get("Platform"), None);
        assert_eq!(macros.expand("$(Platform)"), "$(Platform)");
    }
}
