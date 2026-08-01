//! UTF-16 ↔ byte-offset position mapping for one document's text.
//!
//! The #1 LSP defect source. LSP `Position` is `(line, character)` where
//! `character` counts **UTF-16 code units** on that line; the Delphi parser
//! addresses source by **byte offset** into the (UTF-8) document text. Every
//! feature — diagnostics ranges, go-to-definition, hover, rename — depends on
//! converting between the two exactly. A single off-by-one here surfaces as a
//! squiggle on the wrong character, a jump to the wrong token, an edit that
//! corrupts the buffer. So this module is correct-by-construction and tested
//! exhaustively in BOTH directions (ASCII, CRLF, multibyte UTF-8, astral /
//! surrogate-pair characters, position at end-of-line and end-of-file).
//!
//! Design:
//! - A [`LineIndex`] is built once per document text (on didOpen/didChange).
//! - It stores, per line, the byte offset where the line starts.
//! - Byte↔position conversions then walk only the target line's bytes, decoding
//!   UTF-8 char by char and counting UTF-16 code units (1 for BMP, 2 for astral
//!   characters whose `char::len_utf16() == 2`).
//!
//! Line boundaries: a line ends at `\n`; a preceding `\r` (CRLF) is part of the
//! same line's content region for byte purposes but neither `\r` nor `\n` are
//! addressable columns beyond the line's code-unit length. We clamp
//! out-of-range positions to the end of the line / document rather than
//! panicking — an editor can legitimately send a position one past the last
//! character (end-of-line/end-of-file cursor), and a stale position after a
//! race must degrade to a safe offset, never crash the server.

use tower_lsp::lsp_types::Position;

/// Precomputed line-start byte offsets for one document's text, enabling exact
/// UTF-16 code-unit ↔ byte-offset mapping in both directions.
#[derive(Debug, Clone)]
pub struct LineIndex {
    /// The full document text (UTF-8). Borrowed offsets index into this.
    text: String,
    /// Byte offset at which each line begins. `line_starts[0] == 0`; there is
    /// one entry per line. A trailing newline creates a final empty line, whose
    /// start is `text.len()` (so a document ending in `\n` has an addressable
    /// empty last line — matching editor behavior).
    line_starts: Vec<usize>,
}

impl LineIndex {
    /// Build the index for `text`. O(n) over the bytes once.
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let mut line_starts = Vec::with_capacity(text.len() / 40 + 1);
        line_starts.push(0);
        for (offset, byte) in text.bytes().enumerate() {
            if byte == b'\n' {
                line_starts.push(offset + 1);
            }
        }
        Self { text, line_starts }
    }

    /// The document text this index describes.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Number of lines. A document with no trailing newline has one more line
    /// than it has newlines; a trailing `\n` adds a final empty line.
    pub fn line_count(&self) -> u32 {
        self.line_starts.len() as u32
    }

    /// Byte offset of the start of `line`. Clamped: a line index past the end
    /// returns the document length (end-of-file), never panics.
    fn line_start_byte(&self, line: u32) -> usize {
        match self.line_starts.get(line as usize) {
            Some(&start) => start,
            None => self.text.len(),
        }
    }

    /// Byte offset just past the last content byte of `line` (i.e. the offset of
    /// its terminating `\n`, or the trailing `\r` of a CRLF, or the document end
    /// for the last line). The addressable-column region of a line is
    /// `[line_start_byte(line), line_content_end_byte(line))`.
    fn line_content_end_byte(&self, line: u32) -> usize {
        let start = self.line_start_byte(line);
        // The next line begins right after this line's `\n`. If there is no next
        // line, this line runs to the document end.
        let raw_end = self
            .line_starts
            .get(line as usize + 1)
            .copied()
            .unwrap_or(self.text.len());
        // Strip the line terminator (`\n`, and a preceding `\r`) from the
        // addressable region: neither is an addressable column.
        let mut end = raw_end;
        if end > start && self.text.as_bytes().get(end - 1) == Some(&b'\n') {
            end -= 1;
        }
        if end > start && self.text.as_bytes().get(end.wrapping_sub(1)) == Some(&b'\r') {
            end -= 1;
        }
        end
    }

    /// Convert an LSP [`Position`] (line, UTF-16 character) to a byte offset into
    /// the document text.
    ///
    /// Clamping (never panics, never a wrong crash):
    /// - a `line` past the last line → document end;
    /// - a `character` past the line's UTF-16 length → the line's content end
    ///   (the offset of its terminator, or document end).
    ///
    /// The returned offset always sits on a UTF-8 char boundary.
    pub fn offset_of(&self, position: Position) -> usize {
        let line_start = self.line_start_byte(position.line);
        let line_end = self.line_content_end_byte(position.line);
        let line_text = &self.text[line_start..line_end];

        let mut utf16_remaining = position.character;
        let mut byte = line_start;
        for character in line_text.chars() {
            let units = character.len_utf16() as u32;
            if utf16_remaining < units {
                // The requested column lands INSIDE this character (e.g. the
                // low half of a surrogate pair, an invalid intra-char column).
                // Clamp to the character's start — never split a UTF-8 boundary.
                return byte;
            }
            utf16_remaining -= units;
            byte += character.len_utf8();
            if utf16_remaining == 0 {
                return byte;
            }
        }
        // Ran past the line's content (end-of-line, or a character column beyond
        // the last character) → the line's content end.
        line_end
    }

    /// Convert a byte offset into the document text to an LSP [`Position`]
    /// (line, UTF-16 character).
    ///
    /// Clamping: an offset past the document end maps to the end-of-file
    /// position; an offset not on a char boundary is treated as its enclosing
    /// character's start (rounded down). Never panics.
    pub fn position_of(&self, offset: usize) -> Position {
        let offset = offset.min(self.text.len());
        // Binary search for the line whose start is the greatest ≤ offset.
        let line = match self.line_starts.binary_search(&offset) {
            Ok(exact) => exact,
            // `insert_point` is where `offset` would go to stay sorted; the line
            // containing `offset` is the one before it.
            Err(insert_point) => insert_point - 1,
        };
        let line_start = self.line_starts[line];

        // Count UTF-16 code units from the line start up to `offset`, decoding
        // whole characters (an offset splitting a char rounds down to that
        // char's start, so we never miscount).
        let mut character: u32 = 0;
        let mut byte = line_start;
        for ch in self.text[line_start..].chars() {
            let next = byte + ch.len_utf8();
            if next > offset {
                break;
            }
            character += ch.len_utf16() as u32;
            byte = next;
            if byte >= offset {
                break;
            }
        }
        Position {
            line: line as u32,
            character,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(line: u32, character: u32) -> Position {
        Position { line, character }
    }

    /// Every ADDRESSABLE byte offset (a char boundary that is not inside a line
    /// terminator) must round-trip byte→position→byte exactly. A byte offset
    /// pointing at a `\r`/`\n` is not an addressable source column (LSP columns
    /// stop at the line's content end), so those are excluded — mapping them is
    /// still defined (they clamp to the content end) but is not a fixed point.
    ///
    /// This is the load-bearing direction for diagnostics: a parser byte span
    /// end must map to a stable LSP position and back.
    fn assert_round_trips(text: &str) {
        let index = LineIndex::new(text);
        for boundary in addressable_boundaries(text) {
            let position = index.position_of(boundary);
            let recovered = index.offset_of(position);
            assert_eq!(
                recovered, boundary,
                "offset {boundary} → {position:?} → {recovered} did not round-trip in {text:?}"
            );
        }
    }

    /// Char boundaries that are addressable columns: every char-start offset
    /// whose preceding byte is not `\r` (a boundary sitting at the `\n` after a
    /// `\r`, or at a `\r`, is a terminator-internal offset). The EOF offset is
    /// addressable. A `\n` start IS a line start (addressable, column 0 of the
    /// next line); a `\r` offset is not.
    fn addressable_boundaries(text: &str) -> Vec<usize> {
        let bytes = text.as_bytes();
        let mut boundaries: Vec<usize> = text
            .char_indices()
            .map(|(offset, _)| offset)
            // A byte that IS a line terminator (`\r` or `\n`) is not an
            // addressable column: LSP columns run up to the line's content end.
            // The offset AFTER a `\n` is the next line's start (column 0) and is
            // included (it is not itself `\r`/`\n`).
            .filter(|&offset| !matches!(bytes.get(offset), Some(&b'\r') | Some(&b'\n')))
            .collect();
        boundaries.push(text.len()); // end-of-file
        boundaries
    }

    #[test]
    fn ascii_single_line() {
        let index = LineIndex::new("unit Foo;");
        assert_eq!(index.offset_of(pos(0, 0)), 0);
        assert_eq!(index.offset_of(pos(0, 5)), 5); // 'F'
        assert_eq!(index.offset_of(pos(0, 9)), 9); // end of line == EOF
        assert_eq!(index.position_of(0), pos(0, 0));
        assert_eq!(index.position_of(5), pos(0, 5));
        assert_eq!(index.position_of(9), pos(0, 9));
        assert_round_trips("unit Foo;");
    }

    #[test]
    fn lf_multiline() {
        // "a\nbc\nd" — three lines. Byte layout:
        // 0:'a' 1:'\n' | 2:'b' 3:'c' 4:'\n' | 5:'d'
        let text = "a\nbc\nd";
        let index = LineIndex::new(text);
        assert_eq!(index.line_count(), 3);
        assert_eq!(index.offset_of(pos(0, 0)), 0);
        assert_eq!(index.offset_of(pos(0, 1)), 1); // end of line 0 (before \n)
        assert_eq!(index.offset_of(pos(1, 0)), 2); // 'b'
        assert_eq!(index.offset_of(pos(1, 2)), 4); // end of line 1 (before \n)
        assert_eq!(index.offset_of(pos(2, 0)), 5); // 'd'
        assert_eq!(index.offset_of(pos(2, 1)), 6); // EOF

        assert_eq!(index.position_of(0), pos(0, 0));
        assert_eq!(index.position_of(2), pos(1, 0));
        assert_eq!(index.position_of(4), pos(1, 2));
        assert_eq!(index.position_of(5), pos(2, 0));
        assert_eq!(index.position_of(6), pos(2, 1));
        assert_round_trips(text);
    }

    #[test]
    fn crlf_line_endings() {
        // "ab\r\ncd" — the \r is NOT an addressable column; column 2 on line 0
        // is the end-of-line (offset of the \r).
        // 0:'a' 1:'b' 2:'\r' 3:'\n' | 4:'c' 5:'d'
        let text = "ab\r\ncd";
        let index = LineIndex::new(text);
        assert_eq!(index.line_count(), 2);
        assert_eq!(index.offset_of(pos(0, 0)), 0);
        assert_eq!(index.offset_of(pos(0, 2)), 2); // end of line 0 == offset of \r
        // a column past the content still clamps to the content end, not into \r\n
        assert_eq!(index.offset_of(pos(0, 5)), 2);
        assert_eq!(index.offset_of(pos(1, 0)), 4); // 'c'
        assert_eq!(index.offset_of(pos(1, 2)), 6); // EOF

        // byte offsets inside the CRLF map to line 0's end / line 1's start
        assert_eq!(index.position_of(2), pos(0, 2)); // the \r itself
        assert_eq!(index.position_of(4), pos(1, 0)); // 'c'
        assert_round_trips(text);
    }

    #[test]
    fn multibyte_utf8_bmp() {
        // 'ä' (U+00E4) is 2 UTF-8 bytes, 1 UTF-16 code unit.
        // '€' (U+20AC) is 3 UTF-8 bytes, 1 UTF-16 code unit.
        // "xäy€z"
        // bytes: 0:'x' 1..3:'ä' 3:'y' 4..7:'€' 7:'z'
        let text = "xäy€z";
        let index = LineIndex::new(text);
        assert_eq!(index.offset_of(pos(0, 0)), 0); // x
        assert_eq!(index.offset_of(pos(0, 1)), 1); // ä start
        assert_eq!(index.offset_of(pos(0, 2)), 3); // y
        assert_eq!(index.offset_of(pos(0, 3)), 4); // € start
        assert_eq!(index.offset_of(pos(0, 4)), 7); // z
        assert_eq!(index.offset_of(pos(0, 5)), 8); // EOF

        assert_eq!(index.position_of(1), pos(0, 1)); // ä
        assert_eq!(index.position_of(4), pos(0, 3)); // €
        assert_eq!(index.position_of(7), pos(0, 4)); // z
        // a byte offset in the MIDDLE of 'ä' rounds down to ä's start column
        assert_eq!(index.position_of(2), pos(0, 1));
        assert_round_trips(text);
    }

    #[test]
    fn surrogate_pair_astral() {
        // '😀' (U+1F600) is 4 UTF-8 bytes and TWO UTF-16 code units (surrogate
        // pair) — the classic LSP off-by-one. "a😀b"
        // bytes: 0:'a' 1..5:'😀' 5:'b'
        let text = "a😀b";
        let index = LineIndex::new(text);
        assert_eq!(index.offset_of(pos(0, 0)), 0); // a
        assert_eq!(index.offset_of(pos(0, 1)), 1); // 😀 start
        // column 2 is the LOW surrogate half — an invalid intra-char column;
        // clamp to the emoji's start (never split the 4-byte sequence).
        assert_eq!(index.offset_of(pos(0, 2)), 1);
        assert_eq!(index.offset_of(pos(0, 3)), 5); // b (past the 2 surrogate units)
        assert_eq!(index.offset_of(pos(0, 4)), 6); // EOF

        assert_eq!(index.position_of(0), pos(0, 0)); // a
        assert_eq!(index.position_of(1), pos(0, 1)); // 😀
        // 'b' is at UTF-16 column 3 (a=1 unit, 😀=2 units)
        assert_eq!(index.position_of(5), pos(0, 3));
        assert_eq!(index.position_of(6), pos(0, 4)); // EOF
        assert_round_trips(text);
    }

    #[test]
    fn emoji_in_comment_across_lines() {
        // Realistic: an emoji in a comment on line 0, code on line 1.
        // "// 😀\nunit X;"
        let text = "// 😀\nunit X;";
        let index = LineIndex::new(text);
        assert_eq!(index.line_count(), 2);
        // line 0: '/','/',' ','😀' → UTF-16 length = 2+1+1+... = '/'+'/'+' ' = 3
        // units + 😀 = 2 units = 5 units total.
        let line0_end = index.offset_of(pos(0, 5));
        assert_eq!(&text[..line0_end], "// 😀");
        // line 1 starts after the \n
        assert_eq!(index.offset_of(pos(1, 0)), "// 😀\n".len());
        assert_eq!(index.offset_of(pos(1, 5)), text.len() - 2); // 'unit ' end
        assert_round_trips(text);
    }

    #[test]
    fn empty_document_and_trailing_newline() {
        let empty = LineIndex::new("");
        assert_eq!(empty.line_count(), 1);
        assert_eq!(empty.offset_of(pos(0, 0)), 0);
        assert_eq!(empty.offset_of(pos(5, 5)), 0); // clamp everything to EOF
        assert_eq!(empty.position_of(0), pos(0, 0));
        assert_eq!(empty.position_of(99), pos(0, 0));

        // A trailing newline creates an addressable empty final line.
        let trailing = LineIndex::new("x\n");
        assert_eq!(trailing.line_count(), 2);
        assert_eq!(trailing.offset_of(pos(1, 0)), 2); // start of empty last line
        assert_eq!(trailing.position_of(2), pos(1, 0));
        assert_round_trips("x\n");
    }

    #[test]
    fn out_of_range_positions_clamp_not_panic() {
        let index = LineIndex::new("abc\ndef");
        // line past the end → EOF offset
        assert_eq!(index.offset_of(pos(99, 0)), index.text().len());
        // character past the line → line content end
        assert_eq!(index.offset_of(pos(0, 99)), 3);
        // byte past the end → EOF position
        assert_eq!(index.position_of(9999), index.position_of(index.text().len()));
    }

    #[test]
    fn crlf_document_full_round_trip() {
        assert_round_trips("unit X;\r\ninterface\r\n\r\nimplementation\r\nend.\r\n");
    }

    #[test]
    fn mixed_content_round_trip() {
        assert_round_trips("procedure P;\nbegin\n  // ä€😀 mixed\n  DoThing;\nend;\n");
    }
}
