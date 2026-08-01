use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use crate::context::{DefineSet, Identifier, ProjectContext, SwitchFlags, SwitchState};
use crate::meta::{CodeLocation, FileId};
use crate::unit_cache::Dependency;
use crate::unit_meta::UnitMeta;

/// Result of requesting another unit's parsed interface. "Request the parsed
/// version" — fulfillment (cache hit, parse-on-miss, sidecar manifest) is the
/// implementor's business.
#[derive(Debug)]
pub enum LoadOutcome {
    Loaded(Arc<UnitMeta>),
    /// Requested unit is on the current parse chain: interface uses-cycle.
    Cycle,
    /// No source found (missing from search paths, DCU-only, ...).
    NotFound,
    /// The unit exists but failed to parse (cached failure included).
    Failed,
}

/// Serves interface requests during a parse. One instance per top-level
/// parse chain (its cycle stack is chain-local) — `Rc`, not shared across
/// threads.
pub trait InterfaceLoader {
    fn interface_of(&self, unit_key: Identifier) -> LoadOutcome;

    /// Called by the grammar parser as soon as a unit header names itself —
    /// registers the unit on the chain's cycle stack (covers the top-level
    /// unit, which never goes through `interface_of`).
    fn begin_unit(&self, _unit_key: Identifier) {}

    /// Paired with [`Self::begin_unit`] when the unit's parse completed.
    fn end_unit(&self, _unit_key: Identifier) {}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConditionalKind {
    IfDef,  // {$IFDEF}
    IfNDef, // {$IFNDEF}
    If,     // {$IF expr}
    IfOpt,  // {$IFOPT X+}
}

/// One level of conditional-directive nesting.
#[derive(Debug, Clone, Copy)]
pub struct ConditionalFrame {
    pub kind: ConditionalKind,
    /// Was the surrounding context emitting when this frame opened?
    parent_active: bool,
    /// Is the current branch of this frame emitting?
    this_active: bool,
    /// Some branch of this if-chain was already taken ($ELSEIF/$ELSE logic).
    taken_any: bool,
    /// $ELSE seen — a second $ELSE or a late $ELSEIF is an error.
    had_else: bool,
    /// Where the frame opened; reported for unterminated conditionals.
    pub origin: CodeLocation,
}

#[derive(Debug)]
pub enum DirectiveError {
    /// $ELSE / $ELSEIF / $ENDIF with empty conditional stack.
    DanglingDirective(CodeLocation),
    /// Second $ELSE, or $ELSEIF after $ELSE, in the same frame.
    ElseAfterElse {
        location: CodeLocation,
        opened_at: CodeLocation,
    },
    /// EOF with open conditional frames.
    UnterminatedConditional { opened_at: CodeLocation },
    /// Unit already on this parse's own DFS stack: interface uses-cycle.
    CircularUses { chain: Vec<Identifier> },
}

/// A source file on the `{$I}` include stack, for diagnostics.
#[derive(Debug, Clone, Copy)]
pub struct SourceFrame {
    pub file: FileId,
    /// Location of the `{$I}` directive that pulled this file in.
    pub included_from: CodeLocation,
}

/// One own interface type's flattened members plus whether it may inherit.
/// `members` maps a member's lookup key to its SIMPLE member-type key (`None`
/// for complex/anonymous types, enabling nested own-type walks). `can_inherit`
/// is `true` for every class and interface (implicit `TObject`/`IInterface`
/// plus any explicit, possibly cross-unit, ancestors) and `false` for types
/// that carry no inherited member space (records, enums, …). A member absent
/// from an inheriting type's DIRECT members must degrade to Unknown, never a
/// confident false (ledger #19).
#[derive(Default)]
pub struct OwnTypeMembers {
    pub members: std::collections::HashMap<Identifier, Option<Identifier>>,
    pub can_inherit: bool,
}

/// Mutable per-file layer over the shared immutable [`ProjectContext`].
/// Created fresh for every unit parse, dead afterwards. Defines and switches
/// start as copies of the project baseline because `{$DEFINE}`/`{$A}` etc.
/// are unit-local — nothing here ever leaks back into the context.
/// (No `Debug` derive: the loader trait object isn't `Debug`.)
pub struct UnitParseState {
    pub context: Arc<ProjectContext>,
    /// Unit-local view; starts as clone of `context.base_defines`.
    pub defines: DefineSet,
    /// Unit-local view; starts as copy of `context.default_switches`.
    pub switches: SwitchState,
    /// Conditional nesting. Lives here, not per include file: a conditional
    /// opened in an `.inc` and closed in the main file is legal.
    conditional_stack: Vec<ConditionalFrame>,
    include_stack: Vec<SourceFrame>,
    /// Units whose interface parse is on THIS parse's call stack. Cycle
    /// detection — the cache's `InProgress` can't serve that role across
    /// tasks (foreign `InProgress` is a race, not a cycle).
    dfs_stack: Vec<Identifier>,
    /// Every `{$I}` file ever spliced (never popped) — artifact stamps.
    seen_includes: Vec<FileId>,
    /// Lookup keys of units in already-seen uses clauses, in order.
    imports: Vec<Identifier>,
    /// Lookup keys declared in the own interface section so far.
    own_interface_keys: HashSet<Identifier>,
    /// Values of own simple constants (single-literal initializers).
    own_constants: std::collections::HashMap<Identifier, crate::unit_cache::ConstantValue>,
    /// Own interface types' members, keyed by the type's lookup key. Each entry
    /// maps a member's lookup key to its SIMPLE type key (`None` for
    /// complex/anonymous types) plus whether the type may inherit members from a
    /// base. Feeds scoped `Declared(OwnType.Member[.Sub])` (ledger #19) during
    /// the parse, before the derived interface index exists. The member-type key
    /// enables nested own-type walks; the inherit flag keeps a member absent
    /// from an inheriting type's DIRECT members at Unknown, never a false.
    own_type_members: std::collections::HashMap<Identifier, OwnTypeMembers>,
    /// Full `TypeExpression` of each own interface type, keyed by lookup key.
    /// Feeds `SizeOf(OwnType)` during the parse: a `{$IF SizeOf(TFoo) = 8}` on a
    /// type declared earlier in THIS unit's interface needs the whole structure
    /// (packed flag, nested inline types, field types), which the flattened
    /// `own_type_members` does not carry. `Rc` so the layout engine can borrow it
    /// without tangling with the `&mut self` resolver. Recorded BEFORE trailing
    /// directives, same as `own_type_members`, so a following directive sees it.
    own_type_expressions:
        std::collections::HashMap<Identifier, std::rc::Rc<crate::ast::TypeExpression>>,
    /// Units whose interface was actually consulted (→ artifact staleness).
    dependencies: Vec<Dependency>,
    /// Identifier occurrences collected from the implementation section
    /// (usage index skeleton — unresolved, key + location).
    usages: Vec<crate::unit_cache::Usage>,
    /// Implementation-section routine structure (params/locals per body), for
    /// same-unit local resolution. Filled by the structure-aware impl pass.
    impl_scopes: Vec<crate::ast::ImplRoutine>,
    /// True while the impl-section structure pass is still fully trusted. Flipped
    /// false on ANY recovery in that pass (an unmodeled construct, an unexpected
    /// token, unbalanced `end`); consumers then ignore `impl_scopes`.
    impl_scopes_reliable: bool,
    /// Serves `Declared()`/`SizeOf()` requests for imported units. `None`
    /// = no import resolution available; those queries answer Unknown.
    pub loader: Option<Rc<dyn InterfaceLoader>>,
    /// An import request hit an interface uses-cycle (invalid Delphi — the
    /// compiler rejects it with F2047). The parse degrades gracefully, but
    /// the result must not be persisted as trustworthy.
    cycle_tainted: bool,
}

impl UnitParseState {
    pub fn new(context: Arc<ProjectContext>) -> Self {
        Self {
            defines: context.base_defines.clone(),
            switches: context.default_switches,
            conditional_stack: Vec::new(),
            include_stack: Vec::new(),
            dfs_stack: Vec::new(),
            seen_includes: Vec::new(),
            imports: Vec::new(),
            own_interface_keys: HashSet::new(),
            own_constants: std::collections::HashMap::new(),
            own_type_members: std::collections::HashMap::new(),
            own_type_expressions: std::collections::HashMap::new(),
            dependencies: Vec::new(),
            usages: Vec::new(),
            impl_scopes: Vec::new(),
            impl_scopes_reliable: true,
            loader: None,
            cycle_tainted: false,
            context,
        }
    }

    pub fn mark_cycle_tainted(&mut self) {
        self.cycle_tainted = true;
    }

    pub fn is_cycle_tainted(&self) -> bool {
        self.cycle_tainted
    }

    // ─── Imports, own symbols, dependencies ──────────────────────────────

    /// Called by the grammar parser for each `uses` entry (lookup key).
    pub fn record_import(&mut self, unit_key: Identifier) {
        if !self.imports.contains(&unit_key) {
            self.imports.push(unit_key);
        }
    }

    /// Imports in reverse uses order (later units shadow earlier ones).
    pub fn imports_reversed(&self) -> Vec<Identifier> {
        self.imports.iter().rev().copied().collect()
    }

    /// Called by the grammar parser for each interface declaration.
    pub fn declare_interface_key(&mut self, key: Identifier) {
        self.own_interface_keys.insert(key);
    }

    pub fn own_interface_declared(&self, key: Identifier) -> bool {
        self.own_interface_keys.contains(&key)
    }

    pub fn record_own_constant(&mut self, key: Identifier, value: crate::unit_cache::ConstantValue) {
        self.own_constants.insert(key, value);
    }

    pub fn own_constant(&self, key: Identifier) -> Option<crate::unit_cache::ConstantValue> {
        self.own_constants.get(&key).copied()
    }

    /// Record the members of an own interface type as (member key → simple
    /// member-type key) pairs (scoped `Declared` support, ledger #19).
    /// `can_inherit` marks classes/interfaces (implicit + explicit ancestors),
    /// so a member absent from the direct members degrades to Unknown, never a
    /// confident false.
    pub fn record_own_type_members(
        &mut self,
        type_key: Identifier,
        can_inherit: bool,
        members: impl IntoIterator<Item = (Identifier, Option<Identifier>)>,
    ) {
        let entry = self.own_type_members.entry(type_key).or_default();
        entry.members.extend(members);
        // A later partial re-record (forward-declared class completed) must not
        // clear an inherit flag already set; OR the flags together.
        entry.can_inherit |= can_inherit;
    }

    /// Is `type_key` a known own interface type (members recorded)?
    pub fn own_type_members_known(&self, type_key: Identifier) -> bool {
        self.own_type_members.contains_key(&type_key)
    }

    /// Record the full `TypeExpression` of an own interface type (for
    /// `SizeOf(OwnType)` during the parse). See the field docs.
    pub fn record_own_type_expression(
        &mut self,
        type_key: Identifier,
        type_expression: std::rc::Rc<crate::ast::TypeExpression>,
    ) {
        self.own_type_expressions.insert(type_key, type_expression);
    }

    /// The full `TypeExpression` of an own interface type, if declared earlier
    /// in this unit's interface section.
    pub fn own_type_expression(
        &self,
        type_key: Identifier,
    ) -> Option<std::rc::Rc<crate::ast::TypeExpression>> {
        self.own_type_expressions.get(&type_key).cloned()
    }

    /// Resolve a scoped `Declared` chain wholly within own interface types.
    /// `first_type` must be an own type; each following segment is a member of
    /// the current type, and to descend the member's simple type key must be
    /// another own type. Returns `Some(true/false)` when the chain resolves
    /// (member present/absent), `None` when a segment cannot be followed
    /// (member's type is not an own type → Unknown).
    pub fn own_scoped_declared(
        &self,
        first_type: Identifier,
        member_segments: &[Identifier],
    ) -> Option<bool> {
        let mut current = self.own_type_members.get(&first_type)?;
        for (index, member) in member_segments.iter().enumerate() {
            let Some(member_type) = current.members.get(member) else {
                // Member absent from this type's DIRECT declarations. If the
                // type can inherit (class/interface, or explicit ancestors) the
                // member may come from a base we do not flatten here → Unknown,
                // never a confident false (ledger #19). Only an ancestor-less
                // type (record/enum/…) proves genuine absence.
                if current.can_inherit {
                    return None;
                }
                return Some(false);
            };
            if index + 1 == member_segments.len() {
                return Some(true);
            }
            // descend into the member's own type, if it is one
            let next_type = (*member_type)?; // complex/anonymous type → Unknown
            current = self.own_type_members.get(&next_type)?; // not an own type
        }
        Some(true) // no member segments: the type itself is declared
    }

    /// Record that `meta`'s interface influenced this parse.
    pub fn record_dependency(&mut self, meta: &UnitMeta) {
        if self
            .dependencies
            .iter()
            .any(|dependency| dependency.unit == meta.name())
        {
            return;
        }
        self.dependencies.push(Dependency {
            unit: meta.name(),
            source_path: meta.source_path.clone(),
            source_hash: meta.source_hash,
            includes: meta.includes.clone(),
        });
    }

    pub fn seen_includes(&self) -> &[FileId] {
        &self.seen_includes
    }

    pub fn take_dependencies(&mut self) -> Vec<Dependency> {
        std::mem::take(&mut self.dependencies)
    }

    pub fn record_usage(&mut self, usage: crate::unit_cache::Usage) {
        self.usages.push(usage);
    }

    pub fn take_usages(&mut self) -> Vec<crate::unit_cache::Usage> {
        std::mem::take(&mut self.usages)
    }

    /// Record one fully-parsed implementation-section routine (its params,
    /// locals and body span) for same-unit local resolution.
    pub fn record_impl_routine(&mut self, routine: crate::ast::ImplRoutine) {
        self.impl_scopes.push(routine);
    }

    /// Mark the implementation-section structure pass as having recovered from a
    /// construct it does not confidently model — consumers must then ignore
    /// `impl_scopes` (never a wrong local attribution).
    pub fn mark_impl_scopes_unreliable(&mut self) {
        self.impl_scopes_reliable = false;
    }

    /// True while the impl-section structure pass is still fully trusted.
    pub fn impl_scopes_reliable(&self) -> bool {
        self.impl_scopes_reliable
    }

    pub fn take_impl_scopes(&mut self) -> Vec<crate::ast::ImplRoutine> {
        std::mem::take(&mut self.impl_scopes)
    }

    // ─── Conditional compilation ─────────────────────────────────────────

    /// Are tokens at the current position emitted? False inside a dead
    /// `{$IFDEF}` branch. `this_active` already ANDs the whole chain, so the
    /// innermost frame answers for all.
    pub fn is_active(&self) -> bool {
        self.conditional_stack.last().is_none_or(|frame| frame.this_active)
    }

    /// Must the parser evaluate the condition of an incoming `{$IF}`-family
    /// directive? False in a dead branch — the expression may reference
    /// undeclared symbols and must NOT be evaluated; pass `condition: false`.
    /// The frame is still pushed to keep nesting depth balanced.
    pub fn needs_condition(&self) -> bool {
        self.is_active()
    }

    /// Same question for an incoming `{$ELSEIF}`: only evaluate when the
    /// current frame could still activate a branch (live parent, no branch
    /// taken yet, no $ELSE seen).
    pub fn elseif_needs_condition(&self) -> bool {
        self.conditional_stack
            .last()
            .is_some_and(|frame| frame.parent_active && !frame.taken_any && !frame.had_else)
    }

    /// Open a conditional frame ($IFDEF/$IFNDEF/$IF/$IFOPT).
    pub fn push_conditional(&mut self, kind: ConditionalKind, condition: bool, origin: CodeLocation) {
        let parent_active = self.is_active();
        let this_active = parent_active && condition;
        self.conditional_stack.push(ConditionalFrame {
            kind,
            parent_active,
            this_active,
            taken_any: this_active,
            had_else: false,
            origin,
        });
    }

    /// `{$ELSEIF condition}`. Evaluate the condition only when
    /// [`Self::needs_condition`] held BEFORE this call and no branch was
    /// taken yet; otherwise pass `false`.
    pub fn elseif_branch(
        &mut self,
        condition: bool,
        location: CodeLocation,
    ) -> Result<(), DirectiveError> {
        let frame = self
            .conditional_stack
            .last_mut()
            .ok_or(DirectiveError::DanglingDirective(location))?;
        if frame.had_else {
            return Err(DirectiveError::ElseAfterElse {
                location,
                opened_at: frame.origin,
            });
        }
        frame.this_active = frame.parent_active && !frame.taken_any && condition;
        frame.taken_any |= frame.this_active;
        Ok(())
    }

    /// `{$ELSE}`.
    pub fn else_branch(&mut self, location: CodeLocation) -> Result<(), DirectiveError> {
        let frame = self
            .conditional_stack
            .last_mut()
            .ok_or(DirectiveError::DanglingDirective(location))?;
        if frame.had_else {
            return Err(DirectiveError::ElseAfterElse {
                location,
                opened_at: frame.origin,
            });
        }
        frame.had_else = true;
        frame.this_active = frame.parent_active && !frame.taken_any;
        frame.taken_any |= frame.this_active;
        Ok(())
    }

    /// `{$ENDIF}` / `{$IFEND}`.
    pub fn pop_conditional(&mut self, location: CodeLocation) -> Result<ConditionalFrame, DirectiveError> {
        self.conditional_stack
            .pop()
            .ok_or(DirectiveError::DanglingDirective(location))
    }

    /// Call at EOF of the main file (not at include EOF — conditionals may
    /// span include boundaries).
    pub fn finish(&self) -> Result<(), DirectiveError> {
        match self.conditional_stack.first() {
            Some(frame) => Err(DirectiveError::UnterminatedConditional {
                opened_at: frame.origin,
            }),
            None => Ok(()),
        }
    }

    // ─── Defines and switches ────────────────────────────────────────────

    /// `{$DEFINE}` — ignored inside a dead branch.
    pub fn apply_define(&mut self, symbol: Identifier) {
        if self.is_active() {
            self.defines.define(symbol);
        }
    }

    /// `{$UNDEF}` — ignored inside a dead branch.
    pub fn apply_undef(&mut self, symbol: Identifier) {
        if self.is_active() {
            self.defines.undef(symbol);
        }
    }

    /// `{$IFDEF}` / `Defined()` lookup against the unit-local define set.
    pub fn is_defined(&self, symbol: Identifier) -> bool {
        self.defines.contains(symbol)
    }

    /// `{$IFOPT X+}` lookup against the unit-local switch state.
    pub fn switch_enabled(&self, flag: SwitchFlags) -> bool {
        self.switches.flags.contains(flag)
    }

    // ─── Includes ────────────────────────────────────────────────────────

    pub fn push_include(&mut self, file: FileId, included_from: CodeLocation) {
        self.seen_includes.push(file);
        self.include_stack.push(SourceFrame {
            file,
            included_from,
        });
    }

    pub fn pop_include(&mut self) -> Option<SourceFrame> {
        self.include_stack.pop()
    }

    pub fn include_depth(&self) -> usize {
        self.include_stack.len()
    }

    // ─── Uses DFS (interface-parse recursion) ────────────────────────────

    /// Call before descending into a used unit's interface parse. Errors if
    /// the unit is already on this parse's own stack: interface uses-cycle,
    /// reported with the full chain.
    pub fn enter_unit(&mut self, unit: Identifier) -> Result<(), DirectiveError> {
        if let Some(pos) = self.dfs_stack.iter().position(|&u| u == unit) {
            let mut chain = self.dfs_stack[pos..].to_vec();
            chain.push(unit);
            return Err(DirectiveError::CircularUses { chain });
        }
        self.dfs_stack.push(unit);
        Ok(())
    }

    /// Call after the used unit's interface parse completed (or failed).
    pub fn leave_unit(&mut self) {
        self.dfs_stack.pop();
    }
}
