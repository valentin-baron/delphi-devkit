//! Symbol → occurrences index for `textDocument/references` (task 5, A.3).
//!
//! Maps a folded symbol key to every occurrence of that key across cached
//! units: interface symbol declarations, member declarations, and the
//! implementation/interface-body usages recorded per [`UnitMeta`]. Each
//! occurrence carries the owning unit key and its source span.
//!
//! INVALIDATION DISCIPLINE (mirrors task-3's `dfm_links` purge): the index must
//! never point into an evicted unit. It is keyed per unit so a unit's
//! occurrences can be dropped wholesale on invalidation and re-added when the
//! unit is re-parsed — exactly the reverse-dependency-index / dfm-links pattern.
//! A stale references index that references a gone unit is a bug.
//!
//! OVER-APPROXIMATION (documented, safe direction): the usage index is not yet
//! scope-resolved, so `references(key)` returns a CANDIDATE set — every textual
//! occurrence of that folded key. It never MISSES a real occurrence in a cached
//! unit (the requirement); it may include occurrences that a full name binder
//! would attribute to a shadowing local. Refinement is a later semantic stage.

use std::collections::HashMap;

use crate::context::Identifier;
use crate::meta::CodeLocation;
use crate::unit_meta::UnitMeta;

/// One occurrence of a symbol key: the unit it lives in and its source span.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Occurrence {
    pub unit: Identifier,
    pub location: CodeLocation,
}

/// Symbol key → occurrences, plus a per-unit reverse map so a unit's
/// contribution can be purged in one step on invalidation.
#[derive(Default)]
pub struct ReferenceIndex {
    /// folded symbol key → occurrences (across all cached units).
    by_symbol: HashMap<Identifier, Vec<Occurrence>>,
    /// unit key → the symbol keys that unit contributed (for O(1) purge).
    by_unit: HashMap<Identifier, Vec<Identifier>>,
}

impl ReferenceIndex {
    /// Index one unit's occurrences. Idempotent per unit: a prior contribution
    /// for the same unit key is purged first, so a re-parse replaces rather than
    /// duplicates. Occurrences come from the derived interface surface
    /// (declarations + members) AND the recorded usages.
    pub fn index_unit(&mut self, unit_key: Identifier, meta: &UnitMeta) {
        self.purge_unit(unit_key);
        let mut contributed: Vec<Identifier> = Vec::new();

        let add = |index: &mut Self,
                       contributed: &mut Vec<Identifier>,
                       key: Identifier,
                       location: CodeLocation| {
            index
                .by_symbol
                .entry(key)
                .or_default()
                .push(Occurrence { unit: unit_key, location });
            if !contributed.contains(&key) {
                contributed.push(key);
            }
        };

        // interface declaration sites + their member declaration sites
        for symbol in &meta.interface().symbols {
            add(self, &mut contributed, symbol.key, symbol.location);
            for member in &symbol.members {
                add(self, &mut contributed, member.key, member.location);
            }
        }
        // recorded identifier occurrences (interface bodies + implementation)
        for usage in &meta.usages {
            add(self, &mut contributed, usage.symbol, usage.location);
        }

        if !contributed.is_empty() {
            self.by_unit.insert(unit_key, contributed);
        }
    }

    /// Drop everything a unit contributed. Called on invalidation BEFORE the
    /// cache entry is gone — no occurrence may outlive its unit (the invariant).
    pub fn purge_unit(&mut self, unit_key: Identifier) {
        let Some(symbol_keys) = self.by_unit.remove(&unit_key) else {
            return;
        };
        for symbol_key in symbol_keys {
            if let Some(occurrences) = self.by_symbol.get_mut(&symbol_key) {
                occurrences.retain(|occurrence| occurrence.unit != unit_key);
                if occurrences.is_empty() {
                    self.by_symbol.remove(&symbol_key);
                }
            }
        }
    }

    /// Every recorded occurrence of `symbol_key`, across all cached units.
    pub fn occurrences(&self, symbol_key: Identifier) -> &[Occurrence] {
        self.by_symbol
            .get(&symbol_key)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Rebuild wholesale from the current cache contents (after a full sweep,
    /// same discipline as the reverse-dependency index rebuild). Clears first,
    /// so a shrunken cache leaves no stale mappings.
    pub fn rebuild_from<'a>(
        &mut self,
        units: impl Iterator<Item = (Identifier, &'a UnitMeta)>,
    ) {
        self.by_symbol.clear();
        self.by_unit.clear();
        for (unit_key, meta) in units {
            self.index_unit(unit_key, meta);
        }
    }
}
