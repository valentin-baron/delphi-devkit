//! Format a parser [`HoverInfo`] into LSP hover markdown, honestly.
//!
//! The parser resolves the symbol under the cursor to its declared facts (kind,
//! declared type, method directives, visibility, owning type) through the same
//! cross-unit machinery as go-to-definition. This module renders those facts —
//! and ONLY those facts — as a fenced `delphi` signature line. It never invents
//! a type: when the parser did not reduce the declared type to a simple key
//! (an anonymous record, an inline array, a procedural type), `type_key` is
//! `None` and the rendered line shows the KIND alone, not a fabricated type.
//!
//! All identifier strings come from the global interner via `globals::resolve`
//! (the display track — as written at the declaration).

use delphi_parser::ast::Visibility;
use delphi_parser::globals;
use delphi_parser::query::{CompletionKind, HoverInfo};
use delphi_parser::unit_cache::{MemberKind, SymbolKind};

/// Render `info` as a fenced `delphi` signature. Returns the markdown body of
/// the hover. Never fabricates a type it cannot derive.
pub fn format_hover(info: &HoverInfo) -> String {
    let name = globals::resolve(info.display);
    let signature = match info.kind {
        CompletionKind::Symbol(kind) => format_symbol(kind, name),
        CompletionKind::Member(kind) => format_member(info, kind, name),
        CompletionKind::Builtin => format!("type {name}"),
    };
    // A fenced delphi block so the editor syntax-highlights the signature.
    format!("```delphi\n{signature}\n```")
}

/// A top-level symbol: `type TFoo`, `const MaxThings`, `var Counter`,
/// `procedure DoThing`, `function Compute`. The derived interface index does
/// not carry a top-level symbol's own type reference, so no `: T` is appended
/// here — showing kind + name only, never an invented type.
fn format_symbol(kind: SymbolKind, name: &str) -> String {
    let keyword = match kind {
        SymbolKind::Type => "type",
        SymbolKind::Const => "const",
        SymbolKind::ResourceString => "resourcestring",
        SymbolKind::Var => "var",
        SymbolKind::ThreadVar => "threadvar",
        SymbolKind::Procedure => "procedure",
        SymbolKind::Function => "function",
    };
    format!("{keyword} {name}")
}

/// A type member. Qualifies with the owning type (`TFoo.Bar`) and renders the
/// facts the parser captured:
/// - a field/property with a known simple type → `Owner.Name: T;`;
/// - a method → `procedure Owner.Name; virtual;` (return type unknown from the
///   member index → no `: T`, so a function still renders as `procedure`-shaped
///   without a fabricated return type — the directives ARE known and shown);
/// - an anonymous/complex type (`type_key` None) → kind + qualified name only.
/// A leading visibility keyword is prefixed as a comment-free modifier when it
/// is a meaningful, non-`Unspecified` value.
fn format_member(info: &HoverInfo, kind: MemberKind, name: &str) -> String {
    let qualified = match info.owner_type {
        Some(owner) => format!("{}.{}", globals::resolve(owner), name),
        None => name.to_string(),
    };
    let visibility = visibility_prefix(info.visibility);

    let core = match kind {
        MemberKind::Field => match type_suffix(info) {
            Some(type_name) => format!("{qualified}: {type_name};"),
            None => qualified,
        },
        MemberKind::Property => match type_suffix(info) {
            Some(type_name) => format!("property {qualified}: {type_name};"),
            None => format!("property {qualified};"),
        },
        MemberKind::Method => {
            let directives = format_directives(info);
            // The member index does not distinguish procedure vs. function
            // return type, so render the neutral `procedure`-shaped signature
            // and append the KNOWN directives — never a fabricated return type.
            format!("procedure {qualified};{directives}")
        }
        MemberKind::NestedConst => match type_suffix(info) {
            Some(type_name) => format!("const {qualified}: {type_name};"),
            None => format!("const {qualified};"),
        },
        MemberKind::NestedType => format!("type {qualified}"),
    };

    match visibility {
        Some(prefix) => format!("{prefix} {core}"),
        None => core,
    }
}

/// The declared simple type name, when the parser captured a `type_key`. `None`
/// for an anonymous/complex type — the caller then omits the `: T` rather than
/// inventing one.
fn type_suffix(info: &HoverInfo) -> Option<&'static str> {
    info.type_key.map(globals::resolve)
}

/// The method directives rendered as ` virtual; override;` (leading space,
/// each `;`-terminated). Empty string for a method with no directives.
fn format_directives(info: &HoverInfo) -> String {
    if info.directives.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    for directive in &info.directives {
        out.push(' ');
        out.push_str(globals::resolve(*directive));
        out.push(';');
    }
    out
}

/// A meaningful visibility keyword, or `None` for `Unspecified` (a member
/// before any visibility section — the resolution is a semantic concern, so we
/// do not label it).
fn visibility_prefix(visibility: Visibility) -> Option<&'static str> {
    match visibility {
        Visibility::Unspecified => None,
        Visibility::Private => Some("private"),
        Visibility::Protected => Some("protected"),
        Visibility::Public => Some("public"),
        Visibility::Published => Some("published"),
        Visibility::Automated => Some("automated"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use delphi_parser::meta::{CodeLocation, FileId, Span};

    fn location() -> CodeLocation {
        CodeLocation {
            file: FileId(0),
            span: Span::new(0, 1),
        }
    }

    fn info(kind: CompletionKind) -> HoverInfo {
        HoverInfo {
            display: globals::intern("Sample"),
            kind,
            type_key: None,
            directives: Vec::new(),
            visibility: Visibility::Unspecified,
            owner_type: None,
            occurrence: location(),
        }
    }

    #[test]
    fn field_with_known_type_shows_the_type() {
        let mut hover = info(CompletionKind::Member(MemberKind::Field));
        hover.display = globals::intern("Boss");
        hover.owner_type = Some(globals::intern("TManager"));
        hover.type_key = Some(globals::intern("TUser"));
        let text = format_hover(&hover);
        assert!(text.contains("Boss: TUser;"), "{text}");
        assert!(text.starts_with("```delphi"));
    }

    #[test]
    fn method_shows_directives_not_a_fabricated_return_type() {
        let mut hover = info(CompletionKind::Member(MemberKind::Method));
        hover.display = globals::intern("Greet");
        hover.owner_type = Some(globals::intern("TUser"));
        hover.directives = vec![globals::intern("virtual")];
        let text = format_hover(&hover);
        assert!(text.contains("procedure TUser.Greet; virtual;"), "{text}");
        // no invented `: SomeType`
        assert!(!text.contains(':'), "no fabricated return type: {text}");
    }

    #[test]
    fn anonymous_type_field_shows_kind_only_no_fabricated_type() {
        // type_key None → the field renders as its qualified name only, never a
        // guessed type.
        let mut hover = info(CompletionKind::Member(MemberKind::Field));
        hover.display = globals::intern("Payload");
        hover.owner_type = Some(globals::intern("TThing"));
        hover.type_key = None;
        let text = format_hover(&hover);
        assert!(text.contains("TThing.Payload"), "{text}");
        assert!(!text.contains(':'), "no fabricated type when type_key is None: {text}");
    }

    #[test]
    fn top_level_type_symbol() {
        let mut hover = info(CompletionKind::Symbol(SymbolKind::Type));
        hover.display = globals::intern("TWidget");
        let text = format_hover(&hover);
        assert!(text.contains("type TWidget"), "{text}");
    }

    #[test]
    fn visibility_prefixes_a_member() {
        let mut hover = info(CompletionKind::Member(MemberKind::Field));
        hover.display = globals::intern("FSecret");
        hover.owner_type = Some(globals::intern("TThing"));
        hover.type_key = Some(globals::intern("Integer"));
        hover.visibility = Visibility::Private;
        let text = format_hover(&hover);
        assert!(text.contains("private TThing.FSecret: Integer;"), "{text}");
    }
}
