//! LSP query API surface over cached [`UnitMeta`]s (task 5, Deliverable A).
//!
//! This is the query layer delphi-devkit consumes to answer
//! `textDocument/{definition,references,completion,hover}` — the LSP *server*
//! itself is NOT built here (SESSION decision); these methods return owned,
//! location-bearing results the devkit maps to LSP types.
//!
//! GOVERNING RULE (same family as scoped `Declared`/SizeOf): a query must never
//! return a WRONG answer. Insufficient information yields empty/none, never a
//! guess. Definition/references resolve through the SAME dependency-recorded,
//! cycle-safe machinery as scoped `Declared` — an unresolved target is
//! no-result, never a wrong location.
//!
//! The query types live here; the methods that need cache/loader/arena access
//! live on [`crate::driver::ProjectSession`].

use crate::ast::Visibility;
use crate::context::Identifier;
use crate::meta::CodeLocation;
use crate::token_cursor::Severity;
use crate::unit_cache::{MemberKind, SymbolKind};

/// What the identifier under a cursor position resolves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind {
    /// The declaring occurrence of an interface symbol (`type TFoo = …`).
    Declaration,
    /// The declaring occurrence of a member (`FBar: Integer;` inside a type).
    Member,
    /// A use of some identifier (interface body reference or implementation
    /// occurrence). Over-approximating: the usage index does not yet resolve
    /// scopes, so this is a candidate identity (its folded key), not a proven
    /// binding.
    Usage,
}

/// The identifier occurrence under a byte position: the folded lookup key, its
/// display spelling, what kind of thing it is and its exact source span.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueryTarget {
    /// Case-folded lookup key — feeds `definition`/`references`.
    pub key: Identifier,
    /// Display spelling (as written at this occurrence).
    pub display: Identifier,
    pub kind: TargetKind,
    /// Exact source span of the occurrence.
    pub location: CodeLocation,
    /// When the occurrence is a member (or a `Type.Member` usage), the owning
    /// type's folded key — needed to resolve a member definition. `None` for a
    /// top-level symbol or an unqualified usage.
    pub owner_type: Option<Identifier>,
}

/// One completion candidate. Carries the rich member/symbol facts from the
/// derived interface index (task 2) so the devkit can render kind/type/detail.
#[derive(Debug, Clone)]
pub struct Completion {
    /// Display spelling to insert/show.
    pub display: Identifier,
    /// Folded key (de-duplication identity).
    pub key: Identifier,
    pub kind: CompletionKind,
    /// Declared simple type key (field/property/return type), when known.
    pub type_key: Option<Identifier>,
    /// Method directive keys (`virtual`/`override`/…), empty for non-methods.
    pub directives: Vec<Identifier>,
    /// Member visibility (only meaningful for member completions).
    pub visibility: Visibility,
}

/// The kind of a completion candidate — either a top-level symbol kind or a
/// member kind, unified so the devkit maps one enum to LSP `CompletionItemKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionKind {
    Symbol(SymbolKind),
    Member(MemberKind),
    /// A compiler built-in surfaced at the top level (`Integer`, `string`, …).
    Builtin,
}

/// The declared facts of the symbol under a cursor, for `textDocument/hover`.
///
/// Resolved through the SAME cross-unit machinery as [`crate::driver::ProjectSession::definition`]:
/// a hover over an imported symbol carries the IMPORTED declaration's facts (its
/// kind, declared type, directives, visibility), not a guess. The never-wrong
/// rule holds: a cursor that resolves to no interface declaration (an unknown
/// identifier, an implementation-only local) yields `None`, never fabricated
/// facts. When the declared type is anonymous/complex the parser does not
/// capture a simple key for (`type_key` is `None`), the devkit shows the KIND
/// only — never an invented type string.
#[derive(Debug, Clone)]
pub struct HoverInfo {
    /// Display spelling of the resolved declaration (as written at its
    /// declaration site).
    pub display: Identifier,
    /// What the symbol is — a top-level symbol kind or a member kind.
    pub kind: CompletionKind,
    /// The declared simple type key (field/property/var/const type or a
    /// method's return type), when the parser captured one. `None` for an
    /// anonymous/complex type or a symbol with no meaningful type (a type
    /// declaration, a procedure) — the devkit then shows kind only.
    pub type_key: Option<Identifier>,
    /// Method directive keys (`virtual`/`override`/`abstract`/…), in source
    /// order. Empty for non-methods.
    pub directives: Vec<Identifier>,
    /// The declaration's visibility (only meaningful for a member; a top-level
    /// symbol carries `Unspecified`).
    pub visibility: Visibility,
    /// The owning type's DISPLAY name when the symbol is a member
    /// (`Owner.Member`) — as written at the owner's declaration, so the hover
    /// reads `TUser.Greet`, not the folded lookup key `TUSER.Greet`. `None` for
    /// a top-level symbol.
    pub owner_type: Option<Identifier>,
    /// The span of the OCCURRENCE under the cursor (not the declaration) — the
    /// devkit maps this to the hover's highlight range in the requesting
    /// document.
    pub occurrence: CodeLocation,
}

/// A resolved routine signature for `textDocument/signatureHelp`, read from the
/// AST's [`crate::ast::RoutineType`] (parameters + return type).
///
/// Never fabricated: [`crate::driver::ProjectSession::signature_help`] returns
/// `None` when the callee does not resolve to a routine (an unknown name, a
/// non-routine symbol, a member on an unresolved owner). A procedure carries
/// `return_type = None`. Parameter labels are built from the declared parameter
/// modifier/names/type as written (display track via `globals::resolve`), with
/// an untyped parameter (`var Buffer`) rendered without a `: Type` and a
/// defaulted parameter (`X: Integer = 0`) carrying its ` = default`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureInfo {
    /// The whole signature line, e.g.
    /// `function Compute(const A: Integer; B: string = ''): Boolean` — built from
    /// the resolved parts, never a fabrication.
    pub label: String,
    /// One entry per PARAMETER GROUP name (a `const A, B: Integer` group yields
    /// two parameters `const A: Integer` and `const B: Integer`), in source
    /// order. The `label` of each is a substring of [`Self::label`] so the editor
    /// can highlight the active parameter.
    pub parameters: Vec<ParameterInfo>,
    /// The return type display, `None` for a procedure/constructor/destructor
    /// (no return) or a function whose return type is anonymous/complex and could
    /// not be rendered (never fabricated).
    pub return_type: Option<String>,
}

/// One parameter of a [`SignatureInfo`]: its rendered label
/// (`const Name: Type`, `var Buffer`, `X: Integer = 0`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParameterInfo {
    pub label: String,
}

/// Where a diagnostic came from, so the devkit can group/filter them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticSource {
    /// A cursor/parser finding (unknown `{$IF}`, dropped attribute, recovery
    /// resync, lexer error in an active region).
    Parse,
    /// A DFM↔PAS linker finding (dangling component, missing handler, …).
    Dfm,
    /// A cross-unit ANALYSIS finding, computed on demand rather than during the
    /// parse (currently the conservative unused-uses hint). Kept distinct from
    /// [`Self::Parse`] so the devkit can group/filter "you might remove this"
    /// advice separately from real parse findings.
    Analysis,
}

/// One diagnostic in the unit's unified list (parse + dfm), for
/// `textDocument/publishDiagnostics`.
#[derive(Debug, Clone)]
pub struct UnifiedDiagnostic {
    pub source: DiagnosticSource,
    /// Per-finding severity, set at the creation site (a syntax error is an
    /// Error, an unknown `{$IF}` a Warning, a benign note Information, an
    /// unused-uses candidate a Hint). The server maps this 1:1 onto LSP
    /// `DiagnosticSeverity` — it is NOT a blanket default.
    pub severity: Severity,
    /// The `.pas` source location the finding refers to, when one exists. Parse
    /// findings always carry one. A DFM finding carries a pas location only when
    /// it names a concrete pas member (e.g. a type mismatch points at the
    /// field); a finding whose only anchor is a byte offset INTO the dfm file
    /// leaves this `None` (its dfm-side offset is exposed via [`Self::dfm_offset`])
    /// — never a fabricated pas location (the never-wrong rule).
    pub location: Option<CodeLocation>,
    /// For a DFM finding, the byte offset into the dfm file. `None` for parse
    /// findings.
    pub dfm_offset: Option<usize>,
    pub message: String,
}
