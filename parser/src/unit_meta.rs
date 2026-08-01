//! [`UnitMeta`]: the persisted + cached form of a parsed unit.
//!
//! Supersedes the lossy `UnitArtifact` projection as the durable format. A
//! `UnitMeta` owns the whole unit AST plus the metadata a clean-parse cannot
//! reproduce from the shallow AST alone (source/include/dependency stamps,
//! implementation-side usages, cycle taint). The importable **interface
//! surface** — symbols, their flattened members, constant values — is a
//! `#[serde(skip)]` index DERIVED from the AST and built lazily on first query
//! (cached in a `OnceCell`). Persisted bytes = AST + stamps + deps + usages +
//! cycle_taint; the interface index is never written and is rebuilt on demand.
//!
//! Interned `Identifier`s and session-local `FileId`s inside the AST/stamps
//! serialize transparently as strings/paths through the process-global
//! interner and arena ([`crate::globals`]), so the snapshot is process
//! independent with no hand-written mirror struct.

use std::path::{Path, PathBuf};

use once_cell::sync::OnceCell;
use serde::{Deserialize, Serialize};

use crate::ast::{Member, TypeExpression, Unit, VariantPart};
use crate::context::Identifier;
use crate::unit_cache::{
    Dependency, InterfaceSymbol, MemberKind, MemberSymbol, SourceStamp, SymbolKind, UnitInterface,
    Usage,
};

/// Complete cached/persisted result of parsing one unit.
#[derive(Debug, Serialize, Deserialize)]
pub struct UnitMeta {
    /// The unit AST root (the whole interface structure, uses clauses, …).
    pub ast: Unit,
    /// Parsed while an interface uses-cycle was active (invalid Delphi).
    /// Usable in-session as best effort, but NEVER persisted — the save path
    /// skips tainted metas because a clean parse may differ.
    pub cycle_tainted: bool,
    /// Produced by ERROR-TOLERANT recovery: at least one broken interface
    /// declaration was dropped (each marked by a diagnostic). The surviving
    /// declarations are real, but the interface is INCOMPLETE — so, exactly like
    /// `cycle_tainted`, a recovered meta is usable in-session but NEVER persisted
    /// as a clean interface (the save path skips it). This preserves the
    /// never-wrong discipline for the durable cache: a half-recovered parse must
    /// not masquerade as a complete, trustworthy interface across sessions.
    pub recovered: bool,
    pub source_path: PathBuf,
    pub source_hash: u64,
    /// Every `{$I}` include spliced in, with its content hash. A `.inc` edit
    /// must invalidate every unit that included it.
    pub includes: Vec<SourceStamp>,
    pub dependencies: Vec<Dependency>,
    /// Implementation-side identifier occurrences (find-references source).
    /// Not reconstructible from the shallow interface AST, so it is stored.
    pub usages: Vec<Usage>,
    /// The sibling `.dfm` design file (`Unit1.pas` ↔ `Unit1.dfm`), if one exists
    /// in the same directory. A path+hash stamp — same shape as an include
    /// stamp, so it serializes transparently (no raw id) and a `.dfm` edit
    /// stales the unit exactly like an include edit. `None` for units with no
    /// form. The stamp only records the ASSOCIATION + content hash for
    /// invalidation; the dfm↔pas LINK itself is computed on demand by
    /// [`crate::dfm_link`].
    pub dfm: Option<SourceStamp>,
    /// Decoded source length in bytes at parse time — the CHEAP, ROBUST proxy
    /// for this meta's real AST heap footprint (Task 16 D). The whole AST is
    /// derived from these source bytes, so parsed heap grows roughly linearly
    /// with them; the moka weigher ([`Self::estimated_bytes`]) multiplies this
    /// by a per-byte factor so the byte cap actually bounds RAM. Persisted (a
    /// reloaded unit must weigh the same as when first parsed, else eviction
    /// pressure would differ across sessions). `0` for a meta built through
    /// [`Self::new`] without a source length (older callers / tests); the
    /// weigher then falls back to a structural estimate.
    #[serde(default)]
    pub source_len: u32,
    /// Derived interface surface (symbols + flattened members), built lazily
    /// from `ast` and cached. Never serialized — rebuilt on demand.
    #[serde(skip)]
    interface_index: OnceCell<UnitInterface>,
}

impl UnitMeta {
    pub fn new(
        ast: Unit,
        cycle_tainted: bool,
        source_path: PathBuf,
        source_hash: u64,
        includes: Vec<SourceStamp>,
        dependencies: Vec<Dependency>,
        usages: Vec<Usage>,
    ) -> Self {
        Self {
            ast,
            cycle_tainted,
            recovered: false,
            source_path,
            source_hash,
            includes,
            dependencies,
            usages,
            dfm: None,
            source_len: 0,
            interface_index: OnceCell::new(),
        }
    }

    /// Record the decoded source length (builder style, keeps [`Self::new`]'s
    /// positional signature stable). Set by [`crate::pipeline::build_unit_meta`]
    /// from the arena so the weigher has its robust size proxy.
    pub fn with_source_len(mut self, source_len: u32) -> Self {
        self.source_len = source_len;
        self
    }

    /// Mark this meta as produced by error-tolerant recovery (builder style,
    /// keeps [`Self::new`]'s positional signature stable). A recovered meta is
    /// never persisted as a clean interface — same gate as `cycle_tainted`.
    pub fn with_recovered(mut self, recovered: bool) -> Self {
        self.recovered = recovered;
        self
    }

    /// Attach the sibling-dfm stamp (builder style, keeps [`Self::new`]'s
    /// positional signature stable for existing callers). The driver calls this
    /// after locating a `Unit1.dfm` next to `Unit1.pas`.
    pub fn with_dfm(mut self, dfm: Option<SourceStamp>) -> Self {
        self.dfm = dfm;
        self
    }

    /// Case-folded unit key (the cache identity), taken from the AST header.
    pub fn name(&self) -> Identifier {
        self.ast.name.key
    }

    /// The lazily-built, AST-derived interface surface. Idempotent: the first
    /// call flattens the interface declarations into symbols + members; later
    /// calls return the cached index.
    pub fn interface(&self) -> &UnitInterface {
        self.interface_index
            .get_or_init(|| build_interface(&self.ast))
    }

    /// All files whose change makes this meta stale: own source, sibling dfm,
    /// includes, dependency sources (and their includes).
    pub fn watched_files(&self) -> impl Iterator<Item = &Path> {
        std::iter::once(self.source_path.as_path())
            .chain(self.dfm.iter().map(|stamp| stamp.path.as_path()))
            .chain(self.includes.iter().map(|stamp| stamp.path.as_path()))
            .chain(self.dependencies.iter().flat_map(|dependency| {
                std::iter::once(dependency.source_path.as_path())
                    .chain(dependency.includes.iter().map(|stamp| stamp.path.as_path()))
            }))
    }

    /// Real-heap footprint estimate for the moka weigher — a CLOSE proxy so the
    /// byte cap actually bounds process RAM (Task 16 D). It must NOT undercount:
    /// `UnitMeta` owns the WHOLE unit AST (nested `TypeExpression`s, member vecs,
    /// every declaration), whose heap grows roughly linearly with the source it
    /// was derived from. The old estimate counted only shallow members × struct
    /// sizes and undercounted a real VCL/RTL unit by an order of magnitude, so
    /// the "512MB cap" never bounded RAM.
    ///
    /// PRIMARY proxy: the decoded SOURCE byte length ([`Self::source_len`]),
    /// scaled by [`Self::AST_BYTES_PER_SOURCE_BYTE`] — a parsed AST typically
    /// costs several bytes of heap per source byte (nodes, spans, interned refs,
    /// the usage index). This is O(1), never touches the `OnceCell`, and is
    /// robust across unit shapes. On TOP of that, the explicitly-owned side
    /// vectors (usages, dependencies, includes) are added at their real element
    /// size — they scale with cross-unit references, not just source length.
    ///
    /// FALLBACK: a meta with no recorded `source_len` (built via [`Self::new`]
    /// by an older caller / a test) uses a structural estimate that STILL scales
    /// with declaration + member counts, so it never collapses to a constant.
    ///
    /// Deliberately never calls [`interface()`](Self::interface): the weigher
    /// runs on the insert hot path under moka's lock; building the derived index
    /// there would mutate the `OnceCell` under that lock.
    pub fn estimated_bytes(&self) -> u32 {
        // Real per-element sizes of the owned side tables (these scale with
        // cross-unit references / occurrences, independent of source length).
        let side_tables = self.usages.len() * std::mem::size_of::<Usage>()
            + self.dependencies.len() * (std::mem::size_of::<Dependency>() + 96)
            + self.includes.len() * (std::mem::size_of::<SourceStamp>() + 96)
            + self.source_path.as_os_str().len();

        let ast_estimate = if self.source_len > 0 {
            // Primary: source-length proxy for the owned AST heap.
            (self.source_len as usize) * Self::AST_BYTES_PER_SOURCE_BYTE
        } else {
            // Fallback (no source_len): a structural estimate that still scales
            // with the AST's declaration + member counts, generously weighted so
            // it does not undercount the nested TypeExpression heap it stands in
            // for. Never a flat constant.
            let member_count: usize = self
                .ast
                .interface_declarations
                .iter()
                .map(|declaration| {
                    declaration
                        .type_expression
                        .as_ref()
                        .map(shallow_member_count)
                        .unwrap_or(0)
                })
                .sum();
            self.ast.interface_declarations.len()
                * (256 + std::mem::size_of::<InterfaceSymbol>())
                + member_count * (128 + std::mem::size_of::<MemberSymbol>())
        };

        let total = 512 + ast_estimate + side_tables;
        total.min(u32::MAX as usize) as u32
    }

    /// Heap bytes charged per source byte for the owned AST (Task 16 D). A
    /// parsed interface AST costs several heap bytes per source byte once nodes,
    /// spans, interned-identifier references and the derived-on-demand surface
    /// are accounted for. `8` is a deliberately conservative (over- rather than
    /// under-counting) multiplier: the weigher must NOT undercount, since an
    /// undercount is exactly what let RAM blow past the cap. Tuned together with
    /// the editor default capacity ([`crate::unit_cache::DEFAULT_CAPACITY_BYTES`]).
    pub const AST_BYTES_PER_SOURCE_BYTE: usize = 8;
}

// ─── Interface surface derivation (AST → queryable index) ────────────────

fn symbol_kind(kind: crate::ast::DeclarationKind) -> SymbolKind {
    use crate::ast::DeclarationKind;
    match kind {
        DeclarationKind::Type => SymbolKind::Type,
        DeclarationKind::Const => SymbolKind::Const,
        DeclarationKind::ResourceString => SymbolKind::ResourceString,
        DeclarationKind::Var => SymbolKind::Var,
        DeclarationKind::ThreadVar => SymbolKind::ThreadVar,
        DeclarationKind::Procedure => SymbolKind::Procedure,
        DeclarationKind::Function => SymbolKind::Function,
    }
}

/// Build the interface surface from a unit's shallow interface declarations.
/// This is the single source of truth for the derived index; the pipeline no
/// longer flattens separately.
pub fn build_interface(unit: &Unit) -> UnitInterface {
    let symbols = unit
        .interface_declarations
        .iter()
        .map(|declaration| InterfaceSymbol {
            name: declaration.name.name,
            key: declaration.name.key,
            kind: symbol_kind(declaration.kind),
            location: declaration.name.location,
            constant_value: declaration.constant_value,
            members: declaration
                .type_expression
                .as_ref()
                .map(collect_member_symbols)
                .unwrap_or_default(),
            attributes: attribute_keys(&declaration.attributes),
            has_ancestors: declaration
                .type_expression
                .as_ref()
                .map(type_can_inherit)
                .unwrap_or(false),
        })
        .collect();
    UnitInterface {
        name: unit.name.key,
        symbols,
    }
}

/// May a type of this shape inherit members from a base — OR otherwise carry a
/// member surface we do not flatten here? A class always can (implicit
/// `TObject`) and an interface always can (implicit `IInterface`); either may
/// also name explicit — possibly cross-unit — ancestors. In addition, a bare
/// `Reference` alias (`TFoo = TBar`) redirects to the aliased type's ENTIRE
/// member surface (its direct members included), a `Distinct` type
/// (`T = type Integer`) likewise, and a `ClassReference` (`class of T`) exposes
/// `T`'s class-level members — for all of these the members visible here are not
/// authoritative, so an absent member must degrade to Unknown, never a confident
/// false. Only genuinely self-contained, ancestor-less shapes (records, enums,
/// sets, subranges, pointers, routine types, …) carry no unseen member space and
/// keep the confident `false`. This drives the "member-not-directly-found →
/// Unknown, not false" rule for scoped `Declared(Type.Member)` (ledger #19).
fn type_can_inherit(type_expression: &TypeExpression) -> bool {
    matches!(
        type_expression,
        TypeExpression::Class(_)
            | TypeExpression::Interface(_)
            | TypeExpression::Reference { .. }
            | TypeExpression::Distinct(_)
            | TypeExpression::ClassReference(_)
            // Forward declarations: member surface completed elsewhere, not
            // knowable from the forward alone → missing member is Unknown, not
            // a confident false (#19).
            | TypeExpression::ForwardClass
            | TypeExpression::ForwardInterface
            | TypeExpression::ForwardDispInterface
    )
}

/// Attribute name lookup keys, source order.
fn attribute_keys(attributes: &[crate::ast::Attribute]) -> Vec<Identifier> {
    attributes.iter().map(|attribute| attribute.name.key).collect()
}

/// The member's declared type as a SIMPLE reference key, else `None`. Only a
/// bare `TypeExpression::Reference` (`Integer`, `TFoo`, `TList<T>`) yields a
/// key — anonymous/complex types (inline records, arrays, pointers, procedural
/// types) stay `None`; their structure lives in the AST.
fn simple_type_key(type_expression: &TypeExpression) -> Option<Identifier> {
    match type_expression {
        TypeExpression::Reference { name, .. } => Some(name.key),
        _ => None,
    }
}

/// Return type key of a routine, if it has a simple-reference return type.
fn routine_return_type_key(routine: &crate::ast::RoutineType) -> Option<Identifier> {
    routine
        .return_type
        .as_ref()
        .and_then(simple_type_key)
}

/// Count a structured type's members WITHOUT building the queryable surface —
/// the cheap term the moka weigher needs (see [`UnitMeta::estimated_bytes`]).
/// Mirrors the traversal of [`collect_member_symbols`] but only tallies; a
/// `Member::Field` with several names counts once per name (as it flattens).
fn shallow_member_count(type_expression: &TypeExpression) -> usize {
    fn count_members(source: &[Member]) -> usize {
        source
            .iter()
            .map(|member| match member {
                Member::Field { names, .. } => names.len(),
                _ => 1,
            })
            .sum()
    }
    fn count_variant(variant_part: &VariantPart) -> usize {
        variant_part
            .arms
            .iter()
            .map(|arm| {
                count_members(&arm.fields)
                    + arm.nested.as_deref().map(count_variant).unwrap_or(0)
            })
            .sum()
    }
    match type_expression {
        TypeExpression::Class(class_type) => class_type
            .sections
            .iter()
            .map(|section| count_members(&section.members))
            .sum(),
        TypeExpression::Record(structured) => {
            structured
                .sections
                .iter()
                .map(|section| count_members(&section.members))
                .sum::<usize>()
                + structured
                    .variant_part
                    .as_ref()
                    .map(count_variant)
                    .unwrap_or(0)
        }
        TypeExpression::Interface(interface_type) => count_members(&interface_type.members),
        _ => 0,
    }
}

/// Flatten a structured type's members into the queryable surface. Nested
/// types are listed as members but NOT flattened into their parent (they own
/// their own member space). Each member records the visibility of the section
/// it came from; records/interfaces (no visibility sections) get
/// `Visibility::Unspecified`.
fn collect_member_symbols(type_expression: &TypeExpression) -> Vec<MemberSymbol> {
    use crate::ast::Visibility;
    let mut members = Vec::new();
    match type_expression {
        TypeExpression::Class(class_type) => {
            for section in &class_type.sections {
                collect_from_members(
                    &section.members,
                    section.visibility,
                    section.strict,
                    &mut members,
                );
            }
        }
        TypeExpression::Record(structured) => {
            for section in &structured.sections {
                collect_from_members(
                    &section.members,
                    section.visibility,
                    section.strict,
                    &mut members,
                );
            }
            if let Some(variant_part) = &structured.variant_part {
                collect_from_variant(variant_part, Visibility::Unspecified, false, &mut members);
            }
        }
        TypeExpression::Interface(interface_type) => {
            collect_from_members(
                &interface_type.members,
                Visibility::Unspecified,
                false,
                &mut members,
            );
        }
        _ => {}
    }
    members
}

fn collect_from_variant(
    variant_part: &VariantPart,
    visibility: crate::ast::Visibility,
    strict: bool,
    members: &mut Vec<MemberSymbol>,
) {
    for arm in &variant_part.arms {
        collect_from_members(&arm.fields, visibility, strict, members);
        if let Some(nested) = &arm.nested {
            collect_from_variant(nested, visibility, strict, members);
        }
    }
}

fn collect_from_members(
    source: &[Member],
    visibility: crate::ast::Visibility,
    strict: bool,
    members: &mut Vec<MemberSymbol>,
) {
    for member in source {
        match member {
            Member::Field {
                names,
                field_type,
                attributes,
                ..
            } => {
                let type_key = simple_type_key(field_type);
                for name in names {
                    members.push(MemberSymbol {
                        name: name.name,
                        key: name.key,
                        kind: MemberKind::Field,
                        location: name.location,
                        read_target: None,
                        write_target: None,
                        type_key,
                        directives: Vec::new(),
                        visibility,
                        strict,
                        attributes: attribute_keys(attributes),
                    });
                }
            }
            Member::Method(method) => members.push(MemberSymbol {
                name: method.name.name,
                key: method.name.key,
                kind: MemberKind::Method,
                location: method.name.location,
                read_target: None,
                write_target: None,
                type_key: routine_return_type_key(&method.routine),
                directives: method.directives.clone(),
                visibility,
                strict,
                attributes: attribute_keys(&method.attributes),
            }),
            Member::Property(property) => members.push(MemberSymbol {
                name: property.name.name,
                key: property.name.key,
                kind: MemberKind::Property,
                location: property.name.location,
                read_target: property.read_target.as_ref().map(|target| target.key),
                write_target: property.write_target.as_ref().map(|target| target.key),
                type_key: property.property_type.as_ref().and_then(simple_type_key),
                directives: Vec::new(),
                visibility,
                strict,
                attributes: attribute_keys(&property.attributes),
            }),
            Member::NestedType(declaration) => members.push(MemberSymbol {
                name: declaration.name.name,
                key: declaration.name.key,
                kind: MemberKind::NestedType,
                location: declaration.name.location,
                read_target: None,
                write_target: None,
                type_key: None,
                directives: Vec::new(),
                visibility,
                strict,
                attributes: attribute_keys(&declaration.attributes),
            }),
            Member::NestedConst {
                name, attributes, ..
            } => members.push(MemberSymbol {
                name: name.name,
                key: name.key,
                kind: MemberKind::NestedConst,
                location: name.location,
                read_target: None,
                write_target: None,
                type_key: None,
                directives: Vec::new(),
                visibility,
                strict,
                attributes: attribute_keys(attributes),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Source;
    use crate::context::{DefineSet, ProjectContext, SwitchState, TargetPlatform};
    use crate::parser::parse_file_full;
    use crate::unit_cache::UnitCache;
    use std::collections::HashMap;
    use std::sync::Arc;

    fn test_context() -> Arc<ProjectContext> {
        Arc::new(ProjectContext {
            configuration: "Debug".to_string(),
            platform_name: "Win32".to_string(),
            platform: TargetPlatform::Win32,
            compiler_version: 36.0,
            rtl_version: 36.0,
            base_defines: DefineSet::default(),
            search_paths: Vec::new(),
            include_paths: Vec::new(),
            namespaces: Vec::new(),
            unit_aliases: HashMap::new(),
            default_switches: SwitchState::default(),
            unit_cache: UnitCache::default(),
        })
    }

    fn parse_meta(path: &Path) -> UnitMeta {
        // parse through the GLOBAL arena so a serde round-trip re-registers
        let arena = crate::globals::arena();
        let context = test_context();
        let file = arena.load(path).unwrap();
        let mut outcome = parse_file_full(arena, context, file, None).unwrap();
        let Some(Source::Unit(unit)) = outcome.source.take() else {
            panic!("expected unit");
        };
        let source_hash = crate::unit_cache::hash_file(path).unwrap();
        UnitMeta::new(
            unit,
            outcome.cycle_tainted,
            path.to_path_buf(),
            source_hash,
            Vec::new(),
            outcome.dependencies,
            outcome.usages,
        )
    }

    #[test]
    fn derived_interface_flattens_members() {
        let directory = std::env::temp_dir().join("delphi_parser_unit_meta");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("Derived.pas");
        std::fs::write(
            &path,
            "unit Derived;\ninterface\n\
             type TThing = class\n  FValue: Integer;\n  procedure Go;\n\
             property Value: Integer read FValue write SetValue;\nend;\n\
             const MaxThings = 3;\n\
             implementation\nend.",
        )
        .unwrap();

        let meta = parse_meta(&path);
        let interface = meta.interface();
        assert_eq!(interface.name, meta.name());
        let thing = interface
            .find(crate::globals::intern_key("TThing"))
            .expect("type symbol");
        assert_eq!(thing.members.len(), 3);
        let value = thing
            .find_member(crate::globals::intern_key("Value"))
            .unwrap();
        assert_eq!(value.kind, MemberKind::Property);
        assert_eq!(value.read_target, Some(crate::globals::intern_key("FValue")));
        assert!(interface.contains_key(crate::globals::intern_key("MaxThings")));

        // second call returns the cached index (idempotent)
        let interface_again = meta.interface();
        assert_eq!(interface_again.symbols.len(), interface.symbols.len());
    }

    #[test]
    fn meta_serde_round_trip_rebuilds_index() {
        let directory = std::env::temp_dir().join("delphi_parser_unit_meta_serde");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("Persisted.pas");
        std::fs::write(
            &path,
            "unit Persisted;\ninterface\n\
             type TFoo = class FCount: Integer; end;\n\
             const Answer = 42;\n\
             implementation\nend.",
        )
        .unwrap();

        let meta = parse_meta(&path);
        let bytes = bincode::serialize(&meta).unwrap();
        // no raw integers for names/paths — the strings themselves are present
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("Persisted"));
        assert!(text.contains("TFoo"));
        assert!(text.contains("Answer"));

        let restored: UnitMeta = bincode::deserialize(&bytes).unwrap();
        // interface_index was skipped → rebuilt lazily and correct
        assert_eq!(crate::globals::resolve(restored.name()), "PERSISTED");
        let interface = restored.interface();
        assert!(interface.contains_key(crate::globals::intern_key("TFoo")));
        let answer = interface
            .find(crate::globals::intern_key("Answer"))
            .unwrap();
        assert_eq!(
            answer.constant_value,
            Some(crate::unit_cache::ConstantValue::Int(42))
        );
        // cycle taint + stamps survived
        assert!(!restored.cycle_tainted);
        assert_eq!(restored.source_hash, meta.source_hash);
    }

    /// L6: a `$FFFFFFFFFFFFFFFF` constant (above i64::MAX) is captured as
    /// `ConstantValue::UInt` — NOT dropped to None and NOT bit-cast to a
    /// negative i64 — and survives a serde round-trip under the bumped format.
    #[test]
    fn large_unsigned_constant_captured_and_round_trips() {
        let directory = std::env::temp_dir().join("delphi_parser_unit_meta_uint");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("BigConst.pas");
        std::fs::write(
            &path,
            "unit BigConst;\ninterface\n\
             const AllBits = $FFFFFFFFFFFFFFFF;\n\
             const Fits = 42;\n\
             implementation\nend.",
        )
        .unwrap();

        let meta = parse_meta(&path);
        let bytes = bincode::serialize(&meta).unwrap();
        let restored: UnitMeta = bincode::deserialize(&bytes).unwrap();
        let interface = restored.interface();

        let all_bits = interface
            .find(crate::globals::intern_key("AllBits"))
            .unwrap();
        assert_eq!(
            all_bits.constant_value,
            Some(crate::unit_cache::ConstantValue::UInt(u64::MAX)),
            "an unsigned constant above i64::MAX must be UInt, not None or a wrong negative"
        );
        // an ordinary in-range constant is still Int
        let fits = interface.find(crate::globals::intern_key("Fits")).unwrap();
        assert_eq!(
            fits.constant_value,
            Some(crate::unit_cache::ConstantValue::Int(42))
        );
    }

    #[test]
    fn member_symbol_exposes_type_directives_visibility_attributes() {
        use crate::ast::Visibility;
        let directory = std::env::temp_dir().join("delphi_parser_rich_members");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("Rich.pas");
        std::fs::write(
            &path,
            "unit Rich;\ninterface\n\
             [Entity]\n\
             type TThing = class\n\
             private\n  FValue: Integer;\n\
             strict private\n  FSecret: Integer;\n\
             public\n  [Weak] procedure Go; virtual; abstract;\n\
               property Value: Integer read FValue;\nend;\n\
             implementation\nend.",
        )
        .unwrap();

        let meta = parse_meta(&path);
        let interface = meta.interface();
        let thing = interface
            .find(crate::globals::intern_key("TThing"))
            .expect("type symbol");
        // InterfaceSymbol carries declaration attributes
        assert_eq!(thing.attributes, vec![crate::globals::intern_key("Entity")]);

        let field = thing
            .find_member(crate::globals::intern_key("FValue"))
            .unwrap();
        assert_eq!(field.visibility, Visibility::Private);
        assert!(!field.strict, "plain private is not strict");
        assert_eq!(field.type_key, Some(crate::globals::intern_key("Integer")));
        assert!(field.directives.is_empty());

        // `strict private` carries the strict modifier alongside Private
        let secret = thing
            .find_member(crate::globals::intern_key("FSecret"))
            .unwrap();
        assert_eq!(secret.visibility, Visibility::Private);
        assert!(secret.strict, "strict private must set the strict flag");
        // a non-strict member in the same type keeps strict = false
        let go_strict = thing.find_member(crate::globals::intern_key("Go")).unwrap();
        assert!(!go_strict.strict);

        let go = thing.find_member(crate::globals::intern_key("Go")).unwrap();
        assert_eq!(go.visibility, Visibility::Public);
        assert_eq!(go.attributes, vec![crate::globals::intern_key("Weak")]);
        // method directives captured, folded, in order
        assert_eq!(
            go.directives,
            vec![
                crate::globals::intern_key("virtual"),
                crate::globals::intern_key("abstract")
            ]
        );

        let value = thing
            .find_member(crate::globals::intern_key("Value"))
            .unwrap();
        assert_eq!(value.type_key, Some(crate::globals::intern_key("Integer")));
        assert_eq!(value.visibility, Visibility::Public);
        // existing read-target behaviour preserved
        assert_eq!(value.read_target, Some(crate::globals::intern_key("FValue")));
    }

    #[test]
    fn attributes_survive_serde_round_trip() {
        // #16: an attribute captured on a declaration + a member must be
        // present after a bincode save/load — proves persistence via the AST.
        use crate::ast::{Member, TypeExpression};
        let directory = std::env::temp_dir().join("delphi_parser_attr_serde");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("AttrMeta.pas");
        std::fs::write(
            &path,
            "unit AttrMeta;\ninterface\n\
             [Entity('tbl')]\n\
             type TFoo = class\n  [Weak] FBar: Integer;\nend;\n\
             implementation\nend.",
        )
        .unwrap();

        let meta = parse_meta(&path);
        let bytes = bincode::serialize(&meta).unwrap();
        // the attribute NAME survives as text (no raw Spur)
        assert!(String::from_utf8_lossy(&bytes).contains("Entity"));

        let restored: UnitMeta = bincode::deserialize(&bytes).unwrap();
        let declaration = &restored.ast.interface_declarations[0];
        assert_eq!(
            crate::globals::resolve(declaration.attributes[0].name.name),
            "Entity"
        );
        // argument span survived and still resolves to its source text
        assert_eq!(
            crate::globals::arena()
                .try_location_text(declaration.attributes[0].arguments.unwrap())
                .unwrap(),
            "('tbl')"
        );
        let Some(TypeExpression::Class(class_type)) = declaration.type_expression.as_ref() else {
            panic!("expected class");
        };
        let Member::Field { attributes, .. } = &class_type.sections[0].members[0] else {
            panic!("expected field");
        };
        assert_eq!(crate::globals::resolve(attributes[0].name.name), "Weak");
    }
}
