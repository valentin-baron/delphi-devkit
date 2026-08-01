use serde::{Deserialize, Serialize};

use crate::{
    context::Identifier,
    meta::{CodeLocation, Span},
};

#[derive(Debug, Serialize, Deserialize)]
pub enum Source {
    Unit(Unit),
    Program(Program),
    Library(Library),
    Package(Package),
}

/// A possibly dotted name (`Winapi.Windows`) as one symbol. The location
/// spans all parts.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct QualifiedName {
    /// Display track: spelled exactly as in source.
    pub name: Identifier,
    /// Lookup track: case-folded — use for every comparison, cache key and
    /// symbol-table access.
    pub key: Identifier,
    pub location: CodeLocation,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Unit {
    pub name: QualifiedName,
    pub interface_uses: Option<UsesDeclarations>,
    /// Shallow slice-2 view: names, kinds and locations of everything the
    /// interface section declares. Bodies/types follow in later slices.
    pub interface_declarations: Vec<InterfaceDeclaration>,
    pub implementation_uses: Option<UsesDeclarations>,
    // todo: implementation index
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeclarationKind {
    Type,
    Const,
    ResourceString,
    Var,
    ThreadVar,
    Procedure,
    Function,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterfaceDeclaration {
    pub kind: DeclarationKind,
    pub name: QualifiedName,
    /// For `Const` with a single-literal initializer (`= 3;`, `= 'x';`,
    /// `= -1;`): the value, usable in downstream `{$IF}` evaluation.
    pub constant_value: Option<crate::unit_cache::ConstantValue>,
    /// For `Type` declarations: the structured right-hand side.
    pub type_expression: Option<TypeExpression>,
    /// Declared generic type parameters (`TList<T: class>`,
    /// `function Map<T>`): empty for non-generic declarations. Parameter
    /// names are declarations; constraint clauses are spans.
    pub generic_parameters: Vec<GenericParameter>,
    /// `[Foo(1)]` attributes preceding this declaration, in source order.
    /// Empty when none. Also carries a `NestedType`'s attributes.
    pub attributes: Vec<Attribute>,
}

/// A `[Name]` / `[Name(arguments)]` attribute annotation. The name is captured
/// AS WRITTEN (dual-track interned): Delphi treats a trailing `Attribute`
/// suffix as implicit (`[Foo]` binds `TFooAttribute`), but we do NOT normalize
/// it away — the display track keeps the exact spelling and the key track its
/// case-folded form; suffix-aware matching is a later semantic concern
/// (SESSION.md ledger #16). Arguments stay a raw source span — attribute
/// argument expressions are never eagerly evaluated (AST-carries-no-strings /
/// expressions-are-spans invariant).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attribute {
    /// Attribute name, possibly dotted (`Xml.Serializable`), dual-track.
    pub name: QualifiedName,
    /// The `(...)` argument list as one source span, `None` for a bare
    /// `[Foo]` with no parentheses. Contents are never parsed.
    pub arguments: Option<CodeLocation>,
    /// The whole `[...]` group location up to and including this attribute's
    /// name/arguments (used for go-to / hover ranges).
    pub location: CodeLocation,
}

/// One declared generic type parameter and its (optional) constraint clause.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenericParameter {
    /// The parameter name (`T`) — a declaration, dual-track interned.
    pub name: QualifiedName,
    /// `: class, constructor` / `: IComparable<T>` — the whole constraint
    /// clause captured as one source span (`None` when unconstrained).
    /// Left as a span; constraint resolution is a later semantic stage.
    pub constraints: Option<CodeLocation>,
}

// ─── Type expressions (deep type parse) ──────────────────────────────────
//
// Expressions inside types (array bounds, enum values, string lengths,
// parameter defaults, subranges) are captured as source spans, not parsed —
// constant-expression evaluation is a later, separate stage.

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TypeExpression {
    /// `TFoo` | `Unit.TFoo` | `TList<Integer>` | generic parameter `T`
    Reference {
        name: QualifiedName,
        type_arguments: Vec<TypeExpression>,
    },
    /// `^T`
    Pointer(Box<TypeExpression>),
    /// `class of TFoo`
    ClassReference(QualifiedName),
    /// `array of T` (bounds None) | `array[0..3, Byte] of T` (bounds span)
    Array {
        bounds: Option<CodeLocation>,
        element: Box<TypeExpression>,
    },
    /// `array of const` (open varargs parameter)
    ArrayOfConst,
    /// `set of T`
    SetOf(Box<TypeExpression>),
    /// `file` | `file of T`
    File(Option<Box<TypeExpression>>),
    /// `(meA, meB = 2)`
    Enumeration(Vec<EnumerationMember>),
    /// `0..100`, `'a'..'z'` — bounds as one span
    Subrange(CodeLocation),
    /// `string[80]` — length expression span
    SizedString(CodeLocation),
    /// `procedure(...)` / `function(...): T` (+ `of object`)
    Routine(Box<RoutineType>),
    /// `reference to procedure/function(...)`
    AnonymousMethod(Box<RoutineType>),
    Record(Box<StructuredType>),
    Class(Box<ClassType>),
    Interface(Box<InterfaceType>),
    /// `TFoo = class;` / `IBar = interface;` / `TP = ^TFoo;` forward class
    ForwardClass,
    ForwardInterface,
    ForwardDispInterface,
    /// `TMyInt = type Integer` (distinct type)
    Distinct(Box<TypeExpression>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnumerationMember {
    pub name: QualifiedName,
    /// `= 2` explicit ordinal, as a span.
    pub explicit_value: Option<CodeLocation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoutineKind {
    Procedure,
    Function,
    Constructor,
    Destructor,
    Operator,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutineType {
    pub kind: RoutineKind,
    pub parameters: Vec<Parameter>,
    pub return_type: Option<TypeExpression>,
    /// `of object` (method pointer)
    pub of_object: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParameterModifier {
    None,
    Var,
    Const,
    Out,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Parameter {
    pub modifier: ParameterModifier,
    pub names: Vec<QualifiedName>,
    /// `None` for untyped parameters (`var Buffer`).
    pub parameter_type: Option<TypeExpression>,
    /// `= DefaultValue` span.
    pub default: Option<CodeLocation>,
    /// `[ref] const A: T` — parameter attributes, source order.
    pub attributes: Vec<Attribute>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Visibility {
    /// Members before any visibility keyword (class default: published with
    /// $M+, else public; record: public) — resolution is a semantic concern.
    Unspecified,
    Private,
    Protected,
    Public,
    Published,
    Automated,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisibilitySection {
    pub visibility: Visibility,
    pub strict: bool,
    pub members: Vec<Member>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Member {
    Field {
        names: Vec<QualifiedName>,
        field_type: TypeExpression,
        is_class_var: bool,
        /// `[Foo]` attributes preceding the field group, source order.
        attributes: Vec<Attribute>,
    },
    Method(Box<MethodDeclaration>),
    Property(Box<PropertyDeclaration>),
    NestedType(Box<InterfaceDeclaration>),
    NestedConst {
        name: QualifiedName,
        constant_value: Option<crate::unit_cache::ConstantValue>,
        /// `[Foo]` attributes preceding the nested const, source order.
        attributes: Vec<Attribute>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MethodDeclaration {
    pub name: QualifiedName,
    pub routine: RoutineType,
    pub is_class_method: bool,
    /// Directive keywords after the signature (`virtual`, `override`,
    /// `stdcall`, `message`…), folded keys in source order.
    pub directives: Vec<Identifier>,
    /// Declared generic type parameters (`procedure Map<T: class>`): empty
    /// for non-generic methods.
    pub generic_parameters: Vec<GenericParameter>,
    /// Method resolution clause `procedure IFoo.Method = Impl;`: `name` holds
    /// the qualified `IFoo.Method` being resolved, this the implementing
    /// routine (`Impl`). `None` for ordinary method declarations. When set,
    /// `routine` carries no real signature.
    pub resolution_target: Option<QualifiedName>,
    /// `[Foo]` attributes preceding the method, source order.
    pub attributes: Vec<Attribute>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PropertyDeclaration {
    pub name: QualifiedName,
    /// Indexed properties: `property Items[Index: Integer]: T`.
    pub index_parameters: Vec<Parameter>,
    pub property_type: Option<TypeExpression>,
    /// `read` target (field or getter) — dfm handler linking + completion.
    pub read_target: Option<QualifiedName>,
    /// `write` target (field or setter).
    pub write_target: Option<QualifiedName>,
    /// `; default;` marker (default array property).
    pub is_default: bool,
    /// `class property` — a class-level (static) property.
    pub is_class: bool,
    /// `[Foo]` attributes preceding the property, source order.
    pub attributes: Vec<Attribute>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StructuredKind {
    Record,
    /// Legacy `object`
    Object,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StructuredType {
    pub kind: StructuredKind,
    pub is_packed: bool,
    pub sections: Vec<VisibilitySection>,
    pub variant_part: Option<VariantPart>,
    /// `record helper for T`
    pub helper_for: Option<QualifiedName>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariantPart {
    /// `case Tag: Byte of` — selector name (None for `case Byte of`).
    pub selector_name: Option<QualifiedName>,
    pub selector_type: QualifiedName,
    pub arms: Vec<VariantArm>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariantArm {
    /// Label list span (`0, 1:`).
    pub labels: CodeLocation,
    pub fields: Vec<Member>,
    /// Variant parts nest: an arm may end in another `case … of`.
    pub nested: Option<Box<VariantPart>>,
}

/// An entry in a class/interface ancestor list, retaining any generic
/// instantiation arguments (`TList<Integer>`, `IEnumerable<T>`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ancestor {
    pub name: QualifiedName,
    /// `<Integer>` on `class(TList<Integer>)` — empty for non-generic bases.
    pub type_arguments: Vec<TypeExpression>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassType {
    pub is_packed: bool,
    pub is_sealed: bool,
    pub is_abstract: bool,
    /// `class(TBase, IIntf)` ancestor/interface list (with generic args).
    pub ancestors: Vec<Ancestor>,
    /// `class helper [for T]`.
    pub helper_for: Option<QualifiedName>,
    pub sections: Vec<VisibilitySection>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterfaceType {
    pub is_dispinterface: bool,
    pub ancestors: Vec<Ancestor>,
    /// `['{GUID}']` span.
    pub guid: Option<CodeLocation>,
    /// Interfaces have no visibility sections — one flat member list.
    pub members: Vec<Member>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Program {
    pub name: QualifiedName,
    pub uses: Option<UsesDeclarations>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Library {
    pub name: QualifiedName,
    pub uses: Option<UsesDeclarations>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Package {
    pub name: QualifiedName,
    pub requires: Vec<QualifiedName>,
    pub contains: Option<UsesDeclarations>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UsesDeclarations {
    pub uses: Vec<UsedUnit>,
    pub location: CodeLocation,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UsedUnit {
    pub name: QualifiedName,
    /// `Main in 'Main.pas'` — the quoted path, DPR/DPK style.
    pub source_file: Option<InClause>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct InClause {
    /// Unquoted path text, display-interned verbatim (paths keep their case).
    pub path: Identifier,
    pub location: CodeLocation,
}

// ─── Implementation-section routine structure (same-unit local resolution) ──
//
// The implementation section is otherwise a flat token scan (its identifiers
// become bare `Usage` occurrences). These types add just enough STRUCTURE to
// resolve a local variable / parameter to its own declaration WITHIN the routine
// body it sits in — no expression parse, no type resolution beyond a trivial
// simple-reference type key. Every field is a span or a declaration key, in
// keeping with the "AST carries no strings / expressions are spans" invariant.

/// What a body-local declaration is. Distinguishes the declaration part kinds a
/// routine body may open (`var`/`const`/`type`/`label`) from a parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LocalKind {
    Var,
    Const,
    Type,
    Param,
    Label,
}

/// One local variable, constant, type, label or parameter declared inside a
/// routine. `name` gives both the lookup key and the exact source span of the
/// declaring occurrence (`QualifiedName` carries `key` + `location`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalDeclaration {
    /// The declared name — its folded lookup key and its own declaration span.
    pub name: QualifiedName,
    pub decl_kind: LocalKind,
    /// The declared type as a SIMPLE reference key (`Local: TThing` → `TThing`),
    /// else `None` for an anonymous/complex/absent type. Captured only when
    /// trivial; a later stage refines it.
    pub type_key: Option<Identifier>,
}

/// One implementation-section routine, with enough structure to resolve a body
/// position to an enclosing routine and thence to its params/locals.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImplRoutine {
    /// The routine header name and its source span (`procedure TThing.Run` →
    /// the `Run` occurrence).
    pub name: QualifiedName,
    /// `Some(TThing)` for a qualified `procedure TThing.Run`; `None` for a free
    /// routine.
    pub owner_type_key: Option<Identifier>,
    /// The WHOLE routine span, from its header keyword through the terminating
    /// `end` — a body position is enclosed when it falls inside this span. For
    /// nested routines the tightest-covering span wins (spans may overlap).
    pub body_span: Span,
    pub params: Vec<LocalDeclaration>,
    pub locals: Vec<LocalDeclaration>,
}
