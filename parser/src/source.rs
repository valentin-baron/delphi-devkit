//! Source buffer storage. One [`SourceArena`] per parse run; every loaded
//! file (units, `{$I}` includes) lives here for the arena's lifetime, so
//! tokens stay payload-free — text is recovered from `(FileId, Span)`.
//!
//! Growable through `&self` (elsa) with stable addresses: a `&str` handed out
//! for one file survives later loads, which is what lets a logos lexer borrow
//! from the arena while the preprocessor keeps loading includes.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    },
};

use crate::meta::{CodeLocation, FileId, Span};

use windows::Win32::Globalization::{MB_PRECOMPOSED, CP_ACP, MultiByteToWideChar};

#[derive(Debug, Clone)]
pub struct FileReadError {
    pub path: PathBuf,
    pub message: String,
}

struct SourceEntry {
    path: PathBuf,
    /// Lazily materialized AND CLEARABLE storage for a DISK file's decoded text.
    /// `None` for a merely-`register`ed (never-read) file, a cleared entry, or a
    /// virtual buffer (which keeps its text in [`Self::virtual_content`]).
    ///
    /// DISK entries fill this on first [`SourceArena::content`] access (reading
    /// the file) and a [`SourceArena::trim_disk_content`] can DROP the `Box<str>`
    /// again — a later `content` access RE-READS from disk (the entry keeps its
    /// real path). This is the Task-19 bound: a disk file's text is resident only
    /// while it is in the working set, re-read on demand otherwise. The `Mutex`
    /// makes fill/clear a short critical section and keeps the entry `Sync`; the
    /// handed-out `&str` stays valid until the next between-checkpoints clear (see
    /// [`disk_content_ref`]'s SAFETY note — the same discipline as virtual).
    ///
    /// VIRTUAL entries do NOT use this field — their content lives in
    /// [`Self::virtual_content`], REPLACED (prior box freed) on re-parse and NEVER
    /// trimmed (a virtual path can't be re-read → data loss). `is_virtual`
    /// distinguishes the two.
    content: Mutex<Option<Box<str>>>,
    /// The RAW on-disk bytes this file was read from (before any BOM/UTF-16/ANSI
    /// decoding), retained so hash stamps can be taken without a second disk
    /// read (no TOCTOU window — L15). Populated together with `content` when a
    /// disk file is read, and CLEARED together with it by a trim (re-read on the
    /// next `raw_bytes`/`content`, giving the same bytes unless the file changed
    /// — hash-validation already handles change). `None` for a virtual buffer (no
    /// disk bytes) — the stamp then falls back to hashing the decoded content,
    /// which by design never matches a disk read and drops the entry as stale on
    /// load (#21/#25).
    raw: Mutex<Option<Box<[u8]>>>,
    /// `true` for a virtual (in-memory editor) buffer, `false` for a disk file.
    /// A virtual entry reads/writes [`Self::virtual_content`]; a disk entry uses
    /// [`Self::content`]/[`Self::raw`]. This flag is fixed at creation and never
    /// changes. Only DISK entries are ever trimmed.
    is_virtual: bool,
    /// REPLACEABLE storage for a virtual buffer's decoded content. `None` for a
    /// disk entry. The `Box<str>` is owned here so [`SourceArena::set_virtual`]
    /// can swap in the new text and DROP the prior box — bounding memory to one
    /// live copy per open document (Task-15 part 2). The mutex makes the swap a
    /// short critical section and keeps the entry `Sync`; the handed-out `&str`
    /// stays valid until the next swap under the caller's serialization (see
    /// [`SourceArena::set_virtual`]'s soundness note).
    virtual_content: Mutex<Option<Box<str>>>,
    /// Monotonic last-access tick for a DISK entry's LRU trim: bumped from the
    /// arena's global counter each time [`SourceArena::content`] returns this
    /// entry's disk text (fresh read OR cached hit). `trim_disk_content` evicts
    /// the entries with the SMALLEST ticks first (least recently accessed).
    /// Never consulted for virtual entries (they are not trimmable). `0` until
    /// the first access.
    last_access: AtomicU64,
}

/// Append-only store of source buffers, shared across a parse run.
/// Loading the same file twice (same canonical path) returns the same
/// [`FileId`] without re-reading.
#[derive(Default)]
pub struct SourceArena {
    files: elsa::sync::FrozenVec<Box<SourceEntry>>,
    /// Canonicalized path → id. Windows canonicalization resolves case and
    /// 8.3 names, so `FOO.PAS` and `foo.pas` dedup to one entry.
    ids_by_path: Mutex<HashMap<PathBuf, FileId>>,
    /// Display path (as-given, NOT canonicalized) → virtual FileId. A virtual
    /// buffer's path is a display-only name that may not exist on disk, so it
    /// cannot canonicalize; this map dedups virtual buffers by that exact path
    /// so an editor's repeated re-parses of one open document REUSE a single
    /// arena entry (its content replaced in place) instead of appending forever.
    /// Kept SEPARATE from `ids_by_path` so disk-file dedup-by-canonical-path is
    /// unchanged (#21/#25: a virtual id never enters `ids_by_path`, never
    /// canonicalizes, still fails `register` on load → never persisted).
    virtual_ids_by_path: Mutex<HashMap<PathBuf, FileId>>,
    /// Monotonic access clock for the disk-content LRU (Task-19). Every
    /// [`Self::content`] hit on a DISK entry stamps that entry's `last_access`
    /// with the next value here; `trim_disk_content` evicts the smallest ticks
    /// first. A plain `AtomicU64` — no wraparound concern in any realistic
    /// process lifetime (2^64 accesses).
    access_clock: AtomicU64,
}

impl SourceArena {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load a file from disk (BOM-aware: UTF-8, UTF-16 LE, else Windows ANSI
    /// code page). Idempotent per canonical path. Content is read eagerly.
    pub fn load(&self, path: impl AsRef<Path>) -> Result<FileId, FileReadError> {
        let file = self.register(path)?;
        self.content(file)?;
        Ok(file)
    }

    /// Issue a [`FileId`] for a path WITHOUT reading the file. The content is
    /// read lazily on first [`Self::content`] access. Used when re-attaching
    /// persisted cache data whose code locations reference files that may
    /// never be opened this session.
    pub fn register(&self, path: impl AsRef<Path>) -> Result<FileId, FileReadError> {
        let path = path.as_ref();
        let canonical = path.canonicalize().map_err(|error| FileReadError {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;

        let mut ids_by_path = self.ids_by_path.lock().unwrap();
        if let Some(&file) = ids_by_path.get(&canonical) {
            return Ok(file);
        }
        let file = self.push_entry(canonical.clone(), None);
        ids_by_path.insert(canonical, file);
        Ok(file)
    }

    /// Register an in-memory buffer (tests, unsaved editor content). The
    /// `path` is a display name only — no deduplication, no disk access.
    ///
    /// NOTE: this appends a FRESH entry every call and never frees it — fine for
    /// tests, but the LSP editor path must use [`Self::set_virtual`] so repeated
    /// re-parses of one open document do not grow the process-global arena
    /// without bound (Task-15).
    pub fn insert_virtual(&self, path: impl Into<PathBuf>, content: impl Into<String>) -> FileId {
        self.push_entry(path.into(), Some(content.into()))
    }

    /// Register OR RE-REGISTER an in-memory (unsaved editor) buffer for `path`,
    /// bounding the arena to ONE virtual entry per open-document path. The first
    /// call for a path appends a virtual entry and returns its stable `FileId`;
    /// every later call for the SAME display path REUSES that `FileId` and
    /// REPLACES its content, DROPPING the prior `Box<str>` — so an editor that
    /// re-parses on every keystroke keeps exactly one live copy per document
    /// rather than appending a full-file copy forever (the OOM the old
    /// `insert_virtual`-per-parse caused).
    ///
    /// Dedup is by the EXACT display path (not canonicalized): a virtual path is
    /// a display-only name that may not exist on disk, so it cannot canonicalize.
    /// The virtual id therefore never enters `ids_by_path` and still fails
    /// `register` on load → never persisted (#21/#25 preserved).
    ///
    /// SOUNDNESS (stable-`&str` during a parse + span-provenance):
    /// - A `&str` from [`Self::content`]/[`Self::text`] for a virtual FileId
    ///   borrows the heap allocation the entry's current `Box<str>` owns. That
    ///   allocation is freed ONLY here, when the content is replaced.
    /// - The ONLY reader of a virtual FileId's content is the owning session —
    ///   its synchronous parse and its query methods, all serialized by the LSP
    ///   session lock. A parse borrows the content, runs to completion, and
    ///   drops every borrow before returning; the next `parse_buffer` (which is
    ///   what calls `set_virtual`) cannot start until then. So a replace happens
    ///   strictly BETWEEN parses, when no `&str` into the prior content is live.
    /// - `UnitMeta` stores `(FileId, Span)`, never a borrowed `&str` (tokens are
    ///   payload-free), so a cached meta never holds a dangling reference across
    ///   a replace. Worker threads (moka eviction, import loads) read OTHER files
    ///   (disk units), never this virtual buffer.
    /// - Span-provenance: `set_virtual` runs at the START of `parse_buffer`, and
    ///   the meta that parse produces indexes THIS newly-stored content. So after
    ///   the re-parse, `content(file)` returns exactly the text the new meta's
    ///   spans index — replacement and re-parse are atomic per document version.
    pub fn set_virtual(&self, path: impl Into<PathBuf>, content: impl Into<String>) -> FileId {
        let path = path.into();
        let content = content.into().into_boxed_str();
        let mut virtual_ids = self.virtual_ids_by_path.lock().unwrap();
        if let Some(&file) = virtual_ids.get(&path) {
            // Reuse the entry: swap in the new content and DROP the prior box.
            let entry = self.entry(file);
            let mut slot = entry.virtual_content.lock().unwrap();
            *slot = Some(content); // the previous Box<str> is dropped here → freed
            return file;
        }
        // First time this document is parsed: append one virtual entry.
        let file = self.push_entry(path.clone(), Some(String::new()));
        // push_entry stored an empty placeholder; write the real content.
        *self.entry(file).virtual_content.lock().unwrap() = Some(content);
        virtual_ids.insert(path, file);
        file
    }

    /// Release the content of the virtual buffer registered for `path`, freeing
    /// its `Box<str>` (Task-15 `did_close` cleanup). The `FileId` and its map
    /// entry are RETAINED so a reopen re-registers the SAME id (stable ids), but
    /// the memory the content held is returned immediately. A subsequent
    /// `content`/`loaded_content` on the freed id errors/panics respectively
    /// until the document is re-parsed via `set_virtual`. No-op for a path that
    /// was never a virtual buffer.
    pub fn free_virtual(&self, path: impl AsRef<Path>) {
        let virtual_ids = self.virtual_ids_by_path.lock().unwrap();
        if let Some(&file) = virtual_ids.get(path.as_ref()) {
            *self.entry(file).virtual_content.lock().unwrap() = None;
        }
    }

    /// The number of virtual (in-memory editor) buffers the arena currently
    /// tracks — one per open-document path that has been `set_virtual`-parsed.
    /// Bounded regardless of how many times each document is re-parsed (that is
    /// the Task-15 memory-bound property this exposes for tests).
    pub fn virtual_buffer_count(&self) -> usize {
        self.virtual_ids_by_path.lock().unwrap().len()
    }

    /// Bytes of DISK-file text + raw bytes currently RESIDENT in the arena — the
    /// sum, over every disk entry, of its materialized `content` length (UTF-8
    /// bytes) plus its retained `raw` length. Virtual entries are excluded (they
    /// are the Task-15 bound, not this one). This is the quantity
    /// [`Self::trim_disk_content`] shrinks; exposed for the bound test/metrics.
    pub fn resident_disk_bytes(&self) -> usize {
        let mut total = 0;
        for index in 0..self.files.len() {
            let entry = self.files.get(index).unwrap();
            if entry.is_virtual {
                continue;
            }
            total += entry_resident_bytes(entry);
        }
        total
    }

    /// The number of DISK entries whose content is currently resident (read and
    /// not yet trimmed). For the bound test to assert entries were cleared.
    pub fn resident_disk_entry_count(&self) -> usize {
        let mut count = 0;
        for index in 0..self.files.len() {
            let entry = self.files.get(index).unwrap();
            if entry.is_virtual {
                continue;
            }
            if entry.content.lock().unwrap().is_some() {
                count += 1;
            }
        }
        count
    }

    /// LRU-evict DISK-file content (decoded text + raw bytes) until the total
    /// resident disk bytes is at most `cap_bytes`, dropping the LEAST-recently-
    /// accessed entries first. Returns the number of bytes freed. Virtual entries
    /// are NEVER touched (their path is display-only and cannot be re-read — a
    /// clear would be irreversible data loss; only re-readable disk entries are
    /// trimmable). A cleared disk entry re-reads from disk on the next
    /// [`Self::content`]/[`Self::raw_bytes`] access.
    ///
    /// SOUNDNESS (Task-19 — the adversarial-review target; mirrors the Task-15
    /// SAFETY note on `set_virtual`/`virtual_content_ref`):
    ///
    /// Clearing an entry drops its `Box<str>`/`Box<[u8]>`; any outstanding
    /// lifetime-extended `&str`/`&[u8]` into it (handed out by `content`/`text`/
    /// `try_text`/`raw_bytes`, all via `disk_content_ref`/`disk_raw_ref`, whose
    /// pointers outlive the mutex guard) would DANGLE. This is sound ONLY because
    /// this method is called EXCLUSIVELY at a SAFE CHECKPOINT where NO borrow into
    /// the arena is live:
    /// - Every `content`/`text`/`try_text`/`raw_bytes` caller consumes the
    ///   returned reference (copies to owned, or uses it) WITHIN a single
    ///   synchronous parse or query, and drops it before that parse/query returns.
    /// - The LSP session serializes ALL parses/queries under one `blocking_lock()`
    ///   (see `server::session`). `trim_disk_content` is invoked via
    ///   `ProjectSession::trim_arena` at the END of a blocking parse/query section
    ///   — after the owned LSP results are built, still under the SAME session
    ///   lock, before it releases. So a trim runs strictly BETWEEN parses/queries,
    ///   when every arena borrow from the just-finished one is already dropped and
    ///   the next one cannot have started (it needs the lock this trim holds).
    /// - It must therefore NEVER be called reactively inside `content` (a borrow
    ///   from an earlier file in the SAME parse chain may still be live — trimming
    ///   it would UAF). The only call site is the post-section checkpoint.
    /// - No other thread reads or clears disk content in a racing way: the moka
    ///   eviction persister serializes bincoded metas (paths, never arena text)
    ///   and the import loader reads content only during a parse (under the lock).
    ///   The per-entry `Mutex` on `content`/`raw` further makes an individual
    ///   fill/clear atomic; a concurrent `content` on a DIFFERENT entry is fine.
    pub fn trim_disk_content(&self, cap_bytes: usize) -> usize {
        // Snapshot (index, last_access, resident_bytes) for every resident disk
        // entry, then evict coldest-first until at or below the cap.
        let mut resident: Vec<(usize, u64, usize)> = Vec::new();
        let mut total: usize = 0;
        for index in 0..self.files.len() {
            let entry = self.files.get(index).unwrap();
            if entry.is_virtual {
                continue;
            }
            let bytes = entry_resident_bytes(entry);
            if bytes == 0 {
                continue; // already cleared / never read
            }
            total += bytes;
            resident.push((index, entry.last_access.load(Ordering::Relaxed), bytes));
        }
        if total <= cap_bytes {
            return 0;
        }
        // Coldest (smallest last_access) first.
        resident.sort_by_key(|&(_, tick, _)| tick);

        let mut freed = 0;
        for (index, _, bytes) in resident {
            if total <= cap_bytes {
                break;
            }
            let entry = self.files.get(index).unwrap();
            // Drop the boxes → free the heap allocations. Safe: no live borrow
            // into them (checkpoint discipline, above).
            *entry.content.lock().unwrap() = None;
            *entry.raw.lock().unwrap() = None;
            total -= bytes;
            freed += bytes;
        }
        freed
    }

    pub fn path(&self, file: FileId) -> &Path {
        &self.entry(file).path
    }

    /// Non-panicking [`path`](Self::path): returns `None` when `file` was not
    /// issued by this arena (foreign or out-of-range index). The persistence
    /// layer needs this — a `FileId` reaching `save` from a non-global arena
    /// must serialize to an error, never panic (M2).
    pub fn try_path(&self, file: FileId) -> Option<&Path> {
        self.files.get(file.0 as usize).map(|entry| entry.path.as_path())
    }

    /// Full text of a file, reading it from disk on first access if the entry
    /// was only registered. The reference stays valid for the arena's whole
    /// lifetime — later loads never move existing buffers. The raw on-disk bytes
    /// are retained alongside the decoded text (see [`Self::raw_bytes`]) so a
    /// hash stamp needs no second read.
    pub fn content(&self, file: FileId) -> Result<&str, FileReadError> {
        let entry = self.entry(file);
        // Virtual buffer: return the CURRENT replaceable content. The `&str`
        // borrows the heap allocation the entry's `Box<str>` owns; that
        // allocation stays valid until the next `set_virtual` for this FileId,
        // which only happens BETWEEN parses under the session's serialization
        // (see `set_virtual`'s soundness note). Reading a virtual entry whose
        // content was freed (`did_close`) is an error, never a panic. Virtual
        // entries are NEVER trimmed, so no LRU tick is taken for them.
        if entry.is_virtual {
            return virtual_content_ref(entry).ok_or_else(|| FileReadError {
                path: entry.path.clone(),
                message: "virtual buffer content was released (closed document)".into(),
            });
        }

        // DISK entry. Stamp the LRU clock: this access makes the entry the
        // most-recently-used, so a concurrent/subsequent trim evicts colder
        // entries first. Done for a cache HIT and a fresh READ alike.
        entry
            .last_access
            .store(self.access_clock.fetch_add(1, Ordering::Relaxed) + 1, Ordering::Relaxed);

        // Fast path: content already resident. Return a lifetime-extended `&str`
        // into the entry's `Box<str>` (stable heap address). Sound because the
        // box is only dropped by `trim_disk_content`, which runs at a checkpoint
        // with no live arena borrow (see `disk_content_ref`'s SAFETY note).
        if let Some(content) = disk_content_ref(entry) {
            return Ok(content);
        }

        // Cleared or never-read: re-read from disk on demand (the entry keeps
        // its real, re-readable path). A read failure surfaces as `FileReadError`
        // (never a panic) — the existing FileReadError path, unchanged.
        let (raw, decoded) = read_source_file(&entry.path)?;
        // Store raw FIRST so any reader that observes `content` also sees `raw`
        // (matches the prior write-once ordering). Under the session lock these
        // fills do not race a trim of the SAME entry.
        *entry.raw.lock().unwrap() = Some(raw.into_boxed_slice());
        *entry.content.lock().unwrap() = Some(decoded.into_boxed_str());
        // Return the just-stored content as a lifetime-extended `&str`.
        Ok(disk_content_ref(entry).expect("content just stored is present"))
    }

    /// The RAW on-disk bytes a disk file was read from, before decoding. `None`
    /// for a virtual buffer (no disk bytes). Used by the pipeline to hash a
    /// file's on-disk bytes for its validity stamp; the bytes are byte-identical
    /// to what `std::fs::read` would return, so the resulting hash matches
    /// [`crate::unit_cache::hash_file`] and existing snapshots still validate.
    ///
    /// For a DISK file whose bytes are RESIDENT (materialized and not trimmed)
    /// this returns them with no disk touch (the L15 no-re-read property on the
    /// hot path). If the bytes were TRIMMED (or never read), it re-reads them
    /// from disk on demand and re-populates the entry — so a stamp taken after a
    /// trim still hashes the exact on-disk bytes rather than silently falling
    /// through to decoded content. A disk-read failure surfaces as `None` (the
    /// caller then hashes decoded content as a defensive fallback — see
    /// `stamp_file`), never a panic. Also stamps the LRU clock, since a re-read
    /// materializes bytes into the working set exactly like `content`.
    pub fn raw_bytes(&self, file: FileId) -> Option<&[u8]> {
        let entry = self.entry(file);
        // Virtual buffer: no disk bytes, ever.
        if entry.is_virtual {
            return None;
        }
        // Resident: hand back a lifetime-extended slice into the entry's
        // `Box<[u8]>` (stable heap address), same discipline as `content`.
        if let Some(bytes) = disk_raw_ref(entry) {
            entry
                .last_access
                .store(self.access_clock.fetch_add(1, Ordering::Relaxed) + 1, Ordering::Relaxed);
            return Some(bytes);
        }
        // Trimmed or never read: re-read from disk. A read failure → None (the
        // stamp's decoded-content fallback handles it), never a panic.
        let (raw, decoded) = read_source_file(&entry.path).ok()?;
        entry
            .last_access
            .store(self.access_clock.fetch_add(1, Ordering::Relaxed) + 1, Ordering::Relaxed);
        *entry.raw.lock().unwrap() = Some(raw.into_boxed_slice());
        *entry.content.lock().unwrap() = Some(decoded.into_boxed_str());
        Some(disk_raw_ref(entry).expect("raw just stored is present"))
    }

    /// Content of a file whose text a caller already knows exists (a span for
    /// this file was produced, so it was lexed at least once). This is the parse
    /// hot-path accessor.
    ///
    /// For a DISK file this now transparently RE-READS if the text was trimmed
    /// since it was last lexed (Task-19): a `(FileId, Span)` outlives the resident
    /// text, and `text`/`loaded_content` must still resolve it. The re-read gives
    /// byte-identical content unless the file changed on disk (in which case a
    /// span may fall out of bounds — the panic-free [`Self::try_text`] is the
    /// accessor for locations that may be stale; `text` stays the guaranteed-
    /// materialized hot path). Panics ONLY when the disk file genuinely cannot be
    /// read (deleted mid-session) or a virtual buffer's content was released —
    /// the same "should never happen on the hot path" contract as before, now
    /// covering the trim case by re-reading rather than panicking on it.
    pub fn loaded_content(&self, file: FileId) -> &str {
        let entry = self.entry(file);
        if entry.is_virtual {
            return virtual_content_ref(entry)
                .expect("virtual buffer content requested after it was released");
        }
        // Disk file: `content` re-reads a trimmed/never-read entry on demand.
        self.content(file)
            .expect("disk file content requested but the file could not be read")
    }

    /// Text under a span of a file. The file must be materialized (spans only
    /// exist for lexed content); a disk file trimmed since lexing is re-read by
    /// [`Self::loaded_content`].
    pub fn text(&self, file: FileId, span: Span) -> &str {
        &self.loaded_content(file)[span.start as usize..span.end as usize]
    }

    /// Text under a [`CodeLocation`].
    pub fn location_text(&self, location: CodeLocation) -> &str {
        self.text(location.file, location.span)
    }

    /// Panic-free text accessor for callers that may hold a location into a
    /// merely-`register`ed (not yet lexed) file — e.g. a symbol restored from a
    /// cache snapshot. Lazily loads the file and bounds-checks the span,
    /// returning an error instead of panicking. This is the accessor for the
    /// public / LSP surface; [`text`](Self::text) stays for the parse hot path
    /// where materialization is guaranteed.
    pub fn try_text(&self, file: FileId, span: Span) -> Result<&str, FileReadError> {
        let content = self.content(file)?;
        content
            .get(span.start as usize..span.end as usize)
            .ok_or_else(|| FileReadError {
                path: self.path(file).to_path_buf(),
                message: "span out of bounds for current file content (stale location)".into(),
            })
    }

    /// Panic-free [`location_text`](Self::location_text). See [`try_text`](Self::try_text).
    pub fn try_location_text(&self, location: CodeLocation) -> Result<&str, FileReadError> {
        self.try_text(location.file, location.span)
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    fn push_entry(&self, path: PathBuf, content: Option<String>) -> FileId {
        // A pushed-with-content entry is a virtual buffer (no disk bytes): `raw`
        // stays empty, so `raw_bytes` returns None and the stamp falls back to
        // hashing decoded content (the intended virtual-buffer behaviour).
        let is_virtual = content.is_some();
        // Disk entries keep their content in `content` (clearable/re-readable);
        // virtual entries keep it in `virtual_content` (replaceable, freed on
        // re-parse). A disk entry starts empty (filled on first `content`).
        let virtual_content = Mutex::new(content.map(String::into_boxed_str));
        let index = self.files.push_get_index(Box::new(SourceEntry {
            path,
            content: Mutex::new(None),
            raw: Mutex::new(None),
            is_virtual,
            virtual_content,
            last_access: AtomicU64::new(0),
        }));
        FileId(index as u32)
    }

    fn entry(&self, file: FileId) -> &SourceEntry {
        self.files
            .get(file.0 as usize)
            .expect("FileId not issued by this arena")
    }
}

/// The current text of a virtual entry as a `&str` borrowing the entry (whose
/// heap address is stable — it lives in a `Box` inside the append-only
/// `FrozenVec`). Returns `None` if the content was released (`free_virtual`).
///
/// SAFETY: the returned `&str` outlives the `MutexGuard` taken here. That is
/// sound under the arena's single-writer-between-parses discipline
/// ([`SourceArena::set_virtual`]): the `Box<str>` this points into is replaced
/// or freed ONLY between parses, when no borrow into it is live. The guard is
/// released immediately (it only protects the swap, not the read lifetime), and
/// the pointer stays valid until the next `set_virtual`/`free_virtual` for this
/// entry — exactly the contract `content` documents. We must not hold the guard
/// for the borrow's lifetime because the parse hot path borrows content for the
/// whole parse while other files (never this one) may be inserted concurrently.
fn virtual_content_ref(entry: &SourceEntry) -> Option<&str> {
    let guard = entry.virtual_content.lock().unwrap();
    let text: &str = guard.as_deref()?;
    // Extend the borrow from the guard to the entry. The bytes live in the
    // `Box<str>`'s heap allocation, not in the guard/Option, and are stable
    // until the next between-parses replace. See the function's SAFETY note.
    let extended: &str = unsafe { std::mem::transmute::<&str, &str>(text) };
    drop(guard);
    Some(extended)
}

/// The resident decoded text of a DISK entry as a `&str` borrowing the entry
/// (whose heap address is stable — a `Box` inside the append-only `FrozenVec`).
/// `None` if the content was trimmed or never read.
///
/// SAFETY: identical discipline to [`virtual_content_ref`] — the returned `&str`
/// outlives the `MutexGuard` taken here. The bytes live in the `Box<str>`'s heap
/// allocation, stable until [`SourceArena::trim_disk_content`] clears it, which
/// runs ONLY at a checkpoint with no live arena borrow (see
/// `trim_disk_content`'s SOUNDNESS note). The guard is released immediately (it
/// protects the fill/clear swap, not the read lifetime); we must not hold it for
/// the borrow's whole life because the parse hot path borrows content across the
/// entire parse while OTHER files are read/filled concurrently.
fn disk_content_ref(entry: &SourceEntry) -> Option<&str> {
    let guard = entry.content.lock().unwrap();
    let text: &str = guard.as_deref()?;
    let extended: &str = unsafe { std::mem::transmute::<&str, &str>(text) };
    drop(guard);
    Some(extended)
}

/// The resident raw on-disk bytes of a DISK entry as a `&[u8]` borrowing the
/// entry. `None` if trimmed or never read. Same SAFETY discipline as
/// [`disk_content_ref`] — the slice outlives the guard; the `Box<[u8]>` heap
/// allocation is stable until `trim_disk_content` clears it at a checkpoint.
fn disk_raw_ref(entry: &SourceEntry) -> Option<&[u8]> {
    let guard = entry.raw.lock().unwrap();
    let bytes: &[u8] = guard.as_deref()?;
    let extended: &[u8] = unsafe { std::mem::transmute::<&[u8], &[u8]>(bytes) };
    drop(guard);
    Some(extended)
}

/// The bytes a DISK entry currently keeps resident: decoded content length +
/// retained raw length. Zero when both are cleared. Only meaningful for disk
/// entries (a virtual entry's storage is the Task-15 bound, excluded here).
fn entry_resident_bytes(entry: &SourceEntry) -> usize {
    let content = entry
        .content
        .lock()
        .unwrap()
        .as_ref()
        .map(|text| text.len())
        .unwrap_or(0);
    let raw = entry
        .raw
        .lock()
        .unwrap()
        .as_ref()
        .map(|bytes| bytes.len())
        .unwrap_or(0);
    content + raw
}

impl std::fmt::Debug for SourceArena {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut map = formatter.debug_map();
        for index in 0..self.files.len() {
            let entry = self.files.get(index).unwrap();
            map.entry(&index, &entry.path.display());
        }
        map.finish()
    }
}

// ─── File reading with Delphi-typical encodings ──────────────────────────

/// Read a file's RAW on-disk bytes and its DECODED UTF-8 text. Both are
/// returned so the arena can retain the raw bytes for hash stamping (L15)
/// without a second read. The decoding branches are unchanged; only the raw
/// bytes are now handed back alongside the string.
fn read_source_file(path: &Path) -> Result<(Vec<u8>, String), FileReadError> {
    let error = |message: String| FileReadError {
        path: path.to_path_buf(),
        message,
    };

    let bytes = std::fs::read(path).map_err(|e| error(e.to_string()))?;
    let decoded = decode_source_bytes(&bytes, &error)?;
    Ok((bytes, decoded))
}

/// Decode already-read source bytes to UTF-8 (BOM-aware: UTF-8, UTF-16 LE/BE,
/// else Windows ANSI). Split out of [`read_source_file`] so the raw bytes stay
/// owned by the caller for retention.
fn decode_source_bytes(
    bytes: &[u8],
    error: &impl Fn(String) -> FileReadError,
) -> Result<String, FileReadError> {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        // UTF-8 BOM
        String::from_utf8(rest.to_vec()).map_err(|e| error(e.to_string()))
    } else if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        // UTF-16 LE BOM
        if rest.len() % 2 != 0 {
            return Err(error("truncated UTF-16LE source (odd byte count)".into()));
        }
        let wide: Vec<u16> = rest
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        String::from_utf16(&wide).map_err(|e| error(e.to_string()))
    } else if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        // UTF-16 BE BOM. dcc rejects BE source, but decoding it correctly is
        // strictly better for the LSP than silently mojibake-ing it as ANSI.
        if rest.len() % 2 != 0 {
            return Err(error("truncated UTF-16BE source (odd byte count)".into()));
        }
        let wide: Vec<u16> = rest
            .chunks_exact(2)
            .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
            .collect();
        String::from_utf16(&wide).map_err(|e| error(e.to_string()))
    } else {
        // No BOM: assume the Windows ANSI code page, like the Delphi compiler.
        ansi_bytes_to_utf8(bytes).map_err(|e| error(e.to_string()))
    }
}

fn ansi_bytes_to_utf8(input: &[u8]) -> Result<String, std::io::Error> {
    if input.is_empty() {
        return Ok(String::new());
    }
    unsafe {
        let length = MultiByteToWideChar(CP_ACP, MB_PRECOMPOSED, input, None);
        if length == 0 {
            return Err(std::io::Error::last_os_error());
        }

        let mut wide = vec![0u16; length as usize];
        let written = MultiByteToWideChar(CP_ACP, MB_PRECOMPOSED, input, Some(&mut wide));
        if written == 0 {
            return Err(std::io::Error::last_os_error());
        }

        String::from_utf16(&wide)
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid UTF-16"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtual_files_and_spans() {
        let arena = SourceArena::new();
        let file = arena.insert_virtual("test.pas", "unit Test;");
        assert_eq!(arena.content(file).unwrap(), "unit Test;");
        assert_eq!(arena.text(file, Span::new(5, 9)), "Test");
        assert_eq!(arena.path(file), Path::new("test.pas"));
        // L15: a virtual buffer has no disk bytes → raw_bytes is None, so the
        // stamp falls back to hashing decoded content (never matches a disk
        // read, dropped as stale on load — the intended #21/#25 behaviour).
        assert!(arena.raw_bytes(file).is_none());
    }

    #[test]
    fn raw_bytes_retained_after_disk_read() {
        // L15: reading a disk file retains its RAW on-disk bytes byte-identical
        // to `std::fs::read`, so a hash stamp needs no second read.
        let directory = std::env::temp_dir().join("delphi_parser_raw_retain");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("Raw.pas");
        let bytes = b"unit Raw; // \xE4 high byte, ANSI".to_vec();
        std::fs::write(&path, &bytes).unwrap();

        let arena = SourceArena::new();
        let file = arena.register(&path).unwrap();
        // merely registered → nothing resident yet
        assert_eq!(entry_resident_bytes(arena.entry(file)), 0);
        // Task-19: `raw_bytes` on an un-materialized disk file re-reads on demand
        // (rather than returning None), so a stamp taken after a trim still hashes
        // the exact on-disk bytes. The bytes equal the on-disk bytes.
        assert_eq!(arena.raw_bytes(file).unwrap(), bytes.as_slice());
        // materialize the decoded content → raw bytes remain byte-identical.
        arena.content(file).unwrap();
        assert_eq!(arena.raw_bytes(file).unwrap(), bytes.as_slice());
    }

    #[test]
    fn set_virtual_bounds_the_arena_to_one_entry_per_path() {
        // Task-15 memory bound: re-parsing ONE document N times must keep the
        // arena at a single virtual entry (its content REPLACED, prior freed),
        // not N appended copies. Proven by a stable FileId, a bounded virtual
        // count, and `content` always returning the LATEST text with resolvable
        // spans — the exact property the old `insert_virtual`-per-parse violated.
        let arena = SourceArena::new();
        let path = "C:/editor/Editing.pas";

        let first = arena.set_virtual(path, "unit Editing; // v0".to_string());
        assert_eq!(arena.virtual_buffer_count(), 1);
        assert_eq!(arena.len(), 1);

        for version in 1..=1000 {
            let content = format!("unit Editing; // v{version} {}", "x".repeat(version));
            let file = arena.set_virtual(path, content.clone());
            // SAME FileId every time — the entry is reused, not appended.
            assert_eq!(file, first, "re-parse must reuse the virtual FileId");
            // The arena never grows: one virtual entry, one total file.
            assert_eq!(arena.virtual_buffer_count(), 1);
            assert_eq!(arena.len(), 1);
            // `content` returns the LATEST text (span-provenance), and a span
            // into it resolves to the expected substring.
            assert_eq!(arena.content(file).unwrap(), content);
            assert_eq!(arena.text(file, Span::new(0, 4)), "unit");
        }

        // A DIFFERENT path gets its own single entry (dedup is per-path).
        let other = arena.set_virtual("C:/editor/Other.pas", "unit Other;".to_string());
        assert_ne!(other, first);
        assert_eq!(arena.virtual_buffer_count(), 2);
        assert_eq!(arena.len(), 2);
    }

    #[test]
    fn free_virtual_releases_content_but_keeps_id_stable() {
        // did_close cleanup: freeing a virtual buffer releases its content
        // (subsequent `content` errors, never panics) while keeping the id so a
        // reopen re-registers the SAME FileId.
        let arena = SourceArena::new();
        let path = "C:/editor/Closable.pas";
        let file = arena.set_virtual(path, "unit Closable;".to_string());
        assert_eq!(arena.content(file).unwrap(), "unit Closable;");

        arena.free_virtual(path);
        // content is released: an error, not a panic (never-panic contract).
        assert!(arena.content(file).is_err());
        // the id and its map entry survive (count unchanged).
        assert_eq!(arena.virtual_buffer_count(), 1);

        // reopen re-registers the SAME id and restores content.
        let reopened = arena.set_virtual(path, "unit Closable; // reopened".to_string());
        assert_eq!(reopened, file, "reopen reuses the stable FileId");
        assert_eq!(arena.content(file).unwrap(), "unit Closable; // reopened");
    }

    /// Task-19 bound: parse many distinct large DISK units, then
    /// `trim_disk_content(small_cap)` drops resident disk bytes to at most the
    /// cap by LRU-evicting the coldest entries, and a cleared entry's
    /// `content`/`text(span)` RE-READS from disk with byte-identical text and
    /// resolvable spans. Virtual entries are never cleared by trim.
    #[test]
    fn trim_disk_content_bounds_and_reread_is_correct() {
        let directory = std::env::temp_dir().join("delphi_parser_trim_bound");
        std::fs::create_dir_all(&directory).unwrap();
        let arena = SourceArena::new();

        // 20 distinct disk units, each ~2 KiB of ASCII (raw==decoded length).
        let unit_bytes = 2048;
        let count = 20;
        let mut files = Vec::new();
        let mut expected = Vec::new();
        for index in 0..count {
            let text = format!(
                "unit Big{index}; // {}",
                "x".repeat(unit_bytes - 20)
            );
            let path = directory.join(format!("Big{index}.pas"));
            std::fs::write(&path, &text).unwrap();
            let file = arena.load(&path).unwrap(); // reads → resident
            files.push(file);
            expected.push(text);
        }

        // All resident: content + raw both retained → ~2x unit_bytes each.
        let before = arena.resident_disk_bytes();
        assert!(before >= count * unit_bytes, "all units resident: {before}");
        assert_eq!(arena.resident_disk_entry_count(), count);

        // Trim to a small cap: at most 3 units' worth of (content+raw) may stay.
        let cap = 3 * unit_bytes * 2;
        let freed = arena.trim_disk_content(cap);
        assert!(freed > 0, "trim freed bytes");
        assert!(
            arena.resident_disk_bytes() <= cap,
            "resident ({}) must be <= cap ({cap})",
            arena.resident_disk_bytes()
        );
        assert!(
            arena.resident_disk_entry_count() < count,
            "some disk entries were cleared"
        );

        // Every file — including cleared ones — re-reads correctly: content and
        // a span both match the original (spans resolve after a re-read).
        for (index, &file) in files.iter().enumerate() {
            assert_eq!(arena.content(file).unwrap(), expected[index]);
            // span [5, 8) of "unit BigN;" is "Big"
            assert_eq!(arena.text(file, Span::new(5, 8)), "Big");
        }
    }

    /// A cleared disk file that CHANGED on disk re-reads the NEW bytes without a
    /// crash (hash-validation of a stale cached AST is task-16's job; here we
    /// only confirm the arena re-reads live bytes and never panics).
    #[test]
    fn trim_then_disk_change_rereads_new_bytes_without_crash() {
        let directory = std::env::temp_dir().join("delphi_parser_trim_change");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("Mutable.pas");
        std::fs::write(&path, "unit Mutable; // original padding padding").unwrap();

        let arena = SourceArena::new();
        let file = arena.load(&path).unwrap();
        assert_eq!(arena.content(file).unwrap(), "unit Mutable; // original padding padding");

        // Trim to zero: force the entry cleared.
        arena.trim_disk_content(0);
        assert_eq!(entry_resident_bytes(arena.entry(file)), 0);

        // Change the file on disk, then re-read: NEW bytes, no crash.
        std::fs::write(&path, "unit Mutable; // CHANGED").unwrap();
        assert_eq!(arena.content(file).unwrap(), "unit Mutable; // CHANGED");
        // raw bytes also reflect the new content.
        assert_eq!(
            arena.raw_bytes(file).unwrap(),
            b"unit Mutable; // CHANGED".as_slice()
        );
    }

    /// Virtual entries are NEVER cleared by a trim (their display-only path
    /// cannot be re-read — a clear would lose data). Even a `trim_disk_content(0)`
    /// leaves a virtual buffer's content intact and readable.
    #[test]
    fn trim_never_clears_virtual_entries() {
        let arena = SourceArena::new();
        let file = arena.set_virtual("C:/editor/Live.pas", "unit Live; // unsaved".to_string());
        assert_eq!(arena.content(file).unwrap(), "unit Live; // unsaved");

        // Trim to zero must not touch virtual content.
        arena.trim_disk_content(0);
        assert_eq!(
            arena.content(file).unwrap(),
            "unit Live; // unsaved",
            "a virtual buffer's content survives an aggressive trim"
        );
        // A virtual entry contributes nothing to the disk-resident total.
        assert_eq!(arena.resident_disk_bytes(), 0);
    }

    /// LRU order: the MOST-recently-accessed disk entry survives a trim that can
    /// keep only one, and the colder ones are evicted.
    #[test]
    fn trim_evicts_least_recently_accessed_first() {
        let directory = std::env::temp_dir().join("delphi_parser_trim_lru");
        std::fs::create_dir_all(&directory).unwrap();
        let arena = SourceArena::new();

        let unit = 2048;
        let mut files = Vec::new();
        for index in 0..4 {
            let path = directory.join(format!("Lru{index}.pas"));
            std::fs::write(&path, format!("unit Lru{index}; // {}", "y".repeat(unit)))
                .unwrap();
            files.push(arena.load(&path).unwrap());
        }
        // Touch file 2 LAST so it is the most-recently-accessed (hottest).
        let _ = arena.content(files[0]).unwrap();
        let _ = arena.content(files[1]).unwrap();
        let _ = arena.content(files[3]).unwrap();
        let _ = arena.content(files[2]).unwrap();

        // Keep at most one entry's (content+raw) resident.
        let cap = (unit + 20) * 2;
        arena.trim_disk_content(cap);
        assert!(arena.resident_disk_bytes() <= cap);
        // The hottest (file 2) is still resident; a colder one (file 0) is not.
        assert!(
            arena.entry(files[2]).content.lock().unwrap().is_some(),
            "most-recently-accessed entry survives"
        );
        assert!(
            arena.entry(files[0]).content.lock().unwrap().is_none(),
            "a least-recently-accessed entry was evicted"
        );
    }

    #[test]
    fn references_survive_later_loads() {
        let arena = SourceArena::new();
        let first = arena.insert_virtual("a.pas", "unit A;");
        let first_content: &str = arena.content(first).unwrap();
        for index in 0..100 {
            arena.insert_virtual(format!("f{index}.pas"), "x".repeat(1000));
        }
        // stable address: still readable after arena growth
        assert_eq!(first_content, "unit A;");
    }

    #[test]
    fn decodes_utf16_be_and_le_and_rejects_odd_byte_count() {
        let directory = std::env::temp_dir().join("delphi_parser_utf16_test");
        std::fs::create_dir_all(&directory).unwrap();

        // "unit A;" as UTF-16BE with a BE BOM (FE FF).
        let mut big_endian = vec![0xFE, 0xFF];
        for unit in "unit A;".encode_utf16() {
            big_endian.extend_from_slice(&unit.to_be_bytes());
        }
        let be_path = directory.join("be.pas");
        std::fs::write(&be_path, &big_endian).unwrap();
        let (be_raw, be_text) = read_source_file(&be_path).unwrap();
        assert_eq!(be_text, "unit A;");
        // raw bytes are returned byte-identical to what was written on disk
        assert_eq!(be_raw, big_endian);

        // Same as UTF-16LE (FF FE) — the pre-existing path, kept honest here.
        let mut little_endian = vec![0xFF, 0xFE];
        for unit in "unit A;".encode_utf16() {
            little_endian.extend_from_slice(&unit.to_le_bytes());
        }
        let le_path = directory.join("le.pas");
        std::fs::write(&le_path, &little_endian).unwrap();
        assert_eq!(read_source_file(&le_path).unwrap().1, "unit A;");

        // A dangling half-code-unit must error, not silently drop the byte.
        let mut truncated = big_endian.clone();
        truncated.push(0x00);
        let odd_path = directory.join("odd.pas");
        std::fs::write(&odd_path, &truncated).unwrap();
        assert!(read_source_file(&odd_path).is_err());
    }

    #[test]
    fn disk_load_dedups_by_canonical_path() {
        let directory = std::env::temp_dir().join("delphi_parser_arena_test");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("Sample.pas");
        std::fs::write(&path, "unit Sample;").unwrap();

        let arena = SourceArena::new();
        let first = arena.load(&path).unwrap();
        let second = arena.load(directory.join("sample.PAS")).unwrap(); // case differs
        assert_eq!(first, second);
        assert_eq!(arena.len(), 1);
        assert_eq!(arena.content(first).unwrap(), "unit Sample;");
    }

    #[test]
    fn register_reads_lazily_and_dedups_with_load() {
        let directory = std::env::temp_dir().join("delphi_parser_arena_lazy_test");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("Lazy.pas");
        std::fs::write(&path, "unit Lazy;").unwrap();

        let arena = SourceArena::new();
        let registered = arena.register(&path).unwrap();
        // no content yet — first access reads from disk
        assert_eq!(arena.content(registered).unwrap(), "unit Lazy;");
        // load() of the same path resolves to the same id
        assert_eq!(arena.load(&path).unwrap(), registered);
    }

    #[test]
    fn missing_file_reports_path() {
        let arena = SourceArena::new();
        let result = arena.load(r"C:\does\not\exist.pas");
        assert!(result.is_err());
    }
}
