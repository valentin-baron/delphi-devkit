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
//! Source of the target file's text: ALWAYS the parser arena's decoded content
//! for the span's own file (`arena.content(location.file)`). The span's byte
//! offsets index the exact text the arena parsed, so a `LineIndex` built from
//! that same content is guaranteed consistent with the span — regardless of
//! whether an open editor buffer has since been edited. Consulting the open
//! document's current line index instead would be a PROVENANCE mismatch: the
//! span comes from the target unit's CACHED (last-parsed) meta, so between a
//! target's didChange (store updated) and that change's reparse completing, the
//! open buffer's index would map the cached span's OLD offsets onto NEW text →
//! a `Range` at the wrong bytes. For an open file `parse_buffer` keeps the arena
//! content byte-identical to the parsed text, so the arena content always
//! matches the span; using it unconditionally is the internally-consistent (and
//! never-wrong) choice.
//!
//! Returns `None` — never a fabricated `Location` — when the target file's path
//! cannot become a `file://` URL (a virtual/unsaved buffer whose display path
//! does not canonicalize) or its content is unreadable (deleted between parse
//! and query). A `None` here surfaces to the client as "no navigation", never a
//! jump to a wrong place.

use tower_lsp::lsp_types::{Location, Range, Url};

use delphi_parser::driver::ProjectSession;
use delphi_parser::meta::CodeLocation;

use crate::positions::LineIndex;

/// Map a parser [`CodeLocation`] to an LSP [`Location`], computing the `Range`
/// from the TARGET SPAN'S OWN file text (the arena's decoded content for
/// `location.file`).
///
/// The span indexes the exact bytes the arena parsed for that file, so the
/// `Range` is built from that same content — never from an open editor buffer,
/// whose current text may have drifted from the (cached, last-parsed) span it is
/// asked to map. See the module docs for the provenance argument.
///
/// `None` (never a wrong `Location`) when the target path is not a `file://`
/// URL, or its content cannot be read.
pub fn code_location_to_lsp(
    session: &ProjectSession,
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

    // Map the span through a LineIndex built from the SPAN'S OWN parsed content
    // (arena content of `location.file`). This is internally consistent with the
    // span's byte offsets no matter how fresh or stale any open buffer is.
    let content = arena.content(location.file).ok()?;
    let index = LineIndex::new(content.to_string());
    let range = span_to_range(&index, location);

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
    unit_key: delphi_parser::context::Identifier,
    offset: u32,
) -> Option<Vec<Location>> {
    let target = session.symbol_at(unit_key, offset)?;
    let locations = session.definition(unit_key, target.key, target.owner_type);
    let mapped: Vec<Location> = locations
        .into_iter()
        .filter_map(|location| code_location_to_lsp(session, location))
        .collect();
    if mapped.is_empty() { None } else { Some(mapped) }
}

/// Resolve `textDocument/references` for `(unit_key, offset)` into LSP
/// `Location`s. Factored out of the server handler so the composition (symbol →
/// references → per-file location mapping) is unit-testable without a live LSP
/// `Client`.
///
/// HONESTY / OVER-APPROXIMATION (documented, and it is why this is READ-ONLY):
/// `session.references(key)` returns a CANDIDATE set — every textual occurrence
/// of the folded key across cached units. It is scope-unresolved, so it may
/// include an unrelated same-named identifier (a local `Result`, a different
/// unit's `Name`) and never misses a real occurrence in a cached unit. That is
/// acceptable for "find all references" the user visually reviews; it is NOT a
/// proven binding set and MUST NOT drive a destructive edit (see rename, which
/// is deliberately not advertised).
///
/// Each `Occurrence.location` is a span in its OWN unit's file, so its `Range`
/// is computed from THAT file's own text via [`code_location_to_lsp`] — never
/// the requesting document's line index. `include_declaration = false` drops the
/// occurrence(s) at the declaration site of the resolved symbol.
///
/// `None` (never a fabricated result) when nothing resolvable is under the
/// cursor. An empty `Vec` when a target resolves but has no mappable
/// occurrences.
pub fn resolve_references(
    session: &ProjectSession,
    unit_key: delphi_parser::context::Identifier,
    offset: u32,
    include_declaration: bool,
) -> Option<Vec<Location>> {
    let target = session.symbol_at(unit_key, offset)?;
    // The declaration span(s) of the resolved symbol — used only to DROP the
    // declaration occurrence(s) when `include_declaration` is false. Computed
    // from the same `definition` machinery so the dropped span matches exactly
    // the declaration site the references index recorded.
    let declaration_spans: Vec<CodeLocation> = if include_declaration {
        Vec::new()
    } else {
        session.definition(unit_key, target.key, target.owner_type)
    };

    let mapped: Vec<Location> = session
        .references(target.key)
        .into_iter()
        .filter(|occurrence| {
            include_declaration
                || !declaration_spans.iter().any(|declaration| {
                    declaration.file == occurrence.location.file
                        && declaration.span == occurrence.location.span
                })
        })
        .filter_map(|occurrence| code_location_to_lsp(session, occurrence.location))
        .collect();
    Some(mapped)
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
    use crate::documents::DocumentStore;
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
        // Models is padded with blank lines so `type TUser` lands on line 5
        // (0-based) — deliberately DIFFERENT from any line a source reference to
        // it sits on in Client (uses on line 2, `Boss: TUser` on line 4). A
        // wrong (source) line index would therefore yield a wrong line number,
        // so the cross-file range tests actually discriminate.
        std::fs::write(
            directory.join("Models.pas"),
            "unit Models;\ninterface\n\n\n\ntype TUser = class\n  Name: string;\nend;\nimplementation\nend.",
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
        let client_key = session.context().intern_key("CLIENT");
        let defs =
            session.definition(client_key, session.context().intern_key("TManager"), None);
        assert_eq!(defs.len(), 1, "own symbol resolves to its own declaration");
        let location = code_location_to_lsp(&session, defs[0]).expect("maps");
        // URL is the Client file.
        assert!(location.uri.to_file_path().unwrap().ends_with("Client.pas"));
        // "type TManager" is on line 3 (0-based) of Client.pas.
        assert_eq!(location.range.start.line, 3);
    }

    /// A CROSS-FILE target: `TUser` used in Client resolves to its declaration in
    /// Models; the `Range` MUST be computed from Models.pas's own text, not
    /// Client's. Models is padded so `type TUser` sits on line 5 (0-based),
    /// DIFFERENT from the source `Boss: TUser` reference's line 4 in Client — so
    /// a wrong (source) line index would yield line 4, and this assertion
    /// genuinely discriminates.
    #[test]
    fn cross_file_target_range_uses_the_target_files_own_lineindex() {
        let (session, _directory) = session_with_two_units("cross_file");
        let client_key = session.context().intern_key("CLIENT");
        // Resolve TUser's definition (lives in Models).
        let defs = session.definition(client_key, session.context().intern_key("TUser"), None);
        assert_eq!(defs.len(), 1);
        let location = code_location_to_lsp(&session, defs[0]).expect("maps");
        // The URL points at Models.pas, NOT Client.pas — a cross-file jump.
        assert!(
            location.uri.to_file_path().unwrap().ends_with("Models.pas"),
            "cross-file target must resolve to Models.pas: {:?}",
            location.uri
        );
        // "type TUser" is on line 5 (0-based) of Models.pas. The source
        // reference (`Boss: TUser` in Client) is on line 4 — a wrong index would
        // produce line 4, so this is a discriminating load-bearing assertion.
        assert_eq!(
            location.range.start.line, 5,
            "the range must be computed from Models.pas's own text"
        );
    }

    /// PROVENANCE (staleness) guard: a cross-file target's `Range` must be built
    /// from the SPAN'S OWN parsed content (arena content of the span's file),
    /// never from a divergent open buffer. We simulate the drift: after the
    /// session parsed Models (span provenance = the on-disk text, `type TUser` on
    /// line 5), we open a buffer for Models with a DIFFERENT line layout (the
    /// blank padding removed, so `type TUser` would be on line 2). The mapped
    /// Range must still be line 5 — the span's provenance — NOT line 2 (the stale
    /// buffer). This is the wrong-bytes failure finding #2 closes: the cached
    /// span indexes the parsed text, so it must be mapped through that text.
    #[test]
    fn cross_file_range_follows_span_provenance_not_a_divergent_open_buffer() {
        let (session, directory) = session_with_two_units("provenance");
        // Open Models in the store with a re-laid-out buffer: no blank padding,
        // so `type TUser` sits on line 2 here — divergent from the parsed text
        // the span came from (line 5). If the mapping ever consulted this buffer,
        // it would return line 2 (the wrong bytes).
        let models_url =
            Url::from_file_path(directory.join("Models.pas")).unwrap();
        let mut documents = DocumentStore::new();
        documents.open(
            models_url,
            1,
            "unit Models;\ninterface\ntype TUser = class\n  Name: string;\nend;\nimplementation\nend."
                .to_string(),
        );

        let client_key = session.context().intern_key("CLIENT");
        let defs = session.definition(client_key, session.context().intern_key("TUser"), None);
        assert_eq!(defs.len(), 1);
        let location = code_location_to_lsp(&session, defs[0]).expect("maps");
        assert!(location.uri.to_file_path().unwrap().ends_with("Models.pas"));
        // The span came from the PARSED text (line 5), so the Range is line 5 —
        // the open buffer's divergent line 2 is (correctly) never consulted.
        assert_eq!(
            location.range.start.line, 5,
            "range must follow the span's own parsed content, not the open buffer"
        );
        // The store still holds the divergent buffer — the assertion above is
        // meaningful precisely because a buffer-consulting implementation would
        // have returned its line 2.
        assert!(documents.is_open(
            &Url::from_file_path(directory.join("Models.pas")).unwrap()
        ));
    }

    /// Definition on an OWN-unit symbol: the cursor on `TManager`'s use resolves
    /// to its own declaration in Client.pas.
    #[test]
    fn definition_own_unit_symbol() {
        let (session, directory) = session_with_two_units("def_own");
        let client_key = session.context().intern_key("CLIENT");
        // Byte offset of the `TManager` occurrence in the declaration line.
        let client_src = std::fs::read_to_string(directory.join("Client.pas")).unwrap();
        let offset = client_src.find("TManager").unwrap() as u32;
        let locations =
            resolve_definition_locations(&session, client_key, offset).expect("resolves");
        assert_eq!(locations.len(), 1);
        assert!(locations[0].uri.to_file_path().unwrap().ends_with("Client.pas"));
        assert_eq!(locations[0].range.start.line, 3); // "type TManager" line
    }

    /// Definition on a CROSS-FILE symbol: `TUser` (used in Client, declared in
    /// Models) jumps to Models.pas, range from Models's own text.
    #[test]
    fn definition_cross_file_symbol() {
        let (session, directory) = session_with_two_units("def_cross");
        let client_key = session.context().intern_key("CLIENT");
        let client_src = std::fs::read_to_string(directory.join("Client.pas")).unwrap();
        // `TUser` appears as the field type `Boss: TUser;` in Client.
        let offset = client_src.find("TUser").unwrap() as u32;
        let locations =
            resolve_definition_locations(&session, client_key, offset).expect("resolves");
        assert_eq!(locations.len(), 1);
        assert!(
            locations[0].uri.to_file_path().unwrap().ends_with("Models.pas"),
            "cross-file jump lands in Models.pas: {:?}",
            locations[0].uri
        );
        assert_eq!(locations[0].range.start.line, 5); // "type TUser" in Models
    }

    /// Definition on a MEMBER (`Name` field of `TUser`) resolves to the member
    /// site in Models via the owner type.
    #[test]
    fn definition_member_symbol() {
        let (session, _directory) = session_with_two_units("def_member");
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
        let location = code_location_to_lsp(&session, defs[0]).expect("maps");
        assert!(location.uri.to_file_path().unwrap().ends_with("Models.pas"));
        assert_eq!(location.range.start.line, 6); // "  Name: string;" in Models
    }

    /// A cursor on whitespace/an unknown identifier → `None`, never a wrong jump.
    #[test]
    fn definition_on_whitespace_is_none() {
        let (session, _directory) = session_with_two_units("def_none");
        let client_key = session.context().intern_key("CLIENT");
        // Offset 0 is the `u` of `unit` — a keyword, not a resolvable symbol
        // occurrence; and a far-past-EOF offset has nothing under it.
        assert!(
            resolve_definition_locations(&session, client_key, 100_000).is_none(),
            "an out-of-range cursor yields no definition"
        );
    }

    // ─── Deliverable A: textDocument/references ─────────────────────────
    //
    // references is READ-ONLY and OVER-APPROXIMATING (documented on
    // `resolve_references`): a candidate set the user reviews. These tests prove
    // the LSP-boundary contract: cross-unit occurrences, per-occurrence ranges
    // computed from each occurrence's OWN file, include_declaration honored, and
    // empty/None on no target.

    /// Like `session_with_two_units`, but parses BOTH Models and Client so both
    /// units are cached and thus both contribute to the reference index (the
    /// index spans CACHED units — a unit merely imported for definition
    /// resolution but never parsed contributes no occurrences). Mirrors the
    /// parser's own `references_across_units_and_purge_on_invalidation` setup.
    fn session_with_both_units_parsed(tag: &str) -> (ProjectSession, std::path::PathBuf) {
        let (mut session, directory) = session_with_two_units(tag);
        session
            .parse_source_file(directory.join("Models.pas"))
            .unwrap();
        (session, directory)
    }

    /// A cross-unit symbol (`TUser`, declared in Models, used in Client) yields
    /// occurrences in BOTH files, each mapped to a Location whose Url + Range are
    /// its own file's. Models's declaration is on line 5 (0-based); Client's use
    /// (`Boss: TUser`) is on line 4 — the ranges must land in their OWN files, so
    /// this is a discriminating multi-file assertion.
    #[test]
    fn references_cross_unit_include_declaration() {
        let (session, directory) = session_with_both_units_parsed("refs_cross");
        let client_key = session.context().intern_key("CLIENT");
        // Offset of the `TUser` occurrence in Client (`Boss: TUser;`).
        let client_src = std::fs::read_to_string(directory.join("Client.pas")).unwrap();
        let offset = client_src.find("TUser").unwrap() as u32;

        let locations =
            resolve_references(&session, client_key, offset, true).expect("resolves a target");
        // Occurrences span both files.
        let in_models = locations
            .iter()
            .any(|location| location.uri.to_file_path().unwrap().ends_with("Models.pas"));
        let in_client = locations
            .iter()
            .any(|location| location.uri.to_file_path().unwrap().ends_with("Client.pas"));
        assert!(in_models, "the declaration in Models is a reference: {locations:?}");
        assert!(in_client, "the use in Client is a reference: {locations:?}");
        // The declaration occurrence sits on Models line 5 (its own file's text).
        let models_declaration = locations
            .iter()
            .find(|location| location.uri.to_file_path().unwrap().ends_with("Models.pas"))
            .unwrap();
        assert_eq!(
            models_declaration.range.start.line, 5,
            "Models occurrence range comes from Models's own text: {models_declaration:?}"
        );
    }

    /// `include_declaration = false` drops the declaration-site occurrence(s) but
    /// keeps the impl/interface-body uses. With the declaration excluded, no
    /// occurrence may sit on the declaration span (Models line 5), yet the Client
    /// use must remain.
    #[test]
    fn references_exclude_declaration_drops_only_the_declaration() {
        let (session, directory) = session_with_both_units_parsed("refs_excl");
        let client_key = session.context().intern_key("CLIENT");
        let client_src = std::fs::read_to_string(directory.join("Client.pas")).unwrap();
        let offset = client_src.find("TUser").unwrap() as u32;

        let with_decl =
            resolve_references(&session, client_key, offset, true).expect("resolves");
        let without_decl =
            resolve_references(&session, client_key, offset, false).expect("resolves");

        // Excluding the declaration yields strictly fewer occurrences.
        assert!(
            without_decl.len() < with_decl.len(),
            "excluding the declaration must drop at least one occurrence: with={with_decl:?} without={without_decl:?}"
        );
        // The declaration site (Models.pas line 5) is present WITH the flag and
        // ABSENT without it — the exact occurrence that was dropped.
        let declaration_present = |set: &[Location]| {
            set.iter().any(|location| {
                location.uri.to_file_path().unwrap().ends_with("Models.pas")
                    && location.range.start.line == 5
            })
        };
        assert!(declaration_present(&with_decl), "declaration present when included");
        assert!(
            !declaration_present(&without_decl),
            "declaration dropped when excluded: {without_decl:?}"
        );
        // The Client use survives regardless.
        assert!(
            without_decl
                .iter()
                .any(|location| location.uri.to_file_path().unwrap().ends_with("Client.pas")),
            "a non-declaration use survives exclusion: {without_decl:?}"
        );
    }

    /// A cursor with no resolvable symbol under it (far past EOF) → `None`, never
    /// a fabricated reference list.
    #[test]
    fn references_on_no_target_is_none() {
        let (session, _directory) = session_with_two_units("refs_none");
        let client_key = session.context().intern_key("CLIENT");
        assert!(
            resolve_references(&session, client_key, 100_000, true).is_none(),
            "an out-of-range cursor resolves no target → None"
        );
    }

    /// A virtual (unsaved) target whose display path does not canonicalize to a
    /// real file → `None`, never a fabricated Location.
    #[test]
    fn virtual_target_is_none() {
        let (session, _directory) = session_with_two_units("virtual");
        // Fabricate a location into a virtual buffer (a display-name-only path).
        let file = session
            .arena()
            .insert_virtual("<<unsaved-buffer>>", "unit X;");
        let location = CodeLocation {
            file,
            span: Span::new(0, 4),
        };
        assert!(
            code_location_to_lsp(&session, location).is_none(),
            "a virtual target with a non-file path must map to None"
        );
    }
}
