//! Open-document store: the authoritative in-memory text of every file the
//! editor has opened, keyed by `Url`, with a precomputed [`LineIndex`] for
//! UTF-16 ↔ byte position mapping.
//!
//! The editor owns the truth for open documents (unsaved edits live only here,
//! never on disk), so every language feature reads text from this store, not
//! from the filesystem. Diagnostics, definition, hover — all map their LSP
//! `Position`s through the stored document's `LineIndex`.
//!
//! Sync model: the server advertises INCREMENTAL text sync. Each
//! `didChange` carries content changes that are either a ranged edit (replace
//! the UTF-16 range with new text) or a full-document replacement (no range).
//! [`DocumentStore::apply_change`] applies both correctly using the exact
//! position mapper, then rebuilds the line index. The rebuild is O(n) per
//! change; for the document sizes an editor holds open this is negligible and
//! removes any chance of an incrementally-maintained index drifting out of sync
//! with the text (a wrong index is a wrong answer everywhere downstream).

use std::collections::HashMap;

use tower_lsp::lsp_types::{
    TextDocumentContentChangeEvent, Url,
};

use crate::positions::LineIndex;

/// One open document: its editor `version` and the text+line index.
#[derive(Debug, Clone)]
pub struct Document {
    /// Monotonic editor version from didOpen/didChange. A stale (older) version
    /// is ignored so out-of-order notifications never regress the text.
    pub version: i32,
    /// Text + UTF-16↔byte mapping. The `LineIndex` owns the document text.
    pub line_index: LineIndex,
}

impl Document {
    pub fn text(&self) -> &str {
        self.line_index.text()
    }
}

/// The set of currently open documents. Lives behind the server's lock; every
/// mutation is a single notification handler, so no document is read mid-edit.
#[derive(Debug, Default)]
pub struct DocumentStore {
    documents: HashMap<Url, Document>,
}

impl DocumentStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// didOpen: insert (or replace) the document with the editor's full text.
    pub fn open(&mut self, uri: Url, version: i32, text: String) {
        self.documents.insert(
            uri,
            Document {
                version,
                line_index: LineIndex::new(text),
            },
        );
    }

    /// didChange: apply content changes to the open document, then bump its
    /// version. Returns the updated document text (for a re-parse) or `None` if
    /// the document is not open or the change is stale (older version).
    ///
    /// A change event with a `range` is a ranged edit (replace that UTF-16 range
    /// with `text`); a change event without a `range` is a full-document
    /// replacement. Both are handled; multiple changes in one notification are
    /// applied in order (the LSP spec requires each subsequent change to be
    /// computed against the text produced by the previous one).
    pub fn apply_change(
        &mut self,
        uri: &Url,
        version: i32,
        changes: Vec<TextDocumentContentChangeEvent>,
    ) -> Option<String> {
        let document = self.documents.get_mut(uri)?;
        // Ignore an out-of-order (older) version — never regress the buffer.
        //
        // An EQUAL version needs care. The LSP spec requires every didChange to
        // carry a strictly increasing version, so an equal-version notification
        // is a duplicate. Re-applying a *ranged* edit on a duplicate would
        // double-apply it (ranged edits are NOT idempotent — e.g. an insert would
        // be inserted twice), corrupting the buffer. A *full replacement* is
        // idempotent, so an equal-version full-replacement resend is harmless and
        // is honored. We therefore reject an equal-version notification only when
        // it contains any ranged edit; a pure full-replacement duplicate passes.
        if version < document.version {
            return None;
        }
        if version == document.version && changes.iter().any(|change| change.range.is_some()) {
            return None;
        }

        let mut text = document.line_index.text().to_string();
        for change in changes {
            match change.range {
                None => {
                    // Full-document replacement.
                    text = change.text;
                }
                Some(range) => {
                    // Ranged edit: map both endpoints through the CURRENT text's
                    // line index (each change sees the previous change's result).
                    let index = LineIndex::new(std::mem::take(&mut text));
                    let start = index.offset_of(range.start);
                    let end = index.offset_of(range.end).max(start);
                    let mut edited = index.text().to_string();
                    edited.replace_range(start..end, &change.text);
                    text = edited;
                }
            }
        }

        document.version = version;
        document.line_index = LineIndex::new(text);
        Some(document.line_index.text().to_string())
    }

    /// didClose: drop the document. Returns whether it was open.
    pub fn close(&mut self, uri: &Url) -> bool {
        self.documents.remove(uri).is_some()
    }

    /// The open document for `uri`, if any.
    pub fn get(&self, uri: &Url) -> Option<&Document> {
        self.documents.get(uri)
    }

    // Store-introspection accessors used by tests today; wired to feature
    // providers (definition/hover/references decide "is this file open?" before
    // reading from the store vs. disk) in a later task. Kept as the store's
    // intended public API rather than deleted.
    #[allow(dead_code)]
    pub fn is_open(&self, uri: &Url) -> bool {
        self.documents.contains_key(uri)
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.documents.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.documents.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower_lsp::lsp_types::{Position, Range};

    fn uri() -> Url {
        Url::parse("file:///c:/proj/Unit1.pas").unwrap()
    }

    fn full_change(text: &str) -> TextDocumentContentChangeEvent {
        TextDocumentContentChangeEvent {
            range: None,
            range_length: None,
            text: text.to_string(),
        }
    }

    fn ranged_change(start: (u32, u32), end: (u32, u32), text: &str) -> TextDocumentContentChangeEvent {
        TextDocumentContentChangeEvent {
            range: Some(Range {
                start: Position { line: start.0, character: start.1 },
                end: Position { line: end.0, character: end.1 },
            }),
            range_length: None,
            text: text.to_string(),
        }
    }

    #[test]
    fn open_get_close() {
        let mut store = DocumentStore::new();
        assert!(store.is_empty());
        store.open(uri(), 1, "unit Foo;".to_string());
        assert_eq!(store.len(), 1);
        assert!(store.is_open(&uri()));
        assert_eq!(store.get(&uri()).unwrap().text(), "unit Foo;");
        assert_eq!(store.get(&uri()).unwrap().version, 1);
        assert!(store.close(&uri()));
        assert!(!store.is_open(&uri()));
        assert!(!store.close(&uri())); // second close: already gone
    }

    #[test]
    fn full_replacement_change() {
        let mut store = DocumentStore::new();
        store.open(uri(), 1, "old".to_string());
        let text = store.apply_change(&uri(), 2, vec![full_change("brand new")]);
        assert_eq!(text.as_deref(), Some("brand new"));
        assert_eq!(store.get(&uri()).unwrap().version, 2);
        assert_eq!(store.get(&uri()).unwrap().text(), "brand new");
    }

    #[test]
    fn ranged_insert() {
        // Insert " World" after "Hello" (a zero-length range at the end).
        let mut store = DocumentStore::new();
        store.open(uri(), 1, "Hello".to_string());
        let text = store.apply_change(&uri(), 2, vec![ranged_change((0, 5), (0, 5), " World")]);
        assert_eq!(text.as_deref(), Some("Hello World"));
    }

    #[test]
    fn ranged_replace() {
        // "abcdef" → replace "cd" (cols 2..4) with "XY" → "abXYef".
        let mut store = DocumentStore::new();
        store.open(uri(), 1, "abcdef".to_string());
        let text = store.apply_change(&uri(), 2, vec![ranged_change((0, 2), (0, 4), "XY")]);
        assert_eq!(text.as_deref(), Some("abXYef"));
    }

    #[test]
    fn ranged_delete_across_lines() {
        // "line1\nline2\nline3" → delete from (0,4) to (2,4). Line 0 col 4 is
        // byte 4 ('1'); line 2 col 4 is byte 16 ('3'). Deleting [4,16) leaves
        // "line" + "3" = "line3".
        let mut store = DocumentStore::new();
        store.open(uri(), 1, "line1\nline2\nline3".to_string());
        let text = store.apply_change(&uri(), 2, vec![ranged_change((0, 4), (2, 4), "")]);
        assert_eq!(text.as_deref(), Some("line3"));
    }

    #[test]
    fn multiple_changes_applied_in_order() {
        // Two edits in one notification; the second is computed against the
        // result of the first.
        let mut store = DocumentStore::new();
        store.open(uri(), 1, "abc".to_string());
        // 1) insert "X" at col 0 → "Xabc"; 2) insert "Y" at col 4 (end) → "XabcY"
        let text = store.apply_change(
            &uri(),
            2,
            vec![
                ranged_change((0, 0), (0, 0), "X"),
                ranged_change((0, 4), (0, 4), "Y"),
            ],
        );
        assert_eq!(text.as_deref(), Some("XabcY"));
    }

    #[test]
    fn ranged_edit_with_multibyte_and_surrogate() {
        // "a😀b" — replace the emoji (UTF-16 cols 1..3) with "ä".
        let mut store = DocumentStore::new();
        store.open(uri(), 1, "a😀b".to_string());
        let text = store.apply_change(&uri(), 2, vec![ranged_change((0, 1), (0, 3), "ä")]);
        assert_eq!(text.as_deref(), Some("aäb"));
    }

    #[test]
    fn stale_version_is_ignored() {
        let mut store = DocumentStore::new();
        store.open(uri(), 5, "current".to_string());
        // an older-versioned change must not regress the buffer
        let text = store.apply_change(&uri(), 3, vec![full_change("stale")]);
        assert!(text.is_none());
        assert_eq!(store.get(&uri()).unwrap().text(), "current");
        assert_eq!(store.get(&uri()).unwrap().version, 5);
    }

    #[test]
    fn equal_version_ranged_edit_is_rejected_not_double_applied() {
        // A duplicate notification at the SAME version must not re-apply a
        // ranged edit (which is not idempotent — it would insert twice).
        let mut store = DocumentStore::new();
        store.open(uri(), 1, "Hello".to_string());
        // First v2 change inserts " World".
        let first = store.apply_change(&uri(), 2, vec![ranged_change((0, 5), (0, 5), " World")]);
        assert_eq!(first.as_deref(), Some("Hello World"));
        // A duplicate v2 with the same ranged edit must be dropped — the buffer
        // stays "Hello World", NOT "Hello World World" (or a corrupted splice).
        let duplicate =
            store.apply_change(&uri(), 2, vec![ranged_change((0, 5), (0, 5), " World")]);
        assert!(duplicate.is_none(), "an equal-version ranged edit must be rejected");
        assert_eq!(store.get(&uri()).unwrap().text(), "Hello World");
        assert_eq!(store.get(&uri()).unwrap().version, 2);
    }

    #[test]
    fn equal_version_full_replacement_is_honored() {
        // A full replacement is idempotent, so an equal-version full-replacement
        // resend is safe and honored (per the spec-tolerant comment in
        // apply_change).
        let mut store = DocumentStore::new();
        store.open(uri(), 1, "old".to_string());
        assert_eq!(
            store.apply_change(&uri(), 2, vec![full_change("new")]).as_deref(),
            Some("new")
        );
        // Same version, full replacement again — allowed (idempotent).
        assert_eq!(
            store.apply_change(&uri(), 2, vec![full_change("new")]).as_deref(),
            Some("new")
        );
        assert_eq!(store.get(&uri()).unwrap().text(), "new");
    }

    #[test]
    fn change_to_unopened_document_is_none() {
        let mut store = DocumentStore::new();
        assert!(store.apply_change(&uri(), 1, vec![full_change("x")]).is_none());
    }
}
