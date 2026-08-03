//! Type layout: sizes (and alignments) of Delphi types.
//!
//! **Stage 1** — built-in scalar sizes ([`builtin_size`]): platform-dependent,
//! table driven. `SizeOf(Pointer)`/`SizeOf(Integer)` need no symbol table and
//! cover the dominant real-world `{$IF SizeOf(...)}` patterns.
//!
//! **Stage 2** — structured layout ([`type_layout`]): records/objects, enums,
//! subranges, static arrays, sized strings, pointer-shaped types. This is ABI
//! code: a WRONG confident size silently flips a `{$IF}` branch (the exact
//! silent-corruption class the invariants forbid). Therefore every rule here
//! returns [`Some`] ONLY when the layout is unambiguously computable and matches
//! Delphi's dcc; ANY uncertainty (unknown field type, unresolvable bound, an
//! ABI rule we have not verified — variant records, sets) returns [`None`]
//! (Unknown), which the resolver degrades safely. When in doubt: `None` + a
//! SESSION.md ledger entry, never a guessed number.
//!
//! Windows targets: `LongInt`/`LongWord` are 4 bytes on BOTH Win32 and Win64
//! (LLP64). POSIX 64-bit targets differ (LP64: 8 bytes) — out of scope while
//! Delphi 12/Windows is the target; revisit with platform expansion.

use crate::ast::{EnumerationMember, Member, StructuredType, TypeExpression};
use crate::context::{Identifier, SwitchState, TargetPlatform};
use crate::meta::CodeLocation;

/// Size in bytes of a built-in type, `None` when the name is not a built-in
/// (user types answer through [`type_layout`]).
/// `folded_name` must be the case-folded (uppercase) spelling.
pub fn builtin_size(folded_name: &str, platform: TargetPlatform) -> Option<u64> {
    let pointer = u64::from(platform.pointer_size());
    Some(match folded_name {
        "BYTE" | "SHORTINT" | "ANSICHAR" | "BOOLEAN" | "BYTEBOOL" | "UINT8" | "INT8" => 1,
        "WORD" | "SMALLINT" | "WIDECHAR" | "CHAR" | "WORDBOOL" | "UINT16" | "INT16" => 2,
        "INTEGER" | "CARDINAL" | "LONGINT" | "LONGWORD" | "FIXEDINT" | "FIXEDUINT"
        | "SINGLE" | "LONGBOOL" | "UINT32" | "INT32" | "HRESULT" => 4,
        "INT64" | "UINT64" | "DOUBLE" | "COMP" | "CURRENCY" | "TDATETIME" | "REAL" => 8,
        "REAL48" => 6,
        "SHORTSTRING" => 256,
        // 10-byte x87 extended exists only on Win32; Win64 aliases Double
        "EXTENDED" => match platform {
            TargetPlatform::Win32 => 10,
            TargetPlatform::Win64 | TargetPlatform::Unknown => 8,
        },
        // TVarData: 16 bytes on Win32, 24 on Win64
        "VARIANT" | "OLEVARIANT" => match platform {
            TargetPlatform::Win32 => 16,
            TargetPlatform::Win64 | TargetPlatform::Unknown => 24,
        },
        "POINTER" | "PCHAR" | "PANSICHAR" | "PWIDECHAR" | "NATIVEINT" | "NATIVEUINT"
        | "THANDLE" | "STRING" | "ANSISTRING" | "UNICODESTRING" | "WIDESTRING"
        | "RAWBYTESTRING" | "UTF8STRING" => pointer,
        _ => return None,
    })
}

/// Natural alignment of a built-in type. On Windows targets ($A8 default), a
/// scalar aligns to its own size capped at the platform pointer alignment
/// (8-byte types align to 8 on Win64; on Win32 an 8-byte type like Int64/Double
/// still aligns to 8 under $A8 — Delphi honors the type's natural alignment up
/// to the record's `{$A}` ceiling). `None` for non-builtins.
fn builtin_alignment(folded_name: &str, platform: TargetPlatform) -> Option<u64> {
    let size = builtin_size(folded_name, platform)?;
    Some(match folded_name {
        // A pointer aligns to the pointer size.
        "POINTER" | "PCHAR" | "PANSICHAR" | "PWIDECHAR" | "NATIVEINT" | "NATIVEUINT"
        | "THANDLE" | "STRING" | "ANSISTRING" | "UNICODESTRING" | "WIDESTRING"
        | "RAWBYTESTRING" | "UTF8STRING" => u64::from(platform.pointer_size()),
        // ShortString is a byte array → 1-byte aligned regardless of its 256 size.
        "SHORTSTRING" => 1,
        // Real48 is a 6-byte packed float → 1-byte aligned (legacy).
        "REAL48" => 1,
        // Extended: 10 bytes on Win32 aligns to 8 (record field alignment for
        // Extended is 8 under $A8, not 10 — 10 is not a power of two). On Win64
        // it's an 8-byte Double alias → 8.
        "EXTENDED" => 8,
        // Currency/Comp/Int64/Double/TDateTime: 8-byte natural alignment.
        // Variant: aligns to pointer size (contains pointers).
        "VARIANT" | "OLEVARIANT" => u64::from(platform.pointer_size()),
        // Everything else: natural = size (1/2/4/8), which is already a power
        // of two for the remaining builtins.
        _ => size,
    })
}

/// Computed layout of a type: its size and its field-alignment requirement.
/// `alignment` is needed to lay out an enclosing record; only `size` is exposed
/// to `SizeOf`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub size: u64,
    pub alignment: u64,
}

impl Layout {
    fn scalar(size: u64, alignment: u64) -> Self {
        Self { size, alignment }
    }
}

/// Named-type + span resolution the layout engine needs. The implementor
/// ([`crate::if_eval::StateResolver`]) resolves a `Reference` name to its
/// `TypeExpression` (own interface types first, then imported units via the
/// loader — recording dependencies), reads a source span's text (for bounds),
/// and evaluates a constant expression. Every method returns `None` on any
/// uncertainty so the layout engine can propagate Unknown rather than guess.
pub trait LayoutResolver {
    /// Resolve a simple (possibly dotted) type name to its declared
    /// `TypeExpression`. Own types first, then imports (dependency recorded).
    /// `None` when the name is not a resolvable user type here (builtin, or
    /// unknown/cross-unit-unreachable). The returned expression is owned (a
    /// clone) so the borrow does not tangle with `&mut self`.
    fn resolve_named_type(&mut self, name_key: Identifier) -> Option<TypeExpression>;

    /// Text of a source span (array/subrange/enum/string bound). `None` when
    /// the span cannot be read (stale/foreign location).
    fn span_text(&mut self, location: CodeLocation) -> Option<String>;

    /// Evaluate a constant expression to an `i64`. `None` when the expression
    /// is Unknown (unresolved constant) or not an integer — the layout engine
    /// then returns `None` (never a guessed size).
    fn evaluate_integer(&mut self, expression: &str) -> Option<i64>;

    /// Guard against runaway named-type recursion / alias cycles. The engine
    /// calls `enter`/`leave` around each named-type descent; `enter` returns
    /// `false` when the depth ceiling is hit → the layout is `None`.
    fn enter_type(&mut self) -> bool;
    fn leave_type(&mut self);
}

/// The public entry point. Compute the [`Layout`] of `type_expression` under the
/// given switches/platform. `Some` ONLY when unambiguously computable and
/// dcc-correct; `None` on any uncertainty (see module docs).
pub fn type_layout(
    type_expression: &TypeExpression,
    switches: SwitchState,
    platform: TargetPlatform,
    resolver: &mut dyn LayoutResolver,
) -> Option<Layout> {
    if !resolver.enter_type() {
        return None; // recursion/alias-cycle ceiling → Unknown, never a guess
    }
    let result = type_layout_inner(type_expression, switches, platform, resolver);
    resolver.leave_type();
    result
}

fn type_layout_inner(
    type_expression: &TypeExpression,
    switches: SwitchState,
    platform: TargetPlatform,
    resolver: &mut dyn LayoutResolver,
) -> Option<Layout> {
    let pointer = u64::from(platform.pointer_size());
    match type_expression {
        // ── Named reference: builtin table, else resolve the user type ──────
        TypeExpression::Reference { name, .. } => {
            // `name.key` is already the folded (intern_key) track; re-fold
            // through the ONE identifier fold (idempotent on an already-folded
            // key) so the builtin-table lookup key never diverges from how the
            // table's keys / intern_key were produced.
            let folded = crate::globals::fold_identifier(crate::globals::resolve(name.key));
            if let Some(size) = builtin_size(&folded, platform) {
                // A field's alignment is its natural alignment capped at `{$A}`
                // (and at least 1). The record layout also re-caps per field, so
                // this is belt-and-suspenders but keeps a standalone `SizeOf`
                // alignment sane.
                let alignment = builtin_alignment(&folded, platform)
                    .unwrap_or(size)
                    .min(u64::from(switches.align.max(1)))
                    .max(1);
                return Some(Layout::scalar(size, alignment));
            }
            // A generic instantiation (`TList<Integer>`) is a class → pointer;
            // but we cannot be sure the name is a class without resolving it.
            // Resolve the user type and lay IT out.
            let resolved = resolver.resolve_named_type(name.key)?;
            type_layout(&resolved, switches, platform, resolver)
        }

        // ── Pointer-shaped types: one platform pointer ──────────────────────
        // A class instance variable, an interface reference, a typed/untyped
        // pointer, a class reference, a dynamic array and a `reference to`
        // closure are all a single pointer.
        TypeExpression::Pointer(_)
        | TypeExpression::ClassReference(_)
        | TypeExpression::Class(_)
        | TypeExpression::Interface(_)
        | TypeExpression::AnonymousMethod(_)
        | TypeExpression::ForwardClass
        | TypeExpression::ForwardInterface
        | TypeExpression::ForwardDispInterface => Some(Layout::scalar(pointer, pointer)),

        // ── Procedure/function type ─────────────────────────────────────────
        // A bare `procedure`/`function` type is one code pointer; a method
        // pointer (`of object`) is TWO pointers (code + data).
        TypeExpression::Routine(routine) => {
            let count = if routine.of_object { 2 } else { 1 };
            Some(Layout::scalar(pointer * count, pointer))
        }

        // ── Distinct type: `type Integer` has the inner type's layout ───────
        TypeExpression::Distinct(inner) => type_layout(inner, switches, platform, resolver),

        // ── Enumeration ─────────────────────────────────────────────────────
        TypeExpression::Enumeration(members) => {
            let size = enumeration_size(members, switches, resolver)?;
            Some(Layout::scalar(size, size))
        }

        // ── Subrange (`0..255`, `'a'..'z'`) ─────────────────────────────────
        TypeExpression::Subrange(span) => {
            let size = subrange_size(*span, resolver)?;
            Some(Layout::scalar(size, size))
        }

        // ── ShortString `string[n]` → n + 1 bytes, 1-aligned ────────────────
        TypeExpression::SizedString(span) => {
            let text = resolver.span_text(*span)?;
            let length = resolver.evaluate_integer(&text)?;
            if !(1..=255).contains(&length) {
                return None; // ShortString length is 1..255; anything else is unclear
            }
            Some(Layout::scalar(length as u64 + 1, 1))
        }

        // ── Static array `array[lo..hi] of T` (multi-dim) ───────────────────
        // `array of T` (no bounds) is a dynamic array → one pointer.
        TypeExpression::Array { bounds, element } => match bounds {
            None => Some(Layout::scalar(pointer, pointer)),
            Some(span) => {
                let count = array_element_count(*span, resolver)?;
                let element_layout = type_layout(element, switches, platform, resolver)?;
                let size = element_layout.size.checked_mul(count)?;
                Some(Layout::scalar(size, element_layout.alignment))
            }
        },

        // ── Record / object ─────────────────────────────────────────────────
        TypeExpression::Record(structured) => {
            record_layout(structured, switches, platform, resolver)
        }

        // ── Set / File / ArrayOfConst / distinct-unhandled → Unknown ────────
        // Set sizing and variant records are DEFERRED to `None` (ledgered):
        // returning a wrong size here would silently flip a `{$IF}` branch.
        TypeExpression::SetOf(_)
        | TypeExpression::File(_)
        | TypeExpression::ArrayOfConst => None,
    }
}

/// Smallest of {1,2,4} that holds an enumeration's max ordinal, but not smaller
/// than `{$Z}` (`min_enum_size`). Explicit values (`= 2`) shift the max ordinal
/// and are evaluated via the const machinery; an unresolvable explicit value →
/// `None`.
fn enumeration_size(
    members: &[EnumerationMember],
    switches: SwitchState,
    resolver: &mut dyn LayoutResolver,
) -> Option<u64> {
    // Track the running ordinal: it starts at 0 and increments by 1 for each
    // member unless an explicit `= value` resets it. The max ordinal reached
    // determines the size.
    let mut next_ordinal: i64 = 0;
    let mut max_ordinal: i64 = 0;
    let mut any = false;
    for member in members {
        let ordinal = match member.explicit_value {
            Some(span) => {
                let text = resolver.span_text(span)?;
                resolver.evaluate_integer(&text)?
            }
            None => next_ordinal,
        };
        // A negative enum ordinal makes Delphi widen to a signed 4-byte type;
        // that is a rarer rule we have not verified against dcc for every case,
        // so a negative ordinal is DEFERRED to Unknown rather than guessed.
        if ordinal < 0 {
            return None;
        }
        if !any || ordinal > max_ordinal {
            max_ordinal = ordinal;
        }
        any = true;
        next_ordinal = ordinal.checked_add(1)?;
    }
    if !any {
        return None; // empty enumeration is not a real type
    }
    let natural = if max_ordinal <= 0xFF {
        1
    } else if max_ordinal <= 0xFFFF {
        2
    } else if max_ordinal <= 0xFFFF_FFFF {
        4
    } else {
        return None; // beyond 32-bit ordinal space — unclear, defer
    };
    Some(natural.max(u64::from(switches.min_enum_size.max(1))))
}

/// Size of a subrange type from its `lo..hi` bound span. Integer or char
/// bounds; the size is the smallest integer type holding `[lo, hi]` with
/// correct signedness. Enum-based subranges (`Red..Blue`) whose bounds are not
/// integer/char constants → `None`.
fn subrange_size(span: CodeLocation, resolver: &mut dyn LayoutResolver) -> Option<u64> {
    let text = resolver.span_text(span)?;
    // Split on the range operator `..`. The bounds themselves never contain
    // `..` (they are constant expressions / char literals).
    let (low_text, high_text) = split_range(&text)?;
    let low = bound_ordinal(low_text, resolver)?;
    let high = bound_ordinal(high_text, resolver)?;
    if high < low {
        return None; // malformed subrange
    }
    Some(integer_size_for_range(low, high))
}

/// Evaluate one subrange/array bound to an ordinal. A single-character string
/// literal (`'a'`) contributes its code point; otherwise it is an integer
/// constant expression.
fn bound_ordinal(text: &str, resolver: &mut dyn LayoutResolver) -> Option<i64> {
    let trimmed = text.trim();
    // Char literal `'a'` → its ordinal. The if_eval tokenizer already turns a
    // `'x'` into a one-char string value, but the range operator handling wants
    // the ordinal directly; detect the single-char literal form here.
    if let Some(inner) = single_char_literal(trimmed) {
        return Some(inner as i64);
    }
    resolver.evaluate_integer(trimmed)
}

/// `'a'` → `Some('a')`; `''''` (an escaped quote) → the quote char; anything
/// that is not exactly one character in quotes → `None`.
fn single_char_literal(text: &str) -> Option<char> {
    let bytes = text.as_bytes();
    if bytes.len() >= 3 && bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\'' {
        let inner = &text[1..text.len() - 1];
        // `''''` is an escaped single quote.
        if inner == "''" {
            return Some('\'');
        }
        let mut chars = inner.chars();
        let first = chars.next()?;
        if chars.next().is_none() {
            return Some(first);
        }
    }
    None
}

/// Split a subrange span text `LO..HI` into its two halves at the top-level
/// `..`. Guards against a `..` that could appear inside something else (there
/// is none in a constant-bound subrange, but we scan defensively for the first
/// `..` that is not part of a floating literal — subrange bounds are ordinal,
/// never float, so any `..` is the range operator).
fn split_range(text: &str) -> Option<(&str, &str)> {
    let bytes = text.as_bytes();
    let mut index = 0;
    while index + 1 < bytes.len() {
        if bytes[index] == b'.' && bytes[index + 1] == b'.' {
            return Some((&text[..index], &text[index + 2..]));
        }
        index += 1;
    }
    None
}

/// Smallest Delphi ordinal type size holding `[low, high]` with correct
/// signedness. Mirrors dcc: an all-non-negative range fits an unsigned type of
/// the smallest width; a range with a negative low needs a signed type wide
/// enough for both ends.
fn integer_size_for_range(low: i64, high: i64) -> u64 {
    if low >= 0 {
        // unsigned width by the high bound
        if high <= 0xFF {
            1
        } else if high <= 0xFFFF {
            2
        } else if high <= 0xFFFF_FFFF {
            4
        } else {
            8
        }
    } else {
        // signed width: both ends must fit
        let fits_i8 = (-128..=127).contains(&low) && (-128..=127).contains(&high);
        let fits_i16 = (-32768..=32767).contains(&low) && (-32768..=32767).contains(&high);
        let fits_i32 =
            (i32::MIN as i64..=i32::MAX as i64).contains(&low) && (i32::MIN as i64..=i32::MAX as i64).contains(&high);
        if fits_i8 {
            1
        } else if fits_i16 {
            2
        } else if fits_i32 {
            4
        } else {
            8
        }
    }
}

/// Total element count of a (possibly multi-dimensional) static array from its
/// bounds span `[lo1..hi1, lo2..hi2, ...]` or `[IndexType, lo..hi]`. Each
/// dimension multiplies the count. A dimension that is a named ordinal type
/// (`array[Boolean]`, `array[TColor]`) rather than an explicit `lo..hi` range
/// is DEFERRED to `None` — we would have to resolve the index type's cardinality
/// and that is not yet implemented; a guessed count would corrupt the size.
fn array_element_count(span: CodeLocation, resolver: &mut dyn LayoutResolver) -> Option<u64> {
    let text = resolver.span_text(span)?;
    // Strip a single enclosing pair of brackets if present (`array` bounds spans
    // may or may not include them depending on the parser's capture).
    let inner = text.trim();
    let inner = inner
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(inner);
    let mut total: u64 = 1;
    let mut saw_dimension = false;
    for dimension in split_top_level_commas(inner) {
        let dimension = dimension.trim();
        if dimension.is_empty() {
            continue;
        }
        let (low_text, high_text) = split_range(dimension)?; // named index type → None
        let low = bound_ordinal(low_text, resolver)?;
        let high = bound_ordinal(high_text, resolver)?;
        if high < low {
            return None;
        }
        // Checked arithmetic: a pathological literal bound such as
        // `array[0..9223372036854775807]` (high = i64::MAX) must degrade to
        // None, never panic (debug) or silently wrap (release).
        let count = high.checked_sub(low)?.checked_add(1)?;
        let count = u64::try_from(count).ok()?;
        total = total.checked_mul(count)?;
        saw_dimension = true;
    }
    if saw_dimension { Some(total) } else { None }
}

/// Split on top-level commas (ignoring commas nested inside brackets/parens),
/// so `array[0..3, 0..1]` yields two dimensions.
fn split_top_level_commas(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut parts = Vec::new();
    let mut depth: i32 = 0;
    let mut start = 0;
    for (index, &byte) in bytes.iter().enumerate() {
        match byte {
            b'[' | b'(' => depth += 1,
            b']' | b')' => depth -= 1,
            b',' if depth == 0 => {
                parts.push(&text[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    parts.push(&text[start..]);
    parts
}

/// Lay out a record/object: fields in declaration order, each placed at the next
/// offset aligned to `min(alignment(field_type), {$A})`; the record's alignment
/// is `min(max(field alignments), {$A})`; the final size is padded up to that.
/// `is_packed` (or `{$A}==1`) forces all alignments to 1 (no padding). Any field
/// whose type sizes to `None` → the whole record is `None`. `class var` fields
/// are class-static storage, NOT part of instance layout — excluded.
///
/// A **variant part** (`case … of`) is DEFERRED: a record that has one returns
/// `None` (ledger). We do NOT approximate variant-record layout.
fn record_layout(
    structured: &StructuredType,
    switches: SwitchState,
    platform: TargetPlatform,
    resolver: &mut dyn LayoutResolver,
) -> Option<Layout> {
    // Variant records are deferred (ledger #6-variant): the arm-overlap ABI is
    // not yet verified, and a wrong size flips `{$IF}`. Unknown, never a guess.
    if structured.variant_part.is_some() {
        return None;
    }

    let packed = structured.is_packed || switches.align <= 1;
    let alignment_ceiling = if packed {
        1
    } else {
        u64::from(switches.align.max(1))
    };

    let mut offset: u64 = 0;
    let mut record_alignment: u64 = 1;

    for section in &structured.sections {
        for member in &section.members {
            let Member::Field(field) = member else {
                continue; // methods/properties/nested types are not instance data
            };
            if field.is_class_var {
                continue; // class var = static storage, not in the instance
            }
            let field_layout = type_layout(&field.field_type, switches, platform, resolver)?;
            let field_alignment = field_layout.alignment.min(alignment_ceiling).max(1);
            record_alignment = record_alignment.max(field_alignment);
            // one placement per declared name (`A, B: Integer` = two fields)
            for _ in &field.names {
                offset = align_up(offset, field_alignment)?;
                offset = offset.checked_add(field_layout.size)?;
            }
        }
    }

    let final_alignment = record_alignment.min(alignment_ceiling).max(1);
    let size = align_up(offset, final_alignment)?;
    // An empty record still occupies at least... Delphi: an empty record is 1
    // byte. But `record end` with no fields is rare; be conservative and treat a
    // zero-size record as Unknown rather than assert 0 or 1 without a test.
    if size == 0 {
        return None;
    }
    Some(Layout {
        size,
        alignment: final_alignment,
    })
}

/// Round `offset` up to a multiple of `alignment` (a power of two ≥ 1).
fn align_up(offset: u64, alignment: u64) -> Option<u64> {
    if alignment <= 1 {
        return Some(offset);
    }
    let remainder = offset % alignment;
    if remainder == 0 {
        Some(offset)
    } else {
        offset.checked_add(alignment - remainder)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_dependent_sizes() {
        assert_eq!(builtin_size("POINTER", TargetPlatform::Win32), Some(4));
        assert_eq!(builtin_size("POINTER", TargetPlatform::Win64), Some(8));
        assert_eq!(builtin_size("NATIVEINT", TargetPlatform::Win32), Some(4));
        assert_eq!(builtin_size("NATIVEINT", TargetPlatform::Win64), Some(8));
        assert_eq!(builtin_size("EXTENDED", TargetPlatform::Win32), Some(10));
        assert_eq!(builtin_size("EXTENDED", TargetPlatform::Win64), Some(8));
        assert_eq!(builtin_size("VARIANT", TargetPlatform::Win32), Some(16));
        assert_eq!(builtin_size("VARIANT", TargetPlatform::Win64), Some(24));
    }

    #[test]
    fn llp64_longint_stays_4_bytes() {
        assert_eq!(builtin_size("LONGINT", TargetPlatform::Win64), Some(4));
        assert_eq!(builtin_size("LONGWORD", TargetPlatform::Win64), Some(4));
    }

    #[test]
    fn fixed_sizes_and_unknown_names() {
        assert_eq!(builtin_size("BYTE", TargetPlatform::Win32), Some(1));
        assert_eq!(builtin_size("WIDECHAR", TargetPlatform::Win32), Some(2));
        assert_eq!(builtin_size("INT64", TargetPlatform::Win32), Some(8));
        assert_eq!(builtin_size("REAL48", TargetPlatform::Win32), Some(6));
        assert_eq!(builtin_size("SHORTSTRING", TargetPlatform::Win64), Some(256));
        assert_eq!(builtin_size("TMYRECORD", TargetPlatform::Win32), None);
    }

    #[test]
    fn integer_range_sizing_rule() {
        // unsigned widths
        assert_eq!(integer_size_for_range(0, 255), 1);
        assert_eq!(integer_size_for_range(0, 256), 2);
        assert_eq!(integer_size_for_range(0, 65535), 2);
        assert_eq!(integer_size_for_range(0, 65536), 4);
        // signed widths — dcc picks the smallest signed type holding both ends
        assert_eq!(integer_size_for_range(-1, 100), 1); // fits ShortInt (-128..127)
        assert_eq!(integer_size_for_range(-128, 127), 1); // ShortInt exactly
        assert_eq!(integer_size_for_range(-129, 127), 2); // needs SmallInt
        assert_eq!(integer_size_for_range(-1, 40000), 4); // 40000 > SmallInt max → Integer
    }

    #[test]
    fn range_splitting() {
        assert_eq!(split_range("0..255"), Some(("0", "255")));
        assert_eq!(split_range("'a'..'z'"), Some(("'a'", "'z'")));
        assert_eq!(split_range("Low..High"), Some(("Low", "High")));
        assert_eq!(split_range("nodots"), None);
    }

    #[test]
    fn single_char_literal_ordinals() {
        assert_eq!(single_char_literal("'a'"), Some('a'));
        assert_eq!(single_char_literal("''''"), Some('\''));
        assert_eq!(single_char_literal("'ab'"), None);
        assert_eq!(single_char_literal("5"), None);
    }

    #[test]
    fn align_up_rounds() {
        assert_eq!(align_up(0, 4), Some(0));
        assert_eq!(align_up(1, 4), Some(4));
        assert_eq!(align_up(4, 4), Some(4));
        assert_eq!(align_up(5, 8), Some(8));
        assert_eq!(align_up(7, 1), Some(7));
    }

    #[test]
    fn comma_splitting_ignores_nesting() {
        assert_eq!(split_top_level_commas("0..3, 0..1"), vec!["0..3", " 0..1"]);
        assert_eq!(split_top_level_commas("0..3"), vec!["0..3"]);
    }

    // ── ABI proof tests through the FULL parse pipeline ──────────────────────
    //
    // A `{$IF SizeOf(TRec) = N}` guards a `const Marker = True;` declaration. The
    // marker survives ONLY when the layout engine computes exactly N. Because the
    // cursor uses the AssumeFalse policy for Unknown, a marker under a
    // `SizeOf(...) = N` guard proves the size was computed AND equals N (Unknown
    // would drop it). We assert both the positive N and a wrong N' to pin the
    // number from both sides. Each asserted N encodes a documented Delphi ABI
    // rule, noted inline so a reviewer can check it against dcc.

    use crate::context::{DefineSet, ProjectContext, SwitchState};
    use crate::pipeline::parse_and_cache;
    use crate::unit_cache::UnitCache;
    use std::collections::HashMap;
    use std::sync::Arc;

    fn context_for(platform: TargetPlatform, switches: SwitchState) -> Arc<ProjectContext> {
        context_with_paths(platform, switches, Vec::new())
    }

    fn context_with_paths(
        platform: TargetPlatform,
        switches: SwitchState,
        search_paths: Vec<std::path::PathBuf>,
    ) -> Arc<ProjectContext> {
        Arc::new(ProjectContext {
            configuration: "Debug".to_string(),
            platform_name: match platform {
                TargetPlatform::Win64 => "Win64".to_string(),
                _ => "Win32".to_string(),
            },
            platform,
            compiler_version: 36.0,
            rtl_version: 36.0,
            base_defines: DefineSet::default(),
            search_paths,
            include_paths: Vec::new(),
            namespaces: Vec::new(),
            unit_aliases: HashMap::new(),
            default_switches: switches,
            unit_cache: UnitCache::default(),
        })
    }

    /// Parse `source` as a virtual unit under the given platform/switches and
    /// return the lowercased interface const/marker names that survived the
    /// `{$IF}` guards. A marker present ⇒ its guard evaluated True.
    fn surviving_markers(
        source: &str,
        platform: TargetPlatform,
        switches: SwitchState,
    ) -> Vec<String> {
        let context = context_for(platform, switches);
        let arena = crate::globals::arena();
        let file = arena.insert_virtual("SizeOfProbe.pas", source);
        let (_outcome, meta) = parse_and_cache(arena, &context, file, None, true).unwrap();
        meta.unwrap()
            .ast
            .interface_declarations
            .iter()
            .map(|declaration| crate::globals::resolve(declaration.name.name).to_lowercase())
            .collect()
    }

    /// Convenience: does a single `{$IF SizeOf(<type_decl>) = size}` guard hold?
    /// `type_decl` is the RHS of `type TProbe = <type_decl>;`.
    fn size_of_equals(
        type_decl: &str,
        size: u64,
        platform: TargetPlatform,
        switches: SwitchState,
    ) -> bool {
        let source = format!(
            "unit SizeOfProbe;\ninterface\n\
             type TProbe = {type_decl};\n\
             {{$IF SizeOf(TProbe) = {size}}} const Yes = True; {{$ELSE}} const No = True; {{$IFEND}}\n\
             implementation\nend.",
        );
        surviving_markers(&source, platform, switches).contains(&"yes".to_string())
    }

    fn default_switches() -> SwitchState {
        SwitchState::default() // $A8, $Z1 — the Delphi-12 defaults
    }

    #[test]
    fn record_padding_default_alignment() {
        // `record a: Byte; b: Integer; end` under $A8:
        //   a @0 (1 byte), 3 pad bytes, b @4 (4 bytes) → size 8, align 4.
        // The exact rule: each field aligns to min(natural, {$A}); record size
        // padded to the record alignment (max field alignment, capped at {$A}).
        let record = "record a: Byte; b: Integer; end";
        for platform in [TargetPlatform::Win32, TargetPlatform::Win64] {
            assert!(size_of_equals(record, 8, platform, default_switches()));
            assert!(!size_of_equals(record, 5, platform, default_switches()));
        }
    }

    #[test]
    fn packed_record_has_no_padding() {
        // `packed record a: Byte; b: Integer; end` → 1 + 4 = 5, no padding.
        let record = "packed record a: Byte; b: Integer; end";
        for platform in [TargetPlatform::Win32, TargetPlatform::Win64] {
            assert!(size_of_equals(record, 5, platform, default_switches()));
        }
    }

    #[test]
    fn a1_switch_forces_packed_layout() {
        // {$A1} makes an unpacked record pack: 1 + 4 = 5.
        let mut switches = default_switches();
        switches.align = 1;
        let record = "record a: Byte; b: Integer; end";
        assert!(size_of_equals(record, 5, TargetPlatform::Win32, switches));
    }

    #[test]
    fn int64_field_alignment_win32_and_win64() {
        // `record a: Byte; b: Int64; end`: Int64 has 8-byte natural alignment;
        // under $A8 the field aligns to 8 on BOTH targets → a @0, 7 pad, b @8,
        // size 16. This is where Win32 and Win64 AGREE (both honor 8-byte
        // alignment for Int64 under $A8).
        let record = "record a: Byte; b: Int64; end";
        for platform in [TargetPlatform::Win32, TargetPlatform::Win64] {
            assert!(size_of_equals(record, 16, platform, default_switches()));
        }
    }

    #[test]
    fn nested_record_alignment() {
        // TInner = record x: Integer; end  (size 4, align 4)
        // TOuter = record b: Byte; inner: TInner; end
        //   b @0, 3 pad, inner @4 → size 8, align 4.
        let source = "unit SizeOfProbe;\ninterface\n\
             type TInner = record x: Integer; end;\n\
             type TOuter = record b: Byte; inner: TInner; end;\n\
             {$IF SizeOf(TOuter) = 8} const OuterEight = True; {$IFEND}\n\
             {$IF SizeOf(TInner) = 4} const InnerFour = True; {$IFEND}\n\
             implementation\nend.";
        let markers = surviving_markers(source, TargetPlatform::Win32, default_switches());
        assert!(markers.contains(&"outereight".to_string()));
        assert!(markers.contains(&"innerfour".to_string()));
    }

    #[test]
    fn enum_size_under_z1_and_z4() {
        // An enum with 200 members: max ordinal 199 fits 1 byte under $Z1, but
        // $Z4 forces 4 bytes (min_enum_size floor).
        let members: Vec<String> = (0..200).map(|index| format!("e{index}")).collect();
        let enum_decl = format!("({})", members.join(", "));

        let mut z1 = default_switches();
        z1.min_enum_size = 1;
        assert!(size_of_equals(&enum_decl, 1, TargetPlatform::Win32, z1));

        let mut z4 = default_switches();
        z4.min_enum_size = 4;
        assert!(size_of_equals(&enum_decl, 4, TargetPlatform::Win32, z4));
    }

    #[test]
    fn enum_needing_two_bytes() {
        // 300 members → max ordinal 299 > 255 → 2 bytes under $Z1.
        let members: Vec<String> = (0..300).map(|index| format!("e{index}")).collect();
        let enum_decl = format!("({})", members.join(", "));
        assert!(size_of_equals(&enum_decl, 2, TargetPlatform::Win32, default_switches()));
    }

    #[test]
    fn enum_with_explicit_value_shifts_max_ordinal() {
        // `(a, b = 300, c)`: max ordinal is 301 (c = b+1) → 2 bytes under $Z1.
        assert!(size_of_equals("(a, b = 300, c)", 2, TargetPlatform::Win32, default_switches()));
    }

    #[test]
    fn subrange_sizes() {
        // 0..255 → Byte (1); 0..256 → Word (2); 'a'..'z' → 1 (char ordinals
        // 97..122 fit a byte).
        assert!(size_of_equals("0..255", 1, TargetPlatform::Win32, default_switches()));
        assert!(size_of_equals("0..256", 2, TargetPlatform::Win32, default_switches()));
        assert!(size_of_equals("'a'..'z'", 1, TargetPlatform::Win32, default_switches()));
    }

    #[test]
    fn static_array_size() {
        // array[1..10] of Integer → 10 * 4 = 40.
        assert!(size_of_equals("array[1..10] of Integer", 40, TargetPlatform::Win32, default_switches()));
        // multi-dim: array[0..3, 0..1] of Byte → 4 * 2 * 1 = 8.
        assert!(size_of_equals("array[0..3, 0..1] of Byte", 8, TargetPlatform::Win32, default_switches()));
    }

    #[test]
    fn pathological_array_bound_is_none_not_panic() {
        // `array[0..i64::MAX] of Byte`: the element count would be
        // i64::MAX + 1, which overflows i64. This must degrade to Unknown —
        // never panic (debug) or silently wrap (release). No probed size holds.
        let source = "unit SizeOfProbe;\ninterface\n\
             type TProbe = array[0..9223372036854775807] of Byte;\n\
             {$IF SizeOf(TProbe) = 0} const Zero = True; {$ELSE} const NotZero = True; {$IFEND}\n\
             {$IF SizeOf(TProbe) > 0} const Positive = True; {$ELSE} const NotPositive = True; {$IFEND}\n\
             implementation\nend.";
        let markers = surviving_markers(source, TargetPlatform::Win32, default_switches());
        // Unknown → AssumeFalse for every guard: only the else markers survive.
        assert!(markers.contains(&"notzero".to_string()));
        assert!(markers.contains(&"notpositive".to_string()));
        assert!(!markers.contains(&"zero".to_string()));
        assert!(!markers.contains(&"positive".to_string()));
    }

    #[test]
    fn short_string_size() {
        // string[20] → 20 + 1 length byte = 21, alignment 1.
        assert!(size_of_equals("string[20]", 21, TargetPlatform::Win32, default_switches()));
    }

    #[test]
    fn class_field_is_pointer_sized() {
        // A class-typed field is a reference (pointer): 4 on Win32, 8 on Win64.
        let source = "unit SizeOfProbe;\ninterface\n\
             type TThing = class end;\n\
             type TProbe = record c: TThing; end;\n\
             {$IF SizeOf(TProbe) = 4} const P4 = True; {$IFEND}\n\
             {$IF SizeOf(TProbe) = 8} const P8 = True; {$IFEND}\n\
             implementation\nend.";
        assert!(surviving_markers(source, TargetPlatform::Win32, default_switches())
            .contains(&"p4".to_string()));
        assert!(surviving_markers(source, TargetPlatform::Win64, default_switches())
            .contains(&"p8".to_string()));
    }

    #[test]
    fn string_and_pointer_fields_are_pointer_sized() {
        // A `string` field and a `^Integer` field are both one pointer.
        let record = "record s: string; end";
        assert!(size_of_equals(record, 4, TargetPlatform::Win32, default_switches()));
        assert!(size_of_equals(record, 8, TargetPlatform::Win64, default_switches()));
    }

    #[test]
    fn confidence_discipline_unknown_field_is_none_not_a_wrong_number() {
        // THE GATE'S #1 CHECK. A record with a field of an unknown/unresolvable
        // type must make SizeOf Unknown — NOT a wrong confident number. The guard
        // `SizeOf(TProbe) = <anything>` must evaluate Unknown → AssumeFalse → the
        // `{$ELSE}` marker wins for EVERY candidate size we probe.
        let source = "unit SizeOfProbe;\ninterface\n\
             type TProbe = record a: Byte; b: TCompletelyUnknownType; end;\n\
             {$IF SizeOf(TProbe) = 8} const Guessed8 = True; {$ELSE} const NotEight = True; {$IFEND}\n\
             {$IF SizeOf(TProbe) = 5} const Guessed5 = True; {$ELSE} const NotFive = True; {$IFEND}\n\
             {$IF SizeOf(TProbe) > 0} const Positive = True; {$ELSE} const NotPositive = True; {$IFEND}\n\
             implementation\nend.";
        let markers = surviving_markers(source, TargetPlatform::Win32, default_switches());
        // no confident number of ANY value — all guards fell to the else branch
        assert!(markers.contains(&"noteight".to_string()));
        assert!(markers.contains(&"notfive".to_string()));
        assert!(markers.contains(&"notpositive".to_string()));
        assert!(!markers.contains(&"guessed8".to_string()));
        assert!(!markers.contains(&"guessed5".to_string()));
    }

    #[test]
    fn variant_record_is_deferred_to_unknown() {
        // Variant records are DEFERRED (ledger): a `case … of` record must be
        // Unknown, never an approximated size. Probe several sizes; none holds.
        let source = "unit SizeOfProbe;\ninterface\n\
             type TVariant = record\n\
               Tag: Byte;\n\
               case Integer of\n\
                 0: (AsInt: Integer);\n\
                 1: (AsBytes: array[0..3] of Byte);\n\
             end;\n\
             {$IF SizeOf(TVariant) = 8} const V8 = True; {$ELSE} const NotV8 = True; {$IFEND}\n\
             {$IF SizeOf(TVariant) = 5} const V5 = True; {$ELSE} const NotV5 = True; {$IFEND}\n\
             implementation\nend.";
        let markers = surviving_markers(source, TargetPlatform::Win32, default_switches());
        assert!(markers.contains(&"notv8".to_string()));
        assert!(markers.contains(&"notv5".to_string()));
        assert!(!markers.contains(&"v8".to_string()));
        assert!(!markers.contains(&"v5".to_string()));
    }

    #[test]
    fn cross_unit_sizeof_resolves_and_records_dependency() {
        // `SizeOf(TRec)` where TRec is imported from another unit: the layout
        // engine resolves it via the loader, computes the size, AND records the
        // exporting unit as a dependency (so a layout-affecting edit invalidates
        // this unit — proven separately below).
        use crate::pipeline::parse_and_cache;
        use crate::unit_loader::UnitLoader;

        let directory = std::env::temp_dir().join("delphi_parser_layout_xunit");
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("Shapes.pas"),
            "unit Shapes;\ninterface\n\
             type TPoint = record x: Integer; y: Integer; end;\n\
             implementation end.",
        )
        .unwrap();
        std::fs::write(
            directory.join("UsesShapes.pas"),
            "unit UsesShapes;\ninterface\nuses Shapes;\n\
             {$IF SizeOf(TPoint) = 8} const PointEight = True; {$ELSE} const NotEight = True; {$IFEND}\n\
             implementation end.",
        )
        .unwrap();

        let context =
            context_with_paths(TargetPlatform::Win32, default_switches(), vec![directory.clone()]);
        let arena = crate::globals::arena();
        let loader = UnitLoader::new(arena, context.clone(), None);
        let file = arena.load(directory.join("UsesShapes.pas")).unwrap();
        let (_outcome, meta) = parse_and_cache(arena, &context, file, Some(loader), true).unwrap();
        let meta = meta.unwrap();
        let names: Vec<String> = meta
            .ast
            .interface_declarations
            .iter()
            .map(|declaration| crate::globals::resolve(declaration.name.name).to_lowercase())
            .collect();
        assert!(names.contains(&"pointeight".to_string()), "SizeOf(TPoint) must be 8");
        assert!(!names.contains(&"noteight".to_string()));
        // the consulted export is recorded as a dependency
        assert!(
            meta.dependencies
                .iter()
                .any(|dependency| dependency.unit == context.intern_key("SHAPES")),
            "Shapes must be a recorded dependency of UsesShapes"
        );
    }

    #[test]
    fn qualified_cross_unit_sizeof_resolves_and_records_dependency() {
        // `SizeOf(Shapes.TPoint)` — the QUALIFIED form names the exporting unit
        // explicitly. It must resolve to the same size as the unqualified form
        // and record Shapes as a dependency. A wrong probe (5) must not hold.
        use crate::pipeline::parse_and_cache;
        use crate::unit_loader::UnitLoader;

        let directory = std::env::temp_dir().join("delphi_parser_layout_xunit_qualified");
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("Shapes.pas"),
            "unit Shapes;\ninterface\n\
             type TPoint = record x: Integer; y: Integer; end;\n\
             implementation end.",
        )
        .unwrap();
        std::fs::write(
            directory.join("UsesShapes.pas"),
            "unit UsesShapes;\ninterface\nuses Shapes;\n\
             {$IF SizeOf(Shapes.TPoint) = 8} const PointEight = True; {$ELSE} const NotEight = True; {$IFEND}\n\
             {$IF SizeOf(Shapes.TPoint) = 5} const PointFive = True; {$ELSE} const NotFive = True; {$IFEND}\n\
             implementation end.",
        )
        .unwrap();

        let context =
            context_with_paths(TargetPlatform::Win32, default_switches(), vec![directory.clone()]);
        let arena = crate::globals::arena();
        let loader = UnitLoader::new(arena, context.clone(), None);
        let file = arena.load(directory.join("UsesShapes.pas")).unwrap();
        let (_outcome, meta) = parse_and_cache(arena, &context, file, Some(loader), true).unwrap();
        let meta = meta.unwrap();
        let names: Vec<String> = meta
            .ast
            .interface_declarations
            .iter()
            .map(|declaration| crate::globals::resolve(declaration.name.name).to_lowercase())
            .collect();
        assert!(names.contains(&"pointeight".to_string()), "SizeOf(Shapes.TPoint) must be 8");
        assert!(!names.contains(&"noteight".to_string()));
        assert!(names.contains(&"notfive".to_string()));
        assert!(!names.contains(&"pointfive".to_string()));
        assert!(
            meta.dependencies
                .iter()
                .any(|dependency| dependency.unit == context.intern_key("SHAPES")),
            "Shapes must be a recorded dependency via the qualified SizeOf"
        );
    }

    #[test]
    fn set_type_is_deferred_to_unknown() {
        // `set of` sizing is DEFERRED (ledger). Must be Unknown, not a guess.
        let source = "unit SizeOfProbe;\ninterface\n\
             type TColors = set of Byte;\n\
             {$IF SizeOf(TColors) = 32} const S32 = True; {$ELSE} const NotS32 = True; {$IFEND}\n\
             {$IF SizeOf(TColors) = 1} const S1 = True; {$ELSE} const NotS1 = True; {$IFEND}\n\
             implementation\nend.";
        let markers = surviving_markers(source, TargetPlatform::Win32, default_switches());
        assert!(markers.contains(&"nots32".to_string()));
        assert!(markers.contains(&"nots1".to_string()));
        assert!(!markers.contains(&"s32".to_string()));
        assert!(!markers.contains(&"s1".to_string()));
    }
}
