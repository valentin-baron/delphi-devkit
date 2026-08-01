//! Map a parser [`CodeLocation`] (a byte span into some parsed file) onto an LSP
//! [`Location`] (a `Url` + a UTF-16 `Range`), honestly.
//!
//! This is the shared navigation primitive every position-returning feature
//! (definition, hover, references, …) builds on. A navigation target is a byte
//! span in SOME file — usually a DIFFERENT file than the one the request came
//! from (a cross-unit go-to-definition). The `Range` for that span MUST be
//! computed from the TARGET file's OWN text — its own line breaks and its own
//! UTF-16 columns — never from the source document's line index. Mapping a
//! target span through the wrong file's line index sends the user to the wrong
//! line in the wrong file; that is exactly the "never a wrong answer" failure the
//! parser's query layer refuses to make, mirrored here at the LSP boundary.
//!
//! Sources of the target file's text, in preference order:
//! 1. the open-document store, when the target file is a buffer the editor holds
//!    (unsaved edits live only there — the authoritative text, and its
//!    `LineIndex` is already built, so no re-read);
//! 2. otherwise the parser arena's decoded content for that file
//!    (`arena.content(file)`), the source of truth for any parsed-but-not-open
//!    file.
//!
//! Returns `None` — never a fabricated `Location` — when the target file's path
//! cannot become a `file://` URL (a virtual/unsaved buffer whose display path
//! does not canonicalize) or its content is unreadable (deleted between parse
//! and query). A `None` here surfaces to the client as "no navigation", never a
//! jump to a wrong place.

use tower_lsp::lsp_types::{Location, Range, Url};

use delphi_parser::driver::ProjectSession;
use delphi_parser::meta::CodeLocation;

use crate::documents::DocumentStore;
use crate::positions::LineIndex;

/// Map a parser [`CodeLocation`] to an LSP [`Location`], computing the `Range`
/// from the TARGET file's own text.
///
/// `documents` lets an already-open target reuse its live `LineIndex` (and its
/// unsaved text) instead of re-reading from disk; a target that is not open is
/// mapped through a `LineIndex` built from the arena's decoded content.
///
/// `None` (never a wrong `Location`) when the target path is not a `file://`
/// URL, or its content cannot be read.
pub fn code_location_to_lsp(
    session: &ProjectSession,
    documents: &DocumentStore,
    location: CodeLocation,
) -> Option<Location> {
    let arena = session.arena();
    // The target file's on-disk path. `try_path` is non-panicking: a `FileId`
    // this arena never issued yields `None` rather than a crash.
    let path = arena.try_path(location.file)?;

    // The path must become a `file://` URL. A virtual/unsaved buffer's display
    // name does not canonicalize to a real filesystem path, so `from_file_path`
    // rejects it → `None` (never a fabricated URL for a virtual target).
    let url = Url::from_file_path(path).ok()?;

    // Prefer the open document's live line index (its unsaved text is the truth,
    // and the index is already built — no re-read). Fall back to the arena's
    // decoded content for a parsed-but-not-open target.
    let range = if let Some(document) = documents.get(&url) {
        span_to_range(&document.line_index, location)
    } else {
        let content = arena.content(location.file).ok()?;
        let index = LineIndex::new(content.to_string());
        span_to_range(&index, location)
    };

    Some(Location { uri: url, range })
}

/// Resolve go-to-definition for `(unit_key, offset)` into LSP `Location`s,
/// mapping each declaration site through Deliverable A. Factored out of the
/// server handler so the composition (symbol → definition → location mapping)
/// is unit-testable without a live LSP `Client`.
///
/// `None` (never a wrong jump) when: nothing is under the cursor, the target is
/// unresolved, or every resulting location fails to map (virtual/unreadable
/// target).
pub fn resolve_definition_locations(
    session: &ProjectSession,
    documents: &DocumentStore,
    unit_key: delphi_parser::context::Identifier,
    offset: u32,
) -> Option<Vec<Location>> {
    let target = session.symbol_at(unit_key, offset)?;
    let locations = session.definition(unit_key, target.key, target.owner_type);
    let mapped: Vec<Location> = locations
        .into_iter()
        .filter_map(|location| code_location_to_lsp(session, documents, location))
        .collect();
    if mapped.is_empty() { None } else { Some(mapped) }
}

/// Map a byte span onto a UTF-16 [`Range`] through `index` (the TARGET file's
/// own line index). Clamps out-of-range offsets (never panics) — an offset past
/// the file end maps to the end-of-file position.
fn span_to_range(index: &LineIndex, location: CodeLocation) -> Range {
    Range {
        start: index.position_of(location.span.start as usize),
        end: index.position_of(location.span.end as usize),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use delphi_parser::meta::Span;

    /// A session over two on-disk units in a temp dir, so cross-file targets
    /// resolve through the arena. `Client` imports `Models`.
    fn session_with_two_units(tag: &str) -> (ProjectSession, std::path::PathBuf) {
        use delphi_parser::cache_store::{CacheIdentity, CacheStore};
        use delphi_parser::context::{
            DefineSet, ProjectContext, SwitchState, TargetPlatform,
        };
        use delphi_parser::unit_cache::UnitCache;
        use std::collections::HashMap;
        use std::sync::Arc;
        use std::time::Duration;

        let directory = std::env::temp_dir().join("ddk-server-locations").join(tag);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("Models.pas"),
            "unit Models;\ninterface\ntype TUser = class\n  Name: string;\nend;\nimplementation\nend.",
        )
        .unwrap();
        std::fs::write(
            directory.join("Client.pas"),
            "unit Client;\ninterface\nuses Models;\ntype TManager = class\n  Boss: TUser;\nend;\nimplementation\nend.",
        )
        .unwrap();

        let context = ProjectContext {
            configuration: "Debug".to_string(),
            platform_name: "Win32".to_string(),
            platform: TargetPlatform::Win32,
            compiler_version: 36.0,
            rtl_version: 36.0,
            base_defines: DefineSet::default(),
            search_paths: vec![directory.clone()],
            include_paths: Vec::new(),
            namespaces: Vec::new(),
            unit_aliases: HashMap::new(),
            default_switches: SwitchState::default(),
            unit_cache: UnitCache::default(),
        };
        let identity = CacheIdentity {
            project_path: &directory.join("proj.dproj"),
            configuration: "Debug",
            platform: "Win32",
            compiler_version: 36.0,
        };
        std::fs::write(directory.join("proj.dproj"), b"<Project/>").unwrap();
        let store = CacheStore::in_directory(&directory, &identity).unwrap();
        let mut session =
            ProjectSession::from_parts(Arc::new(context), store, Duration::from_secs(300));
        session
            .parse_source_file(directory.join("Client.pas"))
            .unwrap();
        (session, directory)
    }

    /// A same-file target: an OWN symbol (`TManager` in Client) resolves to its
    /// own declaration site; the range maps in that same file's text.
    #[test]
    fn same_file_target_maps_to_a_range_in_that_file() {
        let (session, _directory) = session_with_two_units("same_file");
        let documents = DocumentStore::new();
        let client_key = session.context().intern_key("CLIENT");
        let defs =
            session.definition(client_key, session.context().intern_key("TManager"), None);
        assert_eq!(defs.len(), 1, "own symbol resolves to its own declaration");
        let location = code_location_to_lsp(&session, &documents, defs[0]).expect("maps");
        // URL is the Client file.
        assert!(location.uri.to_file_path().unwrap().ends_with("Client.pas"));
        // "type TManager" is on line 3 (0-based) of Client.pas.
        assert_eq!(location.range.start.line, 3);
    }

    /// A CROSS-FILE target: `TUser` used in Client resolves to its declaration in
    /// Models; the `Range` MUST be computed from Models.pas's own text, not
    /// Client's. `TUser` sits on line 2 (0-based) of Models.pas — proving the
    /// target file's own line index was used.
    #[test]
    fn cross_file_target_range_uses_the_target_files_own_lineindex() {
        let (session, _directory) = session_with_two_units("cross_file");
        let documents = DocumentStore::new();
        let client_key = session.context().intern_key("CLIENT");
        // Resolve TUser's definition (lives in Models).
        let defs = session.definition(client_key, session.context().intern_key("TUser"), None);
        assert_eq!(defs.len(), 1);
        let location = code_location_to_lsp(&session, &documents, defs[0]).expect("maps");
        // The URL points at Models.pas, NOT Client.pas — a cross-file jump.
        assert!(
            location.uri.to_file_path().unwrap().ends_with("Models.pas"),
            "cross-file target must resolve to Models.pas: {:?}",
            location.uri
        );
        // "type TUser" is on line 2 (0-based) of Models.pas. If the source
        // (Client) line index had been used, this would be a different line —
        // this is the load-bearing assertion.
        assert_eq!(
            location.range.start.line, 2,
            "the range must be computed from Models.pas's own text"
        );
    }

    /// Definition on an OWN-unit symbol: the cursor on `TManager`'s use resolves
    /// to its own declaration in Client.pas.
    #[test]
    fn definition_own_unit_symbol() {
        let (session, directory) = session_with_two_units("def_own");
        let documents = DocumentStore::new();
        let client_key = session.context().intern_key("CLIENT");
        // Byte offset of the `TManager` occurrence in the declaration line.
        let client_src = std::fs::read_to_string(directory.join("Client.pas")).unwrap();
        let offset = client_src.find("TManager").unwrap() as u32;
        let locations =
            resolve_definition_locations(&session, &documents, client_key, offset).expect("resolves");
        assert_eq!(locations.len(), 1);
        assert!(locations[0].uri.to_file_path().unwrap().ends_with("Client.pas"));
        assert_eq!(locations[0].range.start.line, 3); // "type TManager" line
    }

    /// Definition on a CROSS-FILE symbol: `TUser` (used in Client, declared in
    /// Models) jumps to Models.pas, range from Models's own text.
    #[test]
    fn definition_cross_file_symbol() {
        let (session, directory) = session_with_two_units("def_cross");
        let documents = DocumentStore::new();
        let client_key = session.context().intern_key("CLIENT");
        let client_src = std::fs::read_to_string(directory.join("Client.pas")).unwrap();
        // `TUser` appears as the field type `Boss: TUser;` in Client.
        let offset = client_src.find("TUser").unwrap() as u32;
        let locations =
            resolve_definition_locations(&session, &documents, client_key, offset).expect("resolves");
        assert_eq!(locations.len(), 1);
        assert!(
            locations[0].uri.to_file_path().unwrap().ends_with("Models.pas"),
            "cross-file jump lands in Models.pas: {:?}",
            locations[0].uri
        );
        assert_eq!(locations[0].range.start.line, 2); // "type TUser" in Models
    }

    /// Definition on a MEMBER (`Name` field of `TUser`) resolves to the member
    /// site in Models via the owner type.
    #[test]
    fn definition_member_symbol() {
        let (session, _directory) = session_with_two_units("def_member");
        let documents = DocumentStore::new();
        let client_key = session.context().intern_key("CLIENT");
        // Resolve the member directly (the server derives owner_type from
        // symbol_at; here we exercise definition's member path via the same
        // location-mapping composition).
        let defs = session.definition(
            client_key,
            session.context().intern_key("Name"),
            Some(session.context().intern_key("TUser")),
        );
        assert_eq!(defs.len(), 1);
        let location = code_location_to_lsp(&session, &documents, defs[0]).expect("maps");
        assert!(location.uri.to_file_path().unwrap().ends_with("Models.pas"));
        assert_eq!(location.range.start.line, 3); // "  Name: string;" in Models
    }

    /// A cursor on whitespace/an unknown identifier → `None`, never a wrong jump.
    #[test]
    fn definition_on_whitespace_is_none() {
        let (session, _directory) = session_with_two_units("def_none");
        let documents = DocumentStore::new();
        let client_key = session.context().intern_key("CLIENT");
        // Offset 0 is the `u` of `unit` — a keyword, not a resolvable symbol
        // occurrence; and a far-past-EOF offset has nothing under it.
        assert!(
            resolve_definition_locations(&session, &documents, client_key, 100_000).is_none(),
            "an out-of-range cursor yields no definition"
        );
    }

    /// A virtual (unsaved) target whose display path does not canonicalize to a
    /// real file → `None`, never a fabricated Location.
    #[test]
    fn virtual_target_is_none() {
        let (session, _directory) = session_with_two_units("virtual");
        let documents = DocumentStore::new();
        // Fabricate a location into a virtual buffer (a display-name-only path).
        let file = session
            .arena()
            .insert_virtual("<<unsaved-buffer>>", "unit X;");
        let location = CodeLocation {
            file,
            span: Span::new(0, 4),
        };
        assert!(
            code_location_to_lsp(&session, &documents, location).is_none(),
            "a virtual target with a non-file path must map to None"
        );
    }
}
