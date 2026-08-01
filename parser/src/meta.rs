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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeLocation {
    pub file: FileId,
    pub span: Span,
}
