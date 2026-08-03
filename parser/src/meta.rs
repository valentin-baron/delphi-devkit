use serde::{Deserialize, Serialize};

/// A byte-offset range within a single source file (offsets are local to that
/// file, never rebased across an `{$I}` include — see design doc §16).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    #[inline]
    pub fn new(start: usize, end: usize) -> Self {
        Self { start: start as u32, end: end as u32 }
    }

    #[inline]
    pub fn len(self) -> usize {
        (self.end - self.start) as usize
    }

    #[inline]
    pub fn is_empty(self) -> bool {
        self.start == self.end
    }
}

impl From<std::ops::Range<usize>> for Span {
    fn from(r: std::ops::Range<usize>) -> Self {
        Self::new(r.start, r.end)
    }
}

/// Identifies one source buffer in a [`SourceArena`]. Session-local; persisted
/// references use the interned path instead (design doc §15).
///
/// Serde is transparent through the process-global arena ([`crate::globals`]):
/// serialize writes the file's path (never the raw session-local index);
/// deserialize `register`s that path (lazy — no source read) to obtain a fresh
/// `FileId`. A path that cannot be `register`ed (virtual/unsaved buffers, whose
/// display names do not canonicalize, or a deleted file) yields a serde error;
/// the cache loader catches it per unit and counts the entry `unreadable`, so
/// unsaved state never masquerades as on-disk state (invariants #21/#25, M2).
///
/// Symmetrically, serialize yields a serde error (never a panic) for a `FileId`
/// the global arena never issued — a foreign-arena or out-of-range id reaching
/// `save`. `parse_and_cache` is `pub` over an arbitrary `&SourceArena`, so this
/// case is reachable; one bad id must not abort the whole snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FileId(pub u32);

impl Serialize for FileId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // Non-panicking: a `FileId` that the process-global arena never issued
        // (foreign arena, out-of-range index) is a serde error, not a panic —
        // otherwise ONE bad id would abort the whole `UnitCache::save`, and
        // `parse_and_cache` is `pub` over an arbitrary `&SourceArena`. The load
        // side already deserializes to an error and counts the entry
        // `unreadable`; this mirrors it on the save side (M2, #21/#25).
        let path = crate::globals::arena().try_path(*self).ok_or_else(|| {
            serde::ser::Error::custom(format!(
                "FileId({}) not registered in the global source arena",
                self.0
            ))
        })?;
        serializer.serialize_str(&path.to_string_lossy())
    }
}

impl<'de> Deserialize<'de> for FileId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let path = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
        crate::globals::arena()
            .register(path.as_ref())
            .map_err(|error| serde::de::Error::custom(error.message))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodeLocation {
    pub file: FileId,
    pub span: Span,
}

// ─── Per-(de)serialization location context (unit-self-file elision) ────────
//
// A `UnitMeta` holds thousands of `CodeLocation`s, and the overwhelming majority
// point at the unit's OWN source file — repeating that path on every span is
// pure bloat. During `UnitMeta` (de)serialization we install a thread-local
// context that lets a `CodeLocation` in the self file serialize with NO file
// reference at all, and any other (`{$I}` include) file reference a small
// per-unit table of DISTINCT include paths by index.
//
// Thread-local, because bincode is synchronous (one meta (de)serializes on one
// thread) but the bootstrap (de)serializes many metas concurrently on different
// threads — a thread-local is per-thread, so concurrent metas cannot corrupt
// each other's context. Every entry point RESETS the context via an RAII guard
// so a panic mid-(de)serialization cannot leave a stale self-file/table behind.

thread_local! {
    /// The self file of the meta currently being (de)serialized on this thread,
    /// or `None` outside a `UnitMeta` (de)serialization. A `CodeLocation` whose
    /// `file` equals this serializes in the compact SELF form (span only).
    static CURRENT_SELF_FILE: std::cell::Cell<Option<FileId>> = const { std::cell::Cell::new(None) };
    /// Serialize side: distinct NON-self include paths collected in first-seen
    /// order; a `CodeLocation` in a non-self file emits its INDEX here. Emitted
    /// as a field of the serialized `UnitMeta` so the load side can rebuild it.
    static INCLUDE_TABLE_SERIALIZE: std::cell::RefCell<Vec<std::path::PathBuf>> =
        const { std::cell::RefCell::new(Vec::new()) };
    /// Deserialize side: index → registered `FileId`, read from the meta's
    /// include-table field BEFORE any nested `CodeLocation` is deserialized.
    static INCLUDE_TABLE_DESERIALIZE: std::cell::RefCell<Vec<FileId>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// RAII reset for the location context. Constructing it establishes the self
/// file (and clears both tables); dropping it — on ANY exit path, including a
/// panic unwind — restores `CURRENT_SELF_FILE` to `None` and empties both
/// tables. This is what guarantees a failed/paniced (de)serialization cannot
/// leak a stale self file into a later, unrelated (de)serialization on the same
/// thread.
pub struct LocationContextGuard {
    _private: (),
}

impl LocationContextGuard {
    /// Enter a (de)serialization context for a meta whose own file is `self_file`.
    /// Clears both include tables so the context starts empty.
    pub fn enter(self_file: FileId) -> Self {
        CURRENT_SELF_FILE.with(|cell| cell.set(Some(self_file)));
        INCLUDE_TABLE_SERIALIZE.with(|table| table.borrow_mut().clear());
        INCLUDE_TABLE_DESERIALIZE.with(|table| table.borrow_mut().clear());
        Self { _private: () }
    }

    /// Install the deserialize-side index → `FileId` table (read from the meta's
    /// include-table field before nested locations are decoded).
    pub fn set_deserialize_table(table: Vec<FileId>) {
        INCLUDE_TABLE_DESERIALIZE.with(|slot| *slot.borrow_mut() = table);
    }

    /// Take the serialize-side collected distinct include paths (call AFTER the
    /// nested fields have serialized and populated it). The caller emits this as
    /// the meta's include-table field.
    pub fn take_serialize_table() -> Vec<std::path::PathBuf> {
        INCLUDE_TABLE_SERIALIZE.with(|table| std::mem::take(&mut *table.borrow_mut()))
    }
}

impl Drop for LocationContextGuard {
    fn drop(&mut self) {
        CURRENT_SELF_FILE.with(|cell| cell.set(None));
        INCLUDE_TABLE_SERIALIZE.with(|table| table.borrow_mut().clear());
        INCLUDE_TABLE_DESERIALIZE.with(|table| table.borrow_mut().clear());
    }
}

/// Run `run` with the thread-local location context established for `self_file`,
/// then reset it (via [`LocationContextGuard`]'s RAII drop, so the reset happens
/// on any exit path including a panic unwind). Use this to serialize a READABLE
/// view of an AST (e.g. YAML/JSON) so that a `CodeLocation` in the unit's OWN
/// file elides its file (span only) and an `{$I}`-include span shows its path via
/// the `Include`/`Full` variant — the same self-elision the durable `UnitMeta`
/// serde relies on, exposed for on-demand debug dumps.
pub fn with_self_file_context<R>(self_file: FileId, run: impl FnOnce() -> R) -> R {
    let _guard = LocationContextGuard::enter(self_file);
    run()
}

/// Compact on-wire form of a `CodeLocation`, distinguishing a span in the unit's
/// OWN file (no file reference) from a span in an `{$I}` include (an index into
/// the per-unit include table). Outside a `UnitMeta` context the fallback
/// `Full` form carries the whole `FileId` (its path), so a bare `CodeLocation`
/// still round-trips on its own (e.g. a direct `bincode::serialize(&location)`).
#[derive(Serialize, Deserialize)]
enum LocationRepr {
    /// The unit's own file: only the span travels; the file is `CURRENT_SELF_FILE`.
    SelfFile(Span),
    /// A non-self (`{$I}` include) file: an index into the per-unit include table.
    Include { include_index: u32, span: Span },
    /// No active context (bare `CodeLocation` (de)serialization): full `FileId`.
    Full(FileId, Span),
}

impl Serialize for CodeLocation {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let self_file = CURRENT_SELF_FILE.with(|cell| cell.get());
        let repr = match self_file {
            // In a meta context and this location is in the self file → span only.
            Some(self_file) if self_file == self.file => LocationRepr::SelfFile(self.span),
            // In a meta context but a different (include) file → table index.
            Some(_) => {
                // Resolve the file's path up front so a foreign/out-of-range
                // FileId is a serde error (never a panic — M2), mirroring the
                // bare `FileId::serialize` contract.
                let path = crate::globals::arena().try_path(self.file).ok_or_else(|| {
                    serde::ser::Error::custom(format!(
                        "FileId({}) not registered in the global source arena",
                        self.file.0
                    ))
                })?;
                let include_index = INCLUDE_TABLE_SERIALIZE.with(|table| {
                    let mut table = table.borrow_mut();
                    match table.iter().position(|existing| existing == path) {
                        Some(index) => index as u32,
                        None => {
                            let index = table.len() as u32;
                            table.push(path.to_path_buf());
                            index
                        }
                    }
                });
                LocationRepr::Include { include_index, span: self.span }
            }
            // No meta context: fall back to the full FileId (path) so a bare
            // CodeLocation still round-trips standalone.
            None => LocationRepr::Full(self.file, self.span),
        };
        repr.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for CodeLocation {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let repr = LocationRepr::deserialize(deserializer)?;
        match repr {
            LocationRepr::SelfFile(span) => {
                let file = CURRENT_SELF_FILE.with(|cell| cell.get()).ok_or_else(|| {
                    serde::de::Error::custom(
                        "SelfFile CodeLocation decoded with no active self-file context",
                    )
                })?;
                Ok(CodeLocation { file, span })
            }
            LocationRepr::Include { include_index, span } => {
                let file = INCLUDE_TABLE_DESERIALIZE.with(|table| {
                    table.borrow().get(include_index as usize).copied()
                });
                let file = file.ok_or_else(|| {
                    serde::de::Error::custom(format!(
                        "include_index {include_index} out of range of the per-unit include table"
                    ))
                })?;
                Ok(CodeLocation { file, span })
            }
            LocationRepr::Full(file, span) => Ok(CodeLocation { file, span }),
        }
    }
}
