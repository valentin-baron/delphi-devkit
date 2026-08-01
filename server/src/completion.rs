//! Map parser [`Completion`]s onto LSP [`CompletionItem`]s, honestly.
//!
//! The parser's `completions` query already guarantees the never-a-wrong-answer
//! contract at the semantic level:
//! - AFTER A `.` (member access) it returns ONLY the receiver type's members —
//!   never a top-level symbol leaked into the list — or an EMPTY list when the
//!   receiver type is unresolvable (never a wrong member set).
//! - OTHERWISE (top-level) it returns builtins + own interface symbols visible
//!   at the cursor + imported units' interface symbols, de-duplicated by folded
//!   key.
//!
//! This module does NOT re-filter or re-merge; it only translates each
//! [`Completion`] to a [`CompletionItem`], mapping the parser's
//! [`CompletionKind`] to the closest LSP [`CompletionItemKind`] and building a
//! short `detail` string from the resolved facts. Translating (rather than
//! re-deriving) is what keeps the member-only guarantee intact — there is no
//! place here where a top-level symbol could slip into a member list.

use tower_lsp::lsp_types::{CompletionItem, CompletionItemKind};

use delphi_parser::driver::ProjectSession;
use delphi_parser::globals;
use delphi_parser::query::{Completion, CompletionKind};
use delphi_parser::unit_cache::{MemberKind, SymbolKind};

/// Resolve completions for `(unit_key, offset)` into LSP [`CompletionItem`]s.
/// Factored out of the server handler so the composition (query → item mapping)
/// is unit-testable without a live LSP `Client`.
///
/// Returns whatever the parser's context-sensitive query returns, one-to-one:
/// an empty vec (a member access on an unresolved receiver) maps to an empty
/// list — never a fabricated or top-level-leaked member set.
pub fn resolve_completions(
    session: &ProjectSession,
    unit_key: delphi_parser::context::Identifier,
    offset: u32,
) -> Vec<CompletionItem> {
    session
        .completions(unit_key, offset)
        .into_iter()
        .map(to_completion_item)
        .collect()
}

/// Translate one parser [`Completion`] to an LSP [`CompletionItem`]: `label` is
/// the display spelling, `kind` the mapped [`CompletionItemKind`], `detail` a
/// short type/kind string built only from facts the parser captured (never an
/// invented type).
pub fn to_completion_item(completion: Completion) -> CompletionItem {
    let label = globals::resolve(completion.display).to_string();
    let kind = completion_item_kind(completion.kind);
    let detail = detail_for(&completion);
    CompletionItem {
        label,
        kind: Some(kind),
        detail,
        ..CompletionItem::default()
    }
}

/// Map the parser's unified [`CompletionKind`] to the closest LSP
/// [`CompletionItemKind`].
fn completion_item_kind(kind: CompletionKind) -> CompletionItemKind {
    match kind {
        CompletionKind::Symbol(symbol) => symbol_kind(symbol),
        CompletionKind::Member(member) => member_kind(member),
        // A compiler built-in surfaced at the top level (`Integer`, `string`,
        // `TObject`, …) — a struct-like primitive/type. STRUCT reads better than
        // KEYWORD in the editor's completion glyph for `Integer`/`string`.
        CompletionKind::Builtin => CompletionItemKind::STRUCT,
    }
}

fn symbol_kind(symbol: SymbolKind) -> CompletionItemKind {
    match symbol {
        SymbolKind::Type => CompletionItemKind::CLASS,
        SymbolKind::Const => CompletionItemKind::CONSTANT,
        SymbolKind::ResourceString => CompletionItemKind::CONSTANT,
        SymbolKind::Var => CompletionItemKind::VARIABLE,
        SymbolKind::ThreadVar => CompletionItemKind::VARIABLE,
        SymbolKind::Procedure => CompletionItemKind::FUNCTION,
        SymbolKind::Function => CompletionItemKind::FUNCTION,
    }
}

fn member_kind(member: MemberKind) -> CompletionItemKind {
    match member {
        MemberKind::Field => CompletionItemKind::FIELD,
        MemberKind::Method => CompletionItemKind::METHOD,
        MemberKind::Property => CompletionItemKind::PROPERTY,
        MemberKind::NestedType => CompletionItemKind::CLASS,
        MemberKind::NestedConst => CompletionItemKind::CONSTANT,
    }
}

/// A short `detail` string: the resolved simple type key when the parser
/// captured one (`: TUser`, `: Integer`), else the kind keyword alone. Never
/// invents a type — `type_key = None` yields the kind keyword only, matching the
/// hover module's honesty.
fn detail_for(completion: &Completion) -> Option<String> {
    if let Some(type_key) = completion.type_key {
        return Some(format!(": {}", globals::resolve(type_key)));
    }
    let keyword = match completion.kind {
        CompletionKind::Symbol(SymbolKind::Type) => "type",
        CompletionKind::Symbol(SymbolKind::Const) => "const",
        CompletionKind::Symbol(SymbolKind::ResourceString) => "resourcestring",
        CompletionKind::Symbol(SymbolKind::Var) => "var",
        CompletionKind::Symbol(SymbolKind::ThreadVar) => "threadvar",
        CompletionKind::Symbol(SymbolKind::Procedure) => "procedure",
        CompletionKind::Symbol(SymbolKind::Function) => "function",
        CompletionKind::Member(MemberKind::Field) => "field",
        CompletionKind::Member(MemberKind::Method) => "method",
        CompletionKind::Member(MemberKind::Property) => "property",
        CompletionKind::Member(MemberKind::NestedType) => "type",
        CompletionKind::Member(MemberKind::NestedConst) => "const",
        CompletionKind::Builtin => "builtin",
    };
    Some(keyword.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use delphi_parser::cache_store::{CacheIdentity, CacheStore};
    use delphi_parser::context::{
        DefineSet, ProjectContext, SwitchState, TargetPlatform,
    };
    use delphi_parser::unit_cache::UnitCache;
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::Duration;

    /// A session whose search path is `directory`, so cross-unit imports resolve
    /// off disk. Mirrors the parser/locations test harness.
    fn session_in(directory: &Path) -> ProjectSession {
        let context = ProjectContext {
            configuration: "Debug".to_string(),
            platform_name: "Win32".to_string(),
            platform: TargetPlatform::Win32,
            compiler_version: 36.0,
            rtl_version: 36.0,
            base_defines: DefineSet::default(),
            search_paths: vec![directory.to_path_buf()],
            include_paths: Vec::new(),
            namespaces: Vec::new(),
            unit_aliases: HashMap::new(),
            default_switches: SwitchState::default(),
            unit_cache: UnitCache::default(),
        };
        let project = directory.join("proj.dproj");
        std::fs::write(&project, b"<Project/>").unwrap();
        let identity = CacheIdentity {
            project_path: &project,
            configuration: "Debug",
            platform: "Win32",
            compiler_version: 36.0,
        };
        let store = CacheStore::in_directory(directory, &identity).unwrap();
        ProjectSession::from_parts(Arc::new(context), store, Duration::from_secs(300))
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let directory = std::env::temp_dir().join("ddk-server-completion").join(tag);
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }

    /// Top-level completion (no `.` before the cursor) includes an OWN symbol, an
    /// IMPORTED symbol and a BUILTIN — with correct item kinds — and never leaks a
    /// member into the list.
    #[test]
    fn top_level_includes_import_and_builtin_with_kinds() {
        let directory = temp_dir("top_level");
        std::fs::write(
            directory.join("Lib.pas"),
            "unit Lib;\ninterface\n\
             type TWidget = class\npublic\n  procedure Draw;\n  Width: Integer;\nend;\n\
             implementation\nend.",
        )
        .unwrap();
        std::fs::write(
            directory.join("App.pas"),
            "unit App;\ninterface\nuses Lib;\n\
             type TScreen = class end;\n\
             procedure Paint;\n\
             implementation\nend.",
        )
        .unwrap();

        let mut session = session_in(&directory);
        session.parse_source_file(directory.join("App.pas")).unwrap();
        let app_key = session.context().intern_key("APP");

        // A position just after the `Paint` declaration (top-level, no dot).
        let app_meta_end = {
            let app_src = std::fs::read_to_string(directory.join("App.pas")).unwrap();
            // end of the interface `procedure Paint;` line — a top-level cursor.
            app_src.find("Paint").unwrap() as u32 + 5
        };
        let items = resolve_completions(&session, app_key, app_meta_end);

        let by_label = |label: &str| items.iter().find(|item| item.label == label).cloned();

        let widget = by_label("TWidget").expect("imported TWidget present");
        assert_eq!(widget.kind, Some(CompletionItemKind::CLASS));
        let screen = by_label("TScreen").expect("own TScreen present");
        assert_eq!(screen.kind, Some(CompletionItemKind::CLASS));
        let integer = by_label("Integer").expect("builtin Integer present");
        assert_eq!(integer.kind, Some(CompletionItemKind::STRUCT));

        // A member (Draw/Width of TWidget) must NOT appear in the top-level set.
        assert!(
            by_label("Draw").is_none() && by_label("Width").is_none(),
            "no member leaked into the top-level completion set: {:?}",
            items.iter().map(|item| &item.label).collect::<Vec<_>>()
        );
    }

    /// Member completion after `TShape.` lists ONLY the receiver type's members
    /// with correct member kinds — never the unit's top-level const.
    #[test]
    fn member_after_dot_lists_only_members_with_kinds() {
        let directory = temp_dir("member_dot");
        std::fs::write(
            directory.join("Shapes.pas"),
            "unit Shapes;\ninterface\n\
             type TShape = class\npublic\n  procedure Area;\n  Sides: Integer;\nend;\n\
             const Pi = 3;\n\
             implementation\n\
             procedure Use;\nbegin\n  TShape.\nend;\nend.",
        )
        .unwrap();

        let mut session = session_in(&directory);
        session
            .parse_source_file(directory.join("Shapes.pas"))
            .unwrap();
        let key = session.context().intern_key("SHAPES");

        // Cursor one byte past the `.` following the implementation-body `TShape`.
        let content = std::fs::read_to_string(directory.join("Shapes.pas")).unwrap();
        // the LAST `TShape.` occurrence (in the body) — the dot access.
        let dot = content.rfind("TShape.").unwrap() + "TShape.".len();
        let items = resolve_completions(&session, key, dot as u32);
        let labels: Vec<&String> = items.iter().map(|item| &item.label).collect();

        let area = items.iter().find(|item| item.label == "Area").expect("Area member");
        assert_eq!(area.kind, Some(CompletionItemKind::METHOD));
        let sides = items.iter().find(|item| item.label == "Sides").expect("Sides member");
        assert_eq!(sides.kind, Some(CompletionItemKind::FIELD));
        // ONLY members — the top-level const `Pi` must never appear.
        assert!(
            !items.iter().any(|item| item.label == "Pi"),
            "top-level const leaked into member list: {labels:?}"
        );
    }

    /// A member access on an UNRESOLVED receiver returns an EMPTY list — never a
    /// wrong/fabricated member set. (An unknown receiver `TGhost.` has no type.)
    #[test]
    fn member_after_dot_on_unknown_receiver_is_empty_not_wrong() {
        let directory = temp_dir("unknown_receiver");
        std::fs::write(
            directory.join("Only.pas"),
            "unit Only;\ninterface\n\
             type TReal = class\n  Field: Integer;\nend;\n\
             implementation\n\
             procedure Use;\nbegin\n  TGhost.\nend;\nend.",
        )
        .unwrap();

        let mut session = session_in(&directory);
        session.parse_source_file(directory.join("Only.pas")).unwrap();
        let key = session.context().intern_key("ONLY");
        let content = std::fs::read_to_string(directory.join("Only.pas")).unwrap();
        let dot = content.rfind("TGhost.").unwrap() + "TGhost.".len();
        let items = resolve_completions(&session, key, dot as u32);
        // TGhost is not a declared type → no members, and crucially NOT TReal's
        // members either (never a wrong member set).
        assert!(
            !items.iter().any(|item| item.label == "Field"),
            "an unknown receiver must not borrow another type's members: {:?}",
            items.iter().map(|item| &item.label).collect::<Vec<_>>()
        );
    }
}
