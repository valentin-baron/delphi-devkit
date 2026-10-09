//! Maps a selection in an *unformatted* document onto the corresponding range
//! in the *formatted* document.
//!
//! The Delphi formatter needs the whole file for context, so the whole document
//! is formatted and the selection mapped onto the result.
//!
//! The mapping rests on an invariant of that formatter: it rewrites only
//! *whitespace* and letter *case*, never inserting or deleting a non-whitespace
//! character (verified against the RAD Studio formatter option set — the only
//! options touching non-whitespace are the four capitalization ones, and casing
//! changes a character's value, not its position). So the Nth non-whitespace
//! character of the input is the Nth of the output, and that ordinal is the
//! anchor.
//!
//! Offsets crossing the wire are **UTF-16 code units** (VS Code / LSP
//! `TextDocument.offsetAt` semantics); internally everything is UTF-8 byte
//! offsets, converted at the boundaries.

/// A non-whitespace character and its UTF-8 byte offset. Its index in the vec
/// returned by [`enumerate_chars`] is its non-whitespace ordinal — the anchor
/// that survives formatting.
#[derive(Clone, Copy)]
pub struct EnumeratedChar {
    pub position_in_file: usize,
    pub chr: char,
}

/// `start`/`end` are UTF-16 offsets into the **original** document (what to
/// replace); `new_text` is the formatted replacement.
pub struct MappedEdit {
    pub start: usize,
    pub end: usize,
    pub new_text: String,
}

pub fn enumerate_chars(content: &str) -> Vec<EnumeratedChar> {
    content
        .char_indices()
        .filter(|(_, ch)| !ch.is_whitespace())
        .map(|(position_in_file, chr)| EnumeratedChar { position_in_file, chr })
        .collect()
}

fn utf16_to_byte(content: &str, utf16_offset: usize) -> usize {
    let mut utf16 = 0;
    for (byte, ch) in content.char_indices() {
        if utf16 >= utf16_offset {
            return byte;
        }
        utf16 += ch.len_utf16();
    }
    content.len()
}

fn byte_to_utf16(content: &str, byte_offset: usize) -> usize {
    let mut utf16 = 0;
    for (byte, ch) in content.char_indices() {
        if byte >= byte_offset {
            break;
        }
        utf16 += ch.len_utf16();
    }
    utf16
}

fn line_start(content: &str, byte: usize) -> usize {
    content[..byte].rfind('\n').map_or(0, |i| i + 1)
}

/// Whether `byte` is the first non-whitespace position on its line.
fn is_line_leading(content: &str, byte: usize) -> bool {
    content[line_start(content, byte)..byte].chars().all(char::is_whitespace)
}

/// Maps the selection `[start_utf16, end_utf16)` (UTF-16 offsets into
/// `original`) onto `formatted`, returning the sub-range of `original` to
/// replace and the formatted text to put there.
///
/// Selection ends falling in whitespace **around** code snap inward. The one
/// exception is the first selected line's indentation: when the selection's
/// first token is the first thing on its line, the replacement extends back to
/// the line start so that indentation is reformatted too. Trailing whitespace
/// grazed by the selection is left untouched.
///
/// A selection holding no code, or a formatter that changed the non-whitespace
/// character count (invariant broken), yields a collapsed edit at the selection
/// start, which applies as a no-op.
pub fn map_range(
    original: &str,
    formatted: &str,
    start_utf16: usize,
    end_utf16: usize,
) -> MappedEdit {
    let empty_edit = MappedEdit { start: start_utf16, end: start_utf16, new_text: String::new() };

    let orig = enumerate_chars(original);
    let fmt = enumerate_chars(formatted);
    if orig.len() != fmt.len() {
        return empty_edit;
    }

    let start_byte = utf16_to_byte(original, start_utf16);
    let end_byte = utf16_to_byte(original, end_utf16);

    let (Some(i0), Some(i1)) = (
        orig.iter().position(|c| c.position_in_file >= start_byte),
        orig.iter().rposition(|c| c.position_in_file < end_byte),
    ) else {
        return empty_edit;
    };
    if i1 < i0 {
        return empty_edit;
    }

    let orig_i0 = orig[i0].position_in_file;
    let fmt_i0 = fmt[i0].position_in_file;
    // Extending back to the line start is only whitespace, and so safe, when
    // the token leads its line in BOTH texts. One side alone is not enough:
    // the formatter may have merged this token onto a previous line.
    let (orig_start, fmt_start) =
        if is_line_leading(original, orig_i0) && is_line_leading(formatted, fmt_i0) {
            (line_start(original, orig_i0), line_start(formatted, fmt_i0))
        } else {
            (orig_i0, fmt_i0)
        };
    let orig_end = orig[i1].position_in_file + orig[i1].chr.len_utf8();
    let fmt_end = fmt[i1].position_in_file + fmt[i1].chr.len_utf8();

    MappedEdit {
        start: byte_to_utf16(original, orig_start),
        end: byte_to_utf16(original, orig_end),
        new_text: formatted[fmt_start..fmt_end].to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map<'a>(orig: &str, fmt: &str, sel: &str) -> MappedEdit {
        let start = orig.find(sel).expect("selection not found in original");
        // ASCII-only helper: byte offset == UTF-16 offset here.
        map_range(orig, fmt, start, start + sel.len())
    }

    #[test]
    fn maps_selection_onto_reformatted_whitespace() {
        let orig = "begin\nx:=1;\nend;";
        let fmt = "begin\n  x := 1;\nend;";
        let edit = map(orig, fmt, "x:=1");
        // First token on its line, so the new indentation is part of the edit.
        assert_eq!(edit.new_text, "  x := 1");
        assert_eq!(&orig[edit.start..edit.end], "x:=1");
    }

    #[test]
    fn first_line_indentation_is_reformatted() {
        // A mis-indented leading comment (8 spaces) must be reindented to 6.
        let orig = "        // note\n      code;";
        let fmt = "      // note\n      code;";
        let edit = map_range(orig, fmt, 0, orig.len());
        assert_eq!(edit.new_text, fmt);
        assert_eq!(edit.start, 0);
        assert_eq!(&orig[edit.start..edit.end], orig);
    }

    #[test]
    fn selection_boundaries_snap_to_non_whitespace() {
        let orig = "a := b  +  c;";
        let fmt = "a := b + c;";
        let start = orig.find("  +  ").unwrap();
        let edit = map_range(orig, fmt, start, start + "  +  ".len());
        assert_eq!(edit.new_text, "+");
        assert_eq!(&orig[edit.start..edit.end], "+");
    }

    #[test]
    fn selection_with_whitespace_before_and_after_text() {
        let orig = "  x:=1;  ";
        let fmt = "  x := 1;  ";
        let edit = map_range(orig, fmt, 0, orig.len());
        assert_eq!(edit.new_text, "  x := 1;");
        assert_eq!(&orig[edit.start..edit.end], "  x:=1;");
        assert_eq!(edit.start, 0);
    }

    #[test]
    fn selection_including_trailing_newline() {
        let orig = "x:=1;\n\n";
        let fmt = "x := 1;\n\n";
        let edit = map_range(orig, fmt, 0, orig.len());
        assert_eq!(edit.new_text, "x := 1;");
        assert_eq!(&orig[edit.start..edit.end], "x:=1;");
    }

    #[test]
    fn whitespace_only_selection_is_noop() {
        let orig = "a  :=  b;";
        let start = orig.find("  ").unwrap();
        let edit = map_range(orig, orig, start, start + 2);
        assert_eq!(edit.new_text, "");
        assert_eq!(edit.start, edit.end);
    }

    #[test]
    fn changed_non_ws_count_is_noop() {
        // A formatter that (illegally) dropped a token must not corrupt the file.
        let edit = map_range("x := 1;", "x := 1", 0, 3);
        assert_eq!(edit.new_text, "");
        assert_eq!(edit.start, edit.end);
    }

    #[test]
    fn utf16_offsets_survive_multibyte_chars() {
        // 'ä' is 2 UTF-8 bytes but 1 UTF-16 unit.
        let orig = "// ä\nx:=1;";
        let fmt = "// ä\nx := 1;";
        // VS Code offset of 'x': '/','/',' ','ä','\n' = 5 UTF-16 units.
        let start = 5;
        let end = start + "x:=1".len();
        let edit = map_range(orig, fmt, start, end);
        assert_eq!(edit.new_text, "x := 1");
        // Returned offsets are UTF-16: start must be 5, not the byte offset 6.
        assert_eq!(edit.start, 5);
    }

    #[test]
    fn identical_sequences_map_by_position_not_text() {
        // Selecting the *second* of two identical calls must not latch onto the
        // first: the mapping is positional.
        let orig = "abc();abc();";
        let fmt = "abc(); abc();";
        let second = orig.rfind("abc").unwrap();
        let edit = map_range(orig, fmt, second, second + "abc".len());
        assert_eq!(edit.new_text, "abc");
        assert_eq!(edit.start, 6);
        assert_eq!(&orig[edit.start..edit.end], "abc");
    }

    #[test]
    fn casing_change_preserves_mapping() {
        // Capitalization options change a char's value, not its ordinal.
        let orig = "begin end";
        let fmt = "Begin End";
        let edit = map(orig, fmt, "end");
        assert_eq!(edit.new_text, "End");
    }
}
