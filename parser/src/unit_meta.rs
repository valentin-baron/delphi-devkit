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

use crate::ast::{ImplRoutine, Member, TypeExpression, Unit, VariantPart};
use crate::context::Identifier;
use crate::meta::{FileId, LocationContextGuard};
use crate::unit_cache::{
    Dependency, InterfaceSymbol, MemberKind, MemberSymbol, SourceStamp, SymbolKind, UnitInterface,
    Usage,
};

/// Complete cached/persisted result of parsing one unit.
#[derive(Debug)]
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
    ///
    /// Persisted via the custom `UnitMeta` serde (its payload struct marks this
    /// `#[serde(default)]` so an older meta without the field still decodes).
    pub source_len: u32,
    /// The whole implementation-section semantic AST (Stage S3): the scope tree
    /// (routines, methods, nested + anonymous scopes, `with`-blocks), typed
    /// locals, shallow statements, expressions, and `init`/`final` lists. This is
    /// the SINGLE SOURCE OF TRUTH for the implementation section; the flat
    /// `impl_scopes` table and the `impl_scopes_reliable` gate are DERIVED from it
    /// (see [`Self::impl_scopes`] / [`Self::impl_scopes_reliable`]) and are NOT
    /// serialized separately — the persisted form carries the body ONCE. Absent
    /// for an older meta (pre-format-15, via `#[serde(default)]`) or a meta built
    /// through [`Self::new`] without the builder; the derived table is then empty
    /// (matches nothing, safe) and the gate reads `body.reliable` (defaults true).
    ///
    /// NEVER SERIALIZED (`#[serde(skip)]`): the body is WORKING-SET state for the
    /// ONE active editor unit only. The persisted `.unit` cache is interface +
    /// flat usages + stamps — a cross-unit reload can therefore never drag a body
    /// into RAM (the 20 GB OOM this guards). It is retained in the live cache only
    /// for the active unit (`parse_buffer` / a direct `parse_source_file(true)`);
    /// every indexed / bootstrapped / cross-unit-imported meta carries an EMPTY
    /// body (`impl_scopes()` then yields empty — local resolution is
    /// active-unit-only, never a wrong answer).
    ///
    /// NEVER SERIALIZED: the custom `UnitMeta` serde omits it entirely from the
    /// payload; a reload always reconstructs it as `default()` (empty).
    pub implementation_body: crate::ast_impl::ImplementationBody,
    /// Derived interface surface (symbols + flattened members), built lazily
    /// from `ast` and cached. Never serialized (omitted by the custom serde) —
    /// rebuilt on demand.
    interface_index: OnceCell<UnitInterface>,
    /// Derived flat implementation-routine table (params + locals + body span per
    /// routine body), rebuilt lazily from [`Self::implementation_body`] and cached
    /// — the single-source-of-truth replacement for the old separately-serialized
    /// `impl_scopes` field. Never serialized (mirrors `interface_index`): the body
    /// is persisted once and this flat view is rebuilt on first access.
    /// Never serialized (omitted by the custom serde).
    impl_scopes_cache: OnceCell<Vec<ImplRoutine>>,
}

// ─── Custom serde: unit-self-file elision (format v18) ─────────────────────
//
// `UnitMeta` owns the whole AST plus side tables, holding thousands of
// `CodeLocation`s — nearly all in the unit's OWN source file. Rather than repeat
// that path on every span, (de)serialization installs a thread-local location
// context ([`LocationContextGuard`]) so a self-file span serializes as a span
// ONLY (no file), and a `{$I}`-include span references a small per-unit table of
// distinct include paths by index.
//
// ORDER IS LOAD-BEARING. On the wire a `UnitMeta` is:
//   1. `self_file: FileId`   — the unit's OWN file, serialized as its path. Read
//      FIRST on load and registered so `CURRENT_SELF_FILE` is established BEFORE
//      any nested `CodeLocation` (which may be a bare `SelfFile(span)`) decodes.
//   2. `payload`             — every real field (ast, stamps, deps, usages, …).
//      Nested `CodeLocation`s in here consult the active context; self-file spans
//      elide the file, include spans push into the serialize-side table.
//   3. `include_table: Vec<PathBuf>` — the distinct non-self include paths the
//      payload referenced. On SERIALIZE this is emitted LAST (it is only fully
//      populated after the payload has serialized).
//
// The load side cannot decode the payload before it has the include table (an
// `Include{index}` location needs it), yet the table is written last. Bincode is
// a sequential format, so we cannot read field 3 before field 2. The resolution:
// the payload is serialized into an OWNED byte buffer while the guard is active,
// THEN we emit `[self_file, include_table, payload_bytes]` in an order the load
// side can consume — self_file, then table, then the payload bytes (decoded with
// the context already fully established). This keeps a single guarded region on
// each side and makes the self file + table available before the payload decodes.

/// The self file's own `FileId` (→ path) plus the payload bytes and the include
/// table, in load order. `self_file` first so the context is set before the
/// payload decodes; `include_table` before `payload` so include indices resolve.
#[derive(Serialize, Deserialize)]
struct UnitMetaEnvelope {
    /// The unit's own source file, serialized transparently as its path.
    self_file: FileId,
    /// Distinct non-self `{$I}` include paths the payload references, by index.
    include_table: Vec<PathBuf>,
    /// The bincoded payload (all real fields), (de)serialized under the active
    /// location context so self-file spans elide the file.
    payload: Vec<u8>,
}

/// Owning form of every real `UnitMeta` field — used on the DESERIALIZE side.
/// The self-file elision happens inside the nested `CodeLocation` serde; this
/// struct just orders the fields. `#[serde(default)]` mirrors the original
/// derive so an older meta without `source_len` still decodes.
#[derive(Deserialize)]
struct UnitMetaPayloadOwned {
    ast: Unit,
    cycle_tainted: bool,
    recovered: bool,
    source_path: PathBuf,
    source_hash: u64,
    includes: Vec<SourceStamp>,
    dependencies: Vec<Dependency>,
    usages: Vec<Usage>,
    dfm: Option<SourceStamp>,
    #[serde(default)]
    source_len: u32,
}

/// Borrowing form of the same fields — used on the SERIALIZE side so the whole
/// (non-`Clone`) `Unit` AST is serialized in place with no deep copy. Field
/// order MUST match [`UnitMetaPayloadOwned`] byte-for-byte (bincode is
/// positional, not self-describing).
#[derive(Serialize)]
struct UnitMetaPayloadRef<'meta> {
    ast: &'meta Unit,
    cycle_tainted: bool,
    recovered: bool,
    source_path: &'meta PathBuf,
    source_hash: u64,
    includes: &'meta Vec<SourceStamp>,
    dependencies: &'meta Vec<Dependency>,
    usages: &'meta Vec<Usage>,
    dfm: &'meta Option<SourceStamp>,
    source_len: u32,
}

impl Serialize for UnitMeta {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // The unit's own file = the file its name location lives in. Establish it
        // as the self file; every self-file span then serializes span-only.
        let self_file = self.ast.name.location.file;
        let payload = UnitMetaPayloadRef {
            ast: &self.ast,
            cycle_tainted: self.cycle_tainted,
            recovered: self.recovered,
            source_path: &self.source_path,
            source_hash: self.source_hash,
            includes: &self.includes,
            dependencies: &self.dependencies,
            usages: &self.usages,
            dfm: &self.dfm,
            source_len: self.source_len,
        };

        // Guard the context for the payload serialization. The include table is
        // collected as nested CodeLocations serialize; taken AFTER, then dropped.
        let (payload_bytes, include_table) = {
            let _guard = LocationContextGuard::enter(self_file);
            let payload_bytes = bincode::serialize(&payload)
                .map_err(|error| serde::ser::Error::custom(error.to_string()))?;
            let include_table = LocationContextGuard::take_serialize_table();
            (payload_bytes, include_table)
            // _guard drops here → context reset on every exit path
        };

        let envelope = UnitMetaEnvelope {
            self_file,
            include_table,
            payload: payload_bytes,
        };
        envelope.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for UnitMeta {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let envelope = UnitMetaEnvelope::deserialize(deserializer)?;

        // Register each include path → FileId (lazy, no read). An unregisterable
        // include path (deleted between save and load) is a clean serde error →
        // the segment counts unreadable, never a panic (M2, #21/#25).
        let mut include_files: Vec<FileId> = Vec::with_capacity(envelope.include_table.len());
        for path in &envelope.include_table {
            let file = crate::globals::arena()
                .register(path)
                .map_err(|error| serde::de::Error::custom(error.message))?;
            include_files.push(file);
        }

        // Establish the self file (already registered — `self_file` deserialized
        // through the transparent FileId path serde) and install the include
        // table BEFORE decoding the payload, so nested self-file/include
        // CodeLocations resolve.
        let payload: UnitMetaPayloadOwned = {
            let _guard = LocationContextGuard::enter(envelope.self_file);
            LocationContextGuard::set_deserialize_table(include_files);
            bincode::deserialize(&envelope.payload)
                .map_err(|error| serde::de::Error::custom(error.to_string()))?
            // _guard drops here → context reset on every exit path
        };

        Ok(UnitMeta {
            ast: payload.ast,
            cycle_tainted: payload.cycle_tainted,
            recovered: payload.recovered,
            source_path: payload.source_path,
            source_hash: payload.source_hash,
            includes: payload.includes,
            dependencies: payload.dependencies,
            usages: payload.usages,
            dfm: payload.dfm,
            source_len: payload.source_len,
            implementation_body: crate::ast_impl::ImplementationBody::default(),
            interface_index: OnceCell::new(),
            impl_scopes_cache: OnceCell::new(),
        })
    }
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
            implementation_body: crate::ast_impl::ImplementationBody::default(),
            interface_index: OnceCell::new(),
            impl_scopes_cache: OnceCell::new(),
        }
    }

    /// Attach the whole implementation-section semantic AST (builder style, keeps
    /// [`Self::new`]'s positional signature stable). Set by
    /// [`crate::pipeline::build_unit_meta`] from the parse outcome. The flat
    /// `impl_scopes` table and the reliability gate are DERIVED from it on demand
    /// — nothing else to store. `reliable == false` (a construct the impl pass
    /// could not confidently track) is carried on `body.reliable`; the scope-
    /// resolution branch then ignores the derived table entirely (never a wrong
    /// local attribution).
    pub fn with_implementation_body(mut self, body: crate::ast_impl::ImplementationBody) -> Self {
        self.implementation_body = body;
        self
    }

    /// The flat implementation-routine table (params + locals + whole-body span
    /// per routine), DERIVED lazily from [`Self::implementation_body`] and cached.
    /// The single source of truth is the body AST; this view is rebuilt on first
    /// access (mirrors [`Self::interface`]). Same-unit local resolution walks it
    /// unchanged. MUST NOT be called from the weigher — it allocates and would
    /// build the `OnceCell` under moka's insert lock (ledger #29); the weigher
    /// charges the body structurally instead.
    pub fn impl_scopes(&self) -> &[ImplRoutine] {
        self.impl_scopes_cache
            .get_or_init(|| self.implementation_body.flatten_impl_routines())
    }

    /// True only when the WHOLE implementation-section pass completed cleanly (no
    /// recovery). DERIVED directly from [`Self::implementation_body`]`.reliable`:
    /// any construct the pass could not confidently track flips it false and the
    /// scope-resolution branch then ignores the derived flat table entirely — a
    /// WRONG `body_span` that mis-attributes a local is unacceptable, so we
    /// resolve nothing rather than risk it. True for an older/`new`-built meta
    /// with a default (empty) body (its empty table matches nothing, safe).
    pub fn impl_scopes_reliable(&self) -> bool {
        self.implementation_body.reliable
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
}

/// A READABLE serde projection of a [`UnitMeta`] for an on-demand debug dump.
///
/// The `UnitMeta` `Serialize` impl emits the bincode-envelope form (an opaque
/// `payload: Vec<u8>`), which is unreadable in YAML/JSON — so this struct BORROWS
/// the readable fields directly and derives a plain `Serialize`. Serializing it
/// under [`crate::meta::with_self_file_context`] makes every `CodeLocation` in the
/// unit's OWN file elide its file (span only) while an `{$I}`-include span carries
/// its path — readable either way, with the unit's own path present exactly once
/// (`source_path`). `QualifiedName` already omits its interned `key`.
#[derive(Serialize)]
struct AstYamlDump<'meta> {
    /// The unit's own source file path — present exactly once (self-file spans
    /// below elide it).
    source_path: &'meta Path,
    /// The interface AST (unit header, uses clauses, interface declarations).
    ast: &'meta Unit,
    /// The whole implementation-section semantic AST, RECONSTRUCTED into a
    /// readable nested tree from the flat expression arena (see
    /// [`impl_body_to_value`]). The durable in-RAM form is the flat arena
    /// ([`crate::ast_impl::ImplementationBody::expression_arena`]); only this dump
    /// rebuilds the nested shape so `ExprId` integers never leak into the YAML.
    /// Populated only for the ACTIVE editor unit; an indexed/imported meta
    /// carries an empty body.
    implementation_body: serde_json::Value,
    /// The unit's compile dependencies — useful context, kept.
    dependencies: &'meta [Dependency],
    /// Implementation-side identifier occurrences (find-references source).
    /// Included for completeness; can be noisy for large units.
    usages: &'meta [Usage],
}

// ─── YAML-dump reconstruction of the flattened expression tree ───────────────
//
// The implementation body stores its whole expression tree in ONE flat arena and
// references children by `ExprId` index (see `crate::ast_impl`). A naive derive
// would serialize that arena as a flat list plus integer child ids — unreadable.
// The functions below walk from each root `ExprId` back through the arena and
// rebuild the NESTED `serde_json::Value` tree the pre-flattening derive produced,
// so the dump stays a readable expression tree. Nothing here is durable — it runs
// only inside `dump_ast_yaml`, under the unit's self-file location context so the
// nested `CodeLocation`s self-elide exactly as before.

use crate::ast_impl::{Expression, ImplementationBody, Scope, Statement};
use serde_json::{json, Map, Value};

/// Serialize a value via `serde_json`, mapping any error to `null` (a dump is a
/// best-effort debug view; an unserializable leaf degrades to `null`, which
/// `prune_null_fields` then drops — never a hard failure).
fn to_value_or_null<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

/// Reconstruct one expression subtree (rooted at `id`) as a nested `Value`,
/// resolving `ExprId` children through `arena`. The variant/field names mirror
/// the `Expression` enum's derived serde shape (externally-tagged), so the dump
/// reads exactly like the pre-flattening tree.
fn expr_to_value(arena: &[Expression], id: crate::ast_impl::ExprId) -> Value {
    match &arena[id.0 as usize] {
        Expression::Identifier(name) => json!({ "Identifier": to_value_or_null(name) }),
        Expression::Member { receiver, member } => json!({
            "Member": {
                "receiver": expr_to_value(arena, *receiver),
                "member": to_value_or_null(member),
            }
        }),
        Expression::Call { callee, arguments, arguments_span } => json!({
            "Call": {
                "callee": expr_to_value(arena, *callee),
                "arguments": arguments.iter().map(|a| expr_to_value(arena, *a)).collect::<Vec<_>>(),
                "arguments_span": to_value_or_null(arguments_span),
            }
        }),
        Expression::Index { base, indices } => json!({
            "Index": {
                "base": expr_to_value(arena, *base),
                "indices": indices.iter().map(|i| expr_to_value(arena, *i)).collect::<Vec<_>>(),
            }
        }),
        Expression::Cast { type_name, operand } => json!({
            "Cast": {
                "type_name": to_value_or_null(type_name),
                "operand": expr_to_value(arena, *operand),
            }
        }),
        Expression::Inherited { method, keyword_location } => json!({
            "Inherited": {
                "method": to_value_or_null(method),
                "keyword_location": to_value_or_null(keyword_location),
            }
        }),
        Expression::Unary { operator, operand } => json!({
            "Unary": {
                "operator": to_value_or_null(operator),
                "operand": expr_to_value(arena, *operand),
            }
        }),
        Expression::Binary { operator, left, right } => json!({
            "Binary": {
                "operator": to_value_or_null(operator),
                "left": expr_to_value(arena, *left),
                "right": expr_to_value(arena, *right),
            }
        }),
        Expression::AnonymousMethod(scope) => {
            json!({ "AnonymousMethod": scope_to_value(arena, scope) })
        }
        Expression::SetOrArrayLiteral(location) => {
            json!({ "SetOrArrayLiteral": to_value_or_null(location) })
        }
        Expression::Literal(location) => json!({ "Literal": to_value_or_null(location) }),
        Expression::Parenthesized(inner) => {
            json!({ "Parenthesized": expr_to_value(arena, *inner) })
        }
    }
}

/// Reconstruct one statement as a nested `Value`, resolving expression `ExprId`s
/// through `arena` and recursing into child statement lists / scopes.
fn stmt_to_value(arena: &[Expression], statement: &Statement) -> Value {
    match statement {
        Statement::Expression(expression) => {
            json!({ "Expression": expr_to_value(arena, *expression) })
        }
        Statement::Assignment { target, value } => json!({
            "Assignment": {
                "target": expr_to_value(arena, *target),
                "value": expr_to_value(arena, *value),
            }
        }),
        Statement::LocalVar(symbol, initializer) => json!({
            "LocalVar": [
                to_value_or_null(symbol),
                initializer.map(|id| expr_to_value(arena, id)).unwrap_or(Value::Null),
            ]
        }),
        Statement::With { items, body } => json!({
            "With": {
                "items": items.iter().map(|i| expr_to_value(arena, *i)).collect::<Vec<_>>(),
                "body": body.iter().map(|s| stmt_to_value(arena, s)).collect::<Vec<_>>(),
            }
        }),
        Statement::ChildScope(scope) => json!({ "ChildScope": scope_to_value(arena, scope) }),
        Statement::Group(inner) => json!({
            "Group": inner.iter().map(|s| stmt_to_value(arena, s)).collect::<Vec<_>>()
        }),
        Statement::Opaque(location) => json!({ "Opaque": to_value_or_null(location) }),
    }
}

/// Reconstruct one scope as a nested `Value` (its metadata plus its recursively
/// rebuilt statement list).
fn scope_to_value(arena: &[Expression], scope: &Scope) -> Value {
    json!({
        "kind": to_value_or_null(&scope.kind),
        "span": to_value_or_null(&scope.span),
        "self_type_key": to_value_or_null(&scope.self_type_key),
        "declarations": to_value_or_null(&scope.declarations),
        "statements": scope.statements.iter().map(|s| stmt_to_value(arena, s)).collect::<Vec<_>>(),
    })
}

/// Reconstruct the whole implementation body as a readable nested `Value` (the
/// flat `expression_arena` is walked, never emitted). Field names mirror the
/// `ImplementationBody` derive so the dump shape is unchanged EXCEPT that the
/// arena is unfolded back into the nested tree and no `expression_arena` list
/// appears.
fn impl_body_to_value(body: &ImplementationBody) -> Value {
    let arena = &body.expression_arena;
    let routines = body
        .routines
        .iter()
        .map(|routine| {
            json!({
                "name": to_value_or_null(&routine.name),
                "owner_type_key": to_value_or_null(&routine.owner_type_key),
                "kind": to_value_or_null(&routine.kind),
                "scope": scope_to_value(arena, &routine.scope),
            })
        })
        .collect::<Vec<_>>();
    let statement_list = |list: &Option<crate::ast_impl::StatementList>| -> Value {
        match list {
            Some(statements) => Value::Array(
                statements.iter().map(|s| stmt_to_value(arena, s)).collect(),
            ),
            None => Value::Null,
        }
    };
    let mut map = Map::new();
    map.insert("routines".to_string(), Value::Array(routines));
    map.insert("initialization".to_string(), statement_list(&body.initialization));
    map.insert("finalization".to_string(), statement_list(&body.finalization));
    map.insert("reliable".to_string(), Value::Bool(body.reliable));
    Value::Object(map)
}

/// Serialize `meta` to a READABLE YAML string for an on-demand debug dump.
///
/// Unlike `serde_yaml::to_string(meta)` — which would emit the bincode-envelope
/// (an opaque `payload` byte array) because `UnitMeta`'s `Serialize` is the
/// durable-format impl — this serializes an [`AstYamlDump`] projection that
/// borrows the readable fields (interface AST + implementation body + deps +
/// usages). The serialize runs under the unit's self-file location context so
/// `CodeLocation`s self-elide: own-file spans show only their `span`, include
/// spans show their path. The unit's own path therefore appears exactly once, in
/// `source_path`. Any serialization error is mapped to a `String`.
///
/// The projection is serialized to a `serde_json::Value` FIRST, then rendered to
/// YAML from that value. `serde_yaml` 0.9 cannot serialize a Rust enum nested
/// directly inside another enum variant (a shape the AST uses freely, e.g. a
/// `TypeExpression` variant wrapping another enum), whereas `serde_json` can —
/// and the resulting `Value` is a plain map/seq/scalar tree `serde_yaml` renders
/// without hitting that limitation. Both steps run inside the same self-file
/// context so the elision applies to the `Value` build (where the nested
/// `CodeLocation`s actually serialize).
pub fn dump_ast_yaml(meta: &UnitMeta) -> Result<String, String> {
    crate::meta::with_self_file_context(meta.ast.name.location.file, || {
        // Reconstruct the nested body tree INSIDE the self-file context so its
        // `CodeLocation`s self-elide exactly like the rest of the projection.
        let dump = AstYamlDump {
            source_path: &meta.source_path,
            ast: &meta.ast,
            implementation_body: impl_body_to_value(&meta.implementation_body),
            dependencies: &meta.dependencies,
            usages: &meta.usages,
        };
        let mut value = serde_json::to_value(&dump).map_err(|error| error.to_string())?;
        // Drop `null` fields (an absent `Option`) — a dump full of
        // `source_file: null` / `initialization: null` is noise. `serde_json`'s
        // `preserve_order` feature keeps declaration order (interface before
        // implementation), so this only removes the null entries.
        prune_null_fields(&mut value);
        serde_yaml::to_string(&value).map_err(|error| error.to_string())
    })
}

/// Recursively remove every object entry whose value is `null` (an absent
/// `Option` field), in place. Purely cosmetic for the YAML dump — never touches
/// the durable `.unit` form.
fn prune_null_fields(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            map.retain(|_, child| !child.is_null());
            for child in map.values_mut() {
                prune_null_fields(child);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items.iter_mut() {
                prune_null_fields(item);
            }
        }
        _ => {}
    }
}

impl UnitMeta {

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

    /// Real-heap footprint estimate for the moka weigher — a proxy that must be
    /// `>=` the real resident heap of this meta so the byte cap actually bounds
    /// process RAM (Task 16 D). It must NEVER undercount: an undercount is
    /// exactly what let RAM blow past the "512MB cap" in the OOM this fixes.
    /// `UnitMeta` owns the WHOLE unit AST (nested `TypeExpression`s, member vecs,
    /// every declaration), whose heap grows roughly linearly with the source it
    /// was derived from — AND, in the LSP steady state, the query-populated
    /// `interface_index` (a `#[serde(skip)]` `OnceCell`) that a completion /
    /// `Declared` query builds AFTER moka has already fixed this entry's weight.
    /// That derived index is charged HERE, structurally, at insert time — so the
    /// weight already accounts for it before any query builds it.
    ///
    /// Three additive terms, all lower-bounds-or-more of real heap:
    ///   1. AST proxy: `source_len * AST_BYTES_PER_SOURCE_BYTE` — the parsed AST
    ///      costs many heap bytes per source byte (nodes, spans, interned refs,
    ///      owned member/section vecs). Measured ~14.6x on member-dense VCL-shaped
    ///      units; the factor is set ABOVE that (over-count is safe, under-count
    ///      is the bug). O(1), never touches the `OnceCell`.
    ///   2. Derived interface-index cost: counted STRUCTURALLY from the shallow
    ///      AST (declaration count + flattened member count via
    ///      [`shallow_member_count`]) times the real sizes of the
    ///      [`InterfaceSymbol`] / [`MemberSymbol`] they flatten into, WITHOUT
    ///      calling [`interface()`](Self::interface) (that would build the
    ///      `OnceCell` under moka's insert lock — forbidden, ledger #29). This is
    ///      the ~3.3x/byte the old weigher never charged; the reviewer's OOM
    ///      root cause.
    ///   3. Owned side tables: usages, dependencies, includes at real element
    ///      size — they scale with cross-unit references, not source length.
    ///
    /// FALLBACK: a meta with no recorded `source_len` (built via [`Self::new`]
    /// by an older caller / a test) replaces term 1 with a structural AST
    /// estimate that STILL scales with declaration + member counts (never a flat
    /// constant); terms 2 and 3 apply unchanged.
    pub fn estimated_bytes(&self) -> u32 {
        // Flattened member count across all interface declarations — the SAME
        // shallow traversal `build_interface`/`collect_member_symbols` use, so it
        // matches the real element count the derived index will hold, WITHOUT
        // building (or touching) the `interface_index` OnceCell (#29).
        let declaration_count = self.ast.interface_declarations.len();
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

        // Term 2: the derived interface_index the LSP populates on first query.
        // Each declaration flattens to one `InterfaceSymbol`, each member to one
        // `MemberSymbol`; both own small Vecs (members/attributes/directives)
        // whose backing allocations we approximate with a per-element slack so
        // the charge is a LOWER bound of the real derived heap, never an
        // over-count that would be corrected downward.
        let interface_index_estimate = declaration_count
            * (std::mem::size_of::<InterfaceSymbol>() + 32)
            + member_count * (std::mem::size_of::<MemberSymbol>() + 24);

        // Term 3: real per-element sizes of the owned side tables (these scale
        // with cross-unit references / occurrences, independent of source length).
        // implementation_body (S3): the whole scope tree — routines, methods,
        // nested + anonymous scopes, statements, expressions, declarations. Charged
        // as a per-node structural cost over a CHEAP allocation-free walk
        // ([`crate::ast_impl::ImplementationBody::node_count`]); it never touches
        // the lazy `impl_scopes_cache` OnceCell (that would build it under moka's
        // insert lock — forbidden, ledger #29). `BODY_BYTES_PER_NODE` is set above
        // the real per-node heap (each node owns boxed children / small vecs /
        // interned refs) so the charge is a lower-bound-or-more of real body heap:
        // an over-count only evicts a little earlier (safe); an under-count is the
        // OOM bug this guards. The DERIVED flat `impl_scopes` view a query later
        // builds is bounded by the same body (one `ImplRoutine` per routine scope,
        // its declarations a subset of the scope's), so this same charge also
        // covers that cache when it is populated.
        let implementation_body_cost =
            self.implementation_body.node_count() * Self::BODY_BYTES_PER_NODE;

        let side_tables = self.usages.len() * std::mem::size_of::<Usage>()
            + self.dependencies.len() * (std::mem::size_of::<Dependency>() + 96)
            + self.includes.len() * (std::mem::size_of::<SourceStamp>() + 96)
            + implementation_body_cost
            + self.source_path.as_os_str().len();

        // Term 1: the owned AST heap.
        let ast_estimate = if self.source_len > 0 {
            // Primary: source-length proxy for the owned AST heap.
            (self.source_len as usize) * Self::AST_BYTES_PER_SOURCE_BYTE
        } else {
            // Fallback (no source_len): a structural estimate that still scales
            // with the AST's declaration + member counts, generously weighted so
            // it does not undercount the nested TypeExpression heap it stands in
            // for. Never a flat constant.
            declaration_count * (256 + std::mem::size_of::<InterfaceSymbol>())
                + member_count * (128 + std::mem::size_of::<MemberSymbol>())
        };

        let total = 512 + ast_estimate + interface_index_estimate + side_tables;
        total.min(u32::MAX as usize) as u32
    }

    /// Heap bytes charged per source byte for the owned AST (Task 16 D). A
    /// parsed interface AST costs many heap bytes per source byte once nodes,
    /// spans, interned-identifier references and the owned member/section vecs
    /// are accounted for. The reviewer's RSS probe over member-dense VCL-shaped
    /// units MEASURED the real AST heap at ~14.6x source bytes; `16` is set
    /// deliberately ABOVE that so the weigher OVER-counts rather than under-counts
    /// — an over-count only evicts a little earlier (safe), an under-count is the
    /// exact bug that let RAM blow past the cap. Note this covers term 1 only; the
    /// derived `interface_index` (~3.3x/byte, ledger #29) is charged SEPARATELY
    /// and structurally in [`Self::estimated_bytes`]. Tuned together with the
    /// editor default capacity ([`crate::unit_cache::DEFAULT_CAPACITY_BYTES`]).
    pub const AST_BYTES_PER_SOURCE_BYTE: usize = 16;

    /// Heap bytes charged per implementation-body AST node (S3) in the weigher.
    /// Each node — a `Statement`, an `Expression`, a `Scope`, or a declaration —
    /// is an enum/struct that owns boxed recursive children, small `Vec`s
    /// (arguments, declarations, statement lists) and interned/dual-track name
    /// references. `size_of::<Statement>()` / `size_of::<Expression>()` alone are
    /// on the order of 40-64 bytes before their owned allocations; `96` is set
    /// deliberately above the per-node stack size so the per-node charge also
    /// covers those side allocations AND the derived flat `impl_scopes` view a
    /// query may later build (bounded by the same nodes). Over-count is safe
    /// (evicts a touch earlier); under-count is the OOM bug. Tuned together with
    /// [`Self::AST_BYTES_PER_SOURCE_BYTE`] and the editor default capacity.
    pub const BODY_BYTES_PER_NODE: usize = 96;
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
            ancestors: declaration
                .type_expression
                .as_ref()
                .map(ancestor_keys)
                .unwrap_or_default(),
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

/// The FOLDED type keys of a class/interface type's declared ancestors, in
/// source order — the derived `InterfaceSymbol::ancestors` field. Each
/// `Ancestor.name` is a `QualifiedName` whose `.key` is the folded key of its
/// LAST segment (`System.Classes.TComponent` → `TComponent`'s key), which is
/// exactly what the name-keyed interface index resolves against. Every other
/// shape (record/enum/alias/…) carries no ancestor list → empty.
fn ancestor_keys(type_expression: &TypeExpression) -> Vec<Identifier> {
    match type_expression {
        TypeExpression::Class(class_type) => {
            class_type.ancestors.iter().map(|ancestor| ancestor.name.key).collect()
        }
        TypeExpression::Interface(interface_type) => {
            interface_type.ancestors.iter().map(|ancestor| ancestor.name.key).collect()
        }
        _ => Vec::new(),
    }
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
                Member::Field(field) => field.names.len(),
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
            Member::Field(field) => {
                let type_key = simple_type_key(&field.field_type);
                for name in &field.names {
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
                        attributes: attribute_keys(&field.attributes),
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
            Member::NestedConst(nested) => members.push(MemberSymbol {
                name: nested.name.name,
                key: nested.name.key,
                kind: MemberKind::NestedConst,
                location: nested.name.location,
                read_target: None,
                write_target: None,
                type_key: None,
                directives: Vec::new(),
                visibility,
                strict,
                attributes: attribute_keys(&nested.attributes),
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

    /// MEMORY FIX: the `implementation_body` is `#[serde(skip)]` — working-set
    /// state for the active editor unit ONLY, NEVER persisted. Before a round-trip
    /// the body is populated (this meta was built with a body) and the DERIVED flat
    /// `impl_scopes` view reflects it; AFTER a bincode round-trip the body is EMPTY
    /// (skipped, so a cross-unit reload can never drag a body into RAM), and the
    /// derived flat table is correspondingly empty. The interface + flat usages are
    /// what persist and are asserted intact elsewhere; this test locks the
    /// body-not-persisted invariant that bounds memory.
    #[test]
    fn implementation_body_is_not_persisted_across_serde_round_trip() {
        let directory = std::env::temp_dir().join("delphi_parser_impl_scopes_serde");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("Scoped.pas");
        std::fs::write(
            &path,
            "unit Scoped;\ninterface\nimplementation\n\
             procedure TThing.Run;\nvar Local: TThing;\nbegin\n  Local.Free;\nend;\nend.",
        )
        .unwrap();

        // Parse through the global arena, capturing the body the pass built.
        let arena = crate::globals::arena();
        let context = test_context();
        let file = arena.load(&path).unwrap();
        let mut outcome = parse_file_full(arena, context, file, None).unwrap();
        let Some(Source::Unit(unit)) = outcome.source.take() else {
            panic!("expected unit");
        };
        let source_hash = crate::unit_cache::hash_file(&path).unwrap();
        let meta = UnitMeta::new(
            unit,
            outcome.cycle_tainted,
            path.to_path_buf(),
            source_hash,
            Vec::new(),
            outcome.dependencies,
            outcome.usages,
        )
        .with_implementation_body(outcome.implementation_body);

        // The DERIVED flat table matches the body before save.
        assert_eq!(meta.impl_scopes().len(), 1, "one routine derived before save");
        assert!(meta.impl_scopes_reliable());
        assert_eq!(meta.implementation_body.routines.len(), 1);

        let bytes = bincode::serialize(&meta).unwrap();
        let restored: UnitMeta = bincode::deserialize(&bytes).unwrap();
        // The body did NOT survive — it is working-set state for the active unit
        // only, never dragged into RAM by a cross-unit reload.
        assert!(
            restored.implementation_body.routines.is_empty(),
            "the body must NOT survive the round-trip (memory fix: bodies are active-unit-only)"
        );
        // The DERIVED flat view is correspondingly empty for a bodyless meta —
        // local resolution is active-unit-only, so this yields nothing (safe).
        assert!(restored.impl_scopes().is_empty());
        // A bodyless meta's reliability gate defaults true (its empty table matches
        // nothing), so no wrong local attribution is possible.
        assert!(restored.impl_scopes_reliable());
    }

    /// MEMORY FIX, real compressed segment path (`serialize_meta` →
    /// `decode_segment`): a non-trivial body (several routines, a nested + an
    /// anonymous scope, locals) is present on the live meta before save but is
    /// ABSENT after the segment round-trip — the body is `#[serde(skip)]` so the
    /// durable `.unit` segment carries interface + flat usages only and a reload
    /// can never restore a body. The derived flat `impl_scopes` is empty for the
    /// reloaded bodyless meta.
    #[test]
    fn implementation_body_is_not_persisted_through_segment_round_trip() {
        let directory = std::env::temp_dir().join("delphi_parser_impl_body_segment");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("Bodies.pas");
        std::fs::write(
            &path,
            "unit Bodies;\ninterface\nimplementation\n\
             procedure Alpha;\nvar A: Integer;\n\
             \n  procedure Nested;\n  var N: Integer;\n  begin\n    N := A;\n  end;\n\
             begin\n  Nested;\nend;\n\
             function Beta(P: Integer): Integer;\nvar Cb: TProc;\nbegin\n\
               Cb := procedure var Local: Integer; begin Local := P; end;\n\
               Result := P;\nend;\n\
             end.",
        )
        .unwrap();

        let arena = crate::globals::arena();
        let context = test_context();
        let file = arena.load(&path).unwrap();
        let mut outcome = parse_file_full(arena, context, file, None).unwrap();
        let Some(Source::Unit(unit)) = outcome.source.take() else {
            panic!("expected unit");
        };
        let source_hash = crate::unit_cache::hash_file(&path).unwrap();
        let meta = UnitMeta::new(
            unit,
            outcome.cycle_tainted,
            path.to_path_buf(),
            source_hash,
            Vec::new(),
            outcome.dependencies,
            outcome.usages,
        )
        .with_implementation_body(outcome.implementation_body);

        // Two top-level routines captured.
        assert_eq!(meta.implementation_body.routines.len(), 2, "Alpha + Beta");
        assert!(meta.implementation_body.reliable);
        // Alpha's scope has a nested routine ChildScope; the derived flat table
        // therefore holds Alpha + its Nested (2 entries from Alpha alone).
        let flat_before = meta.impl_scopes().len();
        assert!(flat_before >= 3, "Alpha + Nested + Beta at least, got {flat_before}");

        // Round-trip through the real compressed segment path.
        let segment = crate::unit_cache::serialize_meta(&meta).unwrap();
        let restored = crate::unit_cache::decode_segment_for_test(&segment)
            .expect("segment decodes under the bumped format");

        // The body did NOT survive — it is #[serde(skip)], so the durable segment
        // carries interface + flat usages only; a reload restores no body.
        assert!(
            restored.implementation_body.routines.is_empty(),
            "the body must NOT survive the segment round-trip (bodies are active-unit-only)"
        );
        // The DERIVED flat view is empty for the reloaded bodyless meta (the live
        // meta had {flat_before} entries; a reload never restores them).
        let _ = flat_before;
        assert!(restored.impl_scopes().is_empty());
        assert!(restored.impl_scopes_reliable());
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

    /// `dump_ast_yaml` produces a READABLE YAML projection (not the bincode
    /// envelope): it parses as YAML, contains the unit name and the `ast` /
    /// `implementation_body` keys, and self-elides the unit's own path — the full
    /// source path appears AT MOST ONCE (in `source_path`), proving the self-file
    /// location context ran and every own-file span emitted a span only.
    #[test]
    fn dump_ast_yaml_is_readable_and_self_elides_path() {
        let directory = std::env::temp_dir().join("delphi_parser_unit_meta_yaml");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("Dumped.pas");
        std::fs::write(
            &path,
            "unit Dumped;\ninterface\nprocedure Run;\nimplementation\n\
             procedure Run;\nvar Local: Integer;\nbegin\n  Local := 1;\nend;\nend.",
        )
        .unwrap();

        // Parse WITH the implementation body (reuse the same path as the memory
        // tests) so the dump carries a populated `implementation_body`.
        let arena = crate::globals::arena();
        let context = test_context();
        let file = arena.load(&path).unwrap();
        let mut outcome = parse_file_full(arena, context, file, None).unwrap();
        let Some(Source::Unit(unit)) = outcome.source.take() else {
            panic!("expected unit");
        };
        let source_hash = crate::unit_cache::hash_file(&path).unwrap();
        let meta = UnitMeta::new(
            unit,
            outcome.cycle_tainted,
            path.to_path_buf(),
            source_hash,
            Vec::new(),
            outcome.dependencies,
            outcome.usages,
        )
        .with_implementation_body(outcome.implementation_body);
        assert!(
            !meta.implementation_body.routines.is_empty(),
            "the parsed unit has an implementation-section routine"
        );

        let yaml = dump_ast_yaml(&meta).expect("the dump serializes to YAML");

        // It is valid YAML.
        let value: serde_yaml::Value =
            serde_yaml::from_str(&yaml).expect("the dump is valid YAML");
        let map = value.as_mapping().expect("the dump is a YAML mapping");
        assert!(
            map.contains_key(serde_yaml::Value::from("ast")),
            "the dump has an `ast` key"
        );
        assert!(
            map.contains_key(serde_yaml::Value::from("implementation_body")),
            "the dump has an `implementation_body` key"
        );
        assert!(
            map.contains_key(serde_yaml::Value::from("source_path")),
            "the dump has a `source_path` key"
        );

        // The unit name appears (identifiers fold to a canonical upper case).
        assert!(
            yaml.to_ascii_uppercase().contains("DUMPED"),
            "the dump contains the unit name"
        );

        // Self-elision: the full source path is present AT MOST ONCE (only in
        // `source_path`). Every own-file span serialized as a span-only variant,
        // so its path is not repeated. Compare on the canonical path string the
        // dump would emit for `source_path`.
        let source_path_string = path.to_string_lossy();
        let occurrences = yaml.matches(source_path_string.as_ref()).count();
        assert!(
            occurrences <= 1,
            "the self path must appear at most once (in source_path), found {occurrences}"
        );
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

    /// Task 16 (weigher review): `estimated_bytes` must NOT undercount. Two
    /// guarantees, either of which failing is the OOM regression:
    ///   1. a large member-dense unit charges at LEAST its measured lower bound —
    ///      `AST_BYTES_PER_SOURCE_BYTE * source_len` for the AST proxy PLUS a
    ///      per-declaration + per-member term for the derived interface index
    ///      (charged structurally, without building the OnceCell);
    ///   2. a big unit weighs FAR more (>10x) than a tiny one — the weight tracks
    ///      real content, it never collapses toward a constant.
    #[test]
    fn estimated_bytes_does_not_undercount_a_member_dense_unit() {
        let directory = std::env::temp_dir().join("delphi_parser_weigher_undercount");
        std::fs::create_dir_all(&directory).unwrap();

        // A member-dense unit: many types, each with many fields/methods — the
        // exact shape whose derived interface index the old weigher ignored.
        let mut source = String::from("unit Dense;\ninterface\n");
        for type_index in 0..60 {
            source.push_str(&format!("type TThing{type_index} = class\n"));
            for field_index in 0..20 {
                source.push_str(&format!("  Field{field_index}: Integer;\n"));
            }
            for method_index in 0..10 {
                source.push_str(&format!("  procedure Method{method_index};\n"));
            }
            source.push_str("end;\n");
        }
        source.push_str("implementation\nend.");
        let path = directory.join("Dense.pas");
        std::fs::write(&path, &source).unwrap();

        let source_len = source.len() as u32;
        let meta = parse_meta(&path).with_source_len(source_len);

        // Structural counts (WITHOUT building the interface index) — the same
        // shallow traversal the weigher uses. Cross-checked here so an undercount
        // in either the AST proxy OR the interface-index term fails the suite.
        let declaration_count = meta.ast.interface_declarations.len();
        let member_count: usize = meta
            .ast
            .interface_declarations
            .iter()
            .map(|declaration| {
                declaration
                    .type_expression
                    .as_ref()
                    .map(super::shallow_member_count)
                    .unwrap_or(0)
            })
            .sum();
        assert!(declaration_count >= 60, "sanity: many declarations parsed");
        assert!(member_count >= 60 * 30, "sanity: ~1800 flattened members parsed");

        // Lower bound the weigher must MEET OR EXCEED: the AST proxy at the
        // measured-and-then-some factor, PLUS a per-declaration and per-member
        // term for the derived interface index (its raw struct sizes — a strict
        // lower bound on the real index heap it stands in for).
        let ast_lower_bound =
            source_len as usize * UnitMeta::AST_BYTES_PER_SOURCE_BYTE;
        let index_lower_bound = declaration_count * std::mem::size_of::<InterfaceSymbol>()
            + member_count * std::mem::size_of::<MemberSymbol>();
        let lower_bound = ast_lower_bound + index_lower_bound;

        let charged = meta.estimated_bytes() as usize;
        assert!(
            charged >= lower_bound,
            "weigher undercounts: charged {charged} < lower bound {lower_bound} \
             (ast {ast_lower_bound} + index {index_lower_bound}); an undercount is \
             the OOM regression this test guards"
        );

        // The AST factor itself must be at least 16x (>= the measured ~14.6x
        // real heap) — the second half of the non-undercount guarantee.
        assert!(
            UnitMeta::AST_BYTES_PER_SOURCE_BYTE >= 16,
            "AST_BYTES_PER_SOURCE_BYTE must not fall below the measured ~14.6x"
        );

        // Building the interface index AFTER weighing must not exceed what we
        // already charged for it structurally — proves term 2 covers the
        // query-populated index (the uncounted ~3.3x/byte the OOM blamed).
        let real_index_members: usize =
            meta.interface().symbols.iter().map(|symbol| symbol.members.len()).sum();
        assert_eq!(
            real_index_members, member_count,
            "the structural member count the weigher uses must equal the index's \
             real flattened member count — else the charge is for the wrong shape"
        );
    }

    /// A big unit must weigh FAR more than a tiny one — the weight tracks real
    /// content and never collapses toward a constant (the undercount failure
    /// mode is a weight that barely moves with size).
    #[test]
    fn estimated_bytes_big_unit_dwarfs_tiny_unit() {
        let directory = std::env::temp_dir().join("delphi_parser_weigher_ratio");
        std::fs::create_dir_all(&directory).unwrap();

        let tiny_path = directory.join("Tiny.pas");
        std::fs::write(
            &tiny_path,
            "unit Tiny;\ninterface\nconst X = 1;\nimplementation\nend.",
        )
        .unwrap();
        let tiny_source = std::fs::read_to_string(&tiny_path).unwrap();
        let tiny = parse_meta(&tiny_path).with_source_len(tiny_source.len() as u32);

        let mut big_source = String::from("unit Big;\ninterface\n");
        for type_index in 0..80 {
            big_source.push_str(&format!("type TBig{type_index} = class\n"));
            for field_index in 0..15 {
                big_source.push_str(&format!("  F{field_index}: Integer;\n"));
            }
            big_source.push_str("end;\n");
        }
        big_source.push_str("implementation\nend.");
        let big_path = directory.join("Big.pas");
        std::fs::write(&big_path, &big_source).unwrap();
        let big = parse_meta(&big_path).with_source_len(big_source.len() as u32);

        let tiny_bytes = tiny.estimated_bytes() as u64;
        let big_bytes = big.estimated_bytes() as u64;
        assert!(
            big_bytes > tiny_bytes * 10,
            "a big member-dense unit must weigh >>10x a tiny one, got big={big_bytes} \
             tiny={tiny_bytes}"
        );
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
        let Member::Field(field) = &class_type.sections[0].members[0] else {
            panic!("expected field");
        };
        assert_eq!(crate::globals::resolve(field.attributes[0].name.name), "Weak");
    }

    // ─── v18 slimmed-format round-trip correctness ─────────────────────────

    /// Change 1: `QualifiedName.key` is NOT serialized; it is re-derived from
    /// `name`'s display string on load via the deterministic fold-intern. After a
    /// round-trip the key must equal the ORIGINAL key for BOTH an ASCII and a
    /// mixed-case identifier — the invariant `key == intern_key(resolve(name))`
    /// is restored byte-for-byte.
    #[test]
    fn qualified_name_key_rederives_across_round_trip() {
        use crate::ast::QualifiedName;
        use crate::meta::{CodeLocation, Span};

        // Register a real file so the location's FileId round-trips.
        let directory = std::env::temp_dir().join("delphi_parser_qn_key");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("QnKey.pas");
        std::fs::write(&path, "unit QnKey;").unwrap();
        let file = crate::globals::arena().register(&path).unwrap();
        let location = CodeLocation { file, span: Span::new(0, 5) };

        for text in ["System", "MixedCaseName"] {
            let original = QualifiedName {
                name: crate::globals::intern(text),
                key: crate::globals::intern_key(text),
                location,
            };
            // key is the fold of name (the type invariant)
            assert_eq!(original.key, crate::globals::intern_key(text));

            // A bare QualifiedName round-trips OUTSIDE a UnitMeta context
            // (CodeLocation falls back to the Full form).
            let bytes = bincode::serialize(&original).unwrap();
            let restored: QualifiedName = bincode::deserialize(&bytes).unwrap();

            assert_eq!(
                restored.key, original.key,
                "re-derived key must equal the original for {text:?}"
            );
            assert_eq!(restored.name, original.name);
            assert_eq!(
                crate::globals::resolve(restored.key),
                crate::globals::fold_identifier(text)
            );
            assert_eq!(restored.location, original.location);
        }
    }

    /// Collect every `CodeLocation` reachable from a meta that this format's
    /// elision touches: the unit name, each interface declaration name (and its
    /// members' locations via the derived interface), and each usage.
    fn collect_locations(meta: &UnitMeta) -> Vec<crate::meta::CodeLocation> {
        let mut locations = vec![meta.ast.name.location];
        for declaration in &meta.ast.interface_declarations {
            locations.push(declaration.name.location);
        }
        for symbol in &meta.interface().symbols {
            locations.push(symbol.location);
            for member in &symbol.members {
                locations.push(member.location);
            }
        }
        for usage in &meta.usages {
            locations.push(usage.location);
        }
        locations
    }

    /// NEVER-WRONG round-trip: a self-only unit (no includes → EMPTY include
    /// table). Through the REAL compressed segment path (`serialize_meta` →
    /// `decode_segment`), every `CodeLocation`'s resolved file PATH and span must
    /// be byte-identical before/after, and every `key` must match. The self file
    /// is elided on the wire (span-only) yet resolves back to the SAME path.
    #[test]
    fn self_only_unit_round_trips_every_location() {
        let directory = std::env::temp_dir().join("delphi_parser_v18_self_only");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("SelfOnly.pas");
        std::fs::write(
            &path,
            "unit SelfOnly;\ninterface\n\
             type TThing = class\n  FValue: Integer;\n  procedure Go;\nend;\n\
             const MaxThings = 3;\n\
             implementation\nend.",
        )
        .unwrap();

        let meta = parse_meta(&path);

        // Snapshot each location's resolved PATH + span + the name keys BEFORE.
        let before: Vec<(std::path::PathBuf, crate::meta::Span)> = collect_locations(&meta)
            .into_iter()
            .map(|location| {
                (
                    crate::globals::arena().path(location.file).to_path_buf(),
                    location.span,
                )
            })
            .collect();
        let name_key_before = meta.ast.name.key;
        let decl_keys_before: Vec<_> = meta
            .ast
            .interface_declarations
            .iter()
            .map(|declaration| declaration.name.key)
            .collect();

        // Real segment round-trip.
        let segment = crate::unit_cache::serialize_meta(&meta).unwrap();
        let restored = crate::unit_cache::decode_segment_for_test(&segment)
            .expect("segment decodes under v18");

        let after: Vec<(std::path::PathBuf, crate::meta::Span)> = collect_locations(&restored)
            .into_iter()
            .map(|location| {
                (
                    crate::globals::arena().path(location.file).to_path_buf(),
                    location.span,
                )
            })
            .collect();

        assert_eq!(
            before, after,
            "every CodeLocation's resolved PATH + span must survive the round-trip byte-identical"
        );
        // Every location resolves to the SELF file (the unit's own path).
        let self_path = crate::globals::arena().path(meta.ast.name.location.file).to_path_buf();
        assert!(
            after.iter().all(|(resolved, _)| resolved == &self_path),
            "a self-only unit's locations must all resolve to its own file"
        );
        // keys re-derived correctly
        assert_eq!(restored.ast.name.key, name_key_before);
        let decl_keys_after: Vec<_> = restored
            .ast
            .interface_declarations
            .iter()
            .map(|declaration| declaration.name.key)
            .collect();
        assert_eq!(decl_keys_after, decl_keys_before);
    }

    /// NEVER-WRONG round-trip with an INCLUDE: a unit carrying a `CodeLocation`
    /// in a DIFFERENT file (an `{$I}` include) — the include-table path. Through
    /// the real segment path, the self-file locations must resolve back to the
    /// unit's own file and the include-file location back to the INCLUDE's file,
    /// each with its span intact. A location resolving to the WRONG file after
    /// load is exactly the wrong go-to this guards against.
    #[test]
    fn with_include_unit_round_trips_self_and_include_locations() {
        use crate::meta::{CodeLocation, Span};

        let directory = std::env::temp_dir().join("delphi_parser_v18_with_include");
        std::fs::create_dir_all(&directory).unwrap();
        let unit_path = directory.join("WithInc.pas");
        std::fs::write(
            &unit_path,
            "unit WithInc;\ninterface\nconst K = 1;\nimplementation\nend.",
        )
        .unwrap();
        // A REAL include file so its FileId registers on load.
        let include_path = directory.join("Shared.inc");
        std::fs::write(&include_path, "const Shared = 42;").unwrap();

        let mut meta = parse_meta(&unit_path);
        let self_file = meta.ast.name.location.file;
        let include_file = crate::globals::arena().register(&include_path).unwrap();
        assert_ne!(self_file, include_file, "distinct files for the test");

        // Inject a usage whose location is in the INCLUDE file, and one in self.
        let include_span = Span::new(6, 12);
        let self_span = Span::new(0, 4);
        meta.usages = vec![
            Usage {
                symbol: crate::globals::intern_key("SelfSym"),
                location: CodeLocation { file: self_file, span: self_span },
            },
            Usage {
                symbol: crate::globals::intern_key("IncSym"),
                location: CodeLocation { file: include_file, span: include_span },
            },
        ];

        let self_path = crate::globals::arena().path(self_file).to_path_buf();
        let include_resolved = crate::globals::arena().path(include_file).to_path_buf();

        let segment = crate::unit_cache::serialize_meta(&meta).unwrap();
        let restored = crate::unit_cache::decode_segment_for_test(&segment)
            .expect("segment decodes under v18");

        // The unit name (self) still resolves to the unit's own file.
        assert_eq!(
            crate::globals::arena().path(restored.ast.name.location.file),
            self_path.as_path()
        );

        // Usages: the self usage resolves to the self file; the include usage
        // resolves to the INCLUDE file — NOT the self file (the wrong-go-to bug).
        let restored_self = &restored.usages[0];
        assert_eq!(
            crate::globals::arena().path(restored_self.location.file),
            self_path.as_path()
        );
        assert_eq!(restored_self.location.span, self_span);

        let restored_include = &restored.usages[1];
        assert_eq!(
            crate::globals::arena().path(restored_include.location.file),
            include_resolved.as_path(),
            "an include-file location MUST resolve back to the include file, never the self file"
        );
        assert_eq!(restored_include.location.span, include_span);
        assert_ne!(
            restored_include.location.file, restored_self.location.file,
            "self and include files must stay distinct after load"
        );
    }
}
