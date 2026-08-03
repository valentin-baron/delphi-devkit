//! Prints the in-RAM size of the AST node types, to explain why a parsed unit's
//! footprint dwarfs its source bytes. Run:
//!   cargo test -p delphi-parser --test size_probe -- --ignored --nocapture

use std::mem::{align_of, size_of};

use delphi_parser::ast::*;
use delphi_parser::ast_impl::*;
use delphi_parser::context::Identifier;
use delphi_parser::meta::{CodeLocation, Span};

macro_rules! probe {
    ($t:ty) => {
        eprintln!(
            "{:>28}  size {:>4}  align {:>2}",
            stringify!($t),
            size_of::<$t>(),
            align_of::<$t>()
        );
    };
}

#[test]
#[ignore]
fn print_node_sizes() {
    eprintln!("--- leaves ---");
    probe!(Span);
    probe!(CodeLocation);
    probe!(QualifiedName);
    probe!(Identifier);
    probe!(Option<Identifier>);
    probe!(Option<CodeLocation>);

    eprintln!("--- implementation body (the code-heavy part) ---");
    probe!(Expression);
    probe!(Box<Expression>);
    probe!(Statement);
    probe!(Scope);
    probe!(LocalSymbol);
    probe!(RoutineImplementation);

    eprintln!("--- interface AST ---");
    probe!(TypeExpression);
    probe!(Member);
    probe!(MethodDeclaration);
    probe!(PropertyDeclaration);
    probe!(Parameter);
    probe!(InterfaceDeclaration);

    eprintln!("--- Vec / container overhead ---");
    eprintln!("  empty Vec<Expression> heap = 0, but the handle is {} bytes", size_of::<Vec<Expression>>());
    eprintln!("  each Box / Vec is a SEPARATE heap allocation (malloc rounds up + header)");
}
