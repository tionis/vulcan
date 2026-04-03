pub mod ast;
pub mod eval;
pub mod functions;
pub mod methods;
pub mod parse;
pub mod token;
pub mod value;

use ast::Expr;
use parse::Parser;

/// Parse an expression string into an AST.
pub fn parse_expression(source: &str) -> Result<Expr, String> {
    Parser::new(source)?.parse()
}
