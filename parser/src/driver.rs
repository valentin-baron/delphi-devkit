//! Session driver: owns everything one open project needs — context, arena,
//! snapshot store, file watcher, reverse-dependency index — and wires the
//! runtime rules together:
//!
//! - **Open = context swap**: build context, load the LocalAppData snapshot
//!   (hash-validated), rebuild the reverse index from what survived.
//! - **`tick(now)`**: poll the watcher; apply per-file invalidation or (after
//!   a burst like a git checkout, once quiescent) a full hash-revalidation
//!   sweep followed by an index rebuild; autosave when dirty and the save
//!   interval elapsed.
//! - **`shutdown()`**: final save. `Drop` deliberately does NOT save — a
//!   failing save in a destructor is unreportable; owners must call
//!   `shutdown()`.
//!
//! The driver is synchronous; delphi-devkit's async LSP layer wraps it
//! (spawn_blocking + its own cadence for `tick`).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use std::collections::HashMap;

use crate::ast::TypeExpression;
use crate::cache_store::{CacheIdentity, CacheStore};
use crate::context::{CompilerProfile, ContextError, Identifier, ProjectContext};
use crate::token::Token;
use crate::dfm::parse_dfm;
use crate::dfm_link::{DfmLinkResult, link_dfm};
use crate::meta::{CodeLocation, FileId};
use crate::query::{
    Completion, CompletionKind, DiagnosticSource, QueryTarget, TargetKind, UnifiedDiagnostic,
    UnusedUnit,
};
use crate::parse_state::InterfaceLoader;
use crate::references::{Occurrence, ReferenceIndex};
use crate::source::SourceArena;
use crate::parser::ParseOutcome;
use crate::pipeline;
use crate::unit_cache::{CacheEntry, CachePersistError, LoadReport, SaveReport, SymbolKind};
use crate::unit_loader::UnitLoader;
use crate::unit_meta::UnitMeta;
use crate::watcher::{
    ChangeCollectorConfig, FileWatcher, InvalidationPlan, InvalidationReport,
    ReverseDependencyIndex, WatchError, apply_invalidation,
};

/// Resident-disk-content cap for [`ProjectSession::trim_arena`] (Task-19): the
/// most decoded text + raw bytes the process-global arena keeps materialized for
/// DISK files between checkpoints. Chosen at 64 MiB — comfortably above one
/// unit's transitive parse chain (so a single analyze never thrashes: it stays
/// resident through the parse, is trimmed only afterward if the total is over
/// budget), and well under the ~188 MiB the moka AST cache settles at (Task-16),
/// so the arena stops being the dominant unbounded term without competing with
/// the AST working set. Virtual (unsaved) buffers are outside this cap (bounded
/// separately by Task-15 to one entry per open document).
pub const ARENA_DISK_CONTENT_CAP: usize = 64 * 1024 * 1024;

#[derive(Debug, Default)]
pub struct SessionError {
    pub message: String,
    /// The source location the failure points at, when it carries one — the
    /// anchor an LSP layer uses to place a precise squiggle for a hard parse
    /// failure. `None` when the failure has no intrinsic location (I/O, cache,
    /// watcher errors), in which case the caller anchors at the top of the
    /// document. Only [`ProjectSession::parse_buffer`] / `parse_source_file`
    /// populate it (from [`crate::parser::error_location`]); every other
    /// construction site leaves it `None`.
    pub location: Option<CodeLocation>,
}

impl SessionError {
    /// A message-only error with no source location (I/O, cache, watcher).
    pub fn message(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            location: None,
        }
    }
}

impl From<ContextError> for SessionError {
    fn from(error: ContextError) -> Self {
        Self::message(error.0)
    }
}

impl From<CachePersistError> for SessionError {
    fn from(error: CachePersistError) -> Self {
        Self::message(error.message)
    }
}

impl From<WatchError> for SessionError {
    fn from(error: WatchError) -> Self {
        Self::message(error.message)
    }
}

#[derive(Debug, Clone)]
pub struct SessionOptions {
    pub save_interval: Duration,
    pub collector: ChangeCollectorConfig,
    /// Snapshot base directory; `None` = `%LOCALAPPDATA%\delphi-devkit\parser-cache`.
    pub snapshot_base: Option<PathBuf>,
    /// Start the OS file watcher. Off for one-shot batch parses.
    pub watch: bool,
    /// Compiler standard-unit source directories (RTL/VCL), appended to the
    /// project search paths at open. Devkit obtains them via
    /// `ddk::standard_source_directories`.
    pub standard_source_paths: Vec<PathBuf>,
}

impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            save_interval: Duration::from_secs(300),
            collector: ChangeCollectorConfig::default(),
            snapshot_base: None,
            watch: true,
            standard_source_paths: Vec::new(),
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct TickReport {
    pub invalidated_units: usize,
    pub swept: bool,
    /// The autosave outcome for this tick, `None` if no save was due. Carries
    /// the full [`SaveReport`] (written + any skipped metas) so a caller can see
    /// a dropped unit — the skip is threaded, never swallowed.
    pub saved: Option<SaveReport>,
}

pub struct ProjectSession {
    context: Arc<ProjectContext>,
    /// The process-global arena (`&'static`). All files this session parses
    /// register here, so serialized `FileId`s resolve consistently on save.
    arena: &'static SourceArena,
    /// The durable snapshot store. Held behind `Arc` so the SAME instance serves
    /// both the bulk save (`save_now`) and the per-unit persist sink attached to
    /// the cache (persist-on-insert + evict-to-disk, Task 16).
    store: Arc<CacheStore>,
    index: Arc<ReverseDependencyIndex>,
    watcher: Option<FileWatcher>,
    save_interval: Duration,
    last_save: Instant,
    dirty: bool,
    /// Snapshot load outcome of `open` (None = no snapshot existed).
    pub load_report: Option<LoadReport>,
    /// Non-fatal findings (skipped watch directories, ...).
    pub notes: Vec<String>,
    /// DFM↔PAS link results, keyed by the unit's folded key. Populated when a
    /// unit with a sibling `.dfm` is parsed (see [`Self::parse_source_file`]).
    /// This is the session-level surface the LSP layer (task 5) reads for
    /// cross-boundary go-to-definition / rename.
    dfm_links: HashMap<Identifier, DfmLinkResult>,
    /// Symbol → occurrences index for `references` (task 5, A.3). Kept
    /// consistent with cache invalidation exactly like `dfm_links`: a unit's
    /// occurrences are purged when it is evicted and rebuilt on full sweeps, so
    /// no occurrence ever points into a gone unit.
    reference_index: ReferenceIndex,
    /// Per-unit parse diagnostics (from error-tolerant reparse + directive
    /// evaluation), keyed by folded unit key. Unified with dfm diagnostics by
    /// [`Self::diagnostics`]. Purged on invalidation like `dfm_links`.
    parse_diagnostics: HashMap<Identifier, Vec<UnifiedDiagnostic>>,
}

impl ProjectSession {
    /// Open a project: context from dproj, snapshot load, index rebuild,
    /// watcher start. This IS the "context swap" entry point — switching
    /// config/platform means opening a new session.
    pub fn open(
        dproj_path: impl AsRef<Path>,
        configuration: Option<&str>,
        platform: Option<&str>,
        compiler: &CompilerProfile,
        options: SessionOptions,
    ) -> Result<Self, SessionError> {
        let dproj_path = dproj_path.as_ref();
        let mut context =
            ProjectContext::from_dproj(dproj_path, configuration, platform, compiler)?;
        // standard units (System.SysUtils, Vcl.Forms, ...) resolve like any
        // other unit — their sources are just more search paths
        context
            .search_paths
            .extend(options.standard_source_paths.iter().cloned());
        let context = Arc::new(context);

        let identity = CacheIdentity {
            project_path: dproj_path,
            configuration: &context.configuration,
            platform: &context.platform_name,
            compiler_version: context.compiler_version,
        };
        let store = match &options.snapshot_base {
            Some(base) => CacheStore::in_directory(base, &identity)?,
            None => CacheStore::for_project(&identity)?,
        };

        let mut session = Self::from_parts(context, store, options.save_interval);

        // context swap: bring the persisted artifacts back (hash-validated)
        session.load_report = session
            .store
            .load_into(&session.context.unit_cache)?;
        session.rebuild_index();

        if options.watch {
            let directories = session.watch_directories(dproj_path);
            session.watcher = Some(FileWatcher::start(&directories, options.collector)?);
        }
        Ok(session)
    }

    /// Assemble from prebuilt parts (tests, devkit custom setups). No
    /// snapshot load, no watcher — callers wire those explicitly.
    pub fn from_parts(
        context: Arc<ProjectContext>,
        store: CacheStore,
        save_interval: Duration,
    ) -> Self {
        let store = Arc::new(store);
        // Attach the per-unit persist sink to the cache (Task 16): from here a
        // freshly-parsed DISK unit is written to its per-unit file on insert
        // (before it can be evicted) and any not-yet-persisted `Done` entry is
        // written on eviction — so an eviction is always a safe, reloadable drop
        // and RAM holds only the working set. The sink's never-persist gate
        // skips virtual/tainted/recovered metas (#21/#25). Cloning the `Arc`
        // shares the ONE store instance with the bulk `save_now`.
        context.unit_cache.attach_persister(store.clone());
        Self {
            context,
            arena: crate::globals::arena(),
            store,
            index: Arc::new(ReverseDependencyIndex::default()),
            watcher: None,
            save_interval,
            last_save: Instant::now(),
            dirty: false,
            load_report: None,
            notes: Vec::new(),
            dfm_links: HashMap::new(),
            reference_index: ReferenceIndex::default(),
            parse_diagnostics: HashMap::new(),
        }
    }

    /// Parse one source file through the full pipeline: lazy import loading
    /// (nested units land in the cache AND the reverse index via the
    /// loader), artifact production, dirty tracking.
    pub fn parse_source_file(
        &mut self,
        path: impl AsRef<Path>,
    ) -> Result<(ParseOutcome, Option<Arc<UnitMeta>>), SessionError> {
        let file = self.arena.load(path).map_err(|error| {
            SessionError::message(format!("{}: {}", error.path.display(), error.message))
        })?;
        let inserts_before = self.context.unit_cache.insert_count();

        let loader = UnitLoader::with_store(
            self.arena,
            self.context.clone(),
            Some(self.index.clone()),
            Some(self.store.clone()),
        );
        let (outcome, meta) =
            pipeline::parse_and_cache(self.arena, &self.context, file, Some(loader))
                .map_err(|error| SessionError {
                    location: crate::parser::error_location(&error),
                    message: format!("parse failed: {error:?}"),
                })?;

        if let Some(meta) = &meta {
            self.index.index_artifact(meta.name(), meta);
            self.reference_index.index_unit(meta.name(), meta);
            self.record_parse_diagnostics(meta.name(), &outcome);
            self.link_sibling_dfm(meta);
        }
        // Nested units parsed as import side effects also belong in the
        // reference index (find-references spans every cached unit, not only
        // the one explicitly opened). Fold them in from the cache.
        if self.context.unit_cache.insert_count() != inserts_before {
            self.index_nested_units();
        }
        // nested units cached as side effects also count as new state
        if meta.is_some() || self.context.unit_cache.insert_count() != inserts_before {
            self.dirty = true;
        }
        Ok((outcome, meta))
    }

    /// Parse an in-memory (unsaved editor) buffer for `path` through the SAME
    /// pipeline as [`Self::parse_source_file`], seeding the arena with the
    /// caller-supplied `content` via [`SourceArena::insert_virtual`] instead of
    /// reading from disk. This is the LSP entry point: an editor holds unsaved
    /// text that must be analyzed before it is saved.
    ///
    /// INVARIANT PRESERVED (#21/#25): a virtual buffer never persists. The
    /// buffer's `FileId` carries a display-only path that does not canonicalize,
    /// so its source stamp hashes decoded content (never matches a disk read)
    /// and its serialized `FileId` fails to `register` on load — the meta is
    /// dropped as unreadable. `save_now` therefore never writes a virtual unit
    /// as if it were on-disk state. Tested by
    /// [`tests::parse_buffer_virtual_unit_is_not_persisted`].
    ///
    /// The returned `meta.name()` is the parsed unit's folded key — the handle
    /// the LSP maps its `Url` onto for subsequent query calls
    /// (`diagnostics`/`symbol_at`/…).
    ///
    /// The arena dedups virtual buffers by path ([`SourceArena::set_virtual`]):
    /// the FIRST parse of a document issues a stable virtual `FileId`; every
    /// later parse REUSES that id and REPLACES its content (dropping the prior
    /// String + raw bytes), so the process-global arena holds at most one
    /// virtual entry per open document rather than growing per keystroke
    /// (Task-15 memory bound). Re-parsing an edited buffer never reads a stale
    /// version: the content is replaced atomically before the parse, and the
    /// meta this parse produces indexes exactly the new content. The cache entry
    /// for the unit key is replaced by the pipeline's `insert`, so a query after
    /// `parse_buffer` sees the newest buffer.
    pub fn parse_buffer(
        &mut self,
        path: impl AsRef<Path>,
        content: &str,
    ) -> Result<(ParseOutcome, Option<Arc<UnitMeta>>), SessionError> {
        // Bound the arena: reuse ONE virtual entry per open-document path,
        // replacing (freeing) its prior content on re-parse. Using
        // `insert_virtual` here would append a fresh full-file copy to the
        // process-global arena on every keystroke → unbounded growth → OOM
        // (Task-15). `set_virtual` dedups by the display path and drops the
        // prior String. Sound because a parse is synchronous and completes
        // (dropping all `&str` borrows) before the next `set_virtual`; the meta
        // this parse produces indexes exactly this stored content.
        let file = self
            .arena
            .set_virtual(path.as_ref().to_path_buf(), content.to_string());
        let inserts_before = self.context.unit_cache.insert_count();

        // RESIDENT-ONLY loader (Task-15): the editor buffer parse must parse ONLY
        // this buffer. A cross-unit `{$IF Declared/SizeOf}` whose target is not
        // already resident in the RAM cache answers Unknown (safe AssumeFalse)
        // instead of force-loading the whole {$IF}-dependency closure — which
        // cascades through be.core + the RTL/VCL source tree and OOMs (14GB).
        // `parse_source_file` (batch/navigation/didSave) and the query handlers
        // (`make_loader`) stay Full: a single explicit navigation load is fine;
        // only the buffer parse triggered the cascade. Cross-unit precision here
        // improves as the cache warms.
        let loader = UnitLoader::with_store_resident_only(
            self.arena,
            self.context.clone(),
            Some(self.index.clone()),
            Some(self.store.clone()),
        );
        let (outcome, meta) =
            pipeline::parse_and_cache(self.arena, &self.context, file, Some(loader)).map_err(
                |error| SessionError {
                    location: crate::parser::error_location(&error),
                    message: format!("parse failed: {error:?}"),
                },
            )?;

        if let Some(meta) = &meta {
            self.index.index_artifact(meta.name(), meta);
            self.reference_index.index_unit(meta.name(), meta);
            self.record_parse_diagnostics(meta.name(), &outcome);
            self.link_sibling_dfm(meta);
        }
        // Imports pulled in as side effects (from disk — an editor buffer's
        // `uses` still resolves against on-disk units) also belong in the
        // reference index, exactly as `parse_source_file` folds them.
        if self.context.unit_cache.insert_count() != inserts_before {
            self.index_nested_units();
        }
        // A virtual buffer's own meta is NOT durable (it never validates on
        // load), but nested on-disk units pulled in as side effects ARE — mark
        // dirty only when such a real unit was cached, so an unsaved-buffer edit
        // alone does not trigger an autosave of nothing persistable.
        if self.context.unit_cache.insert_count() != inserts_before {
            self.dirty = true;
        }
        Ok((outcome, meta))
    }

    /// If the unit has a sibling `.dfm`, parse it and run the DFM↔PAS linker,
    /// storing the result under the unit key for later query. A dfm that fails
    /// to read/parse (deleted between stamp and link, binary form, syntax
    /// error) is recorded as a note, not fatal — the unit itself is unaffected.
    fn link_sibling_dfm(&mut self, meta: &UnitMeta) {
        let Some(dfm_stamp) = &meta.dfm else {
            // No form for this unit — clear any prior link (the dfm may have
            // been deleted since the last parse).
            self.dfm_links.remove(&meta.name());
            return;
        };
        // Decode through the arena so the dfm gets the same BOM-aware
        // (UTF-8 / UTF-16 / ANSI) decoding as pas sources — binary DFMs decode
        // to the U+00FF marker the parser rejects distinctly (ledger #23).
        let file = match self.arena.load(&dfm_stamp.path) {
            Ok(file) => file,
            Err(error) => {
                self.notes.push(format!(
                    "dfm read failed for {}: {}",
                    dfm_stamp.path.display(),
                    error.message
                ));
                return;
            }
        };
        let source = match self.arena.content(file) {
            Ok(source) => source,
            Err(error) => {
                self.notes.push(format!(
                    "dfm decode failed for {}: {}",
                    dfm_stamp.path.display(),
                    error.message
                ));
                return;
            }
        };
        match parse_dfm(source, &self.context) {
            Ok(root) => {
                let result = link_dfm(meta.interface(), &root);
                self.dfm_links.insert(meta.name(), result);
            }
            Err(error) => {
                self.notes.push(format!(
                    "dfm parse failed for {}: {} (at byte {})",
                    dfm_stamp.path.display(),
                    error.message,
                    error.position
                ));
            }
        }
    }

    /// The stored DFM↔PAS link result for a unit (by folded key), if the unit
    /// has a form and was linked during a parse. This is the session-level
    /// query surface for the LSP layer (task 5).
    pub fn dfm_links(&self, unit_key: Identifier) -> Option<&DfmLinkResult> {
        self.dfm_links.get(&unit_key)
    }

    /// Fold every cached `Done` meta into the reference index. Called after a
    /// parse pulled nested units into the cache so their occurrences are
    /// searchable too (find-references spans all cached units). `index_unit`
    /// is idempotent per unit, so re-indexing an already-present unit is a
    /// no-op-equivalent replace.
    fn index_nested_units(&mut self) {
        self.context.unit_cache.run_pending_tasks();
        let metas: Vec<(Identifier, Arc<UnitMeta>)> = self
            .context
            .unit_cache
            .iter_entries()
            .filter_map(|(unit, entry)| match entry {
                CacheEntry::Done(meta) => Some((unit, meta)),
                CacheEntry::Failed(_) => None,
            })
            .collect();
        for (unit, meta) in metas {
            self.reference_index.index_unit(unit, &meta);
        }
    }

    /// Store the parse diagnostics for a unit (converted to the unified shape),
    /// replacing any prior set. Purged on invalidation like `dfm_links`.
    fn record_parse_diagnostics(&mut self, unit_key: Identifier, outcome: &ParseOutcome) {
        let diagnostics: Vec<UnifiedDiagnostic> = outcome
            .diagnostics
            .iter()
            .map(|diagnostic| UnifiedDiagnostic {
                source: DiagnosticSource::Parse,
                // Carry the per-finding severity the cursor/parser chose at the
                // creation site (unknown `{$IF}` → Warning, dropped attribute →
                // Hint, …) — never re-flatten it here.
                severity: diagnostic.severity,
                location: Some(diagnostic.location),
                dfm_offset: None,
                message: diagnostic.message.clone(),
            })
            .collect();
        if diagnostics.is_empty() {
            self.parse_diagnostics.remove(&unit_key);
        } else {
            self.parse_diagnostics.insert(unit_key, diagnostics);
        }
    }

    // ─── LSP query API (task 5, Deliverable A) ───────────────────────────
    //
    // GOVERNING RULE: never a WRONG answer. Missing information → empty/None,
    // never a guess. Definition/references resolve through the SAME
    // dependency-recorded, cycle-safe loader machinery as scoped `Declared`.

    /// The cached meta for a unit key, if present and successfully parsed.
    fn meta_of(&self, unit_key: Identifier) -> Option<Arc<UnitMeta>> {
        match self.context.unit_cache.get(unit_key)? {
            CacheEntry::Done(meta) => Some(meta),
            CacheEntry::Failed(_) => None,
        }
    }

    /// Public accessor for the cached meta of a unit key — the LSP read handlers
    /// use it to REUSE the meta produced by the last `analyze` (which parsed the
    /// buffer) instead of re-parsing on every hover/completion/definition
    /// request. Returns `None` when the unit was never parsed or its last parse
    /// failed. The returned meta's spans index the arena content of the version
    /// `analyze` last parsed; a read handler maps request positions through the
    /// requesting document's own current [`crate::meta::LineIndex`], which is the
    /// same version because a newer edit triggers a fresh `analyze` on
    /// `didChange`.
    pub fn meta_for(&self, unit_key: Identifier) -> Option<Arc<UnitMeta>> {
        self.meta_of(unit_key)
    }

    /// The identifier occurrence under a byte `position` in `file`'s unit. Scans
    /// the unit's interface symbol + member declaration spans and its recorded
    /// usages for the span covering `position`; returns the folded key, display
    /// spelling, kind and span. `None` if nothing is there or the unit is not
    /// cached under `unit_key`.
    ///
    /// Declaration/member spans win over usage spans at the same position (a
    /// declaration site IS also recorded as a usage in the body index — the
    /// authoritative identity is the declaration).
    pub fn symbol_at(&self, unit_key: Identifier, position: u32) -> Option<QueryTarget> {
        let meta = self.meta_of(unit_key)?;

        // SCOPE FIRST (shadowing): a body-local variable/parameter in the
        // enclosing implementation routine wins over any same-named interface
        // symbol. Only consulted when the impl-section structure pass was fully
        // reliable — a degraded pass falls through to today's behavior (never a
        // wrong local attribution). Outside any routine body this finds nothing
        // and also falls through, so an interface symbol used outside a body
        // still resolves to the interface.
        if let Some(local) = self.local_at(&meta, position) {
            return Some(local);
        }

        let interface = meta.interface();

        // Prefer a declaration/member site (most specific identity).
        for symbol in &interface.symbols {
            if span_covers(symbol.location, position) {
                return Some(QueryTarget {
                    key: symbol.key,
                    display: symbol.name,
                    kind: TargetKind::Declaration,
                    location: symbol.location,
                    owner_type: None,
                });
            }
            for member in &symbol.members {
                if span_covers(member.location, position) {
                    return Some(QueryTarget {
                        key: member.key,
                        display: member.name,
                        kind: TargetKind::Member,
                        location: member.location,
                        owner_type: Some(symbol.key),
                    });
                }
            }
        }
        // Otherwise a usage occurrence. Pick the tightest span covering the
        // position (nested spans can overlap; the smallest is the ident).
        meta.usages
            .iter()
            .filter(|usage| span_covers(usage.location, position))
            .min_by_key(|usage| usage.location.span.len())
            .map(|usage| QueryTarget {
                key: usage.symbol,
                display: usage.symbol,
                kind: TargetKind::Usage,
                location: usage.location,
                owner_type: None,
            })
    }

    /// Resolve `position` to a body-local variable/parameter of the enclosing
    /// implementation routine, if any. Same-unit only (never a cross-unit load).
    ///
    /// Gating (never a wrong answer): returns `None` immediately when the unit's
    /// impl-section structure pass was not fully reliable — a degraded pass may
    /// carry a mis-attributed `body_span`, so we resolve nothing and let the
    /// caller fall through to today's interface/usage logic.
    ///
    /// Enclosing routine = the `ImplRoutine` whose `body_span` covers `position`;
    /// for nested routines the TIGHTEST-covering (smallest span) wins. Within it,
    /// a match is either: the cursor sits directly on a param/local's own
    /// declaration span, OR the identifier under the cursor (found via the flat
    /// usage index at this position) has the folded key of one of the routine's
    /// params/locals. Either way the returned `location` is the DECLARATION's own
    /// span, and the target `kind` is [`TargetKind::Local`].
    fn local_at(&self, meta: &UnitMeta, position: u32) -> Option<QueryTarget> {
        if !meta.impl_scopes_reliable {
            return None;
        }
        // ALL enclosing routines whose body covers `position`, ordered
        // TIGHTEST→WIDEST (ascending body-span length). Nested routines overlap;
        // this models lexical scoping — an inner routine's param/local shadows an
        // outer routine's same-named one, and (Bug 2) an OUTER routine's local is
        // still found before the query falls through to an interface symbol.
        let mut covering_routines: Vec<&crate::ast::ImplRoutine> = meta
            .impl_scopes
            .iter()
            .filter(|routine| {
                let span = routine.body_span;
                span.start <= position && position < span.end
            })
            .collect();
        covering_routines.sort_by_key(|routine| routine.body_span.len());
        if covering_routines.is_empty() {
            return None;
        }

        // 1. Cursor directly on a param/local declaration span. Walk tightest→
        // widest; the first covering routine that owns the declaration wins.
        for routine in &covering_routines {
            for declaration in routine.params.iter().chain(routine.locals.iter()) {
                if span_covers(declaration.name.location, position) {
                    return Some(local_target(declaration));
                }
            }
        }

        // 2. Cursor on a body identifier whose key matches a param/local. Find
        // the identifier key at this position from the flat usage index (the
        // same source `symbol_at`'s usage branch uses), then match by folded key.
        let occurrence = meta
            .usages
            .iter()
            .filter(|usage| span_covers(usage.location, position))
            .min_by_key(|usage| usage.location.span.len())?;

        // Bug 1 (never a WRONG answer): a MEMBER access such as `SomeObj.Value`
        // must NOT bind to a scope local named `Value` — `.Value` is SomeObj's
        // member, not the routine's local. Skip the key match when the occurrence
        // is immediately preceded (modulo whitespace) by a `.`. The guard errs
        // toward "possibly a member" (skip the local) whenever the buffer cannot
        // prove the identifier is NOT dotted, so we never mis-bind a member.
        if self.occurrence_is_member_access(occurrence.location.file, occurrence.location.span.start)
        {
            return None;
        }
        let occurrence_key = occurrence.symbol;

        // Match tightest→widest so an inner routine's local shadows an outer's.
        for routine in &covering_routines {
            if let Some(declaration) = routine
                .params
                .iter()
                .chain(routine.locals.iter())
                .find(|declaration| declaration.name.key == occurrence_key)
            {
                return Some(local_target(declaration));
            }
        }
        None
    }

    /// Whether the identifier starting at byte `ident_start` in `file` is a
    /// MEMBER access — i.e. the last non-whitespace character in the source
    /// *before* `ident_start` is a `.` (as in `SomeObj.Value`). Mirrors
    /// [`Self::dot_precedes`]'s buffer/char-boundary guards.
    ///
    /// Safe default (never a wrong answer): the caller uses this to REJECT a
    /// scope-local bind for a member access. So any case where we cannot PROVE
    /// the identifier is not dotted — an unreadable buffer, an out-of-range or
    /// char-boundary-splitting offset — returns `true` ("possibly a member"),
    /// causing the caller to skip the local. We only return `false` (allow the
    /// local bind) when the buffer is readable AND the preceding non-whitespace
    /// character is provably not a `.`.
    fn occurrence_is_member_access(&self, file: FileId, ident_start: u32) -> bool {
        let Ok(content) = self.arena.content(file) else {
            // Can't read the buffer → cannot prove it is NOT a member → treat as
            // a possible member and skip the local (conservative).
            return true;
        };
        let end = ident_start as usize;
        // An out-of-range or non-char-boundary offset is a stale/foreign span;
        // we cannot trust it, so treat as a possible member.
        if end > content.len() || !content.is_char_boundary(end) {
            return true;
        }
        // The last non-whitespace char before the identifier decides it. When
        // nothing precedes it (start of buffer, or only whitespace), it is NOT a
        // member access → allow the local bind.
        matches!(content[..end].trim_end().chars().next_back(), Some('.'))
    }

    /// Declaration site(s) of a symbol. Resolution order, each cycle-safe and
    /// dependency-honest:
    /// 1. an own interface symbol of `unit_key` → its declaration location;
    /// 2. else resolve through `unit_key`'s imports (reverse uses order, via the
    ///    same loader as scoped `Declared`) to the declaring unit's symbol.
    ///
    /// `member_owner`, when set, targets `Owner.Member`: the owner type is
    /// resolved first (own then imports), then its member's declaration site.
    ///
    /// Returns an empty vec when unresolved — NEVER a wrong location.
    pub fn definition(
        &self,
        unit_key: Identifier,
        symbol_key: Identifier,
        member_owner: Option<Identifier>,
    ) -> Vec<CodeLocation> {
        let Some(meta) = self.meta_of(unit_key) else {
            return Vec::new();
        };

        // Member target: resolve the owner type first, then its member.
        if let Some(owner_key) = member_owner {
            return self.member_definition(&meta, owner_key, symbol_key);
        }

        // (1) own interface symbol
        if let Some(symbol) = meta.interface().find(symbol_key) {
            return vec![symbol.location];
        }

        // (2) imported units, reverse uses order, via the loader (cycle-safe,
        // records the consulted units as dependencies — identical discipline to
        // scoped `Declared`). First declaring unit wins.
        let loader = self.make_loader();
        for import in imports_reversed(&meta) {
            if let crate::parse_state::LoadOutcome::Loaded(imported) = loader.interface_of(import) {
                if let Some(symbol) = imported.interface().find(symbol_key) {
                    return vec![symbol.location];
                }
            }
        }
        Vec::new()
    }

    /// Position-aware go-to-definition. Resolves the occurrence under `position`
    /// via [`Self::symbol_at`], then:
    /// - a [`TargetKind::Local`] target (a body-local variable/parameter) resolves
    ///   directly to its own declaration span (the local's `location`) — the
    ///   key-based [`Self::definition`] cannot see scope, so this position-aware
    ///   entry point is required for locals;
    /// - any other target delegates to the existing key-based
    ///   [`Self::definition`] (own interface first, then imports), unchanged.
    ///
    /// Returns an empty vec when nothing resolves — never a wrong location.
    pub fn definition_at(&self, unit_key: Identifier, position: u32) -> Vec<CodeLocation> {
        let Some(target) = self.symbol_at(unit_key, position) else {
            return Vec::new();
        };
        if target.kind == TargetKind::Local {
            return vec![target.location];
        }
        self.definition(unit_key, target.key, target.owner_type)
    }

    /// Definition site of `member_key` on type `owner_key`, resolving the owner
    /// (own interface first, then imports) and returning the member's
    /// declaration location. Empty if the owner or member is unresolved.
    fn member_definition(
        &self,
        meta: &UnitMeta,
        owner_key: Identifier,
        member_key: Identifier,
    ) -> Vec<CodeLocation> {
        // owner in this unit
        if let Some(owner) = meta.interface().find(owner_key) {
            if let Some(member) = owner.find_member(member_key) {
                return vec![member.location];
            }
            // owner exists here but the member is not a DIRECT member — it may
            // be inherited from a base we do not flatten. Unknown, not wrong →
            // empty (never a bogus location).
            return Vec::new();
        }
        // owner in an imported unit
        let loader = self.make_loader();
        for import in imports_reversed(meta) {
            if let crate::parse_state::LoadOutcome::Loaded(imported) = loader.interface_of(import) {
                if let Some(owner) = imported.interface().find(owner_key) {
                    if let Some(member) = owner.find_member(member_key) {
                        return vec![member.location];
                    }
                    return Vec::new();
                }
            }
        }
        Vec::new()
    }

    /// The declared facts of the symbol under `position` in `unit_key`'s source,
    /// for `textDocument/hover`. Resolves the occurrence via [`Self::symbol_at`],
    /// then its DECLARATION (own interface first, else imports in reverse uses
    /// order via the loader) through the SAME cross-unit machinery as
    /// [`Self::definition`] — a hover over an imported symbol shows the imported
    /// declaration's facts.
    ///
    /// Never-wrong rule: a cursor over an identifier that resolves to no
    /// interface declaration (an unknown name, an implementation-only local, a
    /// member on an unresolved owner) yields `None` — never fabricated facts. An
    /// anonymous/complex declared type the parser did not reduce to a simple key
    /// leaves `type_key` `None`; the caller then shows the KIND only.
    pub fn hover_info(&self, unit_key: Identifier, position: u32) -> Option<crate::query::HoverInfo> {
        let meta = self.meta_of(unit_key)?;
        let target = self.symbol_at(unit_key, position)?;
        let occurrence = target.location;

        // A body-local variable/parameter (same-unit scope): its facts come from
        // its own declaration (kind + simple type key), NEVER from an interface
        // lookup — a same-named interface symbol must not leak into a local's
        // hover. Resolved entirely from the enclosing routine's `impl_scopes`.
        if target.kind == TargetKind::Local {
            return self.local_hover(&meta, position, occurrence);
        }

        // A member occurrence (`Owner.Member`, or a member declaration site):
        // resolve the owner, then read the member's facts.
        if let Some(owner_key) = target.owner_type {
            return self.member_hover(&meta, owner_key, target.key, occurrence);
        }

        // A top-level symbol: own interface first, then imports (reverse uses
        // order), identical resolution order to `definition`.
        if let Some(symbol) = meta.interface().find(target.key) {
            return Some(symbol_hover(symbol, occurrence));
        }
        let loader = self.make_loader();
        for import in imports_reversed(&meta) {
            if let crate::parse_state::LoadOutcome::Loaded(imported) = loader.interface_of(import) {
                if let Some(symbol) = imported.interface().find(target.key) {
                    return Some(symbol_hover(symbol, occurrence));
                }
            }
        }
        // The occurrence resolves to no interface declaration — unknown, not
        // wrong. None (the caller shows no hover).
        None
    }

    /// Hover facts for a body-local variable/parameter at `position`: its kind
    /// (var/const/type/param) and simple declared type key, read entirely from
    /// the enclosing routine's `impl_scopes` — NEVER an interface lookup. `None`
    /// if the local can no longer be located (defensive; `symbol_at` already
    /// matched one, so this normally resolves).
    fn local_hover(
        &self,
        meta: &UnitMeta,
        position: u32,
        occurrence: CodeLocation,
    ) -> Option<crate::query::HoverInfo> {
        // Re-locate the exact declaration the target matched (same tightest-cover
        // + key logic as `local_at`), so the hover reads its kind + type key.
        if !meta.impl_scopes_reliable {
            return None;
        }
        let routine = meta
            .impl_scopes
            .iter()
            .filter(|routine| {
                let span = routine.body_span;
                span.start <= position && position < span.end
            })
            .min_by_key(|routine| routine.body_span.len())?;

        let matches_position = |declaration: &crate::ast::LocalDeclaration| {
            span_covers(declaration.name.location, position)
        };
        let occurrence_key = meta
            .usages
            .iter()
            .filter(|usage| span_covers(usage.location, position))
            .min_by_key(|usage| usage.location.span.len())
            .map(|usage| usage.symbol);

        let declaration = routine
            .params
            .iter()
            .chain(routine.locals.iter())
            .find(|declaration| {
                matches_position(declaration)
                    || occurrence_key == Some(declaration.name.key)
            })?;

        Some(crate::query::HoverInfo {
            display: declaration.name.name,
            kind: local_completion_kind(declaration.decl_kind),
            type_key: declaration.type_key,
            directives: Vec::new(),
            visibility: crate::ast::Visibility::Unspecified,
            owner_type: None,
            occurrence,
        })
    }

    /// Hover facts for `member_key` on type `owner_key`: resolve the owner (own
    /// interface first, then imports) and read the member's kind/type/directives/
    /// visibility. `None` if the owner or the member is unresolved (never
    /// fabricated facts).
    fn member_hover(
        &self,
        meta: &UnitMeta,
        owner_key: Identifier,
        member_key: Identifier,
        occurrence: CodeLocation,
    ) -> Option<crate::query::HoverInfo> {
        if let Some(owner) = meta.interface().find(owner_key) {
            return owner
                .find_member(member_key)
                .map(|member| member_hover(member, owner.name, occurrence));
        }
        let loader = self.make_loader();
        for import in imports_reversed(meta) {
            if let crate::parse_state::LoadOutcome::Loaded(imported) = loader.interface_of(import) {
                if let Some(owner) = imported.interface().find(owner_key) {
                    return owner
                        .find_member(member_key)
                        .map(|member| member_hover(member, owner.name, occurrence));
                }
            }
        }
        None
    }

    /// Every recorded occurrence of `symbol_key` across all cached units (the
    /// candidate set — see [`crate::references`] for the over-approximation
    /// note). Consistent with invalidation: an evicted unit's occurrences are
    /// purged, so this never returns a span in a gone unit.
    pub fn references(&self, symbol_key: Identifier) -> Vec<Occurrence> {
        self.reference_index.occurrences(symbol_key).to_vec()
    }

    /// The resolved signature of a routine callee, for
    /// `textDocument/signatureHelp`. Reads parameters + return type from the
    /// AST's [`crate::ast::RoutineType`] — the derived interface index does NOT
    /// carry parameters, so this query walks `UnitMeta.ast`.
    ///
    /// Resolution (SAME cross-unit loader as [`Self::definition`]):
    /// - `owner = Some(type)` (a member routine `Obj.Method`): resolve the owner
    ///   type (own interface first, then imports), then its `Member::Method`
    ///   whose folded name == `callee_key`; read the method's `routine`.
    /// - `owner = None` (a top-level routine): the interface declaration of kind
    ///   `Procedure`/`Function` with that key (own then imports); its
    ///   `type_expression` is a `TypeExpression::Routine`.
    ///
    /// Never fabricated: an unresolved callee, a non-routine symbol, or a member
    /// on an unresolved owner yields `None`. A procedure carries
    /// `return_type = None`; an untyped parameter renders without a `: Type`; a
    /// defaulted parameter carries its ` = default`.
    ///
    /// OVERLOADS: the interface index folds overloads to one key; the AST keeps
    /// every declaration. This walks ALL declarations/members matching the key
    /// and returns one [`crate::query::SignatureInfo`] per matching routine, in
    /// source order — so distinguishable overloads each get a signature. (A
    /// top-level `overload` set spread across units resolves only within the
    /// first declaring unit found, matching `definition`'s own-then-first-import
    /// order; noted as the cross-unit-overload limitation.)
    pub fn signature_help(
        &self,
        unit_key: Identifier,
        callee_key: Identifier,
        owner: Option<Identifier>,
    ) -> Vec<crate::query::SignatureInfo> {
        let Some(meta) = self.meta_of(unit_key) else {
            return Vec::new();
        };

        // Member routine: resolve the owner type, then its method(s).
        if let Some(owner_key) = owner {
            return self.member_signatures(&meta, owner_key, callee_key);
        }

        // Top-level routine: own interface declarations first.
        let own = top_level_signatures(&meta, callee_key);
        if !own.is_empty() {
            return own;
        }
        // Then imports (reverse uses order); first declaring unit wins.
        let loader = self.make_loader();
        for import in imports_reversed(&meta) {
            if let crate::parse_state::LoadOutcome::Loaded(imported) = loader.interface_of(import) {
                let signatures = top_level_signatures(&imported, callee_key);
                if !signatures.is_empty() {
                    return signatures;
                }
            }
        }
        Vec::new()
    }

    /// Signature help for the callee at byte `callee_offset` — the higher-level
    /// entry the LSP server calls. Resolves the callee identifier and its owner
    /// (for a member call) itself, so the server only supplies a text offset.
    ///
    /// Resolution, in order (each honest, never fabricated):
    /// 1. `symbol_at(callee_offset)` → the callee's folded key. Nothing there →
    ///    empty.
    /// 2. If that occurrence already carries an `owner_type` (a member
    ///    declaration/`Type.Member` usage the index linked), use it.
    /// 3. Else, if a `.` immediately precedes the callee (a `Receiver.Method(`
    ///    call), resolve the RECEIVER's type via the SAME machinery completion
    ///    uses ([`Self::member_receiver_at`]) — a static `TType.Method(` (the
    ///    receiver is a type) resolves; an instance receiver whose declared type
    ///    the index does not carry does NOT (→ no owner, and a top-level lookup
    ///    that fails yields empty, never a wrong signature).
    /// 4. Else treat it as a top-level routine.
    ///
    /// The receiver-type resolution for INSTANCE variables is limited by the
    /// derived index not carrying a top-level symbol's declared type (same
    /// limitation as member completion on an instance receiver); such a call
    /// yields no signature rather than a fabricated one.
    pub fn signature_help_at(
        &self,
        unit_key: Identifier,
        callee_offset: u32,
    ) -> Vec<crate::query::SignatureInfo> {
        let Some(meta) = self.meta_of(unit_key) else {
            return Vec::new();
        };
        let Some(target) = self.symbol_at(unit_key, callee_offset) else {
            return Vec::new();
        };

        // (2) an owner the index already linked (a member declaration site, or a
        // `Type.Member` usage that recorded its owner).
        if let Some(owner_key) = target.owner_type {
            return self.member_signatures(&meta, owner_key, target.key);
        }

        // (3) a `Receiver.` member call: resolve the receiver's type (static type
        // receiver resolves; unresolved receiver → no owner). Position the
        // receiver search at the callee's own start so the `.` before it gates.
        if let Some(receiver_type) = self.member_receiver_at(&meta, callee_offset) {
            let signatures = self.member_signatures(&meta, receiver_type, target.key);
            if !signatures.is_empty() {
                return signatures;
            }
            // A resolved receiver whose type has no such method → empty (never a
            // wrong signature), do NOT fall through to a top-level name clash.
            return Vec::new();
        }

        // (4) top-level routine (own then imports).
        self.signature_help(unit_key, target.key, None)
    }

    /// Signatures of `method_key` on type `owner_key`: resolve the owner (own
    /// interface first, then imports) and read its matching `Member::Method`
    /// routine(s). Empty if the owner or method is unresolved (never fabricated).
    fn member_signatures(
        &self,
        meta: &UnitMeta,
        owner_key: Identifier,
        method_key: Identifier,
    ) -> Vec<crate::query::SignatureInfo> {
        if let Some(declaration) = find_type_declaration(meta, owner_key) {
            return method_signatures(declaration, method_key);
        }
        let loader = self.make_loader();
        for import in imports_reversed(meta) {
            if let crate::parse_state::LoadOutcome::Loaded(imported) = loader.interface_of(import) {
                if let Some(declaration) = find_type_declaration(&imported, owner_key) {
                    return method_signatures(declaration, method_key);
                }
            }
        }
        Vec::new()
    }

    /// Context-sensitive completions at `position` in `unit_key`'s source.
    ///
    /// - After a `.` (member access): the members of the type of the identifier
    ///   before the dot, resolved via the interface index (own type first, then
    ///   imports). Members only, with visibility surfaced. An unresolvable
    ///   receiver yields an EMPTY member list (never a wrong member set).
    /// - Otherwise (top-level): builtins + own interface symbols declared so far
    ///   (up to `position`) + interface symbols of imported units, de-duplicated
    ///   by folded key.
    pub fn completions(&self, unit_key: Identifier, position: u32) -> Vec<Completion> {
        let Some(meta) = self.meta_of(unit_key) else {
            return Vec::new();
        };
        match self.member_receiver_at(&meta, position) {
            Some(receiver_type_key) => self.member_completions(&meta, receiver_type_key),
            None => self.top_level_completions(&meta, position),
        }
    }

    /// Classified tokens over `unit_key`'s source, for `textDocument/semanticTokens`
    /// (task 13). Lexes the unit's own source ONCE (spans into that file), then
    /// classifies each token, emitting a [`crate::query::SemanticToken`] ONLY when
    /// the classification is CERTAIN:
    ///
    /// - LEXICAL (precise, from the lexer): comments → Comment; string/char-code
    ///   literals → String; int/float literals → Number; reserved words →
    ///   Keyword; `{$…}` directives → Macro; operators/punctuation → Operator.
    ///   Trivia (whitespace/newline) produces no token.
    /// - DECLARATION/MEMBER/PARAMETER NAMES (precise, structural): an identifier
    ///   token whose span exactly covers a declaration/member/parameter NAME site
    ///   (via [`Self::symbol_at`] and the own-unit AST) → that site's kind + the
    ///   `declaration` modifier.
    /// - IDENTIFIER USAGES (best-effort, OMIT when unsure): any other identifier
    ///   token is resolved through the SAME cross-unit machinery as `hover_info`;
    ///   it is emitted ONLY if it resolves UNAMBIGUOUSLY to a known kind, else it
    ///   is OMITTED (no token — the editor's TextMate color shows). An unknown
    ///   identifier is NEVER given a class.
    ///
    /// Tokens are returned in SOURCE ORDER (the lex order); the server sorts and
    /// delta-encodes. Returns an empty vec when the unit is not cached or its
    /// source content is unreadable.
    pub fn semantic_tokens(&self, unit_key: Identifier) -> Vec<crate::query::SemanticToken> {
        use logos::Logos;

        let Some(meta) = self.meta_of(unit_key) else {
            return Vec::new();
        };
        // The unit's own source file (the header name's file). Its byte spans are
        // exactly the offsets the lexer produces below, so a token span maps
        // consistently back to this content in the server.
        let file = meta.ast.name.location.file;
        let Ok(content) = self.arena.content(file) else {
            return Vec::new();
        };

        // Pre-collect the own-unit declaration NAME spans and their finer kind, so
        // a name token at its declaring site is classified structurally (with the
        // `declaration` modifier) rather than through the usage path. This also
        // carries the finer class/interface/enum distinction the interface index
        // does not (the AST type_expression does). Namespace (unit-name) spans are
        // kept separately: a dotted unit name (`Winapi.Windows`) is ONE span over
        // several identifier tokens, so those are matched by CONTAINMENT.
        let SemanticSites {
            declaration_sites,
            namespace_spans,
        } = self.own_declaration_sites(&meta);

        let mut tokens: Vec<crate::query::SemanticToken> = Vec::new();
        let mut lexer = Token::lexer(content);
        while let Some(result) = lexer.next() {
            let token = result.unwrap_or(Token::Error);
            // Skip ONLY whitespace/newlines. Comments are trivia to the PARSER
            // but ARE emitted as semantic tokens (their lexical kind is Comment) —
            // so they are handled by `lexical_kind` below, not skipped here.
            if matches!(token, Token::Whitespace | Token::Newline) {
                continue;
            }
            let span = crate::meta::Span::new(lexer.span().start, lexer.span().end);
            let location = CodeLocation { file, span };

            if let Some(kind) = lexical_kind(token) {
                tokens.push(crate::query::SemanticToken {
                    location,
                    token_type: kind,
                    modifiers: crate::query::SemanticModifiers::NONE,
                });
                continue;
            }

            // An identifier (or a context-sensitive keyword usable as one). First
            // ask the own-unit declaration table for an exact-span match (a
            // declaration/member/parameter NAME at its declaring site).
            if token.can_be_identifier() {
                if let Some((kind, modifiers)) = declaration_sites.get(&span).copied() {
                    tokens.push(crate::query::SemanticToken {
                        location,
                        token_type: kind,
                        modifiers,
                    });
                    continue;
                }
                // A unit-name part (inside a header/uses qualified-name span) →
                // Namespace. Matched by containment (dotted names span several
                // identifier tokens under one qualified-name span).
                if namespace_spans
                    .iter()
                    .any(|whole| whole.start <= span.start && span.end <= whole.end)
                {
                    tokens.push(crate::query::SemanticToken {
                        location,
                        token_type: crate::query::SemanticKind::Namespace,
                        modifiers: crate::query::SemanticModifiers::NONE,
                    });
                    continue;
                }
                // Otherwise a usage: resolve it, emit ONLY on an unambiguous known
                // kind, else OMIT (no token — never a wrong color).
                if let Some(kind) = self.usage_semantic_kind(unit_key, &meta, span.start) {
                    tokens.push(crate::query::SemanticToken {
                        location,
                        token_type: kind,
                        modifiers: crate::query::SemanticModifiers::NONE,
                    });
                }
            }
            // A non-identifier token that produced no lexical kind (e.g.
            // `Token::Error`) is left un-highlighted — never a fabricated class.
        }
        tokens
    }

    /// The own-unit declaration/member/parameter NAME spans, each mapped to its
    /// certain [`SemanticKind`] + the `declaration` modifier, plus the whole
    /// qualified-name spans of unit names (header + uses entries) for Namespace
    /// classification by containment. Built from the AST so the finer
    /// class/interface/enum/enum-member distinction (which the interface index
    /// does not carry) is available at a declaring site.
    fn own_declaration_sites(&self, meta: &UnitMeta) -> SemanticSites {
        use crate::query::SemanticModifiers;
        let declaration = SemanticModifiers::DECLARATION;
        let mut declaration_sites = HashMap::new();

        for interface_declaration in &meta.ast.interface_declarations {
            let name_span = interface_declaration.name.location.span;
            let kind = declaration_semantic_kind(interface_declaration);
            declaration_sites.insert(name_span, (kind, declaration));

            // Enum member names are declarations too (`type E = (meA, meB)`).
            if let Some(TypeExpression::Enumeration(members)) =
                interface_declaration.type_expression.as_ref()
            {
                for member in members {
                    declaration_sites.insert(
                        member.name.location.span,
                        (crate::query::SemanticKind::EnumMember, declaration),
                    );
                }
            }

            // Structured-type member names (field/method/property) at their
            // declaring sites, plus method parameter names.
            if let Some(type_expression) = interface_declaration.type_expression.as_ref() {
                collect_member_declaration_sites(type_expression, &mut declaration_sites);
            }
        }

        // Unit-name spans (header + uses entries), matched by containment because a
        // dotted name (`Winapi.Windows`) is one span over several ident tokens.
        let mut namespace_spans = vec![meta.ast.name.location.span];
        for clause in [
            meta.ast.interface_uses.as_ref(),
            meta.ast.implementation_uses.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            for used in &clause.uses {
                namespace_spans.push(used.name.location.span);
            }
        }

        SemanticSites {
            declaration_sites,
            namespace_spans,
        }
    }

    /// Resolve an identifier USAGE at byte `position` to a certain semantic kind,
    /// or `None` (→ OMIT). Uses the SAME machinery as `hover_info`: the occurrence
    /// resolves to an interface declaration (own then imports) whose kind is
    /// known, else nothing. Unknown/ambiguous → `None`, never a guess.
    fn usage_semantic_kind(
        &self,
        unit_key: Identifier,
        meta: &UnitMeta,
        position: u32,
    ) -> Option<crate::query::SemanticKind> {
        let target = self.symbol_at(unit_key, position)?;
        // A declaration/member site handled by `own_declaration_sites` would not
        // reach here; a member usage (`Owner.Member`) resolves through the owner.
        match target.kind {
            // A declaration/member whose exact span did not match the pre-collected
            // table, or an ordinary usage: classify via the resolved facts. (A
            // declaration/member at its exact declaring span is handled earlier by
            // `own_declaration_sites`, WITH the declaration modifier; reaching here
            // means classify by resolution WITHOUT it.)
            TargetKind::Declaration | TargetKind::Member | TargetKind::Usage => {
                self.resolved_usage_kind(meta, target)
            }
            // A body-local variable/parameter (same-unit scope, shadowing an
            // interface symbol). It is CERTAINLY a variable-like binding — the
            // coarse-but-correct `Variable` kind (a parameter rendered as a
            // variable is coarser, never wrong; we do not carry the finer
            // param/var distinction into the usage-classification path).
            TargetKind::Local => Some(crate::query::SemanticKind::Variable),
        }
    }

    /// Classify a resolved [`QueryTarget`] by its DECLARATION's kind, cross-unit
    /// through the same loader as `hover_info`. `None` when the target resolves to
    /// no interface declaration (unknown identifier, implementation-only local, a
    /// member on an unresolved owner) — the OMIT case.
    fn resolved_usage_kind(
        &self,
        meta: &UnitMeta,
        target: QueryTarget,
    ) -> Option<crate::query::SemanticKind> {
        use crate::query::SemanticKind;
        use crate::unit_cache::MemberKind;

        // A member occurrence: resolve the owner, then the member's kind.
        if let Some(owner_key) = target.owner_type {
            let member_kind = self.member_kind_of(meta, owner_key, target.key)?;
            return Some(match member_kind {
                MemberKind::Field => SemanticKind::Field,
                MemberKind::Method => SemanticKind::Method,
                MemberKind::Property => SemanticKind::Property,
                MemberKind::NestedType => SemanticKind::Type,
                MemberKind::NestedConst => SemanticKind::Constant,
            });
        }

        // A top-level symbol: own interface first, then imports (reverse uses
        // order) — the exact resolution order of `definition`/`hover_info`.
        if let Some(symbol) = meta.interface().find(target.key) {
            return Some(symbol_semantic_kind(symbol, meta));
        }
        let loader = self.make_loader();
        for import in imports_reversed(meta) {
            if let crate::parse_state::LoadOutcome::Loaded(imported) = loader.interface_of(import) {
                if let Some(symbol) = imported.interface().find(target.key) {
                    // Cross-unit: the finer type shape is not certainly available
                    // here (the interface index carries no type_expression), so a
                    // type resolves to the coarse-but-correct `Type`.
                    return Some(symbol_semantic_kind_coarse(symbol.kind));
                }
            }
        }
        // Also allow a usage whose key names a UNIT this unit imports → Namespace.
        if imports_reversed(meta).iter().any(|import| *import == target.key) {
            return Some(SemanticKind::Namespace);
        }
        None
    }

    /// The [`MemberKind`] of `member_key` on type `owner_key`, resolving the owner
    /// (own then imports). `None` if owner/member unresolved.
    fn member_kind_of(
        &self,
        meta: &UnitMeta,
        owner_key: Identifier,
        member_key: Identifier,
    ) -> Option<crate::unit_cache::MemberKind> {
        if let Some(owner) = meta.interface().find(owner_key) {
            return owner.find_member(member_key).map(|member| member.kind);
        }
        let loader = self.make_loader();
        for import in imports_reversed(meta) {
            if let crate::parse_state::LoadOutcome::Loaded(imported) = loader.interface_of(import) {
                if let Some(owner) = imported.interface().find(owner_key) {
                    return owner.find_member(member_key).map(|member| member.kind);
                }
            }
        }
        None
    }

    /// If `position` follows a `Receiver.` member access, return the folded
    /// TYPE key of `Receiver` (so its members can be completed). Looks at the
    /// recorded usages: the occurrence ending nearest before `position` whose
    /// declared identity resolves to a type. Best-effort — `None` (→ top-level
    /// completion) when there is no resolvable receiver, never a wrong member
    /// list.
    fn member_receiver_at(&self, meta: &UnitMeta, position: u32) -> Option<Identifier> {
        // The dot-access receiver is the identifier occurrence immediately
        // before `position`. We look for a usage whose span ends at or just
        // before the cursor; its symbol names the receiver. We then map the
        // receiver to a TYPE key: a receiver that IS an interface type (a
        // static `TFoo.` scope access), or whose declared field/var type is a
        // known type. If neither resolves, no member context (top-level).
        let receiver = meta
            .usages
            .iter()
            .filter(|usage| usage.location.span.end <= position)
            .max_by_key(|usage| usage.location.span.end)?;
        let receiver_key = receiver.symbol;

        // A member context requires an actual `.` between the receiver and the
        // cursor. Without this gate, ANY top-level position after a type name
        // (e.g. `TFoo⎸` with no dot) would resolve the nearest type usage and
        // wrongly return that type's members while SUPPRESSING the top-level
        // set — a wrong answer. Spec: incomplete context → top-level, never a
        // wrong member list. So bail to top-level unless a `.` immediately
        // precedes `position` (modulo whitespace) after the receiver ends.
        if !self.dot_precedes(receiver.location.file, receiver.location.span.end, position) {
            return None;
        }

        // (a) the receiver is itself a type in this unit (`TFoo.` scope access)
        if let Some(symbol) = meta.interface().find(receiver_key) {
            if symbol.kind == SymbolKind::Type {
                return Some(receiver_key);
            }
            // (b) a var/const/field whose declared type we know
            if let Some(type_key) = symbol_declared_type_key(symbol) {
                return Some(type_key);
            }
        }
        // (c) the receiver is a type in an imported unit (`TFoo.` from a uses)
        let loader = self.make_loader();
        for import in imports_reversed(meta) {
            if let crate::parse_state::LoadOutcome::Loaded(imported) = loader.interface_of(import) {
                if let Some(symbol) = imported.interface().find(receiver_key) {
                    if symbol.kind == SymbolKind::Type {
                        return Some(receiver_key);
                    }
                }
            }
        }
        None
    }

    /// Whether an actual `.` (member-access dot) sits between `receiver_end` and
    /// `position` in `file`'s source, modulo whitespace — i.e. the last
    /// non-whitespace byte before the cursor is a `.`. The dot gate for member
    /// completion: only a real dot access enters member mode; anything else
    /// (bare type name, junk, an unreadable buffer) falls back to top-level, so
    /// a query never returns a wrong member list for an incomplete context.
    fn dot_precedes(&self, file: FileId, receiver_end: u32, position: u32) -> bool {
        if position <= receiver_end {
            return false;
        }
        let Ok(content) = self.arena.content(file) else {
            // Can't read the buffer → cannot prove a dot → top-level (safe).
            return false;
        };
        let (start, end) = (receiver_end as usize, position as usize);
        // Guard against spans that fall outside this buffer or split a UTF-8
        // char boundary (a stale/foreign location); on any doubt, top-level.
        if end > content.len()
            || !content.is_char_boundary(start)
            || !content.is_char_boundary(end)
        {
            return false;
        }
        // The last non-whitespace byte before the cursor must be `.`.
        matches!(content[start..end].trim_end().chars().next_back(), Some('.'))
    }

    /// The members of `type_key`, resolved own-first then imports. Empty if the
    /// type is unresolved. Correct visibility surfaced (never a wrong member).
    fn member_completions(&self, meta: &UnitMeta, type_key: Identifier) -> Vec<Completion> {
        if let Some(symbol) = meta.interface().find(type_key) {
            return symbol
                .members
                .iter()
                .map(member_completion)
                .collect();
        }
        let loader = self.make_loader();
        for import in imports_reversed(meta) {
            if let crate::parse_state::LoadOutcome::Loaded(imported) = loader.interface_of(import) {
                if let Some(symbol) = imported.interface().find(type_key) {
                    return symbol.members.iter().map(member_completion).collect();
                }
            }
        }
        Vec::new()
    }

    /// Visible top-level symbols: builtins + own interface symbols declared
    /// before `position` + interface symbols of imported units, de-duplicated
    /// by folded key (own/earlier wins).
    fn top_level_completions(&self, meta: &UnitMeta, position: u32) -> Vec<Completion> {
        let mut seen: std::collections::HashSet<Identifier> = std::collections::HashSet::new();
        let mut completions: Vec<Completion> = Vec::new();

        // builtins first (so a same-named later symbol de-dups against them is
        // irrelevant — but list them so `Integer`/`string` complete)
        for name in BUILTIN_TYPE_NAMES {
            let key = crate::globals::intern_key(name);
            if seen.insert(key) {
                completions.push(Completion {
                    display: crate::globals::intern(name),
                    key,
                    kind: CompletionKind::Builtin,
                    type_key: None,
                    directives: Vec::new(),
                    visibility: crate::ast::Visibility::Unspecified,
                });
            }
        }

        // own interface symbols declared up to the cursor (a symbol declared
        // LATER in the file is not yet visible)
        for symbol in &meta.interface().symbols {
            if symbol.location.span.start > position {
                continue;
            }
            if seen.insert(symbol.key) {
                completions.push(symbol_completion(symbol));
            }
        }

        // imported units' interface symbols
        let loader = self.make_loader();
        for import in imports_reversed(meta) {
            if let crate::parse_state::LoadOutcome::Loaded(imported) = loader.interface_of(import) {
                for symbol in &imported.interface().symbols {
                    if seen.insert(symbol.key) {
                        completions.push(symbol_completion(symbol));
                    }
                }
            }
        }
        completions
    }

    /// CONSERVATIVE unused-uses analysis for `unit_key`: each `uses` entry
    /// (interface AND implementation) whose imported unit contributes NO
    /// referenced symbol to this unit. Resolves imports through the SAME
    /// cycle-safe, dependency-recorded loader as [`Self::definition`].
    ///
    /// NEVER-FALSE-FLAG discipline (a false "unused" invites deleting a needed
    /// unit and breaking the build — the highest-severity defect here):
    /// - the importer being CYCLE-TAINTED taints the whole analysis → flag
    ///   nothing (its usage set / import resolution is not trustworthy);
    /// - a uses entry whose unit was CONSULTED AS A DEPENDENCY (`{$IF
    ///   Declared/SizeOf}` reached into its interface) is a real use → skip;
    /// - a uses entry whose interface is NOT ALREADY in the cache cannot be
    ///   PROVEN unused WITHOUT parsing it — and parsing it here would drag the
    ///   whole `uses` graph of a real VCL/RTL-using unit into memory on every
    ///   `analyze` (the Task-15 OOM). So this check is CACHE-ONLY: an import
    ///   with no cached interface is SKIPPED (never flagged), exactly like an
    ///   unresolvable one. This only ever makes the hint MISS a genuinely-unused
    ///   import (until something else parses it), never false-flag one — the
    ///   conservative direction the whole analysis already obeys;
    /// - a uses entry ANY of whose exported keys (its own unit key included)
    ///   appears in the over-approximating usage set is "possibly used" → skip.
    /// Only an ALREADY-CACHED, non-dependency, non-cycle import ZERO of whose
    /// exports is referenced is flagged — and only ever as a [`UnusedUnit`] the
    /// caller surfaces as a HINT with the side-effect caveat, never a removal
    /// claim.
    ///
    /// FORCE-LOAD DISCIPLINE (Task-15): `analyze` calls `diagnostics()` which
    /// calls this on the hot path. It must therefore NEVER parse an uncached
    /// import — `analyze` of a unit parses ONLY that unit (plus its own
    /// directive-forced loads during its own parse), never its whole `uses`
    /// graph. The cache-only [`Self::meta_of`] lookup below is what enforces
    /// that: it reads the cache and returns `None` on a miss, it never parses.
    pub fn unused_units(&self, unit_key: Identifier) -> Vec<UnusedUnit> {
        let Some(meta) = self.meta_of(unit_key) else {
            return Vec::new();
        };
        // A cycle-tainted parse has an untrustworthy import graph AND usage set;
        // proving anything unused off it risks a false flag → flag nothing.
        if meta.cycle_tainted {
            return Vec::new();
        }

        // The importer's usage set: every folded symbol key that occurs anywhere
        // in the unit (interface-body references + implementation occurrences).
        // Over-approximating on purpose — a name-match spares the import (a false
        // "used" is safe; a false "unused" is not).
        let usage_keys: std::collections::HashSet<Identifier> =
            meta.usages.iter().map(|usage| usage.symbol).collect();

        // Units consulted as a DEPENDENCY (a `{$IF Declared(Foo.X)}`/`SizeOf`
        // reached into Foo's interface). That IS a use — never flag such a unit.
        let dependency_units: std::collections::HashSet<Identifier> =
            meta.dependencies.iter().map(|dependency| dependency.unit).collect();

        let mut flagged: Vec<UnusedUnit> = Vec::new();
        let mut seen: std::collections::HashSet<Identifier> = std::collections::HashSet::new();

        for used in uses_entries(&meta) {
            let import_key = used.key;
            // De-duplicate: a unit named in both interface and implementation
            // uses is flagged at most once (its first — interface — entry).
            if !seen.insert(import_key) {
                continue;
            }
            // Consulted as a dependency → a real use.
            if dependency_units.contains(&import_key) {
                continue;
            }
            // CACHE-ONLY interface lookup (Task-15 OOM fix). Deliberately NOT
            // `loader.interface_of`, which parses on a cache miss: on the
            // `analyze` hot path that would drag the entire `uses` graph of a
            // real unit into memory. `meta_of` reads the cache and returns
            // `None` on a miss — it never parses. An import whose interface is
            // not already cached cannot be PROVEN unused without parsing it, so
            // it is SKIPPED (never flagged) — conservative, consistent with the
            // never-false-flag discipline (skipping only ever MISSES an unused
            // hint, never invents one).
            let Some(imported) = self.meta_of(import_key) else {
                continue;
            };
            // The unit's own key can appear as a qualified `Unit.Symbol` usage;
            // a match on it means the unit is referenced. Include it alongside
            // its exported symbol keys.
            let referenced = usage_keys.contains(&import_key)
                || imported
                    .interface()
                    .symbols
                    .iter()
                    .any(|symbol| usage_keys.contains(&symbol.key));
            if referenced {
                continue;
            }
            flagged.push(UnusedUnit {
                unit: used.display,
                location: used.location,
            });
        }
        flagged
    }

    /// The unit's unified diagnostics: parse findings + dfm-linker findings +
    /// the conservative unused-uses HINTS, one queryable list for
    /// `textDocument/publishDiagnostics`.
    pub fn diagnostics(&self, unit_key: Identifier) -> Vec<UnifiedDiagnostic> {
        let mut all: Vec<UnifiedDiagnostic> = self
            .parse_diagnostics
            .get(&unit_key)
            .cloned()
            .unwrap_or_default();
        if let Some(links) = self.dfm_links.get(&unit_key) {
            for diagnostic in &links.diagnostics {
                all.push(UnifiedDiagnostic {
                    source: DiagnosticSource::Dfm,
                    severity: diagnostic.severity(),
                    location: diagnostic.pas_location(),
                    dfm_offset: Some(diagnostic.dfm_offset()),
                    message: diagnostic.message(),
                });
            }
        }
        // Conservative unused-uses hints (never an error/removal instruction).
        for unused in self.unused_units(unit_key) {
            all.push(UnifiedDiagnostic {
                source: DiagnosticSource::Analysis,
                severity: crate::token_cursor::Severity::Hint,
                location: Some(unused.location),
                dfm_offset: None,
                message: format!(
                    "unit '{}' is in the uses clause but none of its symbols are \
                     referenced (it may still be needed for initialization side effects)",
                    crate::globals::resolve(unused.unit)
                ),
            });
        }
        all
    }

    /// Build a loader over this session's arena/context/index — the same one
    /// `parse_source_file` uses, so cross-unit resolution during a query obeys
    /// the identical cycle/dependency rules.
    fn make_loader(&self) -> std::rc::Rc<UnitLoader> {
        UnitLoader::with_store(
            self.arena,
            self.context.clone(),
            Some(self.index.clone()),
            Some(self.store.clone()),
        )
    }

    pub fn context(&self) -> &Arc<ProjectContext> {
        &self.context
    }

    pub fn arena(&self) -> &SourceArena {
        self.arena
    }

    /// Whether the CURRENT bytes of `file` still match what the cached meta that
    /// indexes spans into `file` was PARSED against. The provenance guard for the
    /// LSP mapping layer (task-19 correctness): a cached `UnitMeta` holds
    /// `(FileId, Span)` spans into the file's PARSE-TIME text, but that text is
    /// trimmable — a disk entry's content can be freed and RE-READ on demand, and
    /// if the file CHANGED on disk between parse and query (an unopened import
    /// whose watcher/invalidation lagged) the re-read yields NEW bytes. Mapping a
    /// PARSE-TIME span onto that NEW text produces a WRONG range. Before mapping,
    /// the server asks this: only map when the answer is `true`.
    ///
    /// - VIRTUAL (open editor) buffer: always `true`. Its content is replaced
    ///   only between parses under the session lock and the meta produced by that
    ///   parse indexes exactly the stored text (span-provenance, see
    ///   [`SourceArena::set_virtual`]); it is never trimmed/re-read, so it can
    ///   never drift from its parsed text. No disk hash is taken (its display path
    ///   need not exist on disk).
    /// - DISK file: hash the CURRENT on-disk bytes and compare to the parse-time
    ///   `source_hash` of the cached meta for this file. `true` only when a cached
    ///   meta for this exact file path is found AND the current on-disk hash
    ///   equals its `source_hash`. Any of {`FileId` not issued here, no cached
    ///   meta for the path, on-disk read fails, hashes differ} → `false`: the
    ///   caller then returns NO location rather than a wrong range (never-wrong).
    ///
    /// The parse-time hash is `meta.source_hash` — the hash of the file's RAW
    /// on-disk bytes at parse time (see `pipeline::stamp_file`), byte-identical to
    /// [`crate::unit_cache::hash_file`], so re-hashing the current on-disk bytes
    /// with the same function is an apples-to-apples comparison.
    pub fn content_matches_parsed(&self, file: FileId) -> bool {
        let Some(path) = self.arena.try_path(file) else {
            // A FileId this arena never issued: cannot verify provenance → treat
            // as a mismatch (no location beats a wrong one).
            return false;
        };
        // Virtual buffers are authoritative and cannot drift (see the doc note).
        if self.arena.is_virtual(file) == Some(true) {
            return true;
        }
        let path = path.to_path_buf();
        // The parse-time hash lives in the cached meta whose source is THIS file.
        // metas are keyed by unit name, not FileId, so match on the source path
        // (both `meta.source_path` and `arena.path` come from the arena's
        // canonicalized path in `build_unit_meta`, so they compare byte-equal).
        let Some(parsed_hash) = self.parsed_source_hash_for_path(&path) else {
            // No cached meta indexes this file → we do not know what its spans
            // were parsed against, so we cannot certify the re-read text matches.
            return false;
        };
        // Hash the CURRENT on-disk bytes. A read failure (deleted/locked) → no
        // certification → mismatch.
        match crate::unit_cache::hash_file(&path) {
            Ok(current_hash) => current_hash == parsed_hash,
            Err(_) => false,
        }
    }

    /// The parse-time `source_hash` of the cached, successfully-parsed meta whose
    /// `source_path` is `path`, if any. Used by [`Self::content_matches_parsed`]
    /// to recover the provenance hash for a target `FileId` (metas are keyed by
    /// unit name, not by file, so this matches on the canonical source path).
    fn parsed_source_hash_for_path(&self, path: &std::path::Path) -> Option<u64> {
        self.context
            .unit_cache
            .iter_entries()
            .find_map(|(_, entry)| match entry {
                CacheEntry::Done(meta) if meta.source_path == path => Some(meta.source_hash),
                _ => None,
            })
    }

    /// Bound the process-global source arena's DISK-file text at a SAFE
    /// CHECKPOINT (Task-19). LRU-evicts the coldest disk entries' resident
    /// content+raw until it is at most [`ARENA_DISK_CONTENT_CAP`] bytes; a
    /// cleared entry re-reads from disk on the next access. Virtual (unsaved
    /// editor) buffers are never trimmed. Returns the bytes freed.
    ///
    /// CHECKPOINT DISCIPLINE (soundness-critical — see
    /// [`SourceArena::trim_disk_content`]'s SOUNDNESS note): this must be called
    /// ONLY when NO `&str`/`&[u8]` borrow into the arena is live — i.e. AFTER a
    /// parse/query has completed and its OWNED results are built, still under the
    /// LSP session `blocking_lock()`, before the blocking section returns. The
    /// session lock serializes every parse/query, so a trim between them cannot
    /// race one that holds an arena borrow. NEVER call it reactively inside a
    /// parse (a borrow from an earlier file in the same parse chain may still be
    /// live → use-after-free). The server invokes it at the end of the blocking
    /// `analyze`/read sections; a batch/one-shot driver may call it after each
    /// top-level `parse_source_file`. It is a no-op below the cap.
    ///
    /// Transient peak: one parse chain (a unit + its directive-forced includes/
    /// imports) may materialize several files at once and briefly exceed the cap
    /// DURING the chain; this trim afterwards brings it back down. Since the
    /// eager-load fix bounds one analyze to ~one unit's chain, that transient
    /// peak is small and bounded.
    pub fn trim_arena(&self) -> usize {
        self.arena.trim_disk_content(ARENA_DISK_CONTENT_CAP)
    }

    pub fn index(&self) -> &ReverseDependencyIndex {
        &self.index
    }

    /// The parse pipeline calls this after inserting artifacts so autosave
    /// knows there is something new to persist.
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Drive the session: watcher poll → invalidation → autosave.
    pub fn tick(&mut self, now: Instant) -> Result<TickReport, SessionError> {
        let mut report = TickReport::default();

        if let Some(plan) = self.watcher.as_ref().and_then(|watcher| watcher.poll(now)) {
            let invalidation = self.apply_plan(&plan);
            report.invalidated_units = invalidation.invalidated_units;
            report.swept = matches!(plan, InvalidationPlan::FullSweep { .. });
        }

        if self.dirty && now.duration_since(self.last_save) >= self.save_interval {
            report.saved = Some(self.save_now()?);
        }
        Ok(report)
    }

    /// Apply an invalidation plan. Public so tests and custom drivers can
    /// inject plans without OS watcher events.
    pub fn apply_plan(&mut self, plan: &InvalidationPlan) -> InvalidationReport {
        let report = apply_invalidation(plan, &self.context.unit_cache, &self.index);
        if report.invalidated_units > 0 {
            self.dirty = true;
        }
        // Purge derived DFM links for every evicted unit. Without this the
        // side-table outlives the cache entry and `dfm_links(unit_key)` keeps
        // serving pre-edit links/diagnostics until an explicit re-parse. An
        // evicted unit re-populates this map when it is next parsed+linked.
        for unit_key in &report.invalidated_keys {
            self.dfm_links.remove(unit_key);
            // The references index and diagnostics side-tables mirror the same
            // purge discipline: no occurrence/diagnostic may outlive its unit
            // (a span pointing into an evicted unit is the exact bug this
            // prevents — same rule as the dfm_links purge above).
            self.reference_index.purge_unit(*unit_key);
            self.parse_diagnostics.remove(unit_key);
        }
        if matches!(plan, InvalidationPlan::FullSweep { .. }) {
            // sweep dropped entries wholesale → stale index mappings now only
            // point at gone units; rebuild to stay tight
            self.rebuild_index();
            self.rebuild_reference_index();
        }
        report
    }

    pub fn save_now(&mut self) -> Result<SaveReport, SessionError> {
        let report = self.store.save(&self.context.unit_cache)?;
        // A skipped meta is a dropped unit (re-parses next session), not an
        // error — but it must be visible, symmetric with the load side. Emit a
        // diagnostic; the full report is also returned for programmatic
        // inspection by the LSP layer.
        if !report.skipped.is_empty() {
            for skipped in &report.skipped {
                eprintln!(
                    "delphi-parser: snapshot skipped un-serializable unit {}: {}",
                    skipped.name, skipped.error
                );
            }
        }
        self.dirty = false;
        self.last_save = Instant::now();
        Ok(report)
    }

    /// Final save. Consumes the session — nothing may touch the cache after.
    pub fn shutdown(mut self) -> Result<SaveReport, SessionError> {
        self.watcher = None; // stop OS events before the final snapshot
        self.save_now()
    }

    fn rebuild_index(&mut self) {
        // flush moka so the sweep's invalidations and any just-inserted entries
        // are visible to iter_entries (M13) — otherwise the rebuilt index can
        // re-add units the sweep just dropped, or miss fresh ones
        self.context.unit_cache.run_pending_tasks();
        let artifacts: Vec<_> = self
            .context
            .unit_cache
            .iter_entries()
            .filter_map(|(unit, entry)| match entry {
                CacheEntry::Done(artifact) => Some((unit, artifact)),
                CacheEntry::Failed(_) => None,
            })
            .collect();
        self.index.rebuild_from(
            artifacts
                .iter()
                .map(|(unit, artifact)| (*unit, artifact.as_ref())),
        );
    }

    /// Rebuild the references index wholesale from current cache contents —
    /// same post-sweep discipline as [`Self::rebuild_index`] (a shrunken cache
    /// leaves no occurrence pointing into a gone unit). The parse-diagnostics
    /// map is NOT rebuilt: diagnostics are a byproduct of a live parse, not
    /// reconstructable from a cached meta, so a swept unit simply has no
    /// diagnostics until it is re-parsed (the purge above already dropped the
    /// evicted keys).
    fn rebuild_reference_index(&mut self) {
        self.context.unit_cache.run_pending_tasks();
        let metas: Vec<(Identifier, Arc<UnitMeta>)> = self
            .context
            .unit_cache
            .iter_entries()
            .filter_map(|(unit, entry)| match entry {
                CacheEntry::Done(meta) => Some((unit, meta)),
                CacheEntry::Failed(_) => None,
            })
            .collect();
        self.reference_index
            .rebuild_from(metas.iter().map(|(unit, meta)| (*unit, meta.as_ref())));
    }

    /// Project dir + existing search paths, deduplicated. Missing search
    /// paths are recorded in `notes`, not silently dropped.
    fn watch_directories(&mut self, dproj_path: &Path) -> Vec<PathBuf> {
        let mut directories = Vec::new();
        if let Some(project_directory) = dproj_path.parent() {
            directories.push(project_directory.to_path_buf());
        }
        for search_path in &self.context.search_paths {
            if search_path.is_dir() {
                directories.push(search_path.clone());
            } else {
                self.notes
                    .push(format!("search path not found: {}", search_path.display()));
            }
        }
        directories.sort();
        directories.dedup();
        directories
    }
}

// ─── Query helpers (free functions) ──────────────────────────────────────

/// Does `location`'s span cover byte `position` (start ≤ position < end)? A
/// zero-length span covers nothing.
fn span_covers(location: CodeLocation, position: u32) -> bool {
    location.span.start <= position && position < location.span.end
}

/// Map a body-local declaration kind to a [`CompletionKind`] for hover. A
/// parameter is a variable binding, so it (like a `var`) maps to
/// `Symbol(SymbolKind::Var)`; a `label` likewise has no richer symbol kind. This
/// is the honest coarse classification — the parser never invents finer facts.
fn local_completion_kind(kind: crate::ast::LocalKind) -> CompletionKind {
    use crate::ast::LocalKind;
    match kind {
        LocalKind::Var | LocalKind::Param | LocalKind::Label => {
            CompletionKind::Symbol(SymbolKind::Var)
        }
        LocalKind::Const => CompletionKind::Symbol(SymbolKind::Const),
        LocalKind::Type => CompletionKind::Symbol(SymbolKind::Type),
    }
}

/// Build a [`TargetKind::Local`] [`QueryTarget`] for a body-local declaration:
/// its own folded key, display spelling, DECLARATION span (both `location` and
/// the definition target), and no owner type.
fn local_target(declaration: &crate::ast::LocalDeclaration) -> QueryTarget {
    QueryTarget {
        key: declaration.name.key,
        display: declaration.name.name,
        kind: TargetKind::Local,
        location: declaration.name.location,
        owner_type: None,
    }
}

// ─── Semantic-token classification helpers (task 13) ─────────────────────────

/// The own-unit semantic-token declaration tables built from the AST: exact-span
/// declaration/member/parameter NAME sites, and whole qualified-name spans of
/// unit names (matched by containment).
struct SemanticSites {
    declaration_sites:
        HashMap<crate::meta::Span, (crate::query::SemanticKind, crate::query::SemanticModifiers)>,
    namespace_spans: Vec<crate::meta::Span>,
}

/// The LEXICALLY-certain [`crate::query::SemanticKind`] of a token, or `None` for
/// an identifier / context-sensitive keyword (classified structurally instead) or
/// a token that carries no highlight (`Token::Error`). Trivia is filtered by the
/// caller before this is consulted.
fn lexical_kind(token: Token) -> Option<crate::query::SemanticKind> {
    use crate::query::SemanticKind;
    // A directive (`{$…}`) is a macro; a comment is a comment.
    if token.is_directive() {
        return Some(SemanticKind::Macro);
    }
    match token {
        Token::BlockComment | Token::BlockCommentParen | Token::LineComment => {
            Some(SemanticKind::Comment)
        }
        Token::StringLiteral | Token::CharLiteral => Some(SemanticKind::String),
        Token::IntLiteral | Token::FloatLiteral => Some(SemanticKind::Number),
        // Operators & punctuation.
        Token::Plus | Token::Minus | Token::Star | Token::Slash | Token::Eq | Token::NEq
        | Token::Lt | Token::Gt | Token::LtEq | Token::GtEq | Token::Assign | Token::Colon
        | Token::Semicolon | Token::Comma | Token::DotDot | Token::Dot | Token::Caret
        | Token::At_ | Token::LParen | Token::RParen | Token::LBracket | Token::RBracket => {
            Some(SemanticKind::Operator)
        }
        // An identifier or a context-sensitive keyword usable as an identifier is
        // NOT lexically classifiable — the structural/usage path decides. Every
        // other token that reaches here is a genuine reserved word → Keyword.
        Token::Error => None,
        other if other.can_be_identifier() => None,
        _ => Some(SemanticKind::Keyword),
    }
}

/// The certain [`crate::query::SemanticKind`] of a top-level interface
/// declaration, using the AST type_expression for the finer type shape
/// (class/interface/enum) where present.
fn declaration_semantic_kind(
    declaration: &crate::ast::InterfaceDeclaration,
) -> crate::query::SemanticKind {
    use crate::ast::DeclarationKind;
    use crate::query::SemanticKind;
    match declaration.kind {
        DeclarationKind::Type => declaration
            .type_expression
            .as_ref()
            .map(type_expression_semantic_kind)
            .unwrap_or(SemanticKind::Type),
        DeclarationKind::Const | DeclarationKind::ResourceString => SemanticKind::Constant,
        DeclarationKind::Var | DeclarationKind::ThreadVar => SemanticKind::Variable,
        DeclarationKind::Procedure | DeclarationKind::Function => SemanticKind::Function,
    }
}

/// The finer semantic kind of a type's right-hand side: class/interface/enum are
/// distinguished; every other shape (record, alias, pointer, subrange, routine
/// type, …) is the coarse-but-correct `Type`.
fn type_expression_semantic_kind(
    type_expression: &TypeExpression,
) -> crate::query::SemanticKind {
    use crate::query::SemanticKind;
    match type_expression {
        TypeExpression::Class(_) | TypeExpression::ForwardClass => SemanticKind::Class,
        TypeExpression::Interface(_)
        | TypeExpression::ForwardInterface
        | TypeExpression::ForwardDispInterface => SemanticKind::Interface,
        TypeExpression::Enumeration(_) => SemanticKind::Enum,
        _ => SemanticKind::Type,
    }
}

/// The coarse semantic kind of a symbol from its [`SymbolKind`] alone (no
/// type-shape refinement) — used for CROSS-UNIT usages where the interface index
/// carries no type_expression. Correct-but-coarse: a type resolves to `Type`.
fn symbol_semantic_kind_coarse(kind: SymbolKind) -> crate::query::SemanticKind {
    use crate::query::SemanticKind;
    match kind {
        SymbolKind::Type => SemanticKind::Type,
        SymbolKind::Const | SymbolKind::ResourceString => SemanticKind::Constant,
        SymbolKind::Var | SymbolKind::ThreadVar => SemanticKind::Variable,
        SymbolKind::Procedure | SymbolKind::Function => SemanticKind::Function,
    }
}

/// The semantic kind of an OWN-unit interface symbol, refining a `Type` to its
/// class/interface/enum shape via the AST declaration when available.
fn symbol_semantic_kind(
    symbol: &crate::unit_cache::InterfaceSymbol,
    meta: &UnitMeta,
) -> crate::query::SemanticKind {
    if symbol.kind == SymbolKind::Type {
        if let Some(type_expression) = find_type_declaration_in(meta, symbol.key) {
            return type_expression_semantic_kind(type_expression);
        }
    }
    symbol_semantic_kind_coarse(symbol.kind)
}

/// The own-unit type declaration's type_expression for `key`, if it is a `Type`
/// declaration with a right-hand side. (`find_type_declaration` exists elsewhere
/// but returns the whole declaration for a different shape; this returns the
/// type_expression directly for the semantic-kind refinement.)
fn find_type_declaration_in(meta: &UnitMeta, key: Identifier) -> Option<&TypeExpression> {
    meta.ast
        .interface_declarations
        .iter()
        .find(|declaration| {
            declaration.name.key == key
                && matches!(declaration.kind, crate::ast::DeclarationKind::Type)
        })
        .and_then(|declaration| declaration.type_expression.as_ref())
}

/// Collect the DECLARING NAME spans of a structured type's members (field/
/// method/property/nested) and a method's parameter names, each mapped to its
/// certain [`crate::query::SemanticKind`] + the `declaration` modifier.
fn collect_member_declaration_sites(
    type_expression: &TypeExpression,
    sites: &mut HashMap<
        crate::meta::Span,
        (crate::query::SemanticKind, crate::query::SemanticModifiers),
    >,
) {
    match type_expression {
        TypeExpression::Class(class_type) => {
            for section in &class_type.sections {
                collect_members_into(&section.members, sites);
            }
        }
        TypeExpression::Record(structured) => {
            for section in &structured.sections {
                collect_members_into(&section.members, sites);
            }
        }
        TypeExpression::Interface(interface_type) => {
            collect_members_into(&interface_type.members, sites);
        }
        _ => {}
    }
}

/// Insert member NAME sites (and method parameter names) for a flat member slice.
fn collect_members_into(
    members: &[crate::ast::Member],
    sites: &mut HashMap<
        crate::meta::Span,
        (crate::query::SemanticKind, crate::query::SemanticModifiers),
    >,
) {
    use crate::ast::Member;
    use crate::query::{SemanticKind, SemanticModifiers};
    let declaration = SemanticModifiers::DECLARATION;

    for member in members {
        match member {
            Member::Field { names, .. } => {
                for name in names {
                    sites.insert(name.location.span, (SemanticKind::Field, declaration));
                }
            }
            Member::Method(method) => {
                sites.insert(
                    method.name.location.span,
                    (SemanticKind::Method, declaration),
                );
                for parameter in &method.routine.parameters {
                    for name in &parameter.names {
                        sites.insert(
                            name.location.span,
                            (SemanticKind::Parameter, declaration),
                        );
                    }
                }
            }
            Member::Property(property) => {
                sites.insert(
                    property.name.location.span,
                    (SemanticKind::Property, declaration),
                );
            }
            Member::NestedType(declaration_box) => {
                let kind = declaration_semantic_kind(declaration_box);
                sites.insert(declaration_box.name.location.span, (kind, declaration));
            }
            Member::NestedConst { name, .. } => {
                sites.insert(name.location.span, (SemanticKind::Constant, declaration));
            }
        }
    }
}

/// One `uses`-clause entry: its folded lookup key, its display spelling, and the
/// exact source span of the name — the range the unused-uses hint highlights.
struct UsesEntry {
    key: Identifier,
    display: Identifier,
    location: CodeLocation,
}

/// Every `uses` entry of a unit, interface section first then implementation, in
/// source order. Each carries the name's own span so a hint anchors exactly on
/// the imported unit's name in the clause.
fn uses_entries(meta: &UnitMeta) -> Vec<UsesEntry> {
    let mut entries = Vec::new();
    for clause in [
        meta.ast.interface_uses.as_ref(),
        meta.ast.implementation_uses.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        for used in &clause.uses {
            entries.push(UsesEntry {
                key: used.name.key,
                display: used.name.name,
                location: used.name.location,
            });
        }
    }
    entries
}

/// A unit's imports in reverse uses order (later shadows earlier), read from the
/// AST's interface uses clause — the same order the loader-backed resolution
/// uses everywhere else.
fn imports_reversed(meta: &UnitMeta) -> Vec<Identifier> {
    meta.ast
        .interface_uses
        .as_ref()
        .map(|uses| {
            uses.uses
                .iter()
                .rev()
                .map(|used| used.name.key)
                .collect()
        })
        .unwrap_or_default()
}

/// A top-level interface symbol's declared simple type key, when its shape
/// carries one (a var/const/field whose type is a simple reference). Used to map
/// a completion receiver to the type whose members to complete.
fn symbol_declared_type_key(symbol: &crate::unit_cache::InterfaceSymbol) -> Option<Identifier> {
    // The derived interface index does not carry a top-level symbol's own type
    // reference (only members do), so this is conservative: only a symbol whose
    // single member surface is empty and whose kind is Var/Const cannot be
    // mapped here → None (top-level completion). Kept as a hook for a later
    // enrichment; returning None is always safe (never a wrong member list).
    let _ = symbol;
    None
}

/// The compiler built-in type names surfaced at the top level. Not exhaustive —
/// the common scalar/string set that a completion list should always offer.
/// Adding a name here only widens the candidate set; it never produces a wrong
/// member list.
const BUILTIN_TYPE_NAMES: &[&str] = &[
    "Integer", "Cardinal", "Byte", "Word", "SmallInt", "ShortInt", "LongInt", "LongWord",
    "Int64", "UInt64", "NativeInt", "NativeUInt", "Single", "Double", "Extended", "Currency",
    "Boolean", "ByteBool", "WordBool", "LongBool", "Char", "AnsiChar", "WideChar", "string",
    "AnsiString", "WideString", "UnicodeString", "ShortString", "Pointer", "Variant", "TObject",
];

fn symbol_completion(symbol: &crate::unit_cache::InterfaceSymbol) -> Completion {
    Completion {
        display: symbol.name,
        key: symbol.key,
        kind: CompletionKind::Symbol(symbol.kind),
        type_key: None,
        directives: Vec::new(),
        visibility: crate::ast::Visibility::Unspecified,
    }
}

fn member_completion(member: &crate::unit_cache::MemberSymbol) -> Completion {
    Completion {
        display: member.name,
        key: member.key,
        kind: CompletionKind::Member(member.kind),
        type_key: member.type_key,
        directives: member.directives.clone(),
        visibility: member.visibility,
    }
}

// ─── Signature-help helpers (read RoutineType from the AST) ──────────────

/// Every top-level routine declaration (`procedure`/`function`) in `meta`'s
/// interface whose folded name == `callee_key`, rendered as a signature. Returns
/// one entry per matching declaration (overloads → multiple).
fn top_level_signatures(
    meta: &UnitMeta,
    callee_key: Identifier,
) -> Vec<crate::query::SignatureInfo> {
    use crate::ast::{DeclarationKind, TypeExpression};
    meta.ast
        .interface_declarations
        .iter()
        .filter(|declaration| {
            declaration.name.key == callee_key
                && matches!(
                    declaration.kind,
                    DeclarationKind::Procedure | DeclarationKind::Function
                )
        })
        .filter_map(|declaration| match &declaration.type_expression {
            // A top-level routine stores its signature as a Routine type
            // expression (see parser::parse_routine_header).
            Some(TypeExpression::Routine(routine)) => Some(render_signature(
                declaration.name.name,
                routine,
                &declaration.generic_parameters,
            )),
            _ => None,
        })
        .collect()
}

/// The interface `InterfaceDeclaration` for a TYPE named `owner_key`, if present
/// (`None` for a non-type or absent symbol).
fn find_type_declaration<'meta>(
    meta: &'meta UnitMeta,
    owner_key: Identifier,
) -> Option<&'meta crate::ast::InterfaceDeclaration> {
    use crate::ast::DeclarationKind;
    meta.ast
        .interface_declarations
        .iter()
        .find(|declaration| {
            declaration.name.key == owner_key && declaration.kind == DeclarationKind::Type
        })
}

/// Signatures of the method(s) named `method_key` declared directly on the type
/// `declaration` (class/record/interface). Walks the type's visibility sections.
/// Returns one entry per matching method (overloads → multiple). Empty when the
/// declaration is not a structured type or has no such method.
fn method_signatures(
    declaration: &crate::ast::InterfaceDeclaration,
    method_key: Identifier,
) -> Vec<crate::query::SignatureInfo> {
    let mut signatures = Vec::new();
    for member in type_members(declaration) {
        if let crate::ast::Member::Method(method) = member {
            // A method-resolution clause (`procedure IFoo.M = Impl;`) carries no
            // real signature — skip it (never a fabricated signature).
            if method.resolution_target.is_some() {
                continue;
            }
            if method.name.key == method_key {
                signatures.push(render_signature(
                    method.name.name,
                    &method.routine,
                    &method.generic_parameters,
                ));
            }
        }
    }
    signatures
}

/// Every direct member of a structured type (class/record via visibility
/// sections, interface via its flat member list). Empty for any other type
/// expression (an alias, enum, pointer — no members).
fn type_members(
    declaration: &crate::ast::InterfaceDeclaration,
) -> Vec<&crate::ast::Member> {
    use crate::ast::TypeExpression;
    match declaration.type_expression.as_ref() {
        Some(TypeExpression::Class(class)) => class
            .sections
            .iter()
            .flat_map(|section| section.members.iter())
            .collect(),
        Some(TypeExpression::Record(record)) => record
            .sections
            .iter()
            .flat_map(|section| section.members.iter())
            .collect(),
        Some(TypeExpression::Interface(interface)) => interface.members.iter().collect(),
        _ => Vec::new(),
    }
}

/// Render a [`crate::query::SignatureInfo`] from a routine's display name and
/// its [`crate::ast::RoutineType`]. The label is
/// `<keyword> <name>(<params>)[: <return>]`, built from the resolved parts —
/// never fabricated.
fn render_signature(
    name: Identifier,
    routine: &crate::ast::RoutineType,
    generic_parameters: &[crate::ast::GenericParameter],
) -> crate::query::SignatureInfo {
    use crate::ast::RoutineKind;
    let keyword = match routine.kind {
        RoutineKind::Procedure => "procedure",
        RoutineKind::Function => "function",
        RoutineKind::Constructor => "constructor",
        RoutineKind::Destructor => "destructor",
        RoutineKind::Operator => "operator",
    };
    // One ParameterInfo per parameter-group NAME (a `const A, B: Integer` group
    // is two parameters A and B, each carrying the group's modifier/type).
    let parameters: Vec<crate::query::ParameterInfo> = routine
        .parameters
        .iter()
        .flat_map(render_parameter_group)
        .collect();
    let parameter_list = parameters
        .iter()
        .map(|parameter| parameter.label.clone())
        .collect::<Vec<_>>()
        .join("; ");

    // Return type only for a function (or an operator with one). A procedure /
    // constructor / destructor carries none; a function whose return type is not
    // renderable leaves it None (never fabricated).
    let return_type = routine
        .return_type
        .as_ref()
        .and_then(render_type_expression);

    // A GENERIC routine (`function Map<T>(...)`) carries a type-parameter
    // clause that belongs AFTER the name and BEFORE the `(`. Render only the
    // parameter NAMES (`<T, U>`); constraint clauses are spans not rendered
    // here. Empty for a non-generic routine → no clause.
    let generic_clause = if generic_parameters.is_empty() {
        String::new()
    } else {
        let names = generic_parameters
            .iter()
            .map(|parameter| crate::globals::resolve(parameter.name.name).to_string())
            .collect::<Vec<_>>()
            .join(", ");
        format!("<{names}>")
    };

    let mut label = format!(
        "{keyword} {}{generic_clause}({parameter_list})",
        crate::globals::resolve(name)
    );
    if let Some(return_display) = &return_type {
        label.push_str(": ");
        label.push_str(return_display);
    }

    crate::query::SignatureInfo {
        label,
        parameters,
        return_type,
    }
}

/// Render one parameter GROUP (`const A, B: Integer = 0`) to one
/// [`crate::query::ParameterInfo`] PER NAME, each carrying the shared
/// modifier/type/default. An untyped group (`var Buffer`) renders each name with
/// no `: Type`. The default (` = ...`) is rendered from its source span text.
fn render_parameter_group(parameter: &crate::ast::Parameter) -> Vec<crate::query::ParameterInfo> {
    use crate::ast::ParameterModifier;
    let modifier = match parameter.modifier {
        ParameterModifier::None => "",
        ParameterModifier::Var => "var ",
        ParameterModifier::Const => "const ",
        ParameterModifier::Out => "out ",
    };
    let type_suffix = parameter
        .parameter_type
        .as_ref()
        .and_then(render_type_expression)
        .map(|type_display| format!(": {type_display}"))
        .unwrap_or_default();
    // A default value (`= 0`) is a source span — render it from the arena text,
    // trimmed. Rendered once for the group (Delphi allows a default only on a
    // single-name group, but we render defensively for whatever the AST holds).
    let default_suffix = parameter
        .default
        .and_then(render_span_text)
        .map(|default_text| format!(" = {}", default_text.trim()))
        .unwrap_or_default();

    // An anonymous parameter group (no names) is impossible in valid Delphi;
    // render nothing rather than a fabricated placeholder.
    parameter
        .names
        .iter()
        .map(|name| crate::query::ParameterInfo {
            label: format!(
                "{modifier}{}{type_suffix}{default_suffix}",
                crate::globals::resolve(name.name)
            ),
        })
        .collect()
}

/// Render a [`crate::ast::TypeExpression`] to a display string. A simple
/// `Reference` uses the display track of its (possibly dotted) name plus any
/// generic arguments; other shapes render structurally. Returns `None` only for
/// a shape we cannot honestly render (never a fabricated type name).
fn render_type_expression(type_expression: &crate::ast::TypeExpression) -> Option<String> {
    use crate::ast::TypeExpression;
    match type_expression {
        TypeExpression::Reference {
            name,
            type_arguments,
        } => {
            let base = crate::globals::resolve(name.name).to_string();
            if type_arguments.is_empty() {
                return Some(base);
            }
            let arguments: Vec<String> = type_arguments
                .iter()
                .map(|argument| render_type_expression(argument).unwrap_or_else(|| "…".to_string()))
                .collect();
            Some(format!("{base}<{}>", arguments.join(", ")))
        }
        TypeExpression::Pointer(inner) => {
            render_type_expression(inner).map(|inner| format!("^{inner}"))
        }
        TypeExpression::ClassReference(name) => {
            Some(format!("class of {}", crate::globals::resolve(name.name)))
        }
        TypeExpression::Array { bounds, element } => {
            let element_display = render_type_expression(element)?;
            // A FIXED array (`array[0..3] of T`) carries a bounds span; a
            // DYNAMIC array carries None. These are DIFFERENT types, so never
            // render a fixed array as a bare `array of T`. Render the bounds
            // from their source span; if the span is unrenderable, return None
            // (omit the `: Type`) rather than misrepresent the fixed array.
            match bounds {
                Some(bounds_span) => {
                    let bounds_text = render_span_text(*bounds_span)?;
                    Some(format!("array[{}] of {element_display}", bounds_text.trim()))
                }
                None => Some(format!("array of {element_display}")),
            }
        }
        TypeExpression::ArrayOfConst => Some("array of const".to_string()),
        TypeExpression::SetOf(inner) => {
            render_type_expression(inner).map(|inner| format!("set of {inner}"))
        }
        // Anonymous/complex shapes (inline records, procedural types, subranges)
        // are not reduced to a simple display here — None, so the caller renders
        // the parameter without a `: Type` rather than fabricating one.
        _ => None,
    }
}

/// The trimmed source text a [`CodeLocation`] span covers, via the global arena.
/// `None` if the file is unreadable or the span is out of bounds / splits a
/// UTF-8 boundary (never a wrong substring).
fn render_span_text(location: CodeLocation) -> Option<String> {
    let content = crate::globals::arena().content(location.file).ok()?;
    let (start, end) = (location.span.start as usize, location.span.end as usize);
    if end > content.len()
        || start > end
        || !content.is_char_boundary(start)
        || !content.is_char_boundary(end)
    {
        return None;
    }
    Some(content[start..end].to_string())
}

/// Build [`crate::query::HoverInfo`] from a top-level interface symbol. A
/// top-level symbol carries no simple declared-type key in the derived index
/// (only members do) and `Unspecified` visibility — honest kind-only facts.
fn symbol_hover(
    symbol: &crate::unit_cache::InterfaceSymbol,
    occurrence: CodeLocation,
) -> crate::query::HoverInfo {
    crate::query::HoverInfo {
        display: symbol.name,
        kind: CompletionKind::Symbol(symbol.kind),
        type_key: symbol_declared_type_key(symbol),
        directives: Vec::new(),
        visibility: crate::ast::Visibility::Unspecified,
        owner_type: None,
        occurrence,
    }
}

/// Build [`crate::query::HoverInfo`] from a type member, carrying its declared
/// type key (when simple), directives, visibility and the owner's DISPLAY name
/// (`owner_display`, so hover reads `TUser.Greet`, not the folded `TUSER`).
fn member_hover(
    member: &crate::unit_cache::MemberSymbol,
    owner_display: Identifier,
    occurrence: CodeLocation,
) -> crate::query::HoverInfo {
    crate::query::HoverInfo {
        display: member.name,
        kind: CompletionKind::Member(member.kind),
        type_key: member.type_key,
        directives: member.directives.clone(),
        visibility: member.visibility,
        owner_type: Some(owner_display),
        occurrence,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{QualifiedName, Unit};
    use crate::cache_store::CacheIdentity;
    use crate::context::{DefineSet, SwitchState, TargetPlatform};
    use crate::meta::{CodeLocation, Span};
    use crate::unit_cache::{UnitCache, hash_file};
    use crate::unit_meta::UnitMeta;
    use std::collections::HashMap;

    fn temp_directory(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join("delphi_parser_driver").join(name);
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }

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

    fn store_in(directory: &Path) -> CacheStore {
        let project = directory.join("P.dproj");
        std::fs::write(&project, "<x/>").unwrap();
        CacheStore::in_directory(
            directory,
            &CacheIdentity {
                project_path: &project,
                configuration: "Debug",
                platform: "Win32",
                compiler_version: 36.0,
            },
        )
        .unwrap()
    }

    fn insert_artifact(session: &ProjectSession, unit: &str, source: &Path) -> crate::context::Identifier {
        // register the source in the GLOBAL arena so a serialized FileId in the
        // AST name location resolves back on save/load
        let file = crate::globals::arena().load(source).unwrap();
        let key = crate::globals::intern_key(unit);
        let ast = Unit {
            name: QualifiedName {
                name: crate::globals::intern(unit),
                key,
                location: CodeLocation { file, span: Span::new(0, 4) },
            },
            interface_uses: None,
            interface_declarations: Vec::new(),
            implementation_uses: None,
        };
        let meta = UnitMeta::new(
            ast,
            false,
            source.to_path_buf(),
            hash_file(source).unwrap(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        session.index.index_artifact(key, &meta);
        session.context.unit_cache.insert(key, Arc::new(meta));
        key
    }

    /// Task-19 soundness checkpoint: a parse that materializes SEVERAL files
    /// (a consumer + its on-disk import), then a `trim_arena`-style trim at the
    /// checkpoint, then queries on the parsed unit — no panic, correct results.
    /// This exercises the exact ordering the LSP uses: parse (borrows dropped) →
    /// trim (clears cold disk content) → query (re-reads on demand). Spans that
    /// index a trimmed file must still resolve after the re-read.
    #[test]
    fn parse_then_trim_then_query_is_sound_and_correct() {
        let directory = temp_directory("parse_trim_query");
        // A referenced import with a distinctive type, padded large so its disk
        // content is a meaningful chunk of resident bytes.
        std::fs::write(
            directory.join("Lib19.pas"),
            format!(
                "unit Lib19; interface type TLib = class end; // {}\nimplementation end.",
                "p".repeat(4096)
            ),
        )
        .unwrap();
        std::fs::write(
            directory.join("Con19.pas"),
            "unit Con19; interface uses Lib19;\n\
             type TCon = class Field: TLib; end;\n\
             implementation end.",
        )
        .unwrap();

        let mut context = test_context();
        Arc::get_mut(&mut context)
            .unwrap()
            .search_paths
            .push(directory.clone());
        let mut session =
            ProjectSession::from_parts(context, store_in(&directory), Duration::from_secs(300));

        // Parse the consumer: materializes Con19 AND its import Lib19 into the
        // arena. All borrows from the parse are dropped when it returns.
        let (_, meta) = session.parse_source_file(directory.join("Con19.pas")).unwrap();
        let con_meta = meta.expect("consumer meta");
        let con_key = con_meta.name();

        // CHECKPOINT: clear THIS session's disk entries (Con19 + Lib19) — the
        // effect a trim has on them, applied to the KNOWN FileIds so the shared
        // global arena test runner is not disturbed (in production the session
        // lock serializes; the harness has no cross-test lock, so a cap-based
        // trim-to-zero here would race other tests — see
        // `clear_disk_entry_for_test`). No arena borrow is live (the parse
        // returned), exactly the checkpoint condition.
        let con_file = con_meta.ast.name.location.file;
        let lib_file = session.arena().register(directory.join("Lib19.pas")).unwrap();
        // Con19 (the parsed unit) is definitely resident → clearing frees bytes.
        assert!(session.arena().clear_disk_entry_for_test(con_file) > 0, "Con19 was resident and cleared");
        // Lib19 may be resident (materialized during import) or not (interface
        // loaded without retaining decoded content) — either way, clear it so the
        // cross-unit query below must re-read it from disk on demand.
        session.arena().clear_disk_entry_for_test(lib_file);

        // Queries now re-read trimmed content on demand. A definition into Con19
        // resolves its own type; a definition of TLib resolves cross-unit into
        // the (trimmed, re-read) Lib19. Neither panics; both are correct.
        let tcon = crate::globals::intern_key("TCon");
        let tlib = crate::globals::intern_key("TLib");
        let con_def = session.definition(con_key, tcon, None);
        assert_eq!(con_def.len(), 1, "TCon resolves in Con19 after trim");
        // `location_text` re-reads Con19's content and slices the span — proving
        // a span into a trimmed file still resolves.
        assert_eq!(session.arena().location_text(con_def[0]), "TCon");

        let lib_def = session.definition(con_key, tlib, None);
        assert_eq!(lib_def.len(), 1, "TLib resolves cross-unit into Lib19 after trim");
        assert_eq!(session.arena().location_text(lib_def[0]), "TLib");
    }

    #[test]
    fn reload_on_miss_does_not_reparse_on_hash_match_but_reparses_after_change() {
        // Deliverable E: an evicted imported unit reloads from disk (NO re-parse)
        // on a hash match, and IS re-parsed after its source bytes change.
        let directory = temp_directory("reload_on_miss");
        std::fs::write(
            directory.join("Lib.pas"),
            "unit Lib; interface const Marker = 1; implementation end.",
        )
        .unwrap();
        std::fs::write(
            directory.join("App.pas"),
            "unit App; interface uses Lib;\n\
             {$IF Declared(Marker)} const Uses1 = True; {$IFEND}\n\
             implementation end.",
        )
        .unwrap();

        // A context whose search path finds Lib, so App's `uses Lib` resolves.
        let mut context = test_context();
        Arc::get_mut(&mut context)
            .unwrap()
            .search_paths
            .push(directory.clone());
        let mut session =
            ProjectSession::from_parts(context, store_in(&directory), Duration::from_secs(300));

        let lib_key = crate::globals::intern_key("Lib");

        // First parse of App: parses App AND its import Lib. Lib is persisted on
        // insert (disk unit).
        session.parse_source_file(directory.join("App.pas")).unwrap();
        assert!(session.store.unit_file_path("Lib").exists(), "Lib persisted on insert");

        // Evict Lib from RAM only (its per-unit file survives on disk).
        session.context.unit_cache.invalidate(lib_key);
        session.context.unit_cache.run_pending_tasks();
        assert!(session.context.unit_cache.get(lib_key).is_none(), "Lib evicted from RAM");

        // Re-parse App with Lib evicted, source UNCHANGED. App re-parses (+1);
        // the loader's interface_of(Lib) must RELOAD Lib from disk, NOT re-parse
        // it. So the parse-probe advances by exactly 1 (App only).
        let before = crate::pipeline::parse_probe::count();
        session.parse_source_file(directory.join("App.pas")).unwrap();
        let after_match = crate::pipeline::parse_probe::count();
        assert_eq!(
            after_match - before,
            1,
            "hash match: only App re-parses, Lib reloads from disk (no re-parse)"
        );
        // Lib is back in RAM (reloaded), proving the reload path ran.
        assert!(session.context.unit_cache.get(lib_key).is_some());

        // Now CHANGE Lib's source bytes and evict it again. The per-unit file's
        // recorded hash no longer matches → reload rejected → Lib IS re-parsed.
        std::fs::write(
            directory.join("Lib.pas"),
            "unit Lib; interface const Marker = 1; const Extra = 2; implementation end.",
        )
        .unwrap();
        session.context.unit_cache.invalidate(lib_key);
        session.context.unit_cache.run_pending_tasks();

        let before_change = crate::pipeline::parse_probe::count();
        session.parse_source_file(directory.join("App.pas")).unwrap();
        let after_change = crate::pipeline::parse_probe::count();
        assert_eq!(
            after_change - before_change,
            2,
            "hash mismatch: App AND Lib both re-parse (stale reload rejected)"
        );
    }

    #[test]
    fn persist_on_insert_writes_per_unit_file_for_disk_unit() {
        // Deliverable B: inserting a freshly-parsed DISK unit persists it to its
        // per-unit file immediately (before it could be evicted), so an eviction
        // is always a safe, reloadable drop.
        let directory = temp_directory("persist_on_insert");
        let source = directory.join("Persisted.pas");
        std::fs::write(&source, "unit Persisted;").unwrap();

        let session =
            ProjectSession::from_parts(test_context(), store_in(&directory), Duration::from_secs(300));
        insert_artifact(&session, "Persisted", &source);

        // the per-unit file exists on disk right after insert — no eviction, no
        // bulk save needed
        assert!(
            session.store.unit_file_path("Persisted").exists(),
            "a disk unit is persisted on insert"
        );
        // and it reloads hash-valid
        let reloaded = session.store.load_unit("Persisted").expect("reloads");
        assert_eq!(crate::globals::resolve(reloaded.name()), "PERSISTED");
    }

    #[test]
    fn per_file_plan_invalidates_and_marks_dirty() {
        let directory = temp_directory("per_file");
        let source = directory.join("UnitA.pas");
        std::fs::write(&source, "unit UnitA;").unwrap();

        let mut session =
            ProjectSession::from_parts(test_context(), store_in(&directory), Duration::from_secs(300));
        let unit = insert_artifact(&session, "UNITA", &source);

        let report = session.apply_plan(&InvalidationPlan::PerFile(vec![source]));
        assert_eq!(report.invalidated_units, 1);
        assert!(session.context.unit_cache.get(unit).is_none());
        assert!(session.dirty);
    }

    #[test]
    fn autosave_after_interval_and_reload_on_open_equivalent() {
        let directory = temp_directory("autosave");
        let source = directory.join("UnitA.pas");
        std::fs::write(&source, "unit UnitA;").unwrap();

        let mut session =
            ProjectSession::from_parts(test_context(), store_in(&directory), Duration::from_millis(0));
        insert_artifact(&session, "UNITA", &source);
        session.mark_dirty();

        // save_interval 0 → first tick with dirty flag saves
        let report = session.tick(Instant::now() + Duration::from_millis(1)).unwrap();
        let saved = report.saved.expect("a save was due");
        assert_eq!(saved.written, 1);
        assert!(saved.skipped.is_empty());
        assert!(!session.dirty);

        // fresh "session" (context swap): snapshot loads back
        let fresh = test_context();
        let store = store_in(&directory);
        let loaded = store
            .load_into(&fresh.unit_cache)
            .unwrap()
            .unwrap();
        assert_eq!(loaded.loaded, 1);
    }

    #[test]
    fn shutdown_saves_final_state() {
        let directory = temp_directory("shutdown");
        let source = directory.join("UnitA.pas");
        std::fs::write(&source, "unit UnitA;").unwrap();

        let session =
            ProjectSession::from_parts(test_context(), store_in(&directory), Duration::from_secs(300));
        insert_artifact(&session, "UNITA", &source);
        assert_eq!(session.shutdown().unwrap().written, 1);
    }

    #[test]
    fn parse_source_file_indexes_nested_units_for_invalidation() {
        let directory = temp_directory("nested_index");
        std::fs::write(
            directory.join("UnitA.pas"),
            "unit UnitA; interface const Alpha = 1; implementation end.",
        )
        .unwrap();
        std::fs::write(
            directory.join("UnitB.pas"),
            "unit UnitB; interface uses UnitA;\n\
             {$IF Declared(Alpha)} const HasAlpha = True; {$IFEND}\n\
             implementation end.",
        )
        .unwrap();

        let mut context = ProjectContext {
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
        context.search_paths.dedup();
        let mut session = ProjectSession::from_parts(
            Arc::new(context),
            store_in(&directory),
            Duration::from_secs(300),
        );

        let (_, artifact) = session
            .parse_source_file(directory.join("UnitB.pas"))
            .unwrap();
        let artifact = artifact.unwrap();
        assert_eq!(artifact.dependencies.len(), 1);
        assert!(session.dirty);
        // both units cached (B top-level, A as lazy side effect) — checked
        // via get(): moka's entry_count is eventually consistent
        assert!(session.context.unit_cache.get(session.context.intern_key("UNITA")).is_some());
        assert!(session.context.unit_cache.get(session.context.intern_key("UNITB")).is_some());

        // a change to UnitA.pas must invalidate BOTH:
        // A via its own source, B via its dependency stamp
        let report = session.apply_plan(&InvalidationPlan::PerFile(vec![
            directory.join("UnitA.pas"),
        ]));
        assert_eq!(report.invalidated_units, 2);
    }

    #[test]
    fn parse_links_sibling_dfm_and_dfm_edit_invalidates_unit() {
        // Deliverable B.3 end-to-end: a unit with a sibling `.dfm` is parsed,
        // the linker runs and stores component/handler links on the session,
        // and a `.dfm` edit invalidates the unit (dfm stamp in the reverse
        // index → per-file invalidation).
        let directory = temp_directory("dfm_link_wiring");
        let pas = directory.join("Form1.pas");
        let dfm = directory.join("Form1.dfm");
        std::fs::write(
            &pas,
            "unit Form1;\ninterface\n\
             type TForm1 = class(TForm)\n\
             published\n\
               Button1: TButton;\n\
               procedure Button1Click(Sender: TObject);\n\
             end;\n\
             implementation\nend.",
        )
        .unwrap();
        std::fs::write(
            &dfm,
            "object Form1: TForm1\n\
             \x20 object Button1: TButton\n\
             \x20   OnClick = Button1Click\n\
             \x20 end\n\
             end\n",
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
        let mut session = ProjectSession::from_parts(
            Arc::new(context),
            store_in(&directory),
            Duration::from_secs(300),
        );

        let (_, meta) = session.parse_source_file(&pas).unwrap();
        let meta = meta.unwrap();
        // the sibling dfm was associated
        assert!(meta.dfm.is_some(), "sibling dfm must be stamped");

        // links were computed and stored at the session level
        let unit_key = session.context.intern_key("FORM1");
        let links = session.dfm_links(unit_key).expect("dfm links stored");
        assert!(
            links
                .component_links
                .iter()
                .any(|link| link.component_key == session.context.intern_key("Button1")),
            "Button1 component link: {links:?}"
        );
        assert!(
            links
                .handler_links
                .iter()
                .any(|link| link.method_key == session.context.intern_key("Button1Click")),
            "Button1Click handler link: {links:?}"
        );

        // editing the DFM invalidates the unit (its stamp is in the reverse
        // index via watched_files → per-file plan hits it)
        std::fs::write(&dfm, "object Form1: TForm1\n  Left = 10\nend\n").unwrap();
        let report = session.apply_plan(&InvalidationPlan::PerFile(vec![dfm.clone()]));
        assert_eq!(report.invalidated_units, 1, "dfm edit must invalidate the unit");
        assert!(session.context.unit_cache.get(unit_key).is_none());
        // the invalidation reports which key it evicted (drives the purge)
        assert!(
            report.invalidated_keys.contains(&unit_key),
            "the evicted unit key must be reported: {report:?}"
        );
        // and the derived dfm links were PURGED — a query after the edit must
        // no longer return the stale pre-edit links/diagnostics
        assert!(
            session.dfm_links(unit_key).is_none(),
            "dfm links must be purged when the unit is invalidated"
        );
    }

    // ─── Deliverable A: LSP query API ───────────────────────────────────

    /// A session over a two-unit project on disk: `Models` declares a class
    /// with members and a const; `Client` imports it and uses its symbols. The
    /// context search path is the temp directory so imports resolve.
    fn query_session(directory: &Path) -> ProjectSession {
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
        ProjectSession::from_parts(Arc::new(context), store_in(directory), Duration::from_secs(300))
    }

    #[test]
    fn symbol_at_hits_declaration_and_definition_resolves_own_and_cross_unit() {
        let directory = temp_directory("query_symbol_def");
        std::fs::write(
            directory.join("Models.pas"),
            "unit Models;\ninterface\n\
             type TUser = class\n  Name: string;\n  procedure Greet;\nend;\n\
             const MaxUsers = 10;\n\
             implementation\nend.",
        )
        .unwrap();
        std::fs::write(
            directory.join("Client.pas"),
            "unit Client;\ninterface\nuses Models;\n\
             type TManager = class\n  Boss: TUser;\nend;\n\
             implementation\nend.",
        )
        .unwrap();

        let mut session = query_session(&directory);
        session.parse_source_file(directory.join("Client.pas")).unwrap();

        let client_key = session.context.intern_key("CLIENT");
        let models_key = session.context.intern_key("MODELS");

        // symbol_at: the `TManager` declaration name in Client
        let client_meta = session.meta_of(client_key).unwrap();
        let manager_decl = client_meta
            .interface()
            .find(session.context.intern_key("TManager"))
            .unwrap();
        let position = manager_decl.location.span.start;
        let target = session
            .symbol_at(client_key, position)
            .expect("symbol_at hits the TManager declaration");
        assert_eq!(target.key, session.context.intern_key("TManager"));
        assert_eq!(target.kind, TargetKind::Declaration);

        // definition: own symbol resolves to its own declaration location
        let own_def = session.definition(client_key, session.context.intern_key("TManager"), None);
        assert_eq!(own_def, vec![manager_decl.location]);

        // definition: cross-unit — TUser is declared in Models, imported by
        // Client. Resolution goes through the loader (dependency-recorded).
        let cross_def = session.definition(client_key, session.context.intern_key("TUser"), None);
        assert_eq!(cross_def.len(), 1, "TUser resolves to Models");
        let models_meta = session.meta_of(models_key).expect("Models cached as import");
        let user_decl = models_meta
            .interface()
            .find(session.context.intern_key("TUser"))
            .unwrap();
        assert_eq!(cross_def[0], user_decl.location);

        // member definition: TUser.Name resolves cross-unit to the member site
        let member_def = session.definition(
            client_key,
            session.context.intern_key("Name"),
            Some(session.context.intern_key("TUser")),
        );
        let name_member = user_decl.find_member(session.context.intern_key("Name")).unwrap();
        assert_eq!(member_def, vec![name_member.location]);

        // unresolved target → empty, never a wrong location
        let ghost = session.definition(client_key, session.context.intern_key("Nonexistent"), None);
        assert!(ghost.is_empty());
    }

    // ─── Stage 1: same-unit local variable / parameter resolution ────────────

    /// A body-local variable and a parameter both resolve — via `symbol_at` /
    /// `definition_at` — to their OWN declaration span (never the interface,
    /// never a usage). Cursor on a local's use inside the body → the `var Local`
    /// decl span; cursor on a parameter's use → the parameter span.
    #[test]
    fn local_and_param_resolve_to_their_declaration() {
        let directory = temp_directory("query_local_resolve");
        let source = "unit U;\ninterface\nimplementation\n\
             procedure Run(Amount: Integer);\nvar Local: Integer;\nbegin\n  Local := Amount;\nend;\nend.";
        std::fs::write(directory.join("U.pas"), source).unwrap();
        let mut session = query_session(&directory);
        session.parse_source_file(directory.join("U.pas")).unwrap();
        let unit_key = session.context.intern_key("U");

        let meta = session.meta_of(unit_key).unwrap();
        assert!(meta.impl_scopes_reliable, "sanity: reliable pass");
        let routine = &meta.impl_scopes[0];
        let local_decl_span = routine.locals[0].name.location;
        let param_decl_span = routine.params[0].name.location;

        // Cursor on the USE of `Local` in `Local := Amount;`.
        let local_use = source.rfind("Local").unwrap() as u32;
        let target = session.symbol_at(unit_key, local_use).expect("local use resolves");
        assert_eq!(target.kind, TargetKind::Local);
        assert_eq!(target.key, session.context.intern_key("Local"));
        assert_eq!(target.location, local_decl_span);
        assert_eq!(session.definition_at(unit_key, local_use), vec![local_decl_span]);

        // Cursor on the USE of `Amount` in `Local := Amount;`.
        let amount_use = source.rfind("Amount").unwrap() as u32;
        let target = session.symbol_at(unit_key, amount_use).expect("param use resolves");
        assert_eq!(target.kind, TargetKind::Local);
        assert_eq!(target.key, session.context.intern_key("Amount"));
        assert_eq!(session.definition_at(unit_key, amount_use), vec![param_decl_span]);

        // Cursor directly on the `Local` DECLARATION also yields a Local target.
        let target = session
            .symbol_at(unit_key, local_decl_span.span.start)
            .expect("local decl resolves");
        assert_eq!(target.kind, TargetKind::Local);
        assert_eq!(target.location, local_decl_span);
    }

    /// SHADOWING: an interface `type Local = class … end;` AND a body-local
    /// `Local`. Cursor in the body resolves to the LOCAL; a cursor on an
    /// interface-scope use of `Local` (the field type) resolves to the INTERFACE
    /// type. A body identifier matching nothing in scope falls through.
    #[test]
    fn body_local_shadows_interface_symbol() {
        let directory = temp_directory("query_shadow");
        // `Local` is BOTH an interface type and a body-local var. `TThing` is an
        // interface type used as the local's type (interface-scope use).
        let source = "unit U;\ninterface\n\
             type Local = class end;\n\
             type TThing = class\n  Field: Local;\nend;\n\
             implementation\n\
             procedure Run;\nvar Local: Integer;\nbegin\n  Local := 1;\nend;\nend.";
        std::fs::write(directory.join("U.pas"), source).unwrap();
        let mut session = query_session(&directory);
        session.parse_source_file(directory.join("U.pas")).unwrap();
        let unit_key = session.context.intern_key("U");
        let meta = session.meta_of(unit_key).unwrap();
        assert!(meta.impl_scopes_reliable);

        let local_key = session.context.intern_key("Local");
        let interface_local = meta.interface().find(local_key).unwrap().location;
        let body_local_decl = meta.impl_scopes[0].locals[0].name.location;
        assert_ne!(interface_local, body_local_decl, "distinct sites");

        // In-body use of `Local` → the body-local declaration (shadowing wins).
        let body_use = source.rfind("Local := 1").unwrap() as u32;
        let target = session.symbol_at(unit_key, body_use).expect("body use");
        assert_eq!(target.kind, TargetKind::Local);
        assert_eq!(target.location, body_local_decl, "body use resolves to the local");
        assert_eq!(session.definition_at(unit_key, body_use), vec![body_local_decl]);

        // Interface-scope use of `Local` (the `Field: Local` type) → the
        // INTERFACE type declaration, NOT the body-local (it is outside any body).
        let field_type_use = source.find("Field: Local").unwrap() as u32 + "Field: ".len() as u32;
        let target = session.symbol_at(unit_key, field_type_use).expect("interface use");
        assert_ne!(
            target.kind,
            TargetKind::Local,
            "an interface-scope use must not resolve to a body local"
        );
        assert_eq!(
            session.definition_at(unit_key, field_type_use),
            vec![interface_local],
            "interface-scope use resolves to the interface type"
        );

        // A body identifier matching nothing in scope (`TThing` used nowhere in
        // the body) — cursor on a plain body statement identifier that is not a
        // local falls through to interface/usage, never a wrong Local.
        // Here `Run` (the routine name occurrence) is not a local.
        let run_use = source.find("procedure Run").unwrap() as u32 + "procedure ".len() as u32;
        let target = session.symbol_at(unit_key, run_use);
        if let Some(target) = target {
            assert_ne!(target.kind, TargetKind::Local, "the routine name is not a local");
        }
    }

    /// Bug 1 (never a WRONG answer): a member access `SomeObj.Value` must NOT
    /// bind `.Value` to a same-named routine local. The RECEIVER position and a
    /// bare `Value := …` (no dot) still bind to the local — the guard is not
    /// over-broad.
    #[test]
    fn member_access_does_not_bind_to_same_named_local() {
        let directory = temp_directory("query_member_guard");
        // Local `Value`. Body has a MEMBER access `SomeObj.Value := 1;` and a
        // BARE `Value := 2;`. `SomeObj` is an unresolved receiver (nothing in
        // scope) — the point is purely the dot guard on `.Value`.
        let source = "unit U;\ninterface\nimplementation\n\
             procedure Run;\nvar Value: Integer;\nbegin\n  SomeObj.Value := 1;\n  Value := 2;\nend;\nend.";
        std::fs::write(directory.join("U.pas"), source).unwrap();
        let mut session = query_session(&directory);
        session.parse_source_file(directory.join("U.pas")).unwrap();
        let unit_key = session.context.intern_key("U");
        let meta = session.meta_of(unit_key).unwrap();
        assert!(meta.impl_scopes_reliable, "sanity: reliable pass");
        let local_decl_span = meta.impl_scopes[0].locals[0].name.location;

        // Cursor on the `.Value` in `SomeObj.Value` — the MEMBER — must NOT
        // resolve to the local (never a wrong local attribution).
        let member_value = source.find("SomeObj.Value").unwrap() as u32
            + "SomeObj.".len() as u32;
        let target = session.symbol_at(unit_key, member_value);
        if let Some(target) = target {
            assert_ne!(
                target.kind,
                TargetKind::Local,
                "a `.Value` member access must not bind to the local `Value`"
            );
        }
        assert_ne!(
            session.definition_at(unit_key, member_value),
            vec![local_decl_span],
            "definition of `.Value` must not point at the local decl"
        );

        // Cursor on the RECEIVER `SomeObj` (no dot before it) still enters the
        // normal path; it is not a local here, but must not be misclassified.
        let receiver = source.find("SomeObj.Value").unwrap() as u32;
        let receiver_target = session.symbol_at(unit_key, receiver);
        if let Some(target) = receiver_target {
            assert_ne!(
                target.location, local_decl_span,
                "the receiver must not resolve to the local decl"
            );
        }

        // Cursor on the BARE `Value := 2;` (no dot) DOES bind to the local — the
        // guard did not over-reject.
        let bare_value = source.find("Value := 2").unwrap() as u32;
        let target = session.symbol_at(unit_key, bare_value).expect("bare local use");
        assert_eq!(target.kind, TargetKind::Local, "bare `Value` binds to the local");
        assert_eq!(target.location, local_decl_span);
        assert_eq!(session.definition_at(unit_key, bare_value), vec![local_decl_span]);
    }

    /// Bug 2 (never a WRONG answer): a nested routine referencing an OUTER
    /// routine's local — that also shares a name with an interface symbol — binds
    /// to the OUTER local, not the interface type. Inner shadows outer for a name
    /// declared in both.
    #[test]
    fn nested_routine_binds_outer_local_over_interface() {
        let directory = temp_directory("query_nested_outer_local");
        // Interface type `Helper`. `Outer` has a local `Helper: Integer` that
        // shadows the interface name. `Inner` (nested) references `Helper` — must
        // bind to Outer's local. `Inner` also declares its own `Shadowed` which
        // must win over Outer's `Shadowed` (tightest scope wins).
        let source = "unit U;\ninterface\n\
             type Helper = class end;\n\
             implementation\n\
             procedure Outer;\nvar Helper: Integer;\n  Shadowed: Integer;\n\
             \n  procedure Inner;\n  var Shadowed: string;\n  begin\n    Helper := 2;\n    Shadowed := '';\n  end;\n\
             begin\n  Inner;\nend;\nend.";
        std::fs::write(directory.join("U.pas"), source).unwrap();
        let mut session = query_session(&directory);
        session.parse_source_file(directory.join("U.pas")).unwrap();
        let unit_key = session.context.intern_key("U");
        let meta = session.meta_of(unit_key).unwrap();
        assert!(meta.impl_scopes_reliable, "sanity: reliable pass");

        let helper_key = session.context.intern_key("Helper");
        let interface_helper = meta.interface().find(helper_key).unwrap().location;

        // Locate Outer's `Helper` local and Inner's `Shadowed` local across the
        // two recorded routines (order-independent).
        let outer_helper_decl = meta
            .impl_scopes
            .iter()
            .flat_map(|routine| routine.locals.iter())
            .find(|declaration| declaration.name.key == helper_key)
            .expect("Outer's Helper local")
            .name
            .location;
        assert_ne!(interface_helper, outer_helper_decl, "distinct sites");

        // Cursor on `Helper := 2;` inside Inner → binds to OUTER's local, NOT the
        // interface type (Bug 2: an enclosing local is found before falling
        // through to the interface).
        let inner_helper_use = source.find("Helper := 2").unwrap() as u32;
        let target = session.symbol_at(unit_key, inner_helper_use).expect("inner Helper use");
        assert_eq!(target.kind, TargetKind::Local, "resolves to a scope local");
        assert_eq!(
            target.location, outer_helper_decl,
            "binds Outer's local Helper, not the interface type"
        );
        assert_eq!(
            session.definition_at(unit_key, inner_helper_use),
            vec![outer_helper_decl],
            "definition points at Outer's local decl, never the interface type"
        );
        assert_ne!(
            session.definition_at(unit_key, inner_helper_use),
            vec![interface_helper],
            "must NOT resolve to the interface Helper type"
        );

        // A name declared in BOTH Inner and Outer resolves to Inner's (tightest
        // scope wins) when the cursor is inside Inner. Inner is the TIGHTEST
        // routine (smallest body span) that owns a `Shadowed` local.
        let shadowed_key = session.context.intern_key("Shadowed");
        let mut shadowed_owners: Vec<&crate::ast::ImplRoutine> = meta
            .impl_scopes
            .iter()
            .filter(|routine| {
                routine.locals.iter().any(|declaration| declaration.name.key == shadowed_key)
            })
            .collect();
        assert_eq!(shadowed_owners.len(), 2, "both Inner and Outer declare Shadowed");
        shadowed_owners.sort_by_key(|routine| routine.body_span.len());
        let inner_shadowed_decl = shadowed_owners[0]
            .locals
            .iter()
            .find(|declaration| declaration.name.key == shadowed_key)
            .unwrap()
            .name
            .location;
        let outer_shadowed_decl = shadowed_owners[1]
            .locals
            .iter()
            .find(|declaration| declaration.name.key == shadowed_key)
            .unwrap()
            .name
            .location;
        assert_ne!(inner_shadowed_decl, outer_shadowed_decl, "distinct Shadowed sites");
        let inner_shadowed_use = source.find("Shadowed := ''").unwrap() as u32;
        let target = session.symbol_at(unit_key, inner_shadowed_use).expect("inner Shadowed use");
        assert_eq!(target.kind, TargetKind::Local);
        assert_eq!(
            target.location, inner_shadowed_decl,
            "Inner's Shadowed shadows Outer's (tightest scope wins)"
        );
    }

    /// When `impl_scopes_reliable == false`, scope lookups return no Local — the
    /// query falls through to today's behavior (never a wrong local answer).
    #[test]
    fn unreliable_impl_scopes_suppress_local_resolution() {
        let directory = temp_directory("query_unreliable");
        // An `asm` body degrades the impl-section pass to unreliable.
        let source = "unit U;\ninterface\nimplementation\n\
             procedure Run;\nvar Local: Integer;\nbegin\nasm\n  NOP\nend;\nend;\nend.";
        std::fs::write(directory.join("U.pas"), source).unwrap();
        let mut session = query_session(&directory);
        session.parse_source_file(directory.join("U.pas")).unwrap();
        let unit_key = session.context.intern_key("U");
        let meta = session.meta_of(unit_key).unwrap();
        assert!(
            !meta.impl_scopes_reliable,
            "sanity: the asm body degraded the pass"
        );

        // A use of `Local` in the body must NOT resolve to a Local target — the
        // flag gates the whole scope branch off.
        let local_use = source.rfind("Local").unwrap() as u32;
        let target = session.symbol_at(unit_key, local_use);
        if let Some(target) = target {
            assert_ne!(
                target.kind,
                TargetKind::Local,
                "an unreliable pass must never yield a Local target"
            );
        }
    }

    /// Hover on a body-local yields facts from its kind + type key, never from a
    /// same-named interface symbol.
    #[test]
    fn hover_on_body_local_uses_local_facts() {
        use crate::query::CompletionKind;
        use crate::unit_cache::SymbolKind;
        let directory = temp_directory("query_local_hover");
        let source = "unit U;\ninterface\nimplementation\n\
             procedure Run;\nvar Local: Integer;\nbegin\n  Local := 1;\nend;\nend.";
        std::fs::write(directory.join("U.pas"), source).unwrap();
        let mut session = query_session(&directory);
        session.parse_source_file(directory.join("U.pas")).unwrap();
        let unit_key = session.context.intern_key("U");

        let local_use = source.rfind("Local").unwrap() as u32;
        let hover = session.hover_info(unit_key, local_use).expect("local hover");
        assert_eq!(hover.kind, CompletionKind::Symbol(SymbolKind::Var));
        assert_eq!(hover.type_key, Some(session.context.intern_key("Integer")));
        // display is the display-track spelling as written at the declaration.
        assert_eq!(crate::globals::resolve(hover.display), "Local");
    }

    #[test]
    fn hover_info_resolves_facts_own_member_cross_unit_and_unknown() {
        use crate::query::CompletionKind;
        use crate::unit_cache::MemberKind;

        let directory = temp_directory("query_hover");
        std::fs::write(
            directory.join("Models.pas"),
            "unit Models;\ninterface\n\
             type TUser = class\n  Name: string;\n  procedure Greet; virtual;\nend;\n\
             implementation\nend.",
        )
        .unwrap();
        std::fs::write(
            directory.join("Client.pas"),
            "unit Client;\ninterface\nuses Models;\n\
             type TManager = class\n  Boss: TUser;\nend;\n\
             implementation\nend.",
        )
        .unwrap();

        let mut session = query_session(&directory);
        session.parse_source_file(directory.join("Client.pas")).unwrap();
        let client_key = session.context.intern_key("CLIENT");
        let models_key = session.context.intern_key("MODELS");

        // Hover over the `Boss: TUser` FIELD declaration in Client: a member
        // whose declared type key is `TUser`, kind Field, owner TManager.
        let client_meta = session.meta_of(client_key).unwrap();
        let manager = client_meta
            .interface()
            .find(session.context.intern_key("TManager"))
            .unwrap();
        let boss = manager.find_member(session.context.intern_key("Boss")).unwrap();
        let hover = session
            .hover_info(client_key, boss.location.span.start)
            .expect("hover over the Boss field");
        assert_eq!(hover.kind, CompletionKind::Member(MemberKind::Field));
        // owner_type carries the owner's DISPLAY name (`TManager`), not the
        // folded lookup key.
        assert_eq!(
            hover.owner_type.map(crate::globals::resolve),
            Some("TManager")
        );
        assert_eq!(
            hover.type_key,
            Some(session.context.intern_key("TUser")),
            "the field's declared type is captured"
        );

        // CROSS-UNIT hover: over the `TUser` occurrence in Client (`Boss: TUser`)
        // resolves to the imported type declaration's facts (kind Type). This
        // also loads Models into the cache as an import (dependency-recorded).
        let client_src = std::fs::read_to_string(directory.join("Client.pas")).unwrap();
        let tuser_offset = client_src.find("TUser").unwrap() as u32;
        let cross = session
            .hover_info(client_key, tuser_offset)
            .expect("hover over the imported TUser resolves cross-unit");
        assert_eq!(cross.kind, CompletionKind::Symbol(crate::unit_cache::SymbolKind::Type));
        assert_eq!(crate::globals::resolve(cross.display), "TUser");

        // Hover over the `Greet` METHOD declaration in Models: directives carry
        // `virtual`. (Models is now cached from the cross-unit resolution above.)
        let models_meta = session.meta_of(models_key).expect("Models cached");
        let user = models_meta
            .interface()
            .find(session.context.intern_key("TUser"))
            .unwrap();
        let greet = user.find_member(session.context.intern_key("Greet")).unwrap();
        let method_hover = session
            .hover_info(models_key, greet.location.span.start)
            .expect("hover over the Greet method");
        assert_eq!(method_hover.kind, CompletionKind::Member(MemberKind::Method));
        assert!(
            method_hover
                .directives
                .contains(&session.context.intern_key("virtual")),
            "the method's `virtual` directive is surfaced: {:?}",
            method_hover.directives
        );

        // Unknown identifier → None, never fabricated facts.
        assert!(
            session.hover_info(client_key, 100_000).is_none(),
            "an out-of-range cursor has no hover"
        );
    }

    #[test]
    fn references_across_units_and_purge_on_invalidation() {
        let directory = temp_directory("query_refs");
        std::fs::write(
            directory.join("Shared.pas"),
            "unit Shared;\ninterface\n\
             type TThing = class end;\n\
             implementation\nend.",
        )
        .unwrap();
        std::fs::write(
            directory.join("UserA.pas"),
            "unit UserA;\ninterface\nuses Shared;\n\
             type TA = class\n  Field: TThing;\nend;\n\
             implementation\nend.",
        )
        .unwrap();
        std::fs::write(
            directory.join("UserB.pas"),
            "unit UserB;\ninterface\nuses Shared;\n\
             type TB = class\n  Other: TThing;\nend;\n\
             implementation\nend.",
        )
        .unwrap();

        let mut session = query_session(&directory);
        session.parse_source_file(directory.join("UserA.pas")).unwrap();
        session.parse_source_file(directory.join("UserB.pas")).unwrap();

        let thing_key = session.context.intern_key("TThing");
        let occurrences = session.references(thing_key);
        // TThing occurs: as a declaration in Shared, and as a field type usage
        // in both UserA and UserB → at least 3 occurrences across ≥2 units.
        let units: std::collections::HashSet<_> =
            occurrences.iter().map(|occurrence| occurrence.unit).collect();
        assert!(
            units.contains(&session.context.intern_key("USERA"))
                && units.contains(&session.context.intern_key("USERB")),
            "TThing referenced from both UserA and UserB: {occurrences:?}"
        );

        // invalidate UserA → its occurrences are purged, UserB's remain, and no
        // occurrence points into the evicted unit.
        let report = session.apply_plan(&InvalidationPlan::PerFile(vec![
            directory.join("UserA.pas"),
        ]));
        assert!(report.invalidated_units >= 1);
        let after = session.references(thing_key);
        assert!(
            after.iter().all(|occurrence| occurrence.unit != session.context.intern_key("USERA")),
            "no occurrence may point into the evicted UserA: {after:?}"
        );
        assert!(
            after.iter().any(|occurrence| occurrence.unit == session.context.intern_key("USERB")),
            "UserB occurrences survive the UserA eviction"
        );
    }

    #[test]
    fn member_completion_after_dot_and_top_level_includes_import() {
        let directory = temp_directory("query_completions");
        std::fs::write(
            directory.join("Lib.pas"),
            "unit Lib;\ninterface\n\
             type TWidget = class\n\
             public\n  procedure Draw;\n  Width: Integer;\nend;\n\
             const LibVersion = 2;\n\
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

        let mut session = query_session(&directory);
        session.parse_source_file(directory.join("App.pas")).unwrap();
        let app_key = session.context.intern_key("APP");

        // top-level completion: own symbols + imported Lib symbols + builtins
        let app_meta = session.meta_of(app_key).unwrap();
        let end_position = app_meta
            .interface()
            .find(session.context.intern_key("Paint"))
            .unwrap()
            .location
            .span
            .end;
        let top = session.completions(app_key, end_position + 1);
        let top_keys: std::collections::HashSet<_> = top.iter().map(|c| c.key).collect();
        assert!(top_keys.contains(&session.context.intern_key("TScreen")), "own symbol");
        assert!(
            top_keys.contains(&session.context.intern_key("TWidget")),
            "imported symbol from Lib present in top-level completion"
        );
        assert!(top_keys.contains(&session.context.intern_key("Integer")), "builtin");

        // member completion after `TWidget.`: use symbol_at-free path by asking
        // completions at a position right after a TWidget usage. We synthesize a
        // usage by parsing a unit that references TWidget then a dot. Simpler:
        // resolve the member list directly via member_completions on the type.
        let members = session.member_completions(&app_meta, session.context.intern_key("TWidget"));
        let member_keys: std::collections::HashSet<_> = members.iter().map(|c| c.key).collect();
        assert!(member_keys.contains(&session.context.intern_key("Draw")), "method member");
        assert!(member_keys.contains(&session.context.intern_key("Width")), "field member");
        // members only — no top-level symbols leaked in
        assert!(!member_keys.contains(&session.context.intern_key("LibVersion")));
        let draw = members
            .iter()
            .find(|c| c.key == session.context.intern_key("Draw"))
            .unwrap();
        assert_eq!(draw.visibility, crate::ast::Visibility::Public);
    }

    #[test]
    fn member_completion_after_dot_resolves_receiver_type() {
        // End-to-end member completion: a `TWidget.` scope access. The receiver
        // is the last usage before the cursor; its type is resolved and members
        // completed. Uses a static-scope receiver (a type name) so the receiver
        // maps to itself.
        let directory = temp_directory("query_completion_dot");
        // The implementation body references `TShape` (recorded as a usage
        // occurrence) followed by a `.` — the cursor sits right after the dot,
        // so `member_receiver_at` finds `TShape` as the nearest receiver and
        // resolves it to the type whose members complete.
        std::fs::write(
            directory.join("Shapes.pas"),
            "unit Shapes;\ninterface\n\
             type TShape = class\n  procedure Area;\n  Sides: Integer;\nend;\n\
             const Pi = 3;\n\
             implementation\n\
             procedure Use;\nbegin\n  TShape.\nend;\nend.",
        )
        .unwrap();

        let mut session = query_session(&directory);
        session.parse_source_file(directory.join("Shapes.pas")).unwrap();
        let key = session.context.intern_key("SHAPES");
        let meta = session.meta_of(key).unwrap();

        // Find the implementation-body `TShape` usage. The `.` follows it; the
        // member-access position sits RIGHT AFTER that dot — the dot gate
        // (finding 2) requires an actual `.` between the receiver and the cursor.
        let receiver_usage = meta
            .usages
            .iter()
            .filter(|usage| usage.symbol == session.context.intern_key("TShape"))
            .max_by_key(|usage| usage.location.span.end)
            .expect("TShape usage recorded in the implementation body");
        let receiver_end = receiver_usage.location.span.end;
        // locate the `.` after the receiver and set the cursor one byte past it
        let content = session.arena.content(receiver_usage.location.file).unwrap();
        let dot_offset = content[receiver_end as usize..]
            .find('.')
            .map(|relative| receiver_end as usize + relative)
            .expect("a '.' follows the TShape receiver");
        let position = (dot_offset + 1) as u32;
        let completions = session.completions(key, position);
        let keys: std::collections::HashSet<_> = completions.iter().map(|c| c.key).collect();
        // receiver resolved to TShape → member list, not top-level
        assert!(keys.contains(&session.context.intern_key("Area")), "member Area: {keys:?}");
        assert!(keys.contains(&session.context.intern_key("Sides")), "member Sides");
        assert!(!keys.contains(&session.context.intern_key("Pi")), "top-level const must not leak");

        // NEGATIVE (the dot gate): the SAME receiver end with NO dot after it —
        // the cursor at the receiver's end, before the dot — must NOT enter
        // member mode. It returns the TOP-LEVEL set (builtins + own interface),
        // never TShape's members. Incomplete context → top-level, never wrong.
        let top_level = session.completions(key, receiver_end);
        let top_keys: std::collections::HashSet<_> = top_level.iter().map(|c| c.key).collect();
        assert!(
            top_keys.contains(&session.context.intern_key("Pi")),
            "no dot → top-level set includes the own const Pi: {top_keys:?}"
        );
        assert!(
            top_keys.contains(&session.context.intern_key("Integer")),
            "no dot → top-level set includes builtins: {top_keys:?}"
        );
        assert!(
            !top_keys.contains(&session.context.intern_key("Area")),
            "no dot → TShape members must NOT be returned: {top_keys:?}"
        );
    }

    // ─── Deliverable B: signature_help query ────────────────────────────

    #[test]
    fn signature_help_own_method_params_and_return() {
        // A method on an OWN type: read its parameters + return type from the AST.
        let directory = temp_directory("sig_own_method");
        std::fs::write(
            directory.join("Calc.pas"),
            "unit Calc;\ninterface\n\
             type TCalc = class\npublic\n\
               function Compute(const A: Integer; B: string): Boolean;\n\
             end;\n\
             implementation\nend.",
        )
        .unwrap();

        let mut session = query_session(&directory);
        session.parse_source_file(directory.join("Calc.pas")).unwrap();
        let key = session.context.intern_key("CALC");

        let signatures = session.signature_help(
            key,
            session.context.intern_key("Compute"),
            Some(session.context.intern_key("TCalc")),
        );
        assert_eq!(signatures.len(), 1, "one Compute method: {signatures:?}");
        let signature = &signatures[0];
        assert_eq!(
            signature.label,
            "function Compute(const A: Integer; B: string): Boolean"
        );
        assert_eq!(signature.parameters.len(), 2);
        assert_eq!(signature.parameters[0].label, "const A: Integer");
        assert_eq!(signature.parameters[1].label, "B: string");
        assert_eq!(signature.return_type.as_deref(), Some("Boolean"));
    }

    #[test]
    fn signature_help_cross_unit_routine_and_procedure_no_return() {
        // A top-level routine imported from another unit (cross-unit, SAME loader
        // as definition), and a PROCEDURE (no return type).
        let directory = temp_directory("sig_cross_unit");
        std::fs::write(
            directory.join("Lib.pas"),
            "unit Lib;\ninterface\n\
             function Add(X: Integer; Y: Integer): Integer;\n\
             procedure Log(const Message: string);\n\
             implementation\nend.",
        )
        .unwrap();
        std::fs::write(
            directory.join("Main.pas"),
            "unit Main;\ninterface\nuses Lib;\n\
             implementation\nend.",
        )
        .unwrap();

        let mut session = query_session(&directory);
        session.parse_source_file(directory.join("Main.pas")).unwrap();
        let main_key = session.context.intern_key("MAIN");

        // cross-unit function
        let add = session.signature_help(main_key, session.context.intern_key("Add"), None);
        assert_eq!(add.len(), 1, "Add resolves cross-unit: {add:?}");
        assert_eq!(
            add[0].label,
            "function Add(X: Integer; Y: Integer): Integer"
        );
        assert_eq!(add[0].return_type.as_deref(), Some("Integer"));

        // cross-unit procedure → NO return type
        let log = session.signature_help(main_key, session.context.intern_key("Log"), None);
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].label, "procedure Log(const Message: string)");
        assert_eq!(log[0].return_type, None, "a procedure has no return type");
    }

    #[test]
    fn signature_help_untyped_and_defaulted_params() {
        // An untyped `var` parameter (`var Buffer`) renders without `: Type`; a
        // defaulted parameter (`= 0`) carries its default.
        let directory = temp_directory("sig_untyped_default");
        std::fs::write(
            directory.join("Io.pas"),
            "unit Io;\ninterface\n\
             procedure Read(var Buffer; Count: Integer = 0);\n\
             implementation\nend.",
        )
        .unwrap();

        let mut session = query_session(&directory);
        session.parse_source_file(directory.join("Io.pas")).unwrap();
        let key = session.context.intern_key("IO");

        let signatures = session.signature_help(key, session.context.intern_key("Read"), None);
        assert_eq!(signatures.len(), 1, "{signatures:?}");
        let signature = &signatures[0];
        assert_eq!(signature.parameters.len(), 2);
        // untyped var parameter — no `: Type`
        assert_eq!(signature.parameters[0].label, "var Buffer");
        // defaulted parameter carries its default
        assert_eq!(signature.parameters[1].label, "Count: Integer = 0");
        assert_eq!(
            signature.label,
            "procedure Read(var Buffer; Count: Integer = 0)"
        );
    }

    #[test]
    fn signature_help_multiple_names_per_group_and_overloads() {
        // A `const A, B: Integer` group expands to two parameters; two methods
        // sharing a name (overloads) each yield a distinct signature.
        let directory = temp_directory("sig_group_overload");
        std::fs::write(
            directory.join("Over.pas"),
            "unit Over;\ninterface\n\
             type TOver = class\npublic\n\
               procedure Same(A, B: Integer); overload;\n\
               procedure Same(S: string); overload;\n\
             end;\n\
             implementation\nend.",
        )
        .unwrap();

        let mut session = query_session(&directory);
        session.parse_source_file(directory.join("Over.pas")).unwrap();
        let key = session.context.intern_key("OVER");

        let signatures = session.signature_help(
            key,
            session.context.intern_key("Same"),
            Some(session.context.intern_key("TOver")),
        );
        // two overloads → two signatures
        assert_eq!(signatures.len(), 2, "both overloads returned: {signatures:?}");
        // the (A, B: Integer) overload expands the group to two parameters
        let grouped = signatures
            .iter()
            .find(|signature| signature.parameters.len() == 2)
            .expect("the (A, B: Integer) overload has two parameters");
        assert_eq!(grouped.parameters[0].label, "A: Integer");
        assert_eq!(grouped.parameters[1].label, "B: Integer");
        // The grouped `A, B: Integer` is expanded so every parameter label is a
        // self-contained substring of the signature label — the editor can then
        // highlight the active parameter by matching its label.
        assert_eq!(grouped.label, "procedure Same(A: Integer; B: Integer)");
        assert!(grouped.label.contains(&grouped.parameters[0].label));
        assert!(grouped.label.contains(&grouped.parameters[1].label));
        // the (S: string) overload
        assert!(
            signatures
                .iter()
                .any(|signature| signature.label == "procedure Same(S: string)"),
            "the string overload: {signatures:?}"
        );
    }

    #[test]
    fn signature_help_fixed_array_param_renders_bounds_not_bare_dynamic() {
        // A FIXED-length array parameter (`array[0..3] of Byte`) is a DIFFERENT
        // type from a dynamic/open `array of Byte`. Its bounds must be rendered
        // (never dropped, which would misrepresent it as dynamic). A dynamic
        // array in the same signature still renders as `array of T`.
        let directory = temp_directory("sig_fixed_array");
        std::fs::write(
            directory.join("Buf.pas"),
            "unit Buf;\ninterface\n\
             type TBuf = class\npublic\n\
               procedure Fill(const A: array[0..3] of Byte; const D: array of Byte);\n\
             end;\n\
             implementation\nend.",
        )
        .unwrap();

        let mut session = query_session(&directory);
        session.parse_source_file(directory.join("Buf.pas")).unwrap();
        let key = session.context.intern_key("BUF");

        let signatures = session.signature_help(
            key,
            session.context.intern_key("Fill"),
            Some(session.context.intern_key("TBuf")),
        );
        assert_eq!(signatures.len(), 1, "{signatures:?}");
        let signature = &signatures[0];
        assert_eq!(signature.parameters.len(), 2);
        // FIXED array — bounds rendered, NOT a bare `array of Byte`
        assert_eq!(signature.parameters[0].label, "const A: array[0..3] of Byte");
        assert!(
            !signature.parameters[0].label.contains("A: array of Byte"),
            "a fixed array must not render as a dynamic array: {}",
            signature.parameters[0].label
        );
        // DYNAMIC array — bare `array of Byte`
        assert_eq!(signature.parameters[1].label, "const D: array of Byte");
        assert_eq!(
            signature.label,
            "procedure Fill(const A: array[0..3] of Byte; const D: array of Byte)"
        );
    }

    #[test]
    fn signature_help_generic_method_renders_type_parameter_clause() {
        // A GENERIC method (`function Map<T>(...)`) carries a type-parameter
        // clause that must appear after the name and before the `(`. It must
        // not be silently dropped. A multi-parameter generic renders `<T, U>`.
        let directory = temp_directory("sig_generic_method");
        std::fs::write(
            directory.join("Gen.pas"),
            "unit Gen;\ninterface\n\
             type TGen = class\npublic\n\
               function Map<T>(const Item: T): T;\n\
               function Pair<K, V>(const Key: K; const Value: V): Boolean;\n\
             end;\n\
             implementation\nend.",
        )
        .unwrap();

        let mut session = query_session(&directory);
        session.parse_source_file(directory.join("Gen.pas")).unwrap();
        let key = session.context.intern_key("GEN");

        let map = session.signature_help(
            key,
            session.context.intern_key("Map"),
            Some(session.context.intern_key("TGen")),
        );
        assert_eq!(map.len(), 1, "{map:?}");
        assert!(
            map[0].label.contains("<T>"),
            "generic clause `<T>` must appear: {}",
            map[0].label
        );
        assert_eq!(map[0].label, "function Map<T>(const Item: T): T");

        let pair = session.signature_help(
            key,
            session.context.intern_key("Pair"),
            Some(session.context.intern_key("TGen")),
        );
        assert_eq!(pair.len(), 1, "{pair:?}");
        assert_eq!(
            pair[0].label,
            "function Pair<K, V>(const Key: K; const Value: V): Boolean"
        );
    }

    #[test]
    fn signature_help_at_resolves_top_level_and_static_receiver_by_offset() {
        // The higher-level offset entry: resolve the callee (and, for a
        // `Type.Method(` static call, its receiver type) from a byte offset.
        let directory = temp_directory("sig_help_at");
        std::fs::write(
            directory.join("Api.pas"),
            "unit Api;\ninterface\n\
             type TApi = class\npublic\n\
               class function Fetch(Id: Integer): string;\n\
             end;\n\
             procedure Ping(Host: string);\n\
             implementation\n\
             procedure Use;\nbegin\n  Ping('h');\n  TApi.Fetch(1);\nend;\nend.",
        )
        .unwrap();

        let mut session = query_session(&directory);
        session.parse_source_file(directory.join("Api.pas")).unwrap();
        let key = session.context.intern_key("API");
        let content = std::fs::read_to_string(directory.join("Api.pas")).unwrap();

        // top-level routine at its call site
        let ping_offset = content.rfind("Ping(").unwrap() as u32;
        let ping = session.signature_help_at(key, ping_offset);
        assert_eq!(ping.len(), 1, "{ping:?}");
        assert_eq!(ping[0].label, "procedure Ping(Host: string)");

        // static method call `TApi.Fetch(` → resolve receiver TApi, then Fetch
        let fetch_offset = content.rfind("Fetch(").unwrap() as u32;
        let fetch = session.signature_help_at(key, fetch_offset);
        assert_eq!(fetch.len(), 1, "static receiver resolves: {fetch:?}");
        assert_eq!(fetch[0].label, "function Fetch(Id: Integer): string");

        // an offset with no resolvable callee → empty (never fabricated)
        assert!(session.signature_help_at(key, 100_000).is_empty());
    }

    #[test]
    fn signature_help_unknown_callee_and_non_routine_is_empty() {
        // An unknown callee, and a NON-routine symbol (a type), both yield no
        // signature — never fabricated.
        let directory = temp_directory("sig_unknown");
        std::fs::write(
            directory.join("Types.pas"),
            "unit Types;\ninterface\n\
             type TThing = class end;\n\
             procedure Real(X: Integer);\n\
             implementation\nend.",
        )
        .unwrap();

        let mut session = query_session(&directory);
        session.parse_source_file(directory.join("Types.pas")).unwrap();
        let key = session.context.intern_key("TYPES");

        // unknown name
        assert!(
            session
                .signature_help(key, session.context.intern_key("Nonexistent"), None)
                .is_empty(),
            "an unknown callee yields no signature"
        );
        // a TYPE is not a routine → no signature
        assert!(
            session
                .signature_help(key, session.context.intern_key("TThing"), None)
                .is_empty(),
            "a non-routine symbol yields no signature"
        );
        // a member on an unresolved owner → no signature
        assert!(
            session
                .signature_help(
                    key,
                    session.context.intern_key("Whatever"),
                    Some(session.context.intern_key("TGhost"))
                )
                .is_empty(),
            "a member on an unresolved owner yields no signature"
        );
    }

    #[test]
    fn diagnostics_unifies_parse_and_dfm() {
        // A unit with an unknown `{$IF}` leaves a parse diagnostic; a sibling
        // dfm with a dangling component leaves a dfm diagnostic. diagnostics()
        // returns both in one list, tagged by source.
        let directory = temp_directory("query_diagnostics");
        std::fs::write(
            directory.join("Form9.pas"),
            "unit Form9;\ninterface\n\
             {$IF SizeOf(TUnknownExternal) > 4} const A = 1; {$IFEND}\n\
             type TForm9 = class(TForm)\n  Known: TButton;\nend;\n\
             implementation\nend.",
        )
        .unwrap();
        std::fs::write(
            directory.join("Form9.dfm"),
            "object Form9: TForm9\n  object Known: TButton\n  end\n  object Ghost: TLabel\n  end\nend\n",
        )
        .unwrap();

        let mut session = query_session(&directory);
        session.parse_source_file(directory.join("Form9.pas")).unwrap();
        let key = session.context.intern_key("FORM9");

        let diagnostics = session.diagnostics(key);
        assert!(
            diagnostics.iter().any(|d| d.source == DiagnosticSource::Parse),
            "an unknown {{$IF}} leaves a parse diagnostic: {diagnostics:?}"
        );
        assert!(
            diagnostics.iter().any(|d| d.source == DiagnosticSource::Dfm),
            "the dangling Ghost component leaves a dfm diagnostic: {diagnostics:?}"
        );
    }

    // ─── Part B: conservative unused-uses analysis ─────────────────────────

    /// Set up a project directory with the three helper units the unused-uses
    /// tests share: `Used` (exports `TUsed`), `Unused` (exports `TUnused`), and a
    /// consumer written by the caller.
    fn write_used_and_unused(directory: &Path) {
        std::fs::write(
            directory.join("Used.pas"),
            "unit Used;\ninterface\ntype TUsed = class end;\nimplementation\nend.",
        )
        .unwrap();
        std::fs::write(
            directory.join("Unused.pas"),
            "unit Unused;\ninterface\ntype TUnused = class end;\nimplementation\nend.",
        )
        .unwrap();
    }

    #[test]
    fn unused_uses_flags_only_the_unreferenced_import() {
        // Consumer uses BOTH Used and Unused but references only TUsed → exactly
        // Unused is flagged, and only as a Hint from the Analysis source with the
        // side-effect caveat (never an error/removal instruction).
        let directory = temp_directory("unused_uses_basic");
        write_used_and_unused(&directory);
        std::fs::write(
            directory.join("Consumer.pas"),
            "unit Consumer;\ninterface\nuses Used, Unused;\n\
             implementation\n\
             procedure P;\nvar X: TUsed;\nbegin X := TUsed.Create; end;\n\
             end.",
        )
        .unwrap();

        let mut session = query_session(&directory);
        session
            .parse_source_file(directory.join("Consumer.pas"))
            .unwrap();
        // Cache the imports' interfaces. unused-uses is CACHE-ONLY (Task-15 OOM
        // fix): it never force-parses an import on the analyze hot path, so a
        // uses entry is only evaluable once its interface is already cached
        // (e.g. the user navigated into it). Pre-cache both here to exercise the
        // flagging logic.
        session
            .parse_source_file(directory.join("Used.pas"))
            .unwrap();
        session
            .parse_source_file(directory.join("Unused.pas"))
            .unwrap();
        let key = session.context.intern_key("CONSUMER");

        let unused = session.unused_units(key);
        let flagged: Vec<String> = unused
            .iter()
            .map(|u| crate::globals::resolve(u.unit).to_string())
            .collect();
        assert_eq!(flagged, ["Unused"], "only the unreferenced unit is flagged");

        // surfaced as a HINT via the Analysis source with the caveat wording.
        let diagnostics = session.diagnostics(key);
        let hint = diagnostics
            .iter()
            .find(|d| d.source == DiagnosticSource::Analysis)
            .expect("an unused-uses hint is published");
        assert_eq!(hint.severity, crate::token_cursor::Severity::Hint);
        assert!(hint.message.contains("Unused"));
        assert!(
            hint.message.contains("side effects"),
            "the hint carries the side-effect caveat, not a removal instruction: {}",
            hint.message
        );
        // NEVER flags the referenced unit.
        assert!(
            !flagged.iter().any(|name| name == "Used"),
            "a referenced unit must never be flagged"
        );
        // the hint range is the uses entry span (non-degenerate).
        let location = hint.location.expect("hint carries the uses-entry span");
        assert!(location.span.end > location.span.start);
    }

    #[test]
    fn unused_uses_spares_import_whose_name_matches_a_referenced_symbol() {
        // The consumer references a name (`TShared`) that ALSO exists as an
        // export of the imported unit. Even though the reference may really bind
        // elsewhere, an export-key match spares the import — a false "used" is
        // safe, a false "unused" is not.
        let directory = temp_directory("unused_uses_name_match");
        std::fs::write(
            directory.join("Shared.pas"),
            "unit Shared;\ninterface\ntype TShared = class end;\nimplementation\nend.",
        )
        .unwrap();
        std::fs::write(
            directory.join("Consumer.pas"),
            // A local type named TShared is what the body references; Shared also
            // EXPORTS TShared, so the export-key match spares Shared.
            "unit Consumer;\ninterface\nuses Shared;\n\
             implementation\n\
             type TShared = class end;\n\
             procedure P;\nvar X: TShared;\nbegin X := nil; end;\n\
             end.",
        )
        .unwrap();

        let mut session = query_session(&directory);
        session
            .parse_source_file(directory.join("Consumer.pas"))
            .unwrap();
        let key = session.context.intern_key("CONSUMER");
        let flagged: Vec<String> = session
            .unused_units(key)
            .iter()
            .map(|u| crate::globals::resolve(u.unit).to_string())
            .collect();
        assert!(
            flagged.is_empty(),
            "a name-match on an export key spares the import: {flagged:?}"
        );
    }

    #[test]
    fn unused_uses_never_flags_an_unloadable_import() {
        // The consumer imports a unit with NO source on the search path (DCU-only
        // from our view). It cannot be loaded → cannot be proven unused → never
        // flagged, even though nothing references it.
        let directory = temp_directory("unused_uses_unloadable");
        std::fs::write(
            directory.join("Consumer.pas"),
            "unit Consumer;\ninterface\nuses Vcl.Forms;\n\
             implementation\nend.",
        )
        .unwrap();

        let mut session = query_session(&directory);
        session
            .parse_source_file(directory.join("Consumer.pas"))
            .unwrap();
        let key = session.context.intern_key("CONSUMER");
        let flagged: Vec<String> = session
            .unused_units(key)
            .iter()
            .map(|u| crate::globals::resolve(u.unit).to_string())
            .collect();
        assert!(
            flagged.is_empty(),
            "an unloadable import must never be flagged: {flagged:?}"
        );
    }

    #[test]
    fn unused_uses_never_flags_a_dependency_consulted_import() {
        // Consumer imports Config only to consult it in `{$IF Declared(Answer)}`
        // — no symbol of Config is otherwise referenced. That consult IS a use
        // (Config is recorded as a dependency) → never flagged.
        let directory = temp_directory("unused_uses_dependency");
        std::fs::write(
            directory.join("Config.pas"),
            "unit Config;\ninterface\nconst Answer = 42;\nimplementation\nend.",
        )
        .unwrap();
        std::fs::write(
            directory.join("Consumer.pas"),
            "unit Consumer;\ninterface\nuses Config;\n\
             {$IF Declared(Answer)} const HasAnswer = True; {$IFEND}\n\
             implementation\nend.",
        )
        .unwrap();

        let mut session = query_session(&directory);
        let (_, meta) = session
            .parse_source_file(directory.join("Consumer.pas"))
            .unwrap();
        let meta = meta.unwrap();
        // Config was genuinely consulted as a dependency (proves the guard fired).
        assert!(
            meta.dependencies
                .iter()
                .any(|d| d.unit == session.context.intern_key("CONFIG")),
            "Config must be recorded as a consulted dependency"
        );
        let key = session.context.intern_key("CONSUMER");
        let flagged: Vec<String> = session
            .unused_units(key)
            .iter()
            .map(|u| crate::globals::resolve(u.unit).to_string())
            .collect();
        assert!(
            flagged.is_empty(),
            "a dependency-consulted import is a real use, never flagged: {flagged:?}"
        );
    }

    #[test]
    fn unused_uses_covers_implementation_uses_too() {
        // An UNREFERENCED unit in the IMPLEMENTATION uses clause is flagged just
        // like an interface one; a referenced implementation import is spared.
        let directory = temp_directory("unused_uses_impl");
        write_used_and_unused(&directory);
        std::fs::write(
            directory.join("Consumer.pas"),
            "unit Consumer;\ninterface\nimplementation\nuses Used, Unused;\n\
             procedure P;\nvar X: TUsed;\nbegin X := TUsed.Create; end;\n\
             end.",
        )
        .unwrap();

        let mut session = query_session(&directory);
        session
            .parse_source_file(directory.join("Consumer.pas"))
            .unwrap();
        // Pre-cache the imports — unused-uses is cache-only (Task-15 OOM fix)
        // and never force-parses an import on the analyze hot path.
        session
            .parse_source_file(directory.join("Used.pas"))
            .unwrap();
        session
            .parse_source_file(directory.join("Unused.pas"))
            .unwrap();
        let key = session.context.intern_key("CONSUMER");
        let flagged: Vec<String> = session
            .unused_units(key)
            .iter()
            .map(|u| crate::globals::resolve(u.unit).to_string())
            .collect();
        assert_eq!(
            flagged, ["Unused"],
            "implementation-uses is analyzed too; the referenced import is spared"
        );
    }

    #[test]
    fn recovered_unit_is_flagged_and_not_persisted_clean() {
        // Deliverable B end-to-end through the session: a unit with one broken
        // interface declaration still yields the others, is flagged `recovered`,
        // exposes a diagnostic, and — like a cycle-tainted meta — is NOT written
        // to the snapshot (never masquerades as a clean interface).
        let directory = temp_directory("recovered_not_persisted");
        let source = directory.join("Half.pas");
        std::fs::write(
            &source,
            "unit Half;\ninterface\n\
             type TGood = class end;\n\
             type TBroken = = = = ;\n\
             const AfterBroken = 5;\n\
             implementation\nend.",
        )
        .unwrap();

        let mut session = query_session(&directory);
        let (_, meta) = session.parse_source_file(&source).unwrap();
        let meta = meta.expect("a recovered unit still produces a meta");

        // surviving declarations present, broken one dropped (no bogus symbol)
        assert!(meta.interface().contains_key(session.context.intern_key("TGood")));
        assert!(meta.interface().contains_key(session.context.intern_key("AfterBroken")));
        assert!(!meta.interface().contains_key(session.context.intern_key("TBroken")));
        // flagged recovered
        assert!(meta.recovered, "the meta must be flagged recovered");
        // a diagnostic surfaced through the session query surface
        let key = session.context.intern_key("HALF");
        assert!(
            session
                .diagnostics(key)
                .iter()
                .any(|d| d.source == DiagnosticSource::Parse),
            "the broken declaration surfaces a parse diagnostic"
        );

        // never persisted as clean: save skips the recovered meta (same gate as
        // cycle_tainted). The snapshot therefore holds zero units.
        let report = session.save_now().unwrap();
        assert_eq!(
            report.written, 0,
            "a recovered meta must not be persisted as a clean interface"
        );
    }

    // ─── Part A: parse_buffer (unsaved editor content) ──────────────────

    #[test]
    fn parse_buffer_parses_unsaved_content_same_interface_as_disk() {
        // The same source parsed from an in-memory buffer and from disk must
        // yield the same interface surface (symbols, kinds).
        let directory = temp_directory("parse_buffer_same");
        let source = "unit Widgets;\ninterface\n\
             type TWidget = class\n  Width: Integer;\n  procedure Draw;\nend;\n\
             const MaxWidgets = 7;\n\
             implementation\nend.";

        // on-disk parse
        let disk_path = directory.join("Widgets.pas");
        std::fs::write(&disk_path, source).unwrap();
        let mut disk_session = query_session(&directory);
        let (_, disk_meta) = disk_session.parse_source_file(&disk_path).unwrap();
        let disk_meta = disk_meta.expect("on-disk unit meta");

        // in-memory buffer parse (unsaved) — a DIFFERENT display path so it
        // cannot collide with the on-disk file
        let mut buffer_session = query_session(&directory);
        let (outcome, buffer_meta) = buffer_session
            .parse_buffer(directory.join("Widgets_unsaved.pas"), source)
            .unwrap();
        let buffer_meta = buffer_meta.expect("buffer unit meta");

        assert!(!outcome.recovered, "clean source must not be flagged recovered");
        // same unit name
        assert_eq!(buffer_meta.name(), disk_meta.name());
        // same interface symbol set (folded keys)
        let disk_keys: std::collections::HashSet<_> =
            disk_meta.interface().symbols.iter().map(|s| s.key).collect();
        let buffer_keys: std::collections::HashSet<_> =
            buffer_meta.interface().symbols.iter().map(|s| s.key).collect();
        assert_eq!(disk_keys, buffer_keys, "interface surface must match on-disk");
        // and the member surface of TWidget matches
        let key = buffer_session.context.intern_key("TWidget");
        let buffer_members: std::collections::HashSet<_> = buffer_meta
            .interface()
            .find(key)
            .unwrap()
            .members
            .iter()
            .map(|m| m.key)
            .collect();
        let disk_members: std::collections::HashSet<_> = disk_meta
            .interface()
            .find(key)
            .unwrap()
            .members
            .iter()
            .map(|m| m.key)
            .collect();
        assert_eq!(buffer_members, disk_members, "member surface must match");
    }

    #[test]
    fn parse_buffer_virtual_unit_is_not_persisted() {
        // The proving invariant (#21/#25): a unit parsed from an unsaved buffer
        // must never MASQUERADE as on-disk state across sessions. A virtual
        // FileId carries a display-only path that does not exist on disk, so on
        // load its `FileId` fails to `register` (canonicalize) — the meta is
        // dropped as `unreadable` and never re-enters a fresh cache. This is the
        // load-time gate SESSION.md #21/#25 describe: unsaved state does not
        // survive a save/load round-trip.
        let directory = temp_directory("parse_buffer_no_persist");
        let mut session = query_session(&directory);
        // The buffer's display path is a NON-EXISTENT file (nothing was written
        // to disk) — the essence of an unsaved editor buffer.
        let virtual_path = directory.join("OnlyInEditor_unsaved.pas");
        assert!(!virtual_path.exists(), "the buffer path must not exist on disk");
        let source = "unit OnlyInEditor;\ninterface\n\
             type TDraft = class end;\n\
             implementation\nend.";
        let (_, meta) = session.parse_buffer(&virtual_path, source).unwrap();
        let meta = meta.expect("buffer produces a meta");
        let key = meta.name();
        // it IS in the live cache (queryable this session)
        assert!(session.meta_of(key).is_some(), "queryable in-session");

        // Save, then load into a FRESH cache: the virtual unit must NOT come
        // back — its FileId path cannot re-register, so it is counted unreadable
        // and dropped. A future session therefore never sees unsaved state.
        let snapshot = directory.join("virtual_roundtrip.bin");
        session.context.unit_cache.save(&snapshot).unwrap();

        let fresh = UnitCache::default();
        let report = fresh.load(&snapshot).unwrap();
        assert_eq!(
            report.loaded, 0,
            "the virtual (unsaved) unit must not load back: {report:?}"
        );
        assert!(
            fresh.get(key).is_none(),
            "the virtual unit must be absent from a freshly loaded cache"
        );
        assert!(
            report.unreadable >= 1,
            "the virtual unit is dropped as unreadable on load: {report:?}"
        );
    }

    #[test]
    fn parse_buffer_reparse_bounds_arena_and_keeps_span_provenance() {
        // Task-15 memory bound at the SESSION level: parsing the SAME document
        // buffer N=1000 times (an editor re-parsing on every keystroke) must
        // keep the arena at ONE virtual entry — the content REPLACED, the prior
        // freed — not N appended full-file copies (the old OOM). After each
        // re-parse, `content(file)` must return the CURRENT text and the meta's
        // spans must resolve against it (span-provenance), and virtual buffers
        // must never persist.
        let directory = temp_directory("parse_buffer_bound");
        let mut session = query_session(&directory);
        let path = directory.join("Editing_unsaved.pas");

        // NOTE the arena is process-global, so its absolute virtual count is not
        // deterministic under the parallel test runner (other tests add virtual
        // buffers concurrently). The concurrency-SAFE proof of the bound is that
        // THIS document's FileId is REUSED across all re-parses — a stable id
        // means no new entry is appended per keystroke. The exhaustive absolute
        // count-stays-1 proof lives in `source::tests::
        // set_virtual_bounds_the_arena_to_one_entry_per_path`, which uses a
        // private local arena and so is deterministic.
        let mut last_file = None;
        for version in 0..1000 {
            // Each version declares a distinctly-named type so we can prove the
            // LATEST content is what the arena and the meta index.
            let source = format!(
                "unit Editing;\ninterface\ntype TThing{version} = class end;\n\
                 implementation\nend."
            );
            let (_, meta) = session.parse_buffer(&path, &source).unwrap();
            let meta = meta.expect("buffer meta");
            let file = meta.ast.name.location.file;

            // The virtual FileId is REUSED, not appended — the bound property
            // (no new arena entry per re-parse of this open document).
            if let Some(previous) = last_file {
                assert_eq!(file, previous, "the virtual FileId is reused, not appended");
            }
            last_file = Some(file);

            // span-provenance: content(file) is exactly this version's text, and
            // the meta's own type symbol resolves against it.
            assert_eq!(session.arena().content(file).unwrap(), source);
            let type_key = session.context.intern_key(&format!("TThing{version}"));
            let symbol = meta
                .interface()
                .find(type_key)
                .expect("the current version's type is in the meta");
            let target = session
                .symbol_at(meta.name(), symbol.location.span.start)
                .expect("symbol_at resolves against the current content");
            assert_eq!(target.key, type_key);
            // and the declaration span (via the arena) reads out of the CURRENT
            // content — proving the span indexes the just-parsed text.
            assert!(
                session
                    .arena()
                    .text(file, symbol.location.span)
                    .contains(&format!("TThing{version}")),
                "the current declaration span reads the current type name"
            );
        }

        // virtual-never-persist still holds after all the re-parses: the reused
        // virtual FileId's path does not exist on disk, so a save/load
        // round-trip drops it as unreadable.
        let key = session
            .parse_buffer(
                &path,
                "unit Editing;\ninterface\ntype TFinal = class end;\nimplementation\nend.",
            )
            .unwrap()
            .1
            .unwrap()
            .name();
        let snapshot = directory.join("bound_roundtrip.bin");
        session.context.unit_cache.save(&snapshot).unwrap();
        let fresh = UnitCache::default();
        let report = fresh.load(&snapshot).unwrap();
        assert!(
            fresh.get(key).is_none(),
            "the virtual unit must not survive a save/load round-trip: {report:?}"
        );
        assert!(
            report.unreadable >= 1,
            "the reused virtual unit is dropped unreadable on load: {report:?}"
        );
    }

    #[test]
    fn parse_buffer_diagnostics_and_symbol_queries_work() {
        // After parse_buffer the LSP query surface (diagnostics, symbol_at)
        // works against the buffer's unit key — this is the handle the LSP maps
        // its Url onto.
        let directory = temp_directory("parse_buffer_query");
        let mut session = query_session(&directory);
        // an unknown {$IF} leaves a parse diagnostic; a clean type declares a
        // symbol we can hit with symbol_at.
        let source = "unit Editing;\ninterface\n\
             {$IF SizeOf(TMysteryExternal) > 4} const A = 1; {$IFEND}\n\
             type TThing = class end;\n\
             implementation\nend.";
        let (_, meta) = session
            .parse_buffer(directory.join("Editing.pas"), source)
            .unwrap();
        let meta = meta.expect("buffer meta");
        let key = meta.name();

        // diagnostics query returns the parse finding from the unknown {$IF}
        let diagnostics = session.diagnostics(key);
        assert!(
            diagnostics
                .iter()
                .any(|d| d.source == DiagnosticSource::Parse),
            "unknown {{$IF}} leaves a queryable parse diagnostic: {diagnostics:?}"
        );

        // symbol_at hits the TThing declaration in the buffer
        let thing = meta
            .interface()
            .find(session.context.intern_key("TThing"))
            .unwrap();
        let target = session
            .symbol_at(key, thing.location.span.start)
            .expect("symbol_at hits TThing in the buffer");
        assert_eq!(target.key, session.context.intern_key("TThing"));
        assert_eq!(target.kind, TargetKind::Declaration);
    }

    /// Task-15 no-cascade proof: a `parse_buffer` of a unit that `uses` and
    /// `{$IF Declared(...)}`s an UNCACHED cross-unit unit must NOT force-parse
    /// that import (the OOM cascade). The import stays absent from the cache
    /// after the buffer parse, and the directive whose target lives ONLY in the
    /// uncached import degrades to Unknown → AssumeFalse (safe, never a wrong
    /// answer). Contrast with `parse_source_file`, which DOES force-load — proven
    /// green by the `unit_loader::tests::declared_forces_lazy_import_parse` suite.
    #[test]
    fn parse_buffer_does_not_cascade_into_uncached_import() {
        let directory = temp_directory("parse_buffer_no_cascade");
        // The cross-unit dependency exists ON DISK but is NOT pre-cached.
        std::fs::write(
            directory.join("Dependency.pas"),
            "unit Dependency; interface const Beacon = 1; implementation end.",
        )
        .unwrap();
        // The editor buffer uses it and probes it with {$IF Declared}. In Full
        // mode this forces Dependency's parse; in ResidentOnly (buffer) mode it
        // must not — Beacon is not resident → Unknown → the {$ELSE} branch.
        let source = "unit Editor; interface uses Dependency;\n\
             {$IF Declared(Beacon)} const SawBeacon = True; {$ELSE} const NoBeacon = True; {$IFEND}\n\
             implementation end.";

        let mut session = query_session(&directory);
        let dependency_key = session.context.intern_key("DEPENDENCY");
        // Precondition: the import is not resident before the buffer parse.
        assert!(
            session.meta_of(dependency_key).is_none(),
            "Dependency must not be cached before the buffer parse"
        );

        let (_, meta) = session
            .parse_buffer(directory.join("Editor_unsaved.pas"), source)
            .unwrap();
        let meta = meta.expect("buffer meta");

        // PROOF OF NO CASCADE: Dependency was NOT parsed into the cache by the
        // buffer parse. (`run_pending_tasks` flushes any pending moka insert so a
        // stray force-load could not hide behind lazy task processing.)
        session.context.unit_cache.run_pending_tasks();
        assert!(
            session.meta_of(dependency_key).is_none(),
            "parse_buffer must NOT force-load the uncached cross-unit import \
             (Task-15 no-cascade): Dependency is still absent from the cache"
        );

        // And the {$IF Declared(Beacon)} degraded to Unknown → AssumeFalse: the
        // {$ELSE} branch was taken (NoBeacon), NOT SawBeacon. Never a wrong
        // answer — just a safe Unknown because Beacon was not resident.
        let names: Vec<String> = meta
            .interface()
            .symbols
            .iter()
            .map(|symbol| crate::globals::resolve(symbol.key).to_string())
            .collect();
        assert!(
            names.iter().any(|name| name == "NOBEACON"),
            "uncached cross-unit Declared degrades to AssumeFalse (else branch): {names:?}"
        );
        assert!(
            !names.iter().any(|name| name == "SAWBEACON"),
            "the buffer parse must not have force-loaded Beacon: {names:?}"
        );
    }

    /// Task-15 counterpart: a cross-unit `{$IF Declared(...)}` whose target is
    /// ALREADY RESIDENT in the RAM cache IS used by a `parse_buffer`. Pre-caching
    /// the import (via a `parse_source_file`, the Full path) makes it resident;
    /// the subsequent buffer parse then resolves `Declared` against it — proving
    /// ResidentOnly serves resident hits exactly like Full, and cross-unit
    /// precision improves as the cache warms.
    #[test]
    fn parse_buffer_uses_resident_import() {
        let directory = temp_directory("parse_buffer_resident");
        std::fs::write(
            directory.join("Dependency.pas"),
            "unit Dependency; interface const Beacon = 1; implementation end.",
        )
        .unwrap();

        let mut session = query_session(&directory);
        // Warm the cache: parse Dependency through the FULL path so it is resident.
        session
            .parse_source_file(directory.join("Dependency.pas"))
            .unwrap();
        let dependency_key = session.context.intern_key("DEPENDENCY");
        assert!(
            session.meta_of(dependency_key).is_some(),
            "Dependency must be resident after parse_source_file"
        );

        // Now the editor buffer parse: Beacon IS resident → Declared resolves True.
        let source = "unit Editor; interface uses Dependency;\n\
             {$IF Declared(Beacon)} const SawBeacon = True; {$ELSE} const NoBeacon = True; {$IFEND}\n\
             implementation end.";
        let (_, meta) = session
            .parse_buffer(directory.join("Editor_unsaved.pas"), source)
            .unwrap();
        let meta = meta.expect("buffer meta");

        let names: Vec<String> = meta
            .interface()
            .symbols
            .iter()
            .map(|symbol| crate::globals::resolve(symbol.key).to_string())
            .collect();
        assert!(
            names.iter().any(|name| name == "SAWBEACON"),
            "a RESIDENT cross-unit Declared must resolve (True branch): {names:?}"
        );
        assert!(
            !names.iter().any(|name| name == "NOBEACON"),
            "resident Beacon must not fall to the else branch: {names:?}"
        );
    }

    #[test]
    fn sweep_plan_rebuilds_index() {
        let directory = temp_directory("sweep_rebuild");
        let stays = directory.join("Stays.pas");
        let goes = directory.join("Goes.pas");
        std::fs::write(&stays, "unit Stays;").unwrap();
        std::fs::write(&goes, "unit Goes;").unwrap();

        let mut session =
            ProjectSession::from_parts(test_context(), store_in(&directory), Duration::from_secs(300));
        insert_artifact(&session, "STAYS", &stays);
        let goes_unit = insert_artifact(&session, "GOES", &goes);

        std::fs::write(&goes, "unit Goes; // changed").unwrap();
        let report = session.apply_plan(&InvalidationPlan::FullSweep { changed_files: 99 });
        assert_eq!(report.invalidated_units, 1);
        assert!(session.context.unit_cache.get(goes_unit).is_none());
        // index rebuilt: the dropped unit no longer maps from its path
        assert!(session.index.units_for(&goes).is_empty());
        assert_eq!(session.index.units_for(&stays).len(), 1);
    }

    // ─── Deliverable A: semantic_tokens (task 13) ───────────────────────────
    //
    // The never-a-wrong-answer discipline at the highlight boundary: a token is
    // emitted only when the classification is CERTAIN. These tests prove: lexical
    // precision (keyword/comment/string/number/directive/operator); declaration/
    // member/parameter NAMES get their kind + declaration modifier; a KNOWN usage
    // is classified; an UNKNOWN identifier usage is OMITTED (never a wrong color).

    use crate::query::{SemanticKind, SemanticModifiers};

    /// The classified span's source text (via the arena), for readable asserts.
    fn token_text<'a>(session: &'a ProjectSession, token: &crate::query::SemanticToken) -> &'a str {
        session.arena().location_text(token.location)
    }

    /// Find the (first) token whose source text equals `text`.
    fn find_token<'a>(
        session: &ProjectSession,
        tokens: &'a [crate::query::SemanticToken],
        text: &str,
    ) -> Option<&'a crate::query::SemanticToken> {
        tokens.iter().find(|token| token_text(session, token) == text)
    }

    #[test]
    fn semantic_tokens_lexical_are_precise() {
        let directory = temp_directory("sem_lexical");
        std::fs::write(
            directory.join("Lexy.pas"),
            "unit Lexy;\ninterface\n{$DEFINE FOO}\n\
             { a comment }\n\
             const Answer = 42;\n\
             const Greeting = 'hello';\n\
             implementation\nend.",
        )
        .unwrap();

        let mut session = query_session(&directory);
        session.parse_source_file(directory.join("Lexy.pas")).unwrap();
        let key = session.context.intern_key("LEXY");
        let tokens = session.semantic_tokens(key);
        assert!(!tokens.is_empty());

        // A reserved word → Keyword.
        assert_eq!(find_token(&session, &tokens, "unit").unwrap().token_type, SemanticKind::Keyword);
        assert_eq!(find_token(&session, &tokens, "const").unwrap().token_type, SemanticKind::Keyword);
        // A block comment → Comment.
        assert_eq!(
            find_token(&session, &tokens, "{ a comment }").unwrap().token_type,
            SemanticKind::Comment
        );
        // A `{$…}` directive → Macro.
        assert_eq!(
            find_token(&session, &tokens, "{$DEFINE FOO}").unwrap().token_type,
            SemanticKind::Macro
        );
        // An int literal → Number; a string literal → String.
        assert_eq!(find_token(&session, &tokens, "42").unwrap().token_type, SemanticKind::Number);
        assert_eq!(
            find_token(&session, &tokens, "'hello'").unwrap().token_type,
            SemanticKind::String
        );
        // An operator → Operator (the `=` in a const decl).
        assert_eq!(find_token(&session, &tokens, "=").unwrap().token_type, SemanticKind::Operator);
        // No token ever spans trivia (whitespace/newline produce nothing).
        assert!(tokens.iter().all(|token| !token_text(&session, token).trim().is_empty()));
    }

    #[test]
    fn semantic_tokens_declarations_get_kind_and_declaration_modifier() {
        let directory = temp_directory("sem_decls");
        std::fs::write(
            directory.join("Decl.pas"),
            "unit Decl;\ninterface\n\
             type TThing = class\n  FValue: Integer;\n  procedure Go(const Amount: Integer);\n\
             property Value: Integer read FValue;\nend;\n\
             type IFace = interface\n  procedure Ping;\nend;\n\
             type TColor = (clRed, clBlue);\n\
             const MaxThings = 3;\n\
             var Total: Integer;\n\
             implementation\nend.",
        )
        .unwrap();

        let mut session = query_session(&directory);
        session.parse_source_file(directory.join("Decl.pas")).unwrap();
        let key = session.context.intern_key("DECL");
        let tokens = session.semantic_tokens(key);

        let declaration = SemanticModifiers::DECLARATION;
        // A class type NAME → Class + declaration.
        let thing = find_token(&session, &tokens, "TThing").unwrap();
        assert_eq!(thing.token_type, SemanticKind::Class);
        assert!(thing.modifiers.contains(declaration));
        // An interface type NAME → Interface + declaration.
        let iface = find_token(&session, &tokens, "IFace").unwrap();
        assert_eq!(iface.token_type, SemanticKind::Interface);
        assert!(iface.modifiers.contains(declaration));
        // An enum type NAME → Enum; its members → EnumMember.
        assert_eq!(find_token(&session, &tokens, "TColor").unwrap().token_type, SemanticKind::Enum);
        assert_eq!(
            find_token(&session, &tokens, "clRed").unwrap().token_type,
            SemanticKind::EnumMember
        );
        // A field NAME → Field; a method NAME → Method; a parameter → Parameter.
        assert_eq!(find_token(&session, &tokens, "FValue").unwrap().token_type, SemanticKind::Field);
        let go = find_token(&session, &tokens, "Go").unwrap();
        assert_eq!(go.token_type, SemanticKind::Method);
        assert!(go.modifiers.contains(declaration));
        let amount = find_token(&session, &tokens, "Amount").unwrap();
        assert_eq!(amount.token_type, SemanticKind::Parameter);
        assert!(amount.modifiers.contains(declaration));
        // A property NAME → Property.
        assert_eq!(find_token(&session, &tokens, "Value").unwrap().token_type, SemanticKind::Property);
        // A const → Constant; a var → Variable.
        assert_eq!(
            find_token(&session, &tokens, "MaxThings").unwrap().token_type,
            SemanticKind::Constant
        );
        assert_eq!(find_token(&session, &tokens, "Total").unwrap().token_type, SemanticKind::Variable);
        // The unit's own header name → Namespace.
        assert_eq!(find_token(&session, &tokens, "Decl").unwrap().token_type, SemanticKind::Namespace);
    }

    #[test]
    fn semantic_tokens_known_usage_classified_unknown_omitted() {
        let directory = temp_directory("sem_usage");
        std::fs::write(
            directory.join("Models.pas"),
            "unit Models;\ninterface\n\
             type TUser = class\n  Name: string;\nend;\n\
             implementation\nend.",
        )
        .unwrap();
        std::fs::write(
            directory.join("Client.pas"),
            "unit Client;\ninterface\nuses Models;\n\
             type TManager = class\n  Boss: TUser;\n  Ghost: TUnknownType;\nend;\n\
             implementation\nend.",
        )
        .unwrap();

        let mut session = query_session(&directory);
        session.parse_source_file(directory.join("Client.pas")).unwrap();
        let key = session.context.intern_key("CLIENT");
        let tokens = session.semantic_tokens(key);

        // A KNOWN cross-unit type USAGE (`Boss: TUser`) → classified as Type (the
        // coarse-but-correct kind for a cross-unit type). The `TUser` here is the
        // field-type usage, NOT its declaration (declared in Models).
        let user_usage = find_token(&session, &tokens, "TUser").unwrap();
        assert_eq!(user_usage.token_type, SemanticKind::Type);
        assert!(
            !user_usage.modifiers.contains(SemanticModifiers::DECLARATION),
            "a cross-unit usage is not a declaration site"
        );
        // The imported unit name in the `uses` clause → Namespace.
        assert_eq!(
            find_token(&session, &tokens, "Models").unwrap().token_type,
            SemanticKind::Namespace
        );
        // An UNKNOWN identifier USAGE (`TUnknownType`, resolves to nothing) is
        // OMITTED entirely — never given a (wrong) class. The editor's TextMate
        // color shows instead.
        assert!(
            find_token(&session, &tokens, "TUnknownType").is_none(),
            "an unresolved identifier usage must be omitted, not colored: {:?}",
            tokens
                .iter()
                .map(|token| (token_text(&session, token), token.token_type))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn semantic_tokens_empty_for_uncached_unit() {
        let directory = temp_directory("sem_empty");
        let session = query_session(&directory);
        // Nothing parsed → the unit is not cached → no tokens (never a panic).
        let key = session.context.intern_key("NOPE");
        assert!(session.semantic_tokens(key).is_empty());
    }
}

