//! `textDocument/semanticTokens` support: the LEGEND, the parser
//! `SemanticKind → (typeIndex, modifierBitset)` mapping, and the LSP DELTA
//! ENCODER — all in one place so the advertised legend can never drift from the
//! kinds actually emitted.
//!
//! ## Additive, never-wrong
//!
//! LSP semantic tokens are ADDITIVE over the editor's TextMate grammar. The
//! parser query already emits a token ONLY when its classification is certain
//! (an unresolved identifier is omitted). This module does not re-classify; it
//! only translates + encodes, so that guarantee is preserved end to end.
//!
//! ## The encoding is where bugs live
//!
//! An LSP semantic token CANNOT span lines and is delta-encoded relative to the
//! previous token, with `length`/`deltaStartChar` in UTF-16 code units. The
//! fiddly, correctness-critical parts, each handled here and tested:
//!
//! - **Single-line split.** A multi-line source span (a block comment `{ … }` /
//!   `(* … *)` across lines, a multi-line string) is split into ONE token PER
//!   LINE it covers. An unsplit multi-line token corrupts the ENTIRE delta
//!   stream after it, so this is the load-bearing step.
//! - **UTF-16 units.** `length` and `deltaStartChar` are computed through the
//!   document's [`LineIndex`] (the same UTF-16 mapping the rest of the server
//!   uses), so a multibyte or astral (surrogate-pair) character counts
//!   correctly.
//! - **Sorted + relative.** Tokens are sorted by `(line, startChar)` and encoded
//!   as `(deltaLine, deltaStartChar-relative-to-the-previous-token, length,
//!   typeIndex, modifierBitset)`.
//! - **Never panic.** A span that cannot be mapped (out of the document, a
//!   zero-length or inverted range) is SKIPPED, never encoded as a bad delta and
//!   never a panic.

use tower_lsp::lsp_types::{
    SemanticToken as LspSemanticToken, SemanticTokenModifier, SemanticTokenType, SemanticTokensLegend,
};

use delphi_parser::driver::ProjectSession;
use delphi_parser::query::{SemanticKind, SemanticModifiers, SemanticToken};

use crate::positions::LineIndex;

// ─── Legend (the single source of truth) ─────────────────────────────────────
//
// The ORDER of these arrays IS the legend the server advertises: a token's
// `token_type` / `token_modifiers_bitset` in the delta stream are INDICES into
// these arrays. `type_index` / the modifier bit builder below map the parser's
// `SemanticKind` / `SemanticModifiers` onto these indices, so the mapping and the
// advertised legend are defined together and cannot drift.

/// Ordered semantic token TYPES. A token's `token_type` field is an index here.
const TOKEN_TYPES: &[SemanticTokenType] = &[
    SemanticTokenType::NAMESPACE,  // 0
    SemanticTokenType::TYPE,       // 1
    SemanticTokenType::CLASS,      // 2
    SemanticTokenType::INTERFACE,  // 3
    SemanticTokenType::ENUM,       // 4
    SemanticTokenType::ENUM_MEMBER,// 5
    SemanticTokenType::PARAMETER,  // 6
    SemanticTokenType::VARIABLE,   // 7
    SemanticTokenType::PROPERTY,   // 8
    SemanticTokenType::FUNCTION,   // 9
    SemanticTokenType::METHOD,     // 10
    SemanticTokenType::KEYWORD,    // 11
    SemanticTokenType::COMMENT,    // 12
    SemanticTokenType::STRING,     // 13
    SemanticTokenType::NUMBER,     // 14
    SemanticTokenType::OPERATOR,   // 15
    SemanticTokenType::MACRO,      // 16
    // `Field`/`Constant` have no dedicated standard LSP token type; they map onto
    // `property` and `variable`-adjacent semantics via the closest standard type.
    // Rather than overload those, we add explicit entries so the mapping stays
    // 1:1 and self-documenting: a field → `property`-family is wrong (a field is
    // not a property), so we use `variable` for a field and `enumMember`-adjacent
    // is likewise wrong for a constant. The standard `type` list has no `field`
    // or `constant`; VS Code's default theme colors `variable` for both, which is
    // the closest correct-not-wrong choice. They reuse existing indices below.
];

/// Ordered semantic token MODIFIERS. A token's `token_modifiers_bitset` sets bit
/// `i` for the modifier at index `i` here.
const TOKEN_MODIFIERS: &[SemanticTokenModifier] = &[
    SemanticTokenModifier::DECLARATION, // bit 0
];

/// The legend to advertise in the server capabilities. Built from the SAME
/// ordered arrays the encoder indexes into.
pub fn legend() -> SemanticTokensLegend {
    SemanticTokensLegend {
        token_types: TOKEN_TYPES.to_vec(),
        token_modifiers: TOKEN_MODIFIERS.to_vec(),
    }
}

/// The legend index of a [`SemanticKind`] — the token's `token_type` field.
///
/// `Field` and `Constant` have no dedicated standard LSP token type; they map to
/// the closest correct-not-wrong standard type (`variable`) rather than an
/// unrelated one — a field colored as a property would be a WRONG semantic, while
/// `variable` (VS Code's default field/constant color) is a safe, additive
/// choice. Every other kind has an exact standard type.
fn type_index(kind: SemanticKind) -> u32 {
    match kind {
        SemanticKind::Namespace => 0,
        SemanticKind::Type => 1,
        SemanticKind::Class => 2,
        SemanticKind::Interface => 3,
        SemanticKind::Enum => 4,
        SemanticKind::EnumMember => 5,
        SemanticKind::Parameter => 6,
        SemanticKind::Variable => 7,
        SemanticKind::Property => 8,
        SemanticKind::Function => 9,
        SemanticKind::Method => 10,
        SemanticKind::Keyword => 11,
        SemanticKind::Comment => 12,
        SemanticKind::String => 13,
        SemanticKind::Number => 14,
        SemanticKind::Operator => 15,
        SemanticKind::Macro => 16,
        // No standard `field`/`constant` type — closest correct is `variable`.
        SemanticKind::Field => 7,
        SemanticKind::Constant => 7,
    }
}

/// The `token_modifiers_bitset` for a [`SemanticModifiers`] set — bit 0 is
/// `declaration`, matching [`TOKEN_MODIFIERS`].
fn modifier_bitset(modifiers: SemanticModifiers) -> u32 {
    let mut bits = 0u32;
    if modifiers.contains(SemanticModifiers::DECLARATION) {
        bits |= 1 << 0;
    }
    bits
}

// ─── Encoding ────────────────────────────────────────────────────────────────

/// An intermediate, ALREADY-SINGLE-LINE token: its start position (line, UTF-16
/// character), its UTF-16 length, and the legend indices. Produced by splitting a
/// (possibly multi-line) parser token per line, then sorted and delta-encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FlatToken {
    line: u32,
    start_character: u32,
    length: u32,
    token_type: u32,
    token_modifiers_bitset: u32,
}

/// Encode parser [`SemanticToken`]s into the LSP delta stream, mapping each span
/// through `index` (the requesting document's own [`LineIndex`], for UTF-16
/// positions/lengths).
///
/// Steps: split each token into single-line pieces (a multi-line span becomes one
/// token per line it covers), drop any unmappable/empty piece, sort by
/// `(line, startChar)`, then delta-encode. Never panics; an empty input (or all
/// pieces unmappable) yields an empty stream.
pub fn encode(tokens: &[SemanticToken], index: &LineIndex) -> Vec<LspSemanticToken> {
    let mut flat: Vec<FlatToken> = Vec::new();
    for token in tokens {
        split_token_per_line(token, index, &mut flat);
    }

    // Sort by (line, startChar) so the delta encoding is monotonic. A stable sort
    // keeps same-position pieces in source order (there should be none by
    // construction — lexer + declaration spans do not overlap).
    flat.sort_by_key(|token| (token.line, token.start_character));

    let mut encoded: Vec<LspSemanticToken> = Vec::with_capacity(flat.len());
    let mut previous_line = 0u32;
    let mut previous_start = 0u32;
    for token in flat {
        let delta_line = token.line - previous_line;
        // deltaStartChar is relative to the previous token ONLY when on the same
        // line; a new line resets the reference to absolute (start of line).
        let delta_start = if delta_line == 0 {
            token.start_character - previous_start
        } else {
            token.start_character
        };
        encoded.push(LspSemanticToken {
            delta_line,
            delta_start,
            length: token.length,
            token_type: token.token_type,
            token_modifiers_bitset: token.token_modifiers_bitset,
        });
        previous_line = token.line;
        previous_start = token.start_character;
    }
    encoded
}

/// Split ONE parser token's byte span into single-line [`FlatToken`] pieces
/// (UTF-16 positions/lengths via `index`), appending them to `out`. A span
/// confined to one line yields one piece; a multi-line span (block comment /
/// multi-line string) yields one piece per line it covers, each running from its
/// column to the end of that line's content (interior lines from column 0). A
/// span that maps to nothing (out of range, empty) contributes no piece — never a
/// panic.
fn split_token_per_line(token: &SemanticToken, index: &LineIndex, out: &mut Vec<FlatToken>) {
    let start_byte = token.location.span.start as usize;
    let end_byte = token.location.span.end as usize;
    // A zero-length or inverted span is not a real token — skip (never a bad
    // delta). Byte offsets past the document clamp inside `LineIndex`.
    if end_byte <= start_byte {
        return;
    }

    let start_position = index.position_of(start_byte);
    let end_position = index.position_of(end_byte);
    let token_type = type_index(token.token_type);
    let token_modifiers_bitset = modifier_bitset(token.modifiers);

    if start_position.line == end_position.line {
        // Single-line: one piece from start to end column.
        let length = end_position.character.saturating_sub(start_position.character);
        if length == 0 {
            return;
        }
        out.push(FlatToken {
            line: start_position.line,
            start_character: start_position.character,
            length,
            token_type,
            token_modifiers_bitset,
        });
        return;
    }

    // Multi-line: emit one piece per covered line. The FIRST line runs from the
    // start column to that line's UTF-16 content end; INTERIOR lines run the whole
    // line (column 0 to content end); the LAST line runs from column 0 to the end
    // column. This is the split that keeps the delta stream valid.
    for line in start_position.line..=end_position.line {
        let piece_start_character = if line == start_position.line {
            start_position.character
        } else {
            0
        };
        let piece_end_character = if line == end_position.line {
            end_position.character
        } else {
            line_utf16_length(index, line)
        };
        let length = piece_end_character.saturating_sub(piece_start_character);
        if length == 0 {
            // An empty line inside a multi-line span (or the last line ending at
            // column 0) contributes no visible token — skip it, keep the stream
            // tight. This is NOT a corruption: a zero-length token is meaningless.
            continue;
        }
        out.push(FlatToken {
            line,
            start_character: piece_start_character,
            length,
            token_type,
            token_modifiers_bitset,
        });
    }
}

/// The UTF-16 length of `line`'s addressable content (excluding its line
/// terminator). Derived from the [`LineIndex`]: a character column far past the
/// line clamps to the line's content-end BYTE; mapping that byte back to a
/// position gives the content-end COLUMN, which is exactly the line's UTF-16
/// length. `u32::MAX` never lands inside a real character, so the clamp is exact.
fn line_utf16_length(index: &LineIndex, line: u32) -> u32 {
    let content_end_byte =
        index.offset_of(tower_lsp::lsp_types::Position { line, character: u32::MAX });
    index.position_of(content_end_byte).character
}

/// Resolve `textDocument/semanticTokens/full` for `unit_key`: query the parser
/// for classified tokens, then encode them through `index` (the requesting
/// document's own line index). Factored out of the handler so the composition
/// (query → encode) is unit-testable without a live LSP `Client`.
pub fn resolve_semantic_tokens(
    session: &ProjectSession,
    unit_key: delphi_parser::context::Identifier,
    index: &LineIndex,
) -> Vec<LspSemanticToken> {
    let tokens = session.semantic_tokens(unit_key);
    encode(&tokens, index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use delphi_parser::meta::{CodeLocation, FileId, Span};

    /// A parser token over `[start, end)` bytes with the given kind/modifiers,
    /// anchored in a throwaway `FileId` (the encoder only reads the span, mapping
    /// it through the supplied `LineIndex`, so the file id is irrelevant here).
    fn token(
        start: u32,
        end: u32,
        kind: SemanticKind,
        modifiers: SemanticModifiers,
    ) -> SemanticToken {
        SemanticToken {
            location: CodeLocation {
                file: FileId(0),
                span: Span { start, end },
            },
            token_type: kind,
            modifiers,
        }
    }

    #[test]
    fn empty_input_yields_empty_stream() {
        let index = LineIndex::new("unit X;".to_string());
        assert!(encode(&[], &index).is_empty());
    }

    #[test]
    fn single_line_tokens_delta_encode_relative_and_sorted() {
        // "unit Foo;"  — `unit` (kw, 0..4), `Foo` (namespace, 5..8).
        let text = "unit Foo;";
        let index = LineIndex::new(text.to_string());
        // Deliberately pass OUT OF ORDER to prove the encoder sorts.
        let tokens = vec![
            token(5, 8, SemanticKind::Namespace, SemanticModifiers::NONE),
            token(0, 4, SemanticKind::Keyword, SemanticModifiers::NONE),
        ];
        let encoded = encode(&tokens, &index);
        assert_eq!(encoded.len(), 2);
        // First token: `unit` at line 0, char 0, length 4, type = keyword index.
        assert_eq!(encoded[0].delta_line, 0);
        assert_eq!(encoded[0].delta_start, 0);
        assert_eq!(encoded[0].length, 4);
        assert_eq!(encoded[0].token_type, type_index(SemanticKind::Keyword));
        // Second token: `Foo`, same line, deltaStart = 5 - 0 = 5, length 3.
        assert_eq!(encoded[1].delta_line, 0);
        assert_eq!(encoded[1].delta_start, 5);
        assert_eq!(encoded[1].length, 3);
        assert_eq!(encoded[1].token_type, type_index(SemanticKind::Namespace));
    }

    #[test]
    fn declaration_modifier_sets_bit_zero() {
        let text = "TFoo";
        let index = LineIndex::new(text.to_string());
        let tokens = vec![token(
            0,
            4,
            SemanticKind::Class,
            SemanticModifiers::DECLARATION,
        )];
        let encoded = encode(&tokens, &index);
        assert_eq!(encoded.len(), 1);
        assert_eq!(encoded[0].token_modifiers_bitset, 1 << 0);
        assert_eq!(encoded[0].token_type, type_index(SemanticKind::Class));
    }

    #[test]
    fn multi_line_block_comment_splits_into_per_line_tokens() {
        // A block comment spanning three lines. Byte layout:
        // line0: "{ start"   (0..7), then '\n' at 7
        // line1: "middle"    (8..14), then '\n' at 14
        // line2: "end }"     (15..20)
        // The whole comment span is 0..20.
        let text = "{ start\nmiddle\nend }";
        let index = LineIndex::new(text.to_string());
        let tokens = vec![token(0, 20, SemanticKind::Comment, SemanticModifiers::NONE)];
        let encoded = encode(&tokens, &index);
        // One token PER line the comment covers → 3 tokens, none spanning a line.
        assert_eq!(encoded.len(), 3, "a 3-line comment splits into 3 tokens: {encoded:?}");
        let comment_type = type_index(SemanticKind::Comment);
        assert!(encoded.iter().all(|token| token.token_type == comment_type));

        // Line 0: from char 0, length = "{ start" = 7 UTF-16 units.
        assert_eq!((encoded[0].delta_line, encoded[0].delta_start, encoded[0].length), (0, 0, 7));
        // Line 1: deltaLine 1, absolute start 0, length = "middle" = 6.
        assert_eq!((encoded[1].delta_line, encoded[1].delta_start, encoded[1].length), (1, 0, 6));
        // Line 2: deltaLine 1, absolute start 0, length = "end }" = 5.
        assert_eq!((encoded[2].delta_line, encoded[2].delta_start, encoded[2].length), (1, 0, 5));
    }

    #[test]
    fn multi_line_string_splits_into_per_line_tokens() {
        // The per-line split is KIND-AGNOSTIC: a multi-line `String` token must
        // split exactly like the multi-line comment case above (one token per
        // line, none spanning a line). This closes the coverage gap the spec
        // called out. Byte layout mirrors the comment test:
        // line0: "'start"   (0..6), then '\n' at 6
        // line1: "middle"   (7..13), then '\n' at 13
        // line2: "end'"     (14..18)
        // The whole string span is 0..18.
        let text = "'start\nmiddle\nend'";
        let index = LineIndex::new(text.to_string());
        let tokens = vec![token(0, 18, SemanticKind::String, SemanticModifiers::NONE)];
        let encoded = encode(&tokens, &index);
        // One token PER line the string covers → 3 tokens, none spanning a line.
        assert_eq!(encoded.len(), 3, "a 3-line string splits into 3 tokens: {encoded:?}");
        let string_type = type_index(SemanticKind::String);
        assert!(encoded.iter().all(|token| token.token_type == string_type));

        // Line 0: from char 0, length = "'start" = 6 UTF-16 units.
        assert_eq!((encoded[0].delta_line, encoded[0].delta_start, encoded[0].length), (0, 0, 6));
        // Line 1: deltaLine 1, absolute start 0, length = "middle" = 6.
        assert_eq!((encoded[1].delta_line, encoded[1].delta_start, encoded[1].length), (1, 0, 6));
        // Line 2: deltaLine 1, absolute start 0, length = "end'" = 4.
        assert_eq!((encoded[2].delta_line, encoded[2].delta_start, encoded[2].length), (1, 0, 4));
    }

    #[test]
    fn utf16_length_and_delta_after_multibyte_and_astral() {
        // Line: "// ä😀 x" then a keyword-ish token after the emoji.
        // '//' comment covers the whole line; but here we test a token AFTER a
        // multibyte + astral char to prove UTF-16 columns.
        // bytes: '/'=0 '/'=1 ' '=2 'ä'=3..5 '😀'=5..9 ' '=9 'x'=10..11
        // UTF-16: '/'=1 '/'=1 ' '=1 'ä'=1 '😀'=2 ' '=1 → 'x' is at UTF-16 col 7.
        let text = "// ä😀 x";
        let index = LineIndex::new(text.to_string());
        // Token over the comment prefix "// ä😀" (bytes 0..9) then 'x' (10..11).
        let tokens = vec![
            token(0, 9, SemanticKind::Comment, SemanticModifiers::NONE),
            token(10, 11, SemanticKind::Keyword, SemanticModifiers::NONE),
        ];
        let encoded = encode(&tokens, &index);
        assert_eq!(encoded.len(), 2);
        // The comment "// ä😀": UTF-16 length = 3 (`// `) + 1 (ä) + 2 (😀) = 6.
        assert_eq!(encoded[0].delta_line, 0);
        assert_eq!(encoded[0].delta_start, 0);
        assert_eq!(encoded[0].length, 6);
        // 'x' is at UTF-16 column 7; deltaStart relative to the comment's start
        // column (0) = 7; length 1.
        assert_eq!(encoded[1].delta_line, 0);
        assert_eq!(encoded[1].delta_start, 7);
        assert_eq!(encoded[1].length, 1);
    }

    #[test]
    fn zero_length_and_inverted_spans_are_skipped() {
        let text = "unit X;";
        let index = LineIndex::new(text.to_string());
        let tokens = vec![
            token(3, 3, SemanticKind::Keyword, SemanticModifiers::NONE), // zero-length
            token(5, 4, SemanticKind::Keyword, SemanticModifiers::NONE), // inverted
        ];
        assert!(encode(&tokens, &index).is_empty(), "degenerate spans produce no tokens");
    }

    #[test]
    fn out_of_range_span_clamps_not_panics() {
        let text = "unit X;";
        let index = LineIndex::new(text.to_string());
        // A span far past the document end: LineIndex clamps both offsets to EOF,
        // yielding a zero-length piece that is skipped — never a panic.
        let tokens = vec![token(1000, 2000, SemanticKind::Keyword, SemanticModifiers::NONE)];
        assert!(encode(&tokens, &index).is_empty());
    }

    /// End-to-end through the SERVER composition (parser query → encode): parse a
    /// small unit in a fallback session, resolve its semantic tokens, and prove
    /// the stream is non-empty, valid, and never spans a line — WITHOUT a live LSP
    /// `Client`. This exercises the exact `resolve_semantic_tokens` steps the
    /// handler runs inside `spawn_blocking`.
    #[test]
    fn resolve_semantic_tokens_end_to_end_produces_valid_stream() {
        use crate::session::build_fallback_session_for_test;

        let mut session = build_fallback_session_for_test();
        let text = "unit Demo;\ninterface\n{ note }\ntype TThing = class\n  FValue: Integer;\nend;\nimplementation\nend.";
        let index = LineIndex::new(text.to_string());
        let directory = std::env::temp_dir().join("ddk-server-semantic-e2e");
        std::fs::create_dir_all(&directory).unwrap();
        let (_, meta) = session
            .parse_buffer(directory.join("Demo.pas"), index.text())
            .expect("buffer parses");
        let unit_key = meta.expect("unit meta").name();

        let encoded = resolve_semantic_tokens(&session, unit_key, &index);
        assert!(!encoded.is_empty(), "a real unit produces semantic tokens");
        // No encoded token may have a negative/absurd delta or a zero length.
        assert!(encoded.iter().all(|token| token.length > 0));
        // Reconstruct absolute positions from the deltas and assert monotonic
        // (line, startChar) ordering with no line-spanning token — the delta
        // stream is well-formed.
        let mut line = 0u32;
        let mut character = 0u32;
        let mut previous = (0u32, 0u32);
        for (position, token) in encoded.iter().enumerate() {
            line += token.delta_line;
            character = if token.delta_line == 0 {
                character + token.delta_start
            } else {
                token.delta_start
            };
            let current = (line, character);
            if position > 0 {
                assert!(
                    current >= previous,
                    "tokens are sorted and non-overlapping in the delta stream: {current:?} < {previous:?}"
                );
            }
            previous = current;
        }
        // The `{ note }` comment maps to a comment-typed token.
        let comment_type = type_index(SemanticKind::Comment);
        assert!(
            encoded.iter().any(|token| token.token_type == comment_type),
            "the block comment is classified as a comment token"
        );
    }

    #[test]
    fn legend_and_type_index_are_consistent() {
        // Every SemanticKind maps to a valid index into the advertised legend.
        let legend = legend();
        let kinds = [
            SemanticKind::Namespace,
            SemanticKind::Type,
            SemanticKind::Class,
            SemanticKind::Interface,
            SemanticKind::Enum,
            SemanticKind::EnumMember,
            SemanticKind::Parameter,
            SemanticKind::Variable,
            SemanticKind::Property,
            SemanticKind::Function,
            SemanticKind::Method,
            SemanticKind::Keyword,
            SemanticKind::Comment,
            SemanticKind::String,
            SemanticKind::Number,
            SemanticKind::Operator,
            SemanticKind::Macro,
            SemanticKind::Field,
            SemanticKind::Constant,
        ];
        for kind in kinds {
            assert!(
                (type_index(kind) as usize) < legend.token_types.len(),
                "{kind:?} maps to an in-range legend index"
            );
        }

        // Range-checking alone would pass even if the legend and the index map
        // were SWAPPED (a Class↔Interface swap mis-colors everything while every
        // index stays in range). Assert the legend entry AT each kind's index IS
        // the semantically-correct `SemanticTokenType` — this is the guard that
        // actually catches a legend/index swap.
        let expected: &[(SemanticKind, SemanticTokenType)] = &[
            (SemanticKind::Namespace, SemanticTokenType::NAMESPACE),
            (SemanticKind::Type, SemanticTokenType::TYPE),
            (SemanticKind::Class, SemanticTokenType::CLASS),
            (SemanticKind::Interface, SemanticTokenType::INTERFACE),
            (SemanticKind::Enum, SemanticTokenType::ENUM),
            (SemanticKind::EnumMember, SemanticTokenType::ENUM_MEMBER),
            (SemanticKind::Parameter, SemanticTokenType::PARAMETER),
            (SemanticKind::Property, SemanticTokenType::PROPERTY),
            (SemanticKind::Function, SemanticTokenType::FUNCTION),
            (SemanticKind::Method, SemanticTokenType::METHOD),
            (SemanticKind::Keyword, SemanticTokenType::KEYWORD),
            (SemanticKind::Comment, SemanticTokenType::COMMENT),
            (SemanticKind::String, SemanticTokenType::STRING),
            (SemanticKind::Number, SemanticTokenType::NUMBER),
            (SemanticKind::Operator, SemanticTokenType::OPERATOR),
            (SemanticKind::Macro, SemanticTokenType::MACRO),
            // `Variable`, and the `Field`/`Constant` kinds that lack a dedicated
            // standard type, all map to `variable`.
            (SemanticKind::Variable, SemanticTokenType::VARIABLE),
            (SemanticKind::Field, SemanticTokenType::VARIABLE),
            (SemanticKind::Constant, SemanticTokenType::VARIABLE),
        ];
        for (kind, expected_type) in expected {
            assert_eq!(
                &legend.token_types[type_index(*kind) as usize],
                expected_type,
                "{kind:?} must resolve to {expected_type:?} via its legend index"
            );
        }

        // The declaration modifier maps to a valid modifier bit.
        assert!(!legend.token_modifiers.is_empty());
        assert_eq!(modifier_bitset(SemanticModifiers::DECLARATION), 1);
        assert_eq!(modifier_bitset(SemanticModifiers::NONE), 0);
    }
}
