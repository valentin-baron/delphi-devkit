//! Process-global interner and source arena.
//!
//! One LSP server hosts one project per process, so a single interner and a
//! single source arena are shared by every unit parse and every worker thread.
//! Both are chosen for this: [`Interner`] (`lasso::ThreadedRodeo`) interns
//! through `&self`, and [`SourceArena`] grows through `&self` with stable
//! addresses (elsa). A `thread_local` was explicitly rejected — a `Spur`
//! interned on one thread must resolve on another, which only a shared global
//! guarantees.
//!
//! These globals are what makes transparent `Identifier`/`FileId` serde
//! possible: their `Serialize`/`Deserialize` impls take no context, so they
//! resolve strings/paths through here.
//!
//! The interner never resets within a real process (interning is idempotent,
//! so it never needs to). [`reset_for_tests`] installs a fresh interner and
//! arena so tests that assert on counts or `FileId` identity start clean.
//!
//! Both globals are reached through an [`AtomicPtr`] over a leaked `Box`. A
//! reset leaks the previous instance rather than freeing it, so any `&'static`
//! string or buffer already handed out stays valid forever — sound even if a
//! reset races a reader. In a real process no reset ever happens, so nothing
//! leaks; the leak is a test-only affordance.

use std::sync::atomic::{AtomicPtr, Ordering};

use crate::context::{Identifier, Interner};
use crate::source::SourceArena;

static INTERNER: AtomicPtr<Interner> = AtomicPtr::new(std::ptr::null_mut());
static ARENA: AtomicPtr<SourceArena> = AtomicPtr::new(std::ptr::null_mut());

/// Get the current instance behind `slot`, initializing it on first use.
/// `make` runs at most usefully once per generation; a losing initializer's
/// box is dropped immediately (nothing has been handed out from it yet).
fn current<T>(slot: &AtomicPtr<T>, make: fn() -> T) -> &'static T {
    let existing = slot.load(Ordering::Acquire);
    if !existing.is_null() {
        // SAFETY: a non-null pointer here was produced by `Box::into_raw` of a
        // `T` that is leaked (never freed), so the reference is valid for the
        // whole process lifetime.
        return unsafe { &*existing };
    }
    let fresh = Box::into_raw(Box::new(make()));
    match slot.compare_exchange(
        std::ptr::null_mut(),
        fresh,
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        Ok(_) => unsafe { &*fresh },
        Err(winner) => {
            // Another thread initialized first. Our `fresh` was never observed,
            // so dropping it here is safe (no outstanding references).
            drop(unsafe { Box::from_raw(fresh) });
            unsafe { &*winner }
        }
    }
}

/// The process-global interner. All `Spur`s in the process come from here.
pub fn interner() -> &'static Interner {
    current(&INTERNER, Interner::default)
}

/// The process-global source arena. All `FileId`s in the process come from here.
pub fn arena() -> &'static SourceArena {
    current(&ARENA, SourceArena::new)
}

/// Display-track intern: text exactly as written. Never use the result as a
/// comparison key — Delphi identifiers are case-insensitive, so comparisons
/// must go through [`intern_key`].
pub fn intern(text: &str) -> Identifier {
    Identifier::from(interner().get_or_intern(text))
}

/// The ONE identifier case-fold used everywhere a folded comparison key is
/// produced (interning, define lookup, unit-name resolution, builtin-type
/// lookup). Ordinal ASCII fold: `a`–`z` → `A`–`Z`, every other byte (incl. all
/// non-ASCII UTF-8 continuation bytes) left BYTE-IDENTICAL.
///
/// Why ordinal, not Unicode `to_uppercase()`: Delphi's identifier
/// case-insensitivity is an ordinal ASCII fold — it does NOT Unicode-uppercase
/// (`ß` stays `ß`, it does not become `SS`; `ö` stays `ö`, distinct from `Ö`).
/// The previous `to_uppercase()` on the key track could fold two DIFFERENT
/// non-ASCII identifiers together (a wrong MATCH — silent corruption) and, worse,
/// DIVERGED from the ASCII-only folds elsewhere. This function is the single
/// source of truth so the tracks can never disagree. For the ASCII identifiers
/// that dominate real Delphi it is byte-for-byte identical to the old fold, so
/// the dual-track behaviour is preserved; it only changes (and corrects)
/// non-ASCII, where it never produces a wrong match.
///
/// Because the fold only ever rewrites ASCII bytes in place, the result is
/// always valid UTF-8 (an ASCII byte is never part of a multi-byte sequence).
pub fn fold_identifier(identifier: &str) -> String {
    let mut folded = identifier.as_bytes().to_vec();
    for byte in &mut folded {
        byte.make_ascii_uppercase();
    }
    // SAFETY-free: `make_ascii_uppercase` only rewrites bytes in `a`..=`z`, which
    // are always standalone ASCII in UTF-8 — the byte sequence stays valid UTF-8.
    String::from_utf8(folded).expect("ASCII-only fold preserves UTF-8 validity")
}

/// Lookup-track intern: case-folded key for all identifier comparisons
/// (defines, unit names, aliases, cache, symbol tables). Folds through the ONE
/// [`fold_identifier`] so every comparison domain shares the same key.
pub fn intern_key(identifier: &str) -> Identifier {
    Identifier::from(interner().get_or_intern(fold_identifier(identifier)))
}

/// Resolve an [`Identifier`] back to its interned string. Panics only if the
/// `Spur` was not issued by the current interner generation — impossible for
/// values that round-tripped through serde (which re-interns) or [`intern`].
pub fn resolve(identifier: Identifier) -> &'static str {
    interner().resolve(&identifier.spur())
}

/// Test-only reset: install a fresh, empty interner and arena. The previous
/// instances are leaked (not freed), so any `&'static` reference already handed
/// out stays valid forever — no use-after-free even if a reader races the swap.
///
/// Interning is idempotent, so nearly every test is reset-independent: it can
/// serialize a value and deserialize it back (re-interning reproduces the same
/// `Spur`) without ever emptying the global. Reach for this ONLY when a test
/// must observe an interner/arena that starts empty (asserting on size, or on a
/// specific small `FileId`). Because a swap mid-run invalidates `Spur`s a
/// concurrently-running test interned in the previous generation, such a test
/// MUST run serially with respect to the rest of the suite (e.g. under
/// `--test-threads=1`). The existing suite needs none, so this is not invoked
/// in-tree; it exists for downstream callers per the LSP-serialization spec.
#[cfg(test)]
#[allow(dead_code)] // spec-required affordance for downstream callers; unused in-tree
pub fn reset_for_tests() {
    let fresh_interner = Box::into_raw(Box::new(Interner::default()));
    INTERNER.store(fresh_interner, Ordering::Release);
    let fresh_arena = Box::into_raw(Box::new(SourceArena::new()));
    ARENA.store(fresh_arena, Ordering::Release);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    // These tests are reset-independent by design (interning is idempotent),
    // so they are safe under the parallel test runner — they never swap the
    // global out from under a concurrent test.

    #[test]
    fn identifier_serializes_as_its_string_and_reinterns() {
        let original = intern("System.SysUtils");
        let bytes = bincode::serialize(&original).unwrap();
        // no raw Spur/u32 on the wire: the string itself is present
        assert!(String::from_utf8_lossy(&bytes).contains("System.SysUtils"));

        // deserialize re-interns; idempotence means it resolves to the same
        // string (and, in this same process, the same Spur)
        let restored: Identifier = bincode::deserialize(&bytes).unwrap();
        assert_eq!(resolve(restored), "System.SysUtils");
        assert_eq!(restored, original);
    }

    /// Definitive proof that the serialized form carries the STRING, not the
    /// raw session-local `Spur` integer. Structural, not a byte-scan: decode the
    /// bytes back to a `String` and require the exact interned text, then assert
    /// the bytes are byte-for-byte the bincoded `String`. This mirrors the
    /// `FileId` test (`file_id_bytes_are_a_path_not_a_raw_index`) and drops the
    /// old fragile `contains_subsequence(&bytes, &raw_le)` scan — which only
    /// avoided false positives by a 300-padding coincidence and depended on the
    /// order earlier tests interned strings.
    #[test]
    fn identifier_bytes_are_a_string_not_a_raw_spur() {
        let identifier = intern("A_Distinctive_Symbol_Name");

        let identifier_bytes = bincode::serialize(&identifier).unwrap();
        // 1. decodes structurally back to the exact interned string
        let decoded: String = bincode::deserialize(&identifier_bytes).unwrap();
        assert_eq!(
            decoded, "A_Distinctive_Symbol_Name",
            "Identifier serialized to something other than its string"
        );
        // 2. byte-for-byte identical to serializing the plain String — no
        //    integer tag, no raw Spur index on the wire
        let string_bytes = bincode::serialize(&"A_Distinctive_Symbol_Name").unwrap();
        assert_eq!(
            identifier_bytes, string_bytes,
            "Identifier must serialize exactly as its String, no integer tag"
        );
    }

    /// Definitive proof for `FileId`: bytes are the path string, and the raw
    /// `FileId(u32)` index is absent.
    #[test]
    fn file_id_bytes_are_a_path_not_a_raw_index() {
        let directory = std::env::temp_dir().join("delphi_parser_fileid_raw_scan");
        std::fs::create_dir_all(&directory).unwrap();
        // register padding files so this file's index is distinctive
        for pad in 0..50 {
            let path = directory.join(format!("pad_{pad}.pas"));
            std::fs::write(&path, "unit Pad;").unwrap();
            let _ = arena().register(&path);
        }
        let path = directory.join("Scanned.pas");
        std::fs::write(&path, "unit Scanned;").unwrap();
        let file = arena().register(&path).unwrap();
        let raw = file.0 as u64;

        let bytes = bincode::serialize(&file).unwrap();

        // The serialized form is exactly the path STRING (bincode length-prefix
        // + UTF-8 bytes), never the raw session-local index. Proving that
        // structurally — rather than scanning for the index as a byte
        // subsequence — avoids a false positive: the raw index is a small
        // integer whose bytes coincide both with an ASCII path character and
        // with bincode's own string-length prefix (order-dependent, since the
        // global arena's index depends on how many files earlier tests
        // registered). Decode the bytes back to a String and require the path.
        // `register` canonicalizes, so compare against the arena's stored path.
        let stored = arena().path(file).to_path_buf();
        let decoded: String = bincode::deserialize(&bytes).unwrap();
        assert_eq!(
            std::path::Path::new(&decoded),
            stored.as_path(),
            "FileId serialized to something other than its path"
        );

        // And the encoding is NOT the raw index: a bincoded `u32`/`u64` index
        // is 4/8 bytes, whereas the path string is far longer — a length
        // mismatch is definitive that no bare integer was written.
        assert!(
            bytes.len() > std::mem::size_of::<u64>(),
            "serialized FileId is suspiciously short — looks like a raw index, not a path"
        );
        let _ = raw;
    }

    /// Dual-track integrity: a struct holding a display identifier AND its
    /// folded key round-trips with BOTH strings distinct and correctly
    /// resolvable, and `intern_key` lookups still hit the key track.
    #[test]
    fn dual_track_survives_round_trip() {
        #[derive(Serialize, Deserialize)]
        struct Pair {
            display: Identifier,
            key: Identifier,
        }

        let pair = Pair {
            display: intern("TFooBar"),
            key: intern_key("TFooBar"), // "TFOOBAR"
        };
        // the two tracks are independent interner entries
        assert_ne!(pair.display, pair.key);

        let bytes = bincode::serialize(&pair).unwrap();
        // both distinct strings must be on the wire, never a shared integer
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("TFooBar"));
        assert!(text.contains("TFOOBAR"));

        let restored: Pair = bincode::deserialize(&bytes).unwrap();
        assert_eq!(resolve(restored.display), "TFooBar");
        assert_eq!(resolve(restored.key), "TFOOBAR");
        assert_ne!(restored.display, restored.key);
        // an intern_key lookup for the same identifier resolves to the SAME
        // Spur as the restored key track; the display track stays separate
        assert_eq!(intern_key("tfoobar"), restored.key);
        assert_eq!(intern("TFooBar"), restored.display);
        assert_ne!(intern("TFooBar"), restored.key);
    }

    /// L9: the ONE identifier fold is an ordinal ASCII fold. For ASCII it is
    /// byte-for-byte identical to the old `to_uppercase()` (behaviour preserved).
    #[test]
    fn fold_identifier_is_ordinal_ascii() {
        assert_eq!(fold_identifier("TFooBar"), "TFOOBAR");
        assert_eq!(fold_identifier("already_upper_123"), "ALREADY_UPPER_123");
        // ASCII fold matches to_uppercase for pure-ASCII input
        assert_eq!(fold_identifier("MixedCase"), "MixedCase".to_uppercase());
    }

    /// L9: non-ASCII bytes are left BYTE-IDENTICAL — NOT Unicode-uppercased. A
    /// naive `to_uppercase()` would (wrongly, for a Delphi ordinal fold) expand
    /// `ß`→`SS` and fold `ö`→`Ö`, which could MATCH two distinct identifiers.
    #[test]
    fn fold_identifier_leaves_non_ascii_byte_identical() {
        // only the trailing ASCII `foo` folds; `ß` stays exactly one `ß` byte-seq
        assert_eq!(fold_identifier("ßfoo"), "ßFOO");
        // `ö` is NOT folded to `Ö`; the ASCII `l` folds to `L`
        assert_eq!(fold_identifier("öl"), "öL");
        // consequently `öl` and `Öl` do NOT fold together (distinct, like dcc)
        assert_ne!(fold_identifier("öl"), fold_identifier("Öl"));
        // whereas a Unicode uppercase WOULD have collapsed them — proof we differ
        assert_eq!("öl".to_uppercase(), "ÖL".to_string());
    }

    /// L9: both interning tracks agree on the SAME fold for a non-ASCII name.
    /// `intern_key` routes through `fold_identifier`, so two spellings that fold
    /// identically resolve to the same key Spur, and two that don't stay
    /// distinct — no divergence between the display and key tracks' assumptions.
    #[test]
    fn non_ascii_tracks_agree_through_one_fold() {
        // same identifier interned twice → same key (idempotent fold)
        assert_eq!(intern_key("ßMember"), intern_key("ßmember"));
        // the key track resolves to the ordinal-folded string, byte-identical
        assert_eq!(resolve(intern_key("ßMember")), "ßMEMBER");
        // a non-ASCII-differing pair stays distinct on the key track
        assert_ne!(intern_key("ölVar"), intern_key("Ölvar"));
    }

    #[test]
    fn file_id_serializes_as_path_and_reregisters() {
        let directory = std::env::temp_dir().join("delphi_parser_fileid_serde");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("Sample.pas");
        std::fs::write(&path, "unit Sample;").unwrap();

        let file = arena().register(&path).unwrap();
        let bytes = bincode::serialize(&file).unwrap();
        // path text present, not a raw index
        assert!(String::from_utf8_lossy(&bytes).contains("Sample"));

        // deserialize re-registers the path in the arena (lazy, no read);
        // content is readable on demand
        let restored: crate::meta::FileId = bincode::deserialize(&bytes).unwrap();
        assert_eq!(arena().content(restored).unwrap(), "unit Sample;");
    }

    #[test]
    fn file_id_for_unregisterable_path_errors_not_panics() {
        // A virtual/unsaved buffer's display name does not canonicalize;
        // deserializing such a FileId must be a clean serde error (caught and
        // counted "unreadable" by the cache loader), never a panic (#21/#25/M2).
        let virtual_file = arena().insert_virtual(
            "<unsaved-buffer-that-does-not-exist-on-disk>",
            "unit Ghost;",
        );
        let bytes = bincode::serialize(&virtual_file).unwrap();
        let result: Result<crate::meta::FileId, _> = bincode::deserialize(&bytes);
        assert!(result.is_err());
    }
}
