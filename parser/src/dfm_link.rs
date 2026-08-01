//! DFM↔PAS linker: matches a form's `.dfm` component tree against its `.pas`
//! form-class members, producing navigable links (component ↔ published field,
//! event property ↔ handler method) plus HONEST diagnostics. This is the input
//! for go-to-definition and rename across the form/code boundary in the devkit
//! LSP.
//!
//! Pure over `(&UnitInterface, &DfmObject)` — no I/O. The driver parses the pas
//! and the dfm separately and hands both here.
//!
//! # Honest diagnostics (the Unknown-not-false discipline, ledger #19 analog)
//!
//! A dfm component with no matching published field, or a handler value with no
//! matching method, is UNRESOLVED. But the missing member may be **inherited
//! from a base form** declared in ANOTHER unit (`TForm1 = class(TBaseForm)`, and
//! `TBaseForm` lives elsewhere). We must never claim "missing" for something a
//! base class could legitimately provide. The policy:
//!
//! - **Form class not in this unit's interface** → a single
//!   [`DfmDiagnostic::FormClassNotFound`] note and NO other diagnostics. We
//!   cannot ground any match, so we assert nothing about individual members.
//! - **Form class found, `has_ancestors == true`** (any class — implicit
//!   `TObject` — or one naming explicit/cross-unit ancestors, which every real
//!   VCL form does) → an unresolved component/handler yields at most an
//!   INFO-level [`DfmDiagnostic::UnresolvedComponentPossiblyInherited`] /
//!   [`DfmDiagnostic::UnresolvedHandlerPossiblyInherited`] note, NEVER a hard
//!   error. The member could come from the base.
//! - **Form class found, `has_ancestors == false`** (a genuinely ancestor-less,
//!   self-contained shape that cannot inherit anything) → the member truly
//!   cannot exist, so an unresolved component/handler is a hard
//!   [`DfmDiagnostic::DanglingComponent`] / [`DfmDiagnostic::MissingHandler`].
//!
//! In practice every real form is a class, so `has_ancestors` is almost always
//! `true` and dangling/missing are reserved for a fully-resolved, ancestor-less
//! form class (constructed in tests, and the honest thing to flag when it does
//! occur). We reuse task-2's [`InterfaceSymbol::has_ancestors`] signal directly.
//!
//! # Event-name AND method-match filter
//!
//! `DfmObject::identifier_properties()` over-approximates handler candidates:
//! enum-valued properties (`Align = alClient`, `Color = clBtnFace`) and other
//! ident-valued non-event properties (`Kind = bkOK`, `Action = SomeName`)
//! arrive as `DfmValue::Ident` too. A handler link is produced ONLY when BOTH
//! conditions hold: the property NAME is a genuine event (the `On…`
//! convention) AND its value key matches an actual [`MemberKind::Method`]
//! member of the form class. Method-match alone is NOT sufficient — a non-event
//! ident property whose value collides with a real method name (e.g.
//! `Action = FormCreate` when a `FormCreate` method exists) must NOT produce a
//! HandlerLink, or the LSP would offer a spurious go-to-def/rename across the
//! form boundary. A non-event ident value is not a handler candidate at all, so
//! its non-match is silent, not "missing". A genuine `On…` property whose
//! handler matches no method still warns (subject to the honest-diagnostic
//! ancestor policy above).

use crate::context::Identifier;
use crate::dfm::{DfmName, DfmObject};
use crate::meta::CodeLocation;
use crate::unit_cache::{InterfaceSymbol, MemberKind, MemberSymbol, UnitInterface};

/// A dfm component (`object Button1: TButton`) resolved to a published field of
/// the form class (`Button1: TButton;`). Both endpoints carry a location so the
/// LSP can navigate either direction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentLink {
    /// Component name key (folded) as written in the dfm.
    pub component_key: Identifier,
    /// Byte offset of the component name in the dfm text.
    pub component_position: usize,
    /// The matched published field's key (folded).
    pub field_key: Identifier,
    /// The field's declaration location in the pas.
    pub field_location: CodeLocation,
}

/// A dfm event property (`OnClick = Button1Click`) resolved to a method member
/// of the form class (`procedure Button1Click(Sender: TObject);`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandlerLink {
    /// Event property path key (`OnClick`), folded.
    pub event_key: Identifier,
    /// Byte offset of the event property name in the dfm text.
    pub event_position: usize,
    /// Matched method member key (folded).
    pub method_key: Identifier,
    /// The method's declaration location in the pas.
    pub method_location: CodeLocation,
}

/// Honest diagnostics — never a false "missing" for an inheritable member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DfmDiagnostic {
    /// The root dfm object's class is not declared in THIS unit's interface
    /// (declared elsewhere / DCU-only). No per-member matching is attempted, so
    /// this is the ONLY diagnostic emitted in that case.
    FormClassNotFound {
        class_key: Identifier,
        class_position: usize,
    },
    /// A named component has no matching published field AND the form class is
    /// ancestor-less (cannot inherit) → the field genuinely cannot exist.
    /// Hard error.
    DanglingComponent {
        component_key: Identifier,
        component_position: usize,
        /// The component's declared class in the dfm (`TButton`).
        component_class_key: Identifier,
    },
    /// A named component has no directly-matching published field, but the form
    /// class HAS ancestors (possibly cross-unit) that could declare it →
    /// INFO note, not an error.
    UnresolvedComponentPossiblyInherited {
        component_key: Identifier,
        component_position: usize,
        component_class_key: Identifier,
    },
    /// A named component matches a published field BY NAME but the field's type
    /// differs from the component's class. The name resolves; the type doesn't
    /// — surfaced regardless of ancestors (the field IS here, it just mismatches).
    ComponentTypeMismatch {
        component_key: Identifier,
        component_position: usize,
        /// Component's dfm class (`TButton`).
        component_class_key: Identifier,
        /// The field's declared type key (`TEdit`).
        field_type_key: Identifier,
        field_location: CodeLocation,
    },
    /// An event property's handler value matches no method AND the form class is
    /// ancestor-less → the method genuinely cannot exist. Hard error.
    MissingHandler {
        event_key: Identifier,
        event_position: usize,
        handler_key: Identifier,
    },
    /// An event property's handler value matches no method directly, but the
    /// form class HAS ancestors that could declare it → INFO note, not an error.
    UnresolvedHandlerPossiblyInherited {
        event_key: Identifier,
        event_position: usize,
        handler_key: Identifier,
    },
}

impl DfmDiagnostic {
    /// A human-readable message for this finding (LSP `publishDiagnostics`).
    pub fn message(&self) -> String {
        match self {
            DfmDiagnostic::FormClassNotFound { class_key, .. } => format!(
                "form class '{}' is not declared in this unit",
                crate::globals::resolve(*class_key)
            ),
            DfmDiagnostic::DanglingComponent {
                component_key,
                component_class_key,
                ..
            } => format!(
                "dfm component '{}' ({}) has no matching published field",
                crate::globals::resolve(*component_key),
                crate::globals::resolve(*component_class_key)
            ),
            DfmDiagnostic::UnresolvedComponentPossiblyInherited { component_key, .. } => format!(
                "dfm component '{}' has no field here; it may be inherited from a base form",
                crate::globals::resolve(*component_key)
            ),
            DfmDiagnostic::ComponentTypeMismatch {
                component_key,
                component_class_key,
                field_type_key,
                ..
            } => format!(
                "dfm component '{}' is {} but the field is declared {}",
                crate::globals::resolve(*component_key),
                crate::globals::resolve(*component_class_key),
                crate::globals::resolve(*field_type_key)
            ),
            DfmDiagnostic::MissingHandler {
                event_key,
                handler_key,
                ..
            } => format!(
                "dfm event '{}' references method '{}' which does not exist",
                crate::globals::resolve(*event_key),
                crate::globals::resolve(*handler_key)
            ),
            DfmDiagnostic::UnresolvedHandlerPossiblyInherited {
                event_key,
                handler_key,
                ..
            } => format!(
                "dfm event '{}' references method '{}'; it may be inherited from a base form",
                crate::globals::resolve(*event_key),
                crate::globals::resolve(*handler_key)
            ),
        }
    }

    /// The pas-side source location this finding names, when one exists (only
    /// the type-mismatch finding points at a concrete pas member). Otherwise
    /// `None` — the finding's only anchor is a dfm byte offset ([`Self::dfm_offset`]).
    pub fn pas_location(&self) -> Option<CodeLocation> {
        match self {
            DfmDiagnostic::ComponentTypeMismatch { field_location, .. } => Some(*field_location),
            _ => None,
        }
    }

    /// This finding's severity. A HARD finding — a component/handler that
    /// genuinely cannot resolve (ancestor-less form), or a name that resolves but
    /// with the wrong type — is a [`Severity::Warning`]. A NOTE — the form class
    /// isn't declared here, or an unresolved member that a base form MIGHT still
    /// declare — is a [`Severity::Hint`]: honest, non-alarming, never claiming a
    /// definite defect.
    pub fn severity(&self) -> crate::token_cursor::Severity {
        use crate::token_cursor::Severity;
        match self {
            DfmDiagnostic::DanglingComponent { .. }
            | DfmDiagnostic::ComponentTypeMismatch { .. }
            | DfmDiagnostic::MissingHandler { .. } => Severity::Warning,
            DfmDiagnostic::FormClassNotFound { .. }
            | DfmDiagnostic::UnresolvedComponentPossiblyInherited { .. }
            | DfmDiagnostic::UnresolvedHandlerPossiblyInherited { .. } => Severity::Hint,
        }
    }

    /// The byte offset into the dfm file this finding refers to.
    pub fn dfm_offset(&self) -> usize {
        match self {
            DfmDiagnostic::FormClassNotFound { class_position, .. } => *class_position,
            DfmDiagnostic::DanglingComponent { component_position, .. }
            | DfmDiagnostic::UnresolvedComponentPossiblyInherited { component_position, .. }
            | DfmDiagnostic::ComponentTypeMismatch { component_position, .. } => *component_position,
            DfmDiagnostic::MissingHandler { event_position, .. }
            | DfmDiagnostic::UnresolvedHandlerPossiblyInherited { event_position, .. } => {
                *event_position
            }
        }
    }
}

/// The full result of linking one dfm to one unit interface.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DfmLinkResult {
    pub component_links: Vec<ComponentLink>,
    pub handler_links: Vec<HandlerLink>,
    pub diagnostics: Vec<DfmDiagnostic>,
}

/// Link a parsed dfm tree against a parsed unit interface. Pure, no I/O.
///
/// The root dfm object's `class_name` is the form's class; it is looked up in
/// `interface`. See the module doc for the honest-diagnostic policy.
pub fn link_dfm(interface: &UnitInterface, root: &DfmObject) -> DfmLinkResult {
    let mut result = DfmLinkResult::default();

    let Some(form_class) = interface.find(root.class_name.key) else {
        // The form class isn't declared in THIS unit (elsewhere / DCU). We
        // cannot ground any match — emit exactly one note and stop. Producing
        // dangling/missing here would be pure guessing.
        result.diagnostics.push(DfmDiagnostic::FormClassNotFound {
            class_key: root.class_name.key,
            class_position: root.class_name.position,
        });
        return result;
    };

    // Whether an unresolved member may be inherited from a base form. Reuses
    // task-2's signal: true for any class (implicit TObject) or one naming
    // explicit/cross-unit ancestors — i.e. essentially every real form.
    let may_inherit = form_class.has_ancestors;

    link_components(form_class, root, may_inherit, &mut result);
    link_handlers(form_class, root, may_inherit, &mut result);

    result
}

/// Component links: every named node in the tree matched against a **published
/// field** of the form class whose type equals the node's class.
fn link_components(
    form_class: &InterfaceSymbol,
    root: &DfmObject,
    may_inherit: bool,
    result: &mut DfmLinkResult,
) {
    for node in root.walk() {
        // The ROOT object's name is the form instance itself (`MainForm`), not a
        // published field of the form class — skip it. Only nested components
        // are IDE-managed published fields.
        if std::ptr::eq(node, root) {
            continue;
        }
        let Some(name) = &node.name else {
            continue; // unnamed nodes (`object TMenuItem`) declare no field
        };
        match resolve_field(form_class, name, node.class_name.key) {
            FieldResolution::Matched { field } => {
                result.component_links.push(ComponentLink {
                    component_key: name.key,
                    component_position: name.position,
                    field_key: field.key,
                    field_location: field.location,
                });
            }
            FieldResolution::TypeMismatch { field, field_type } => {
                result
                    .diagnostics
                    .push(DfmDiagnostic::ComponentTypeMismatch {
                        component_key: name.key,
                        component_position: name.position,
                        component_class_key: node.class_name.key,
                        field_type_key: field_type,
                        field_location: field.location,
                    });
            }
            FieldResolution::Absent => {
                result.diagnostics.push(if may_inherit {
                    DfmDiagnostic::UnresolvedComponentPossiblyInherited {
                        component_key: name.key,
                        component_position: name.position,
                        component_class_key: node.class_name.key,
                    }
                } else {
                    DfmDiagnostic::DanglingComponent {
                        component_key: name.key,
                        component_position: name.position,
                        component_class_key: node.class_name.key,
                    }
                });
            }
        }
    }
}

enum FieldResolution<'a> {
    /// A published field of the right name AND type.
    Matched { field: &'a MemberSymbol },
    /// A published field of the right name but a DIFFERENT type.
    TypeMismatch {
        field: &'a MemberSymbol,
        field_type: Identifier,
    },
    /// No published field of that name at all.
    Absent,
}

/// Match a component name against a [`MemberKind::Field`] of the form class.
///
/// We require the member to be a `Field` whose name matches; the type must
/// equal the component's dfm class. Visibility is deliberately NOT gated: the
/// Delphi IDE emits component fields into the class's default (first,
/// unlabeled) section, which the parser records as `Visibility::Unspecified`,
/// not `Published`. Gating on `Published` alone would therefore zero out the
/// links for real forms — ANY matching `Field` links, regardless of its
/// visibility section. A name match with a different type is a `TypeMismatch` —
/// surfaced even though ancestors exist, because the field IS present here.
fn resolve_field<'a>(
    form_class: &'a InterfaceSymbol,
    component: &DfmName,
    component_class_key: Identifier,
) -> FieldResolution<'a> {
    // Only fields are IDE-managed component references. A same-named method or
    // property is not the component field — ignore those and keep looking for a
    // field. Visibility is not consulted (see doc: IDE fields land in the
    // Unspecified default section, so gating on Published would break links).
    let field = form_class
        .members
        .iter()
        .find(|member| member.key == component.key && member.kind == MemberKind::Field);
    let Some(field) = field else {
        return FieldResolution::Absent;
    };
    match field.type_key {
        // The component's dfm class must equal the field's declared type.
        Some(field_type) if field_type == component_class_key => FieldResolution::Matched { field },
        // Field type is a complex/anonymous type we don't reduce to a key — we
        // cannot prove a mismatch, so treat it as a match by name (the honest,
        // non-false-positive direction).
        None => FieldResolution::Matched { field },
        Some(field_type) => FieldResolution::TypeMismatch { field, field_type },
    }
}

/// Handler links: every genuine event property (`On…`) whose ident value
/// matches a METHOD member of the form class. Non-event ident properties (enum
/// values, `Kind`/`Action`/…) are never handler candidates, even when their
/// value collides with a method name — so they produce no link and no
/// diagnostic.
fn link_handlers(
    form_class: &InterfaceSymbol,
    root: &DfmObject,
    may_inherit: bool,
    result: &mut DfmLinkResult,
) {
    for node in root.walk() {
        for (property_path, value) in node.identifier_properties() {
            // Only genuine event properties (`On…`) are handler candidates. A
            // non-event ident property (`Kind = bkOK`, `Action = SomeName`,
            // `Align = alClient`) is NEVER a handler, even if its value key
            // happens to collide with a real method name — event-name AND
            // method-match are BOTH required to form a HandlerLink. Without the
            // event-name gate, such a collision would emit a spurious
            // cross-boundary link the LSP would offer for go-to-def/rename.
            if !is_event_property(property_path) {
                continue;
            }
            let method = form_class
                .members
                .iter()
                .find(|member| member.key == value.key && member.kind == MemberKind::Method);
            match method {
                Some(method) => {
                    result.handler_links.push(HandlerLink {
                        event_key: property_path.key,
                        event_position: property_path.position,
                        method_key: method.key,
                        method_location: method.location,
                    });
                }
                None => {
                    // A genuine event property whose handler resolves to no
                    // method. Subject to the honest-diagnostic ancestor policy:
                    // a soft "possibly inherited" note when the form can inherit,
                    // a hard "missing" only for an ancestor-less form. A
                    // non-event ident value never reaches here (skipped above),
                    // so a plain enum value is silent, not "missing".
                    result.diagnostics.push(if may_inherit {
                        DfmDiagnostic::UnresolvedHandlerPossiblyInherited {
                            event_key: property_path.key,
                            event_position: property_path.position,
                            handler_key: value.key,
                        }
                    } else {
                        DfmDiagnostic::MissingHandler {
                            event_key: property_path.key,
                            event_position: property_path.position,
                            handler_key: value.key,
                        }
                    });
                }
            }
        }
    }
}

/// Does this property name follow the event-handler convention (`On…`)? This
/// is a REQUIRED conjunct for forming a handler link: a property must be an
/// event AND its value must resolve to a method. It also decides whether an
/// unresolved event property deserves a diagnostic. A non-`On` ident property
/// (a plain enum value, or a `Kind`/`Action` whose value collides with a method
/// name) is not a handler and is silently skipped — no link, no diagnostic.
fn is_event_property(property_path: &DfmName) -> bool {
    let name = crate::globals::resolve(property_path.display);
    // The last path segment is what names the property (`Font.OnChange` is not a
    // real shape, but be robust: check the final component).
    let last = name.rsplit('.').next().unwrap_or(name);
    let mut chars = last.chars();
    matches!(chars.next(), Some('O') | Some('o'))
        && matches!(chars.next(), Some('N') | Some('n'))
        // require a third char so `On` alone doesn't fire; real Delphi event
        // names are always `On<Event>` (`OnClick`, `OnCreate`).
        && last.len() > 2
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Source;
    use crate::context::{DefineSet, ProjectContext, SwitchState, TargetPlatform};
    use crate::dfm::parse_dfm;
    use crate::parser::parse_file_full;
    use crate::unit_cache::UnitCache;
    use crate::unit_meta::build_interface;
    use std::collections::HashMap;
    use std::sync::Arc;

    fn test_context() -> Arc<ProjectContext> {
        Arc::new(ProjectContext {
            configuration: "Debug".to_string(),
            platform_name: "Win32".to_string(),
            platform: TargetPlatform::Win32,
            compiler_version: 36.0,
            rtl_version: 36.0,
            base_defines: DefineSet::default(),
            search_paths: Vec::new(),
            include_paths: Vec::new(),
            namespaces: Vec::new(),
            unit_aliases: HashMap::new(),
            default_switches: SwitchState::default(),
            unit_cache: UnitCache::default(),
        })
    }

    /// Parse pas source into its derived `UnitInterface`. The interface is
    /// OWNED and returned; the parse arena is the global one so `Identifier`s
    /// match those the dfm parse interns.
    fn interface_from(pas: &str) -> UnitInterface {
        let arena = crate::globals::arena();
        let context = test_context();
        let file = arena.insert_virtual("Form.pas", pas.to_string());
        let mut outcome = parse_file_full(arena, context, file, None).unwrap();
        let Some(Source::Unit(unit)) = outcome.source.take() else {
            panic!("expected a unit");
        };
        build_interface(&unit)
    }

    fn key(name: &str) -> Identifier {
        crate::globals::intern_key(name)
    }

    /// A realistic-shaped VCL form: `TMainForm = class(TForm)` with published
    /// component fields and matching handler methods.
    const FORM_PAS: &str = "unit MainForm;\ninterface\n\
        uses Vcl.Forms, Vcl.StdCtrls;\n\
        type\n\
          TMainForm = class(TForm)\n\
          published\n\
            OkButton: TButton;\n\
            NameEdit: TEdit;\n\
            procedure FormCreate(Sender: TObject);\n\
            procedure OkButtonClick(Sender: TObject);\n\
          end;\n\
        implementation\nend.";

    const FORM_DFM: &str = "object MainForm: TMainForm\n\
        \x20 Left = 0\n\
        \x20 Caption = 'Hello'\n\
        \x20 Color = clBtnFace\n\
        \x20 OnCreate = FormCreate\n\
        \x20 object OkButton: TButton\n\
        \x20   OnClick = OkButtonClick\n\
        \x20 end\n\
        \x20 object NameEdit: TEdit\n\
        \x20 end\n\
        end\n";

    #[test]
    fn component_and_handler_links_on_realistic_form() {
        let context = test_context();
        let interface = interface_from(FORM_PAS);
        let root = parse_dfm(FORM_DFM, &context).unwrap();
        let result = link_dfm(&interface, &root);

        // component → published field
        assert!(
            result.component_links.iter().any(|link| link.component_key
                == key("OkButton")
                && link.field_key == key("OkButton")),
            "OkButton must link to its published field: {result:?}"
        );
        assert!(
            result
                .component_links
                .iter()
                .any(|link| link.component_key == key("NameEdit")),
            "NameEdit must link: {result:?}"
        );

        // event property → method member
        assert!(
            result.handler_links.iter().any(
                |link| link.event_key == key("OnCreate") && link.method_key == key("FormCreate")
            ),
            "OnCreate must link to FormCreate: {result:?}"
        );
        assert!(
            result.handler_links.iter().any(
                |link| link.event_key == key("OnClick") && link.method_key == key("OkButtonClick")
            ),
            "OnClick must link to OkButtonClick: {result:?}"
        );
    }

    #[test]
    fn enum_valued_property_produces_no_link() {
        // `Color = clBtnFace` is an enum value, not a handler. It must produce
        // NO handler link and NO diagnostic (it's not an `On…` property).
        let context = test_context();
        let interface = interface_from(FORM_PAS);
        let root = parse_dfm(FORM_DFM, &context).unwrap();
        let result = link_dfm(&interface, &root);

        assert!(
            !result
                .handler_links
                .iter()
                .any(|link| link.method_key == key("clBtnFace")),
            "an enum value must never become a handler link: {result:?}"
        );
        // no diagnostic mentions the enum value either
        assert!(
            !result.diagnostics.iter().any(|diagnostic| matches!(
                diagnostic,
                DfmDiagnostic::MissingHandler { handler_key, .. }
                    | DfmDiagnostic::UnresolvedHandlerPossiblyInherited { handler_key, .. }
                    if *handler_key == key("clBtnFace")
            )),
            "an enum value must not produce a missing-handler diagnostic: {result:?}"
        );
    }

    #[test]
    fn dangling_component_on_ancestor_less_form_is_a_hard_diagnostic() {
        // A form class with NO ancestors (a record — genuinely cannot inherit)
        // whose dfm names a component absent from it → hard DanglingComponent.
        // (Records can't be form classes in Delphi, but they are the honest
        // ancestor-less shape for testing the confident-false path.)
        let context = test_context();
        let interface = interface_from(
            "unit Bare;\ninterface\n\
             type TBareForm = record\n  Known: TButton;\nend;\n\
             implementation\nend.",
        );
        let dfm = "object Widget: TBareForm\n\
            \x20 object Ghost: TButton\n\
            \x20 end\n\
            end\n";
        let root = parse_dfm(dfm, &context).unwrap();
        let result = link_dfm(&interface, &root);

        assert!(
            result.diagnostics.iter().any(|diagnostic| matches!(
                diagnostic,
                DfmDiagnostic::DanglingComponent { component_key, .. }
                    if *component_key == key("Ghost")
            )),
            "an absent component on an ancestor-less form must be a hard dangling error: {result:?}"
        );
        // and it must NOT be softened to a possibly-inherited note
        assert!(
            !result.diagnostics.iter().any(|diagnostic| matches!(
                diagnostic,
                DfmDiagnostic::UnresolvedComponentPossiblyInherited { .. }
            )),
            "ancestor-less form must not emit the inherited note: {result:?}"
        );
    }

    #[test]
    fn unknown_base_form_yields_unresolved_note_not_error() {
        // `TMainForm = class(TBaseForm)` where TBaseForm is cross-unit/unknown.
        // A component absent from THIS unit's fields could be inherited → an
        // INFO note, NEVER a hard dangling error (Unknown-not-false).
        let context = test_context();
        let interface = interface_from(
            "unit Derived;\ninterface\n\
             uses BaseUnit;\n\
             type TMainForm = class(TBaseForm)\n\
             published\n  OkButton: TButton;\nend;\n\
             implementation\nend.",
        );
        // `PanelFromBase` is not declared here — but the base could supply it.
        let dfm = "object MainForm: TMainForm\n\
            \x20 object PanelFromBase: TPanel\n\
            \x20 end\n\
            \x20 OnBaseEvent = InheritedHandler\n\
            end\n";
        let root = parse_dfm(dfm, &context).unwrap();
        let result = link_dfm(&interface, &root);

        assert!(
            result.diagnostics.iter().any(|diagnostic| matches!(
                diagnostic,
                DfmDiagnostic::UnresolvedComponentPossiblyInherited { component_key, .. }
                    if *component_key == key("PanelFromBase")
            )),
            "cross-unit-base component must be an inherited note: {result:?}"
        );
        // the `On…` handler with no local method is likewise a soft note
        assert!(
            result.diagnostics.iter().any(|diagnostic| matches!(
                diagnostic,
                DfmDiagnostic::UnresolvedHandlerPossiblyInherited { handler_key, .. }
                    if *handler_key == key("InheritedHandler")
            )),
            "cross-unit-base handler must be an inherited note: {result:?}"
        );
        // NEVER a hard error in either family
        assert!(
            !result.diagnostics.iter().any(|diagnostic| matches!(
                diagnostic,
                DfmDiagnostic::DanglingComponent { .. } | DfmDiagnostic::MissingHandler { .. }
            )),
            "a form with an unknown base must not emit any hard missing/dangling: {result:?}"
        );
    }

    #[test]
    fn form_class_not_in_unit_emits_only_a_note() {
        // The dfm's root class is declared nowhere in this unit → a single
        // FormClassNotFound note and NOTHING else (no per-member guessing).
        let context = test_context();
        let interface = interface_from(
            "unit Empty;\ninterface\n\
             type TSomethingElse = class end;\n\
             implementation\nend.",
        );
        let dfm = "object Foreign: TUnknownForm\n\
            \x20 object Child: TButton\n\
            \x20 end\n\
            \x20 OnClick = Handler\n\
            end\n";
        let root = parse_dfm(dfm, &context).unwrap();
        let result = link_dfm(&interface, &root);

        assert_eq!(result.component_links.len(), 0);
        assert_eq!(result.handler_links.len(), 0);
        assert_eq!(result.diagnostics.len(), 1);
        assert!(matches!(
            result.diagnostics[0],
            DfmDiagnostic::FormClassNotFound { class_key, .. } if class_key == key("TUnknownForm")
        ));
    }

    #[test]
    fn type_mismatch_when_name_matches_but_type_differs() {
        // The form has a published `OkButton: TEdit`, but the dfm declares
        // `object OkButton: TButton`. Name matches, type doesn't → mismatch
        // diagnostic, surfaced even though the form (a class) has ancestors.
        let context = test_context();
        let interface = interface_from(
            "unit Mism;\ninterface\n\
             type TMainForm = class(TForm)\n\
             published\n  OkButton: TEdit;\nend;\n\
             implementation\nend.",
        );
        let dfm = "object MainForm: TMainForm\n\
            \x20 object OkButton: TButton\n\
            \x20 end\n\
            end\n";
        let root = parse_dfm(dfm, &context).unwrap();
        let result = link_dfm(&interface, &root);

        assert!(
            result.diagnostics.iter().any(|diagnostic| matches!(
                diagnostic,
                DfmDiagnostic::ComponentTypeMismatch { component_key, field_type_key, component_class_key, .. }
                    if *component_key == key("OkButton")
                        && *field_type_key == key("TEdit")
                        && *component_class_key == key("TButton")
            )),
            "a name-match/type-mismatch must be flagged: {result:?}"
        );
        // it is NOT counted as a successful component link
        assert!(
            !result
                .component_links
                .iter()
                .any(|link| link.component_key == key("OkButton")),
            "a mismatched field must not produce a link: {result:?}"
        );
    }

    #[test]
    fn non_event_ident_property_colliding_with_method_name_produces_no_link() {
        // A non-event ident property whose value COLLIDES with a real method
        // name (`Action = FormCreate`, and the form has a `FormCreate` method)
        // must NOT produce a HandlerLink — event-name AND method-match are both
        // required. A genuine `OnClick = OkButtonClick` in the same dfm still
        // links, proving the gate is the event-name conjunct, not a blanket ban.
        let context = test_context();
        let interface = interface_from(FORM_PAS);
        let dfm = "object MainForm: TMainForm\n\
            \x20 Action = FormCreate\n\
            \x20 object OkButton: TButton\n\
            \x20   OnClick = OkButtonClick\n\
            \x20 end\n\
            end\n";
        let root = parse_dfm(dfm, &context).unwrap();
        let result = link_dfm(&interface, &root);

        // the colliding non-event property must NOT link to the method
        assert!(
            !result.handler_links.iter().any(|link| link.event_key == key("Action")),
            "a non-event ident property must never form a handler link even when \
             its value collides with a method name: {result:?}"
        );
        assert!(
            !result.handler_links.iter().any(
                |link| link.event_key == key("Action") && link.method_key == key("FormCreate")
            ),
            "Action=FormCreate must not link to the FormCreate method: {result:?}"
        );
        // and it produces no diagnostic (not an event → not "missing")
        assert!(
            !result.diagnostics.iter().any(|diagnostic| matches!(
                diagnostic,
                DfmDiagnostic::MissingHandler { event_key, .. }
                    | DfmDiagnostic::UnresolvedHandlerPossiblyInherited { event_key, .. }
                    if *event_key == key("Action")
            )),
            "a non-event property must not emit a handler diagnostic: {result:?}"
        );
        // the genuine event still links
        assert!(
            result.handler_links.iter().any(
                |link| link.event_key == key("OnClick") && link.method_key == key("OkButtonClick")
            ),
            "a genuine OnClick must still link to its method: {result:?}"
        );
    }

    #[test]
    fn non_event_ident_property_matching_no_method_is_silent() {
        // A plain enum-valued property whose value doesn't match any method and
        // isn't an `On…` name (e.g. `BorderStyle = bsSingle`) is not a handler
        // candidate at all → no link, no diagnostic.
        let context = test_context();
        let interface = interface_from(FORM_PAS);
        let dfm = "object MainForm: TMainForm\n\
            \x20 BorderStyle = bsSingle\n\
            end\n";
        let root = parse_dfm(dfm, &context).unwrap();
        let result = link_dfm(&interface, &root);

        assert!(result.handler_links.is_empty());
        assert!(
            result.diagnostics.is_empty(),
            "a non-On enum property must be silent: {result:?}"
        );
    }
}
