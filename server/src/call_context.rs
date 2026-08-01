//! Call-context detection for `textDocument/signatureHelp`: given the document
//! text and the cursor's byte offset, find the ENCLOSING unclosed `(` at the
//! current call depth and the ACTIVE PARAMETER index (the count of top-level
//! commas between that `(` and the cursor).
//!
//! ## Why a forward single pass (not a backward scan)
//!
//! Correctly skipping Pascal strings and comments is a LEXING problem: a `(`
//! inside `'…'` or `{…}` or `//…` is not a call; a `,` inside them is not an
//! argument separator. A backward scan cannot know whether a given `'` opens or
//! closes a string without re-deriving the whole preceding lexical state. So we
//! scan FORWARD from the document start to the cursor, tracking the exact
//! lexical state (in a string, in each comment flavor) and a stack of open
//! `(`/`[` bracket frames. At the cursor:
//! - the nearest enclosing `(` frame (skipping any `[` index frames above it) is
//!   the call; its recorded callee offset + a comma counter give the answer;
//! - a top-level `,` (one whose innermost frame is that `(`) increments the
//!   active-parameter count; commas inside nested `()`/`[]` or inside a
//!   string/comment never count (they are in a deeper frame or skipped).
//!
//! Pascal lexical forms handled:
//! - string / char literal `'…'` with `''` as an escaped quote (Pascal doubles
//!   the quote; there is no backslash escape);
//! - brace comment `{ … }` (and `{$…}` directives — treated as comments for the
//!   purpose of skipping punctuation, which is correct here);
//! - paren-star comment `(* … *)`;
//! - line comment `// …` to end of line.
//!
//! The result is text-only (`callee_offset`, `active_parameter`); the caller
//! resolves the callee via `symbol_at` and never fabricates a signature.

/// The enclosing-call facts at a cursor: the byte offset of the callee
/// identifier's LAST character-run start (the dotted `Obj.Method` name before
/// the `(`), and the zero-based active parameter index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallContext {
    /// A byte offset that falls INSIDE the callee identifier (its last dotted
    /// segment). `symbol_at` maps this onto the identifier occurrence. `None`
    /// enclosing call ⇒ the whole function returns `None`, so this always names a
    /// real identifier position.
    pub callee_offset: usize,
    /// Zero-based index of the argument the cursor sits in (top-level comma
    /// count between the enclosing `(` and the cursor).
    pub active_parameter: u32,
}

/// One open-bracket frame on the scan stack.
#[derive(Clone, Copy)]
struct Frame {
    /// The bracket that opened this frame: `(` or `[`.
    open: u8,
    /// For a `(` frame: the byte offset just AFTER the `(` (where its callee
    /// search begins) and the callee identifier offset resolved when the `(`
    /// was seen. For a `[` frame these are unused.
    callee_offset: Option<usize>,
    /// Top-level comma count seen so far in this frame.
    comma_count: u32,
}

/// The lexical state of the forward scan.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Code,
    /// Inside a `'…'` string/char literal.
    String,
    /// Inside a `{ … }` brace comment.
    BraceComment,
    /// Inside a `(* … *)` comment.
    ParenStarComment,
    /// Inside a `// …` line comment (ends at the next `\n`).
    LineComment,
}

/// Detect the enclosing call at `cursor` (a byte offset into `text`). Returns
/// the callee offset + active parameter, or `None` when the cursor is not inside
/// any call's argument list (no unclosed `(` at cursor depth, or the cursor sits
/// in a string/comment).
///
/// `cursor` is clamped to `text.len()`; an offset that splits a UTF-8 boundary
/// is treated as its enclosing char's start (the scan only inspects ASCII
/// punctuation, so multibyte content is skipped as opaque identifier/text).
pub fn enclosing_call(text: &str, cursor: usize) -> Option<CallContext> {
    let bytes = text.as_bytes();
    let end = cursor.min(bytes.len());

    let mut mode = Mode::Code;
    let mut stack: Vec<Frame> = Vec::new();
    let mut index = 0usize;

    while index < end {
        let byte = bytes[index];
        match mode {
            Mode::String => {
                if byte == b'\'' {
                    // A doubled quote `''` is an escaped quote INSIDE the string,
                    // not a terminator — skip both and stay in the string.
                    if index + 1 < end && bytes[index + 1] == b'\'' {
                        index += 2;
                        continue;
                    }
                    mode = Mode::Code;
                }
                index += 1;
            }
            Mode::BraceComment => {
                if byte == b'}' {
                    mode = Mode::Code;
                }
                index += 1;
            }
            Mode::ParenStarComment => {
                if byte == b'*' && index + 1 < end && bytes[index + 1] == b')' {
                    mode = Mode::Code;
                    index += 2;
                    continue;
                }
                index += 1;
            }
            Mode::LineComment => {
                if byte == b'\n' {
                    mode = Mode::Code;
                }
                index += 1;
            }
            Mode::Code => {
                match byte {
                    b'\'' => {
                        mode = Mode::String;
                        index += 1;
                    }
                    b'{' => {
                        mode = Mode::BraceComment;
                        index += 1;
                    }
                    b'/' if index + 1 < end && bytes[index + 1] == b'/' => {
                        mode = Mode::LineComment;
                        index += 2;
                    }
                    b'(' if index + 1 < end && bytes[index + 1] == b'*' => {
                        mode = Mode::ParenStarComment;
                        index += 2;
                    }
                    b'(' => {
                        // A call `(` — resolve its callee (the identifier ending
                        // just before, skipping whitespace). A `(` with no
                        // preceding identifier (a grouping paren `(a + b)`) has
                        // `callee_offset = None` and never yields a signature.
                        let callee_offset = callee_before(bytes, index);
                        stack.push(Frame {
                            open: b'(',
                            callee_offset,
                            comma_count: 0,
                        });
                        index += 1;
                    }
                    b'[' => {
                        stack.push(Frame {
                            open: b'[',
                            callee_offset: None,
                            comma_count: 0,
                        });
                        index += 1;
                    }
                    b')' => {
                        // Close the nearest `(` frame. A `)` with no matching `(`
                        // (malformed) is ignored. Pop any stray `[` frames above
                        // a `(` only if they truly nest; a mismatched close just
                        // pops the top frame.
                        pop_matching(&mut stack, b'(');
                        index += 1;
                    }
                    b']' => {
                        pop_matching(&mut stack, b'[');
                        index += 1;
                    }
                    b',' => {
                        if let Some(frame) = stack.last_mut() {
                            // A comma counts only at the top level of ITS frame;
                            // being on the stack already means it is not inside a
                            // deeper bracket. Only `(` frames track arguments.
                            if frame.open == b'(' {
                                frame.comma_count += 1;
                            }
                        }
                        index += 1;
                    }
                    _ => {
                        index += 1;
                    }
                }
            }
        }
    }

    // The cursor sits inside a string/comment → no signature (never fabricate
    // one for text the user is typing as a literal/comment).
    if mode != Mode::Code {
        return None;
    }

    // The enclosing call is the nearest `(` frame still open at the cursor.
    for frame in stack.iter().rev() {
        if frame.open == b'(' {
            let callee_offset = frame.callee_offset?;
            return Some(CallContext {
                callee_offset,
                active_parameter: frame.comma_count,
            });
        }
        // A `[` frame between the cursor and the enclosing `(` means the cursor
        // is inside an index expression, not the call's argument list — the call
        // is still the outer `(`, but a comma at THIS depth is an index
        // separator, not an argument. Keep scanning outward: the `(` frame's own
        // comma_count already excludes commas that occurred inside this `[`
        // (they incremented the `[` frame, which does not count). So continue.
    }
    None
}

/// Close the nearest frame opened by `open`, popping any mismatched frames above
/// it (defensive against malformed bracketing). If no matching frame exists, pop
/// the top frame (best-effort recovery) or nothing when empty.
fn pop_matching(stack: &mut Vec<Frame>, open: u8) {
    if let Some(position) = stack.iter().rposition(|frame| frame.open == open) {
        stack.truncate(position);
    } else if !stack.is_empty() {
        stack.pop();
    }
}

/// The byte offset of the callee identifier that precedes the `(` at
/// `paren_index`, skipping intervening whitespace. Returns an offset INSIDE the
/// identifier's LAST dotted segment (so `symbol_at` resolves the member name of
/// `Obj.Method(`). `None` when no identifier precedes the `(` (a grouping paren,
/// or a `)(` / `](` chain we do not treat as a named call).
fn callee_before(bytes: &[u8], paren_index: usize) -> Option<usize> {
    // Skip whitespace immediately before the `(`.
    let mut position = paren_index;
    while position > 0 && bytes[position - 1].is_ascii_whitespace() {
        position -= 1;
    }
    // `position` is now one past the last identifier byte. Walk back over the
    // identifier run (letters, digits, `_`, and `.`/`&` for dotted/escaped
    // names). Must end on an identifier char, else no callee.
    let identifier_end = position;
    if identifier_end == 0 || !is_identifier_byte(bytes[identifier_end - 1]) {
        return None;
    }
    let mut start = identifier_end;
    while start > 0 && (is_identifier_byte(bytes[start - 1]) || bytes[start - 1] == b'.') {
        start -= 1;
    }
    // The callee we resolve is the LAST dotted segment (`Method` in
    // `Obj.Method`), because `symbol_at` on that offset yields the member. Find
    // the last `.` within [start, identifier_end).
    let last_segment_start = bytes[start..identifier_end]
        .iter()
        .rposition(|&byte| byte == b'.')
        .map(|relative| start + relative + 1)
        .unwrap_or(start);
    // Point at the middle of the last segment (any offset inside it works for
    // span_covers). Its first byte is safe and unambiguous.
    if last_segment_start < identifier_end {
        Some(last_segment_start)
    } else {
        None
    }
}

/// An identifier byte: ASCII alphanumeric, `_`, or `&` (the reserved-word
/// escape). Multibyte (non-ASCII) identifier characters are also accepted so a
/// Unicode identifier is not split; the scan only needs to stay inside the run.
fn is_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'&' || byte >= 0x80
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: place the cursor at the `|` marker in `marked`, returning the
    /// clean text and the cursor byte offset.
    fn at_marker(marked: &str) -> (String, usize) {
        let cursor = marked.find('|').expect("marker");
        let text = marked.replacen('|', "", 1);
        (text, cursor)
    }

    fn call(marked: &str) -> Option<CallContext> {
        let (text, cursor) = at_marker(marked);
        enclosing_call(&text, cursor)
    }

    #[test]
    fn simple_call_first_argument() {
        let context = call("Foo(|").expect("inside Foo's args");
        assert_eq!(context.active_parameter, 0);
        // callee offset points inside `Foo`
        let (text, _) = at_marker("Foo(|");
        assert_eq!(&text[context.callee_offset..context.callee_offset + 3], "Foo");
    }

    #[test]
    fn second_argument_after_comma() {
        let context = call("Foo(a, |").expect("inside args");
        assert_eq!(context.active_parameter, 1);
    }

    #[test]
    fn third_argument() {
        let context = call("Foo(a, b, |").expect("inside args");
        assert_eq!(context.active_parameter, 2);
    }

    #[test]
    fn nested_call_inner_context_wins() {
        // Cursor inside the INNER call Bar(...) — the enclosing call is Bar, at
        // its first argument, NOT Foo.
        let (text, cursor) = at_marker("Foo(a, Bar(|");
        let context = enclosing_call(&text, cursor).expect("inside Bar");
        assert_eq!(context.active_parameter, 0);
        assert_eq!(&text[context.callee_offset..context.callee_offset + 3], "Bar");
    }

    #[test]
    fn nested_call_closed_returns_to_outer() {
        // The inner call is CLOSED; the cursor is back in Foo's argument list,
        // now on Foo's second argument (the comma after the inner call).
        let (text, cursor) = at_marker("Foo(Bar(x), |");
        let context = enclosing_call(&text, cursor).expect("back in Foo");
        assert_eq!(&text[context.callee_offset..context.callee_offset + 3], "Foo");
        assert_eq!(context.active_parameter, 1, "Foo's second argument");
    }

    #[test]
    fn comma_inside_nested_parens_does_not_count() {
        // The commas inside Bar(x, y) belong to Bar; at the cursor (Foo's args,
        // after the inner call) Foo has seen exactly ONE top-level comma.
        let (text, cursor) = at_marker("Foo(Bar(x, y), |");
        let context = enclosing_call(&text, cursor).expect("in Foo");
        assert_eq!(&text[context.callee_offset..context.callee_offset + 3], "Foo");
        assert_eq!(context.active_parameter, 1);
    }

    #[test]
    fn comma_inside_string_does_not_count() {
        // The comma inside the string literal must NOT advance the parameter.
        let context = call("Foo('a, b', |").expect("in Foo");
        assert_eq!(
            context.active_parameter, 1,
            "only the real comma after the string counts"
        );
    }

    #[test]
    fn paren_inside_string_is_not_a_call() {
        // The `(` inside the string is not a call open; the enclosing call is Foo.
        let (text, cursor) = at_marker("Foo('x (y', |");
        let context = enclosing_call(&text, cursor).expect("still in Foo");
        assert_eq!(&text[context.callee_offset..context.callee_offset + 3], "Foo");
        assert_eq!(context.active_parameter, 1);
    }

    #[test]
    fn doubled_quote_escape_inside_string() {
        // `''` is an escaped quote — the string does not end there, so the `(`
        // and `,` inside stay skipped.
        let context = call("Foo('it''s (a, b)', |").expect("in Foo");
        assert_eq!(
            context.active_parameter, 1,
            "the doubled quote keeps us in the string until the real close"
        );
    }

    #[test]
    fn comma_inside_brace_comment_does_not_count() {
        let context = call("Foo(a { , , , } , |").expect("in Foo");
        assert_eq!(
            context.active_parameter, 1,
            "commas inside a brace comment are skipped"
        );
    }

    #[test]
    fn comma_inside_line_comment_does_not_count() {
        // The line comment eats the comma; the real comma is on the next line.
        let context = call("Foo(a // , , ,\n, |").expect("in Foo");
        assert_eq!(context.active_parameter, 1);
    }

    #[test]
    fn comma_inside_paren_star_comment_does_not_count() {
        let context = call("Foo(a (* , , *) , |").expect("in Foo");
        assert_eq!(context.active_parameter, 1);
    }

    #[test]
    fn paren_star_comment_open_is_not_a_call() {
        // `(*` opens a comment, not a call — the enclosing call is Foo.
        let (text, cursor) = at_marker("Foo(a (* nested ) call *), |");
        let context = enclosing_call(&text, cursor).expect("in Foo");
        assert_eq!(&text[context.callee_offset..context.callee_offset + 3], "Foo");
        assert_eq!(context.active_parameter, 1);
    }

    #[test]
    fn multi_line_call() {
        // A call spanning several lines; the active parameter counts commas
        // across the newlines.
        let (text, cursor) = at_marker("Foo(\n  a,\n  b,\n  |\n)");
        let context = enclosing_call(&text, cursor).expect("in Foo");
        assert_eq!(context.active_parameter, 2);
    }

    #[test]
    fn dotted_callee_resolves_last_segment() {
        // `Obj.Method(` — the callee offset must land in `Method` (the member),
        // so symbol_at resolves the method, not the receiver.
        let (text, context) = {
            let (text, cursor) = at_marker("Obj.Method(|");
            (text.clone(), enclosing_call(&text, cursor).expect("in Method"))
        };
        assert!(
            text[context.callee_offset..].starts_with("Method"),
            "callee offset must point at the last dotted segment `Method`: {:?}",
            &text[context.callee_offset..]
        );
    }

    #[test]
    fn no_enclosing_call_is_none() {
        // Cursor not inside any call.
        assert!(call("x := a + b|").is_none());
        assert!(call("|").is_none());
        // A closed call — cursor is after the `)`, not inside any argument list.
        assert!(call("Foo(a, b)|").is_none());
    }

    #[test]
    fn grouping_paren_without_callee_is_none() {
        // `(a + b)` is a grouping paren, not a named call — no callee, no
        // signature (never fabricate one for a bare paren).
        assert!(call("x := (a, |").is_none());
    }

    #[test]
    fn index_brackets_do_not_start_a_call_but_commas_inside_are_skipped() {
        // `Arr[i, j]` is an index; a `,` inside it must not count as a Foo
        // argument. At the cursor (Foo's args after the index) Foo has one
        // top-level comma.
        let (text, cursor) = at_marker("Foo(Arr[i, j], |");
        let context = enclosing_call(&text, cursor).expect("in Foo");
        assert_eq!(&text[context.callee_offset..context.callee_offset + 3], "Foo");
        assert_eq!(context.active_parameter, 1);
    }

    #[test]
    fn cursor_inside_string_yields_none() {
        // The cursor itself sits inside an unterminated string — no signature.
        assert!(call("Foo('abc|").is_none());
    }

    #[test]
    fn cursor_inside_comment_yields_none() {
        assert!(call("Foo(a { comment |").is_none());
        assert!(call("Foo(a // comment |").is_none());
    }
}
