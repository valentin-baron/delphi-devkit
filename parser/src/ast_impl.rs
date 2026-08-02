//! Implementation-section semantic AST — Stage S1: the **expression layer**.
//!
//! This module defines the [`Expression`] tree the implementation-section parser
//! builds over routine bodies (see `impl-ast-spec.md`), plus a precedence-climbing
//! (Pratt) expression parser implemented as `impl UnitParser` methods in
//! `parser.rs` (kept there because the parser needs the crate-private cursor
//! helpers; the grammar is nonetheless cohesive — every expression production
//! lives in the `// ─── Expression parser (Stage S1) ───` block of that file).
//!
//! Design invariants inherited from the interface AST (`ast.rs`):
//! * Identifiers are [`QualifiedName`] — dual-track interned (display + folded
//!   key) and carrying their own source span.
//! * Every literal / opaque region is a [`CodeLocation`] span; the AST carries no
//!   strings and never eagerly evaluates an expression leaf.
//! * Recursive positions are `Box`ed.
//!
//! S1 is standalone: nothing here is wired into `UnitMeta`, serialization or the
//! query layer yet (that is S2–S4). It exists to be unit-tested in isolation.

use serde::{Deserialize, Serialize};

use crate::ast::QualifiedName;
use crate::meta::CodeLocation;

/// A prefix / postfix unary operator over a single operand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnaryOperator {
    /// `@x` — address-of.
    AddressOf,
    /// `not x` — logical / bitwise negation.
    Not,
    /// unary `-x` — arithmetic negation.
    Negate,
    /// unary `+x` — arithmetic identity (accepted for symmetry; a no-op).
    Plus,
    /// `p^` — pointer dereference. This is a POSTFIX operator in Delphi
    /// (`pointer^`), unlike the prefix operators above; the parser applies it in
    /// the postfix chain but models it as a `Unary` so the receiver shape stays
    /// uniform.
    Dereference,
}

/// A binary operator. Membership (`in`), the type test (`is`) and the comparison
/// operators are all modelled here; the value/type-producing `as` cast is NOT a
/// `Binary` — it becomes an [`Expression::Cast`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BinaryOperator {
    // multiplicative level
    Multiply,
    Divide,
    IntegerDivide, // div
    Modulo,        // mod
    And,           // and
    ShiftLeft,     // shl
    ShiftRight,    // shr
    // additive level
    Add,
    Subtract,
    Or,  // or
    Xor, // xor
    // relational level
    Equal,        // =
    NotEqual,     // <>
    Less,         // <
    Greater,      // >
    LessEqual,    // <=
    GreaterEqual, // >=
    In,           // in
    Is,           // is (type test)
}

/// A receiver-shaped expression — enough structure to type a member/call chain
/// for completion and go-to, NOT enough to evaluate. Leaves that carry no
/// navigational structure (literals, set/array bodies, anonymous-method bodies)
/// are kept as opaque [`CodeLocation`] spans.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Expression {
    /// A bare (possibly dotted) name: `Foo`, `System.Classes`.
    Identifier(QualifiedName),
    /// `receiver.member` — a member access.
    Member {
        receiver: Box<Expression>,
        member: QualifiedName,
    },
    /// `callee(arguments)` — a call / value-cast `T(x)`. The `arguments_span`
    /// covers the whole `(...)` group (for hover / signature-help ranges).
    Call {
        callee: Box<Expression>,
        arguments: Vec<Expression>,
        arguments_span: CodeLocation,
    },
    /// `base[indices]` — an array / default-property index (`a[i, j]`).
    Index {
        base: Box<Expression>,
        indices: Vec<Expression>,
    },
    /// `operand as type_name` — the class-reference `as` cast. (`type_name(x)`
    /// value casts stay a [`Expression::Call`]; disambiguating type-vs-value is a
    /// semantic concern, deliberately not attempted here.)
    Cast {
        type_name: QualifiedName,
        operand: Box<Expression>,
    },
    /// `inherited` (bare) or `inherited Method[(args)]`. When a method name and/or
    /// arguments follow, the whole `inherited Foo(...)` postfix chain is built
    /// with this node as its innermost receiver; `method` holds the immediate
    /// method name that followed `inherited`, when present.
    Inherited { method: Option<QualifiedName> },
    /// A prefix (`@ not - +`) or postfix (`^`) unary application.
    Unary {
        operator: UnaryOperator,
        operand: Box<Expression>,
    },
    /// A binary application (see [`BinaryOperator`]).
    Binary {
        operator: BinaryOperator,
        left: Box<Expression>,
        right: Box<Expression>,
    },
    /// An anonymous-method opener in expression position — `procedure … end`,
    /// `function … end` or `reference to …`. The whole opener…`end` region is
    /// captured as one opaque span (begin/end balanced).
    //
    // S2: replace with AnonymousMethod(Box<Scope>) once the Scope type exists;
    // the closure body then becomes a real child scope instead of an opaque span.
    AnonymousMethodOpaque(CodeLocation),
    /// `[a, b, c]` — a set or array-constructor literal. The bracket group span
    /// is captured; identifier occurrences INSIDE are still recorded (so the
    /// reference index stays complete) even though the elements are not retained
    /// as sub-expressions.
    SetOrArrayLiteral(CodeLocation),
    /// A number / string / `nil` / `True` / `False` literal — an opaque leaf.
    Literal(CodeLocation),
    /// `(inner)` — an explicitly parenthesized sub-expression, retained so a
    /// later walk can distinguish grouping from precedence.
    Parenthesized(Box<Expression>),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::UnitParser;

    /// Parse a bare expression snippet and return the built [`Expression`].
    /// Mirrors the cursor-construction the other parser tests use (a virtual
    /// source over a fresh arena), then drives the S1 expression entry point.
    fn parse_expression(source: &str) -> Expression {
        UnitParser::parse_expression_for_test(source)
    }

    /// The display spelling of a `QualifiedName`, via the global interner.
    fn name_of(name: &QualifiedName) -> String {
        crate::globals::resolve(name.name).to_string()
    }

    #[test]
    fn dotted_member_chain_nests_left() {
        // a.b.c  →  Member{ Member{ Identifier(a), b }, c }
        let expression = parse_expression("a.b.c");
        let Expression::Member { receiver, member } = expression else {
            panic!("expected outer Member, got {expression:?}");
        };
        assert_eq!(name_of(&member), "c");
        let Expression::Member { receiver, member } = *receiver else {
            panic!("expected inner Member");
        };
        assert_eq!(name_of(&member), "b");
        let Expression::Identifier(base) = *receiver else {
            panic!("expected Identifier base");
        };
        assert_eq!(name_of(&base), "a");
    }

    #[test]
    fn call_with_two_arguments() {
        let expression = parse_expression("Foo(x, y)");
        let Expression::Call {
            callee, arguments, ..
        } = expression
        else {
            panic!("expected Call, got {expression:?}");
        };
        let Expression::Identifier(callee_name) = *callee else {
            panic!("expected Identifier callee");
        };
        assert_eq!(name_of(&callee_name), "Foo");
        assert_eq!(arguments.len(), 2);
    }

    #[test]
    fn mixed_postfix_chain_nests() {
        // Obj.Method(a)[0].Field
        //   → Member{ Index{ Call{ Member{ Ident(Obj), Method }, [a] }, [0] }, Field }
        let expression = parse_expression("Obj.Method(a)[0].Field");
        let Expression::Member { receiver, member } = expression else {
            panic!("expected outer Member, got {expression:?}");
        };
        assert_eq!(name_of(&member), "Field");
        let Expression::Index { base, indices } = *receiver else {
            panic!("expected Index");
        };
        assert_eq!(indices.len(), 1);
        let Expression::Call { callee, .. } = *base else {
            panic!("expected Call");
        };
        let Expression::Member { receiver, member } = *callee else {
            panic!("expected Member callee");
        };
        assert_eq!(name_of(&member), "Method");
        let Expression::Identifier(base_name) = *receiver else {
            panic!("expected Identifier Obj");
        };
        assert_eq!(name_of(&base_name), "Obj");
    }

    #[test]
    fn pointer_deref_then_member() {
        // p^.Field  →  Member{ Unary{ Dereference, Ident(p) }, Field }
        let expression = parse_expression("p^.Field");
        let Expression::Member { receiver, member } = expression else {
            panic!("expected Member, got {expression:?}");
        };
        assert_eq!(name_of(&member), "Field");
        let Expression::Unary { operator, operand } = *receiver else {
            panic!("expected Unary deref");
        };
        assert_eq!(operator, UnaryOperator::Dereference);
        let Expression::Identifier(base) = *operand else {
            panic!("expected Identifier p");
        };
        assert_eq!(name_of(&base), "p");
    }

    #[test]
    fn as_cast_produces_cast() {
        let expression = parse_expression("x as TFoo");
        let Expression::Cast { type_name, operand } = expression else {
            panic!("expected Cast, got {expression:?}");
        };
        assert_eq!(name_of(&type_name), "TFoo");
        let Expression::Identifier(base) = *operand else {
            panic!("expected Identifier x");
        };
        assert_eq!(name_of(&base), "x");
    }

    #[test]
    fn is_test_produces_binary() {
        let expression = parse_expression("x is TBar");
        let Expression::Binary {
            operator,
            left,
            right,
        } = expression
        else {
            panic!("expected Binary, got {expression:?}");
        };
        assert_eq!(operator, BinaryOperator::Is);
        let Expression::Identifier(left_name) = *left else {
            panic!("expected Identifier x");
        };
        assert_eq!(name_of(&left_name), "x");
        // The right side of `is` is a type name, modelled as an Identifier.
        let Expression::Identifier(right_name) = *right else {
            panic!("expected Identifier TBar");
        };
        assert_eq!(name_of(&right_name), "TBar");
    }

    #[test]
    fn bare_inherited() {
        let expression = parse_expression("inherited");
        let Expression::Inherited { method } = expression else {
            panic!("expected Inherited, got {expression:?}");
        };
        assert!(method.is_none());
    }

    #[test]
    fn inherited_with_method_and_arguments() {
        // inherited Create(x)
        //   → Call{ callee: Inherited{ method: Some(Create) }, [x] }
        let expression = parse_expression("inherited Create(x)");
        let Expression::Call {
            callee, arguments, ..
        } = expression
        else {
            panic!("expected Call over Inherited, got {expression:?}");
        };
        assert_eq!(arguments.len(), 1);
        let Expression::Inherited { method } = *callee else {
            panic!("expected Inherited callee");
        };
        let method = method.expect("method name after inherited");
        assert_eq!(name_of(&method), "Create");
    }

    #[test]
    fn address_of_prefix() {
        let expression = parse_expression("@Proc");
        let Expression::Unary { operator, operand } = expression else {
            panic!("expected Unary, got {expression:?}");
        };
        assert_eq!(operator, UnaryOperator::AddressOf);
        let Expression::Identifier(name) = *operand else {
            panic!("expected Identifier Proc");
        };
        assert_eq!(name_of(&name), "Proc");
    }

    #[test]
    fn precedence_multiply_binds_tighter_than_add() {
        // 1 + 2 * 3  →  Binary{ Add, 1, Binary{ Multiply, 2, 3 } }
        let expression = parse_expression("1 + 2 * 3");
        let Expression::Binary {
            operator,
            left,
            right,
        } = expression
        else {
            panic!("expected Binary, got {expression:?}");
        };
        assert_eq!(operator, BinaryOperator::Add);
        assert!(matches!(*left, Expression::Literal(_)));
        let Expression::Binary { operator, .. } = *right else {
            panic!("expected nested multiply");
        };
        assert_eq!(operator, BinaryOperator::Multiply);
    }

    #[test]
    fn set_or_array_literal() {
        let expression = parse_expression("[1, 2, 3]");
        assert!(
            matches!(expression, Expression::SetOrArrayLiteral(_)),
            "got {expression:?}"
        );
    }

    #[test]
    fn malformed_trailing_dot_degrades_without_panic() {
        // `a .`  — a member access whose member is missing. Must not panic nor
        // loop; degrades to a best-effort partial (the receiver survives) and the
        // cursor is left advanced past the offending region.
        let expression = parse_expression("a . ");
        // Either the bare Identifier(a) (dot dropped) or a Member with a synthetic
        // partial member is acceptable — the invariant under test is "no panic,
        // returns a partial". Assert it is at least an expression rooted at `a`.
        match expression {
            Expression::Identifier(name) => assert_eq!(name_of(&name), "a"),
            Expression::Member { receiver, .. } => {
                assert!(matches!(*receiver, Expression::Identifier(_)));
            }
            other => panic!("unexpected degraded shape: {other:?}"),
        }
    }
}
