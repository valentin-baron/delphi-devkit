//! AST → [`UnitMeta`] and cache insertion: the step that turns a parse into a
//! durable, invalidatable, persistable result. The interface surface is no
//! longer flattened here — [`UnitMeta`] derives it lazily from the AST it owns.

use std::sync::Arc;

use crate::ast::{Source, Unit};
use crate::context::ProjectContext;
use crate::meta::FileId;
use crate::parse_state::InterfaceLoader;
use crate::parser::{ParseError, ParseOutcome, parse_file_full};
use crate::source::SourceArena;
use crate::unit_cache::{SourceStamp, hash_bytes, hash_file};
use crate::unit_meta::UnitMeta;

/// Test-only probe counting real source parses (see [`parse_and_cache`]). Lets a
/// test assert the Task-16 reload-on-miss path did NOT re-parse on a hash match
/// and DID re-parse after the source changed.
///
/// THREAD-LOCAL by design: cargo runs each test on its own thread and a parse
/// runs synchronously on the calling (test) thread, so a thread-local counter
/// isolates this test's parses from every other test parsing in parallel — a
/// process-global counter would be polluted by concurrent tests and make the
/// delta non-deterministic.
#[cfg(test)]
pub mod parse_probe {
    use std::cell::Cell;

    thread_local! {
        static PARSES: Cell<u64> = const { Cell::new(0) };
    }

    pub(super) fn note_parse() {
        PARSES.with(|count| count.set(count.get() + 1));
    }

    /// Count of `parse_and_cache` source parses on the CURRENT thread so far.
    pub fn count() -> u64 {
        PARSES.with(|count| count.get())
    }
}

/// Hash stamp for a parsed file. Disk files hash their RAW on-disk bytes
/// (matches load-time validation, which also hashes raw). The bytes come from
/// the arena's retained copy read at parse time — one read, no TOCTOU window
/// and no redundant re-read (L15). The hash input is byte-identical to
/// `hash_file(path)`, so existing snapshots still validate.
///
/// Virtual buffers (unsaved editor content, tests) have no disk bytes — the
/// decoded content is hashed instead; such a stamp never matches a disk read,
/// so persisted entries for virtual buffers are dropped as stale on the next
/// load. That is the intended behavior: unsaved state must not masquerade as
/// on-disk state across sessions (#21/#25). As a defensive fallback, a disk
/// file whose raw bytes were somehow not retained re-reads via `hash_file`
/// (same bytes, same hash) rather than silently hashing decoded content.
fn stamp_file(arena: &SourceArena, file: FileId) -> SourceStamp {
    let path = arena.path(file).to_path_buf();
    let hash = match arena.raw_bytes(file) {
        Some(raw) => hash_bytes(raw),
        // No retained raw bytes: a virtual buffer (hash decoded — never matches
        // a disk read, by design) or a not-yet-materialized disk file (re-read
        // the raw bytes so the hash still matches `hash_file`).
        None => hash_file(&path).unwrap_or_else(|_| hash_bytes(arena.loaded_content(file).as_bytes())),
    };
    SourceStamp { path, hash }
}

/// The sibling `.dfm` for a unit source path (`Unit1.pas` → `Unit1.dfm`, same
/// directory — the Delphi convention), stamped path+hash, if it exists on disk.
/// A unit may have no form (`None`); a dfm without a matching pas is simply
/// never reached. Only real on-disk dfm files are stamped (a virtual/unsaved
/// pas buffer or a non-file path yields `None`), so the stamp always validates
/// against actual bytes on load.
pub fn sibling_dfm_stamp(unit_source_path: &std::path::Path) -> Option<SourceStamp> {
    let dfm_path = unit_source_path.with_extension("dfm");
    let hash = hash_file(&dfm_path).ok()?;
    Some(SourceStamp {
        path: dfm_path,
        hash,
    })
}

/// Build the cacheable [`UnitMeta`] for a parsed unit: the owned AST plus the
/// stamps/deps/usages/taint the shallow AST cannot reproduce. The unit AST is
/// MOVED into the meta (no clone).
///
/// `retain_body` controls whether the implementation-section body AST is kept on
/// the meta. When `true` (the ACTIVE editor unit only), the body is attached and
/// powers local/member/inherited resolution + completion. When `false` (indexing,
/// RTL bootstrap, every cross-unit import load), the body is DROPPED — the meta
/// carries an EMPTY [`crate::ast_impl::ImplementationBody`]. The flat `usages`
/// (the reference index / find-references source) are attached in BOTH cases:
/// they are `outcome.usages`, independent of the body structures, so cross-unit
/// references never regress. Dropping the body for non-active units is what bounds
/// process RAM (the 20 GB OOM this fixes): a body is working-set state, needed only
/// for the one unit open in the editor.
#[allow(clippy::too_many_arguments)]
pub fn build_unit_meta(
    arena: &SourceArena,
    file: FileId,
    unit: Unit,
    outcome_includes: &[FileId],
    dependencies: Vec<crate::unit_cache::Dependency>,
    usages: Vec<crate::unit_cache::Usage>,
    implementation_body: crate::ast_impl::ImplementationBody,
    cycle_tainted: bool,
    recovered: bool,
    retain_body: bool,
) -> UnitMeta {
    let own = stamp_file(arena, file);
    let includes = outcome_includes
        .iter()
        .map(|&include| stamp_file(arena, include))
        .collect();
    // Associate the sibling `.dfm` (`Unit1.pas` ↔ `Unit1.dfm`) so a form edit
    // stales this unit. Only stamped when the dfm actually exists on disk.
    let dfm = sibling_dfm_stamp(&own.path);
    // Decoded source length — the weigher's robust size proxy (Task 16 D). The
    // content was just materialized by the parse, so this is a cheap length
    // read (no re-decode). Saturating cast: a >4GiB source is implausible and
    // would only saturate the weight, never wrap.
    let source_len = arena.loaded_content(file).len().min(u32::MAX as usize) as u32;
    // The body is retained ONLY for the active editor unit (`retain_body`). For
    // every indexed / bootstrapped / cross-unit-imported unit it is dropped: the
    // meta carries an EMPTY body so it is never held resident (memory fix). The
    // flat `usages` above are attached in BOTH cases (references never regress).
    let body = if retain_body {
        implementation_body
    } else {
        crate::ast_impl::ImplementationBody::default()
    };
    UnitMeta::new(
        unit,
        cycle_tainted,
        own.path,
        own.hash,
        includes,
        dependencies,
        usages,
    )
    .with_dfm(dfm)
    .with_recovered(recovered)
    .with_source_len(source_len)
    .with_implementation_body(body)
}

/// Parse a materialized file; when it is a unit, build + cache its [`UnitMeta`].
/// Non-unit sources (program/library/package) parse fine but produce no
/// importable interface → no cache entry.
///
/// Moved-source contract: for a **unit** the returned `ParseOutcome.source` is
/// [`crate::parser::ParsedSource::Moved`] — the `Unit` AST was moved into the
/// returned [`UnitMeta`]; read it there (`meta.ast`). For a non-unit source it
/// is [`crate::parser::ParsedSource::Present`] with the real AST. A caller can
/// therefore never mistake a moved unit for real `source` data.
/// `retain_body`: keep the implementation-section body on the cached meta (the
/// ACTIVE editor unit only) or drop it (indexing / bootstrap / cross-unit
/// imports — bodyless, memory-bounded). See [`build_unit_meta`]. The flat usages
/// are retained regardless, so find-references / the reference index never
/// regress.
pub fn parse_and_cache(
    arena: &SourceArena,
    context: &Arc<ProjectContext>,
    file: FileId,
    loader: Option<std::rc::Rc<dyn InterfaceLoader>>,
    retain_body: bool,
) -> Result<(ParseOutcome, Option<Arc<UnitMeta>>), ParseError> {
    // Test-only probe: count how many times a source is actually PARSED. The
    // Task-16 reload-on-miss path (loader.interface_of → store.load_unit) must
    // NOT increment this on a hash match (it deserializes the AST from disk),
    // but MUST increment it after the source bytes change (hash mismatch → real
    // re-parse). Cheap relaxed counter; compiled only under test.
    #[cfg(test)]
    parse_probe::note_parse();
    let outcome = parse_file_full(arena, context.clone(), file, loader.clone())?;
    // Take the byproducts we need before consuming `source`.
    let includes = outcome.seen_includes.clone();
    let dependencies = outcome.dependencies.clone();
    let usages = outcome.usages.clone();
    // Clone the body only when it will be retained (the active editor unit); for
    // a bodyless (indexed / imported) meta this clone is skipped entirely — the
    // body is one of the largest structures a parse produces, so not cloning it
    // for the ~1900 bootstrap + every import load is itself a memory + time win.
    let implementation_body = if retain_body {
        outcome.implementation_body.clone()
    } else {
        crate::ast_impl::ImplementationBody::default()
    };
    let cycle_tainted = outcome.cycle_tainted;
    let recovered = outcome.recovered;

    // Move the AST: for a unit, the `Unit` is taken out of the outcome's
    // `source` and OWNED by the meta, leaving `outcome.source` as the explicit
    // `ParsedSource::Moved` variant. Unit callers read the AST from the meta
    // (`meta.ast`); a caller inspecting `outcome.source` for a unit sees
    // `Moved` (typed), never bogus placeholder data. Non-unit sources are put
    // back untouched.
    let mut outcome = outcome;
    let meta = match outcome.source.take() {
        Some(Source::Unit(unit)) => {
            let meta = Arc::new(build_unit_meta(
                arena,
                file,
                unit,
                &includes,
                dependencies,
                usages,
                implementation_body,
                cycle_tainted,
                recovered,
                retain_body,
            ));
            context.unit_cache.insert(meta.name(), meta.clone());
            // begin_unit/end_unit are now balanced inside the parser itself
            // (parse_unit), so a failed parse can't leak a false cycle.
            Some(meta)
        }
        // non-unit: restore the real source so the caller still sees it
        Some(other) => {
            outcome.source = crate::parser::ParsedSource::Present(other);
            None
        }
        // `parse_file_full` always yields `Present`, so `take` is always `Some`.
        None => unreachable!("parse_file_full always yields a present source"),
    };
    Ok((outcome, meta))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{DefineSet, Interner, SwitchState, TargetPlatform};
    use crate::unit_cache::{CacheEntry, UnitCache};
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn test_context(search_paths: Vec<PathBuf>) -> Arc<ProjectContext> {
        Arc::new(ProjectContext {
            configuration: "Debug".to_string(),
            platform_name: "Win32".to_string(),
            platform: TargetPlatform::Win32,
            compiler_version: 36.0,
            rtl_version: 36.0,
            base_defines: DefineSet::default(),
            search_paths,
            include_paths: Vec::new(),
            namespaces: Vec::new(),
            unit_aliases: HashMap::new(),
            default_switches: SwitchState::default(),
            unit_cache: UnitCache::default(),
        })
    }

    fn temp_directory(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join("delphi_parser_pipeline").join(name);
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn unit_becomes_cached_artifact_with_symbols_and_includes() {
        let directory = temp_directory("artifact");
        std::fs::write(directory.join("defs.inc"), "{$DEFINE FROM_INC}").unwrap();
        std::fs::write(
            directory.join("UnitA.pas"),
            "unit UnitA;\ninterface\n{$I defs.inc}\n\
             type TThing = class end;\n\
             const MaxThings = 3;\n\
             function GetThing: TThing;\n\
             implementation\nend.",
        )
        .unwrap();

        let context = test_context(vec![directory.clone()]);
        let arena = SourceArena::new();
        let file = arena.load(directory.join("UnitA.pas")).unwrap();

        let (_, meta) = parse_and_cache(&arena, &context, file, None, true).unwrap();
        let meta = meta.expect("unit produces meta");

        // symbols captured with kinds (derived from the AST on demand)
        assert_eq!(meta.interface().symbols.len(), 3);
        assert!(meta.interface().contains_key(context.intern_key("TTHING")));
        assert!(meta.interface().contains_key(context.intern_key("maxthings")));
        // include stamped
        assert_eq!(meta.includes.len(), 1);
        assert!(meta.includes[0].path.ends_with("defs.inc"));
        // cached under the folded unit key
        let key = context.intern_key("UNITA");
        assert!(matches!(
            context.unit_cache.get(key),
            Some(CacheEntry::Done(_))
        ));
    }

    /// L15: the source stamp hashes the file's RAW on-disk bytes, taken from the
    /// arena's retained copy (no re-read). The hash must be byte-identical to
    /// `hash_file(path)` for BOTH an ANSI (no-BOM, high bytes) and a UTF-16LE
    /// unit — so decoded ≠ raw units still save+load-validate. This is exactly
    /// what load-time `validate_meta` checks (`meta.source_hash == hash_file`).
    #[test]
    fn source_stamp_hashes_raw_bytes_for_ansi_and_utf16() {
        let directory = temp_directory("raw_stamp");

        // ANSI unit: no BOM, a non-ASCII byte (0xE4 = 'ä' in CP1252). Decoded
        // UTF-8 differs from the raw byte, so hashing decoded would NOT match a
        // disk read — this catches a decoded-vs-raw regression.
        let ansi_path = directory.join("Ansi.pas");
        let mut ansi_bytes = b"unit Ansi; interface const S = '".to_vec();
        ansi_bytes.push(0xE4); // lone high byte, invalid UTF-8 on its own
        ansi_bytes.extend_from_slice(b"'; implementation end.");
        std::fs::write(&ansi_path, &ansi_bytes).unwrap();

        // UTF-16LE unit with BOM.
        let utf16_path = directory.join("Utf16.pas");
        let mut utf16_bytes = vec![0xFF, 0xFE];
        for unit in "unit Utf16; interface implementation end.".encode_utf16() {
            utf16_bytes.extend_from_slice(&unit.to_le_bytes());
        }
        std::fs::write(&utf16_path, &utf16_bytes).unwrap();

        let context = test_context(Vec::new());
        let arena = SourceArena::new();

        for path in [&ansi_path, &utf16_path] {
            let file = arena.load(path).unwrap();
            // arena retained the raw bytes, byte-identical to the disk file
            assert_eq!(
                arena.raw_bytes(file).unwrap(),
                std::fs::read(path).unwrap().as_slice(),
                "arena raw bytes must equal on-disk bytes"
            );
            let (_, meta) = parse_and_cache(&arena, &context, file, None, true).unwrap();
            let meta = meta.expect("unit produces meta");
            // the stamp equals hash_file(path) — load validation will match
            assert_eq!(
                meta.source_hash,
                hash_file(path).unwrap(),
                "raw-byte stamp must equal hash_file(path) so snapshots validate"
            );
        }
    }

    #[test]
    fn implementation_usages_collected() {
        let context = test_context(Vec::new());
        let arena = SourceArena::new();
        let file = arena.insert_virtual(
            "u.pas",
            "unit U;\ninterface\ntype TAlias = TBaseThing;\nprocedure Run;\nimplementation\n\
             procedure Run;\nbegin\n  DoThing(GCount);\nend;\nend.",
        );
        let (_, meta) = parse_and_cache(&arena, &context, file, None, true).unwrap();
        let meta = meta.unwrap();
        // interface-side type references are usages too (the `Run`
        // declaration line contributes nothing — no body idents there)
        let keys: Vec<crate::context::Identifier> =
            meta.usages.iter().map(|usage| usage.symbol).collect();
        assert!(keys.contains(&context.intern_key("DoThing")));
        assert!(keys.contains(&context.intern_key("GCOUNT")));
        assert!(keys.contains(&context.intern_key("run")));
        // interface-side type reference indexed too
        assert!(keys.contains(&context.intern_key("TBaseThing")));
        // locations point at real spans
        let dothing = meta
            .usages
            .iter()
            .find(|usage| usage.symbol == context.intern_key("DOTHING"))
            .unwrap();
        assert_eq!(arena.location_text(dothing.location), "DoThing");
    }

    /// MEMORY INVARIANT (the 20 GB OOM fix): a unit parsed with `retain_body=false`
    /// (the indexing / RTL-bootstrap / cross-unit-import path) yields a cached meta
    /// whose `implementation_body` is EMPTY — bodies are working-set state for the
    /// active editor unit ONLY and must never be held resident for the thousands of
    /// indexed/imported units. The SAME source parsed with `retain_body=true` (the
    /// active editor unit, `parse_buffer` / a direct `parse_source_file(true)`)
    /// yields a POPULATED body. Crucially, the flat `usages` (find-references) are
    /// present in BOTH cases, so references never regress with the body dropped.
    #[test]
    fn body_retained_only_for_active_unit_not_when_indexing() {
        let context = test_context(Vec::new());
        let arena = SourceArena::new();
        // A unit with a real implementation body: a routine with a typed local.
        let source = "unit Bodied;\ninterface\nprocedure Run;\nimplementation\n\
             procedure Run;\nvar Local: Integer;\nbegin\n  Local := 1;\n  DoThing(Local);\nend;\nend.";

        // Indexing-style parse (retain_body = false): the meta is BODYLESS.
        let indexed_file = arena.insert_virtual("BodiedIndexed.pas", source);
        let (_, indexed) = parse_and_cache(&arena, &context, indexed_file, None, false).unwrap();
        let indexed = indexed.unwrap();
        assert!(
            indexed.implementation_body.routines.is_empty(),
            "an indexed (retain_body=false) meta must carry an EMPTY body — bodies are \
             active-unit-only (the 20 GB OOM fix)"
        );
        // The derived flat table is correspondingly empty (local resolution is
        // active-unit-only) and its reliability gate defaults true (matches nothing).
        assert!(indexed.impl_scopes().is_empty());
        assert!(indexed.impl_scopes_reliable());
        // References are UNAFFECTED: flat usages are retained even bodyless.
        let indexed_usages: Vec<_> =
            indexed.usages.iter().map(|usage| usage.symbol).collect();
        assert!(
            indexed_usages.contains(&context.intern_key("DoThing")),
            "flat usages (find-references) must be retained even when the body is dropped"
        );

        // Active-editor-style parse (retain_body = true): the meta has a POPULATED
        // body — one routine with its typed local.
        let active_file = arena.insert_virtual("BodiedActive.pas", source);
        let (_, active) = parse_and_cache(&arena, &context, active_file, None, true).unwrap();
        let active = active.unwrap();
        assert!(
            !active.implementation_body.routines.is_empty(),
            "the active editor unit (retain_body=true) must carry a POPULATED body"
        );
        assert!(!active.impl_scopes().is_empty(), "derived flat table is populated for the active unit");
        // And references are present here too (same source, same usages).
        let active_usages: Vec<_> = active.usages.iter().map(|usage| usage.symbol).collect();
        assert!(active_usages.contains(&context.intern_key("DoThing")));
    }

    #[test]
    fn member_symbols_flattened_into_artifact() {
        let directory = temp_directory("member_roundtrip");
        let disk_path = directory.join("U.pas");
        std::fs::write(
            &disk_path,
            "unit U;\ninterface\n\
             type TThing = class\n\
               FValue: Integer;\n\
               procedure Go;\n\
               property Value: Integer read FValue write SetValue;\n\
               type TInner = record I: Byte; end;\n\
               const InnerMax = 5;\n\
             end;\n\
             implementation\nend.",
        )
        .unwrap();
        let context = test_context(Vec::new());
        // parse through the GLOBAL arena so serialized FileIds resolve on load
        let arena = crate::globals::arena();
        let file = arena.load(&disk_path).unwrap();
        let (_, meta) = parse_and_cache(arena, &context, file, None, true).unwrap();
        let meta = meta.unwrap();

        let thing = meta
            .interface()
            .find(context.intern_key("TTHING"))
            .expect("type symbol");
        assert_eq!(thing.members.len(), 5);
        let value = thing
            .find_member(context.intern_key("value"))
            .expect("property member");
        assert_eq!(value.kind, crate::unit_cache::MemberKind::Property);
        assert_eq!(value.read_target, Some(context.intern_key("FVALUE")));
        assert_eq!(value.write_target, Some(context.intern_key("SETVALUE")));
        assert_eq!(
            thing.find_member(context.intern_key("TINNER")).unwrap().kind,
            crate::unit_cache::MemberKind::NestedType
        );

        // persistence roundtrip keeps the full AST → derived members (format v7)
        let snapshot = directory.join("cache.bin");
        context.unit_cache.save(&snapshot).unwrap();

        let fresh_context = test_context(Vec::new());
        let report = fresh_context.unit_cache.load(&snapshot).unwrap();
        assert_eq!(report.loaded, 1, "report: {report:?}");
        let CacheEntry::Done(loaded) = fresh_context
            .unit_cache
            .get(fresh_context.intern_key("U"))
            .unwrap()
        else {
            panic!("loaded meta");
        };
        let loaded_thing = loaded
            .interface()
            .find(fresh_context.intern_key("TTHING"))
            .unwrap();
        assert_eq!(loaded_thing.members.len(), 5);
        assert_eq!(
            loaded_thing
                .find_member(fresh_context.intern_key("VALUE"))
                .unwrap()
                .read_target,
            Some(fresh_context.intern_key("FVALUE"))
        );
    }

    /// M1 (strengthened, Task 16 D): the weigher must NOT undercount — a large
    /// unit must weigh SUBSTANTIALLY more than a tiny one, roughly proportional
    /// to its source size. This catches an undercount regression: the old
    /// shallow estimate counted only member structs and weighed a big unit
    /// almost the same as a small one, so the byte cap never bounded RAM.
    #[test]
    fn weigher_scales_with_source_size_not_undercounting() {
        let context = test_context(Vec::new());
        let arena = SourceArena::new();

        let tiny_src = "unit Tiny; interface implementation end.";
        let tiny_file = arena.insert_virtual("Tiny.pas", tiny_src);
        let (_, tiny) = parse_and_cache(&arena, &context, tiny_file, None, true).unwrap();
        let tiny = tiny.unwrap();

        // a large unit: many declarations → a large source and a large AST
        let mut big_src = String::from("unit Big; interface\n");
        for index in 0..400 {
            big_src.push_str(&format!(
                "type TThing{index} = class F{index}: Integer; procedure Go{index}; end;\n"
            ));
        }
        big_src.push_str("implementation end.");
        let big_file = arena.insert_virtual("Big.pas", &big_src);
        let (_, big) = parse_and_cache(&arena, &context, big_file, None, true).unwrap();
        let big = big.unwrap();

        let tiny_weight = tiny.estimated_bytes();
        let big_weight = big.estimated_bytes();

        // the source-length proxy is recorded and non-trivial
        assert!(big.source_len as usize >= big_src.len() - 4);
        assert!(tiny.source_len > 0);

        // the load-bearing anti-undercount assertion: the big unit weighs at
        // least an order of magnitude more than the tiny one. A flat/undercount
        // estimate (old behaviour) would fail this hard.
        assert!(
            big_weight > tiny_weight * 10,
            "big unit ({big_weight} B, {} src) must weigh >> tiny ({tiny_weight} B, {} src)",
            big.source_len,
            tiny.source_len,
        );
        // and the weight tracks source size closely (within a small band of the
        // per-byte proxy), proving it is proportional, not a coincidental bump.
        let expected = big.source_len as u32 * UnitMeta::AST_BYTES_PER_SOURCE_BYTE as u32;
        assert!(
            big_weight >= expected,
            "weight {big_weight} must be at least the source-length proxy {expected}"
        );
    }

    #[test]
    fn program_parses_without_artifact() {
        let context = test_context(Vec::new());
        let arena = SourceArena::new();
        let file = arena.insert_virtual("d.dpr", "program D; begin end.");
        let (outcome, artifact) = parse_and_cache(&arena, &context, file, None, true).unwrap();
        assert!(artifact.is_none());
        // a program is not moved into a meta — its real source is Present
        assert!(matches!(outcome.source.present(), Some(Source::Program(_))));
        assert_eq!(context.unit_cache.entry_count(), 0);
    }
}
