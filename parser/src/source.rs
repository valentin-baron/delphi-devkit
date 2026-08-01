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
    sync::{Mutex, OnceLock},
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
    /// Lazily materialized: [`SourceArena::register`] creates path-only
    /// entries (e.g. when loading a persisted cache whose locations reference
    /// files nobody has read yet); the text is read on first access.
    ///
    /// DISK entries write this ONCE (first `content` access) and never mutate
    /// it. VIRTUAL entries do NOT use this field — their content lives in
    /// [`Self::virtual_content`] so it can be REPLACED (and the prior String
    /// freed) on re-parse without unbounded arena growth. `is_virtual`
    /// distinguishes the two.
    content: OnceLock<String>,
    /// The RAW on-disk bytes this file was read from (before any BOM/UTF-16/ANSI
    /// decoding), retained so hash stamps can be taken without a second disk
    /// read (no TOCTOU window — L15). Populated together with `content` when a
    /// disk file is read. `None` for a virtual buffer (no disk bytes) — the
    /// stamp then falls back to hashing the decoded content, which by design
    /// never matches a disk read and drops the entry as stale on load (#21/#25).
    raw: OnceLock<Vec<u8>>,
    /// `true` for a virtual (in-memory editor) buffer, `false` for a disk file.
    /// A virtual entry reads/writes [`Self::virtual_content`]; a disk entry uses
    /// [`Self::content`]. This flag is fixed at creation and never changes.
    is_virtual: bool,
    /// REPLACEABLE storage for a virtual buffer's decoded content. `None` for a
    /// disk entry. The `Box<str>` is owned here so [`SourceArena::set_virtual`]
    /// can swap in the new text and DROP the prior box — bounding memory to one
    /// live copy per open document (Task-15 part 2). The mutex makes the swap a
    /// short critical section and keeps the entry `Sync`; the handed-out `&str`
    /// stays valid until the next swap under the caller's serialization (see
    /// [`SourceArena::set_virtual`]'s soundness note).
    virtual_content: Mutex<Option<Box<str>>>,
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
        // content was freed (`did_close`) is an error, never a panic.
        if entry.is_virtual {
            return virtual_content_ref(entry).ok_or_else(|| FileReadError {
                path: entry.path.clone(),
                message: "virtual buffer content was released (closed document)".into(),
            });
        }
        if let Some(content) = entry.content.get() {
            return Ok(content);
        }
        let (raw, decoded) = read_source_file(&entry.path)?;
        // benign race: first writer wins, the duplicate read is discarded. Set
        // `raw` first so any reader that observes `content` also sees `raw`.
        let _ = entry.raw.set(raw);
        let _ = entry.content.set(decoded);
        Ok(entry.content.get().unwrap())
    }

    /// The RAW on-disk bytes a disk file was read from, before decoding. `None`
    /// for a virtual buffer (no disk bytes) or a merely-registered file whose
    /// content has never been materialized. Used by the pipeline to hash a
    /// file's on-disk bytes for its validity stamp WITHOUT re-reading it from
    /// disk (one read, no TOCTOU window — L15). The bytes are byte-identical to
    /// what `std::fs::read` would return, so the resulting hash matches
    /// [`crate::unit_cache::hash_file`] and existing snapshots still validate.
    pub fn raw_bytes(&self, file: FileId) -> Option<&[u8]> {
        self.entry(file).raw.get().map(Vec::as_slice)
    }

    /// Content that is guaranteed to be materialized already (a span for this
    /// file exists, so someone lexed it). Panics on merely-registered files.
    pub fn loaded_content(&self, file: FileId) -> &str {
        let entry = self.entry(file);
        if entry.is_virtual {
            return virtual_content_ref(entry)
                .expect("virtual buffer content requested after it was released");
        }
        entry
            .content
            .get()
            .expect("file content requested before it was loaded")
    }

    /// Text under a span of a file. The file must be materialized (spans only
    /// exist for lexed content).
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
        // Disk entries keep their content in `content` (write-once); virtual
        // entries keep it in `virtual_content` (replaceable, freed on re-parse).
        let virtual_content = Mutex::new(content.map(String::into_boxed_str));
        let index = self.files.push_get_index(Box::new(SourceEntry {
            path,
            content: OnceLock::new(),
            raw: OnceLock::new(),
            is_virtual,
            virtual_content,
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
        // merely registered → not read yet → no raw bytes
        assert!(arena.raw_bytes(file).is_none());
        // materialize → raw bytes now retained, equal to the on-disk bytes
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
