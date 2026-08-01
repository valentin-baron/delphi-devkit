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
    content: OnceLock<String>,
    /// The RAW on-disk bytes this file was read from (before any BOM/UTF-16/ANSI
    /// decoding), retained so hash stamps can be taken without a second disk
    /// read (no TOCTOU window — L15). Populated together with `content` when a
    /// disk file is read. `None` for a virtual buffer (no disk bytes) — the
    /// stamp then falls back to hashing the decoded content, which by design
    /// never matches a disk read and drops the entry as stale on load (#21/#25).
    raw: OnceLock<Vec<u8>>,
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
    pub fn insert_virtual(&self, path: impl Into<PathBuf>, content: impl Into<String>) -> FileId {
        self.push_entry(path.into(), Some(content.into()))
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
        self.entry(file)
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
        let cell = OnceLock::new();
        if let Some(content) = content {
            let _ = cell.set(content);
        }
        // A pushed-with-content entry is a virtual buffer (no disk bytes): `raw`
        // stays empty, so `raw_bytes` returns None and the stamp falls back to
        // hashing decoded content (the intended virtual-buffer behaviour).
        let index = self.files.push_get_index(Box::new(SourceEntry {
            path,
            content: cell,
            raw: OnceLock::new(),
        }));
        FileId(index as u32)
    }

    fn entry(&self, file: FileId) -> &SourceEntry {
        self.files
            .get(file.0 as usize)
            .expect("FileId not issued by this arena")
    }
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
