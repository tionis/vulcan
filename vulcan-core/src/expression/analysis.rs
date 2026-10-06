//! Static analysis of Vulcan expressions for query planning (QRY.3).

use super::ast::Expr;

/// Whether evaluating `expr` can build the file object, and so read the
/// hydrated fields (tags, links, inlinks, tasks, lists), of a note other
/// than the current row and `this`: `.file` on anything but `this`,
/// `asFile()`, `linksTo()`, or indexing that could name `file`.
/// Conservative. A formula
/// reference reaches whatever its formula reaches; callers that analyze
/// every formula themselves pass `formula_refs_reach = false`.
pub(crate) fn reaches_other_file_objects(expr: &Expr, formula_refs_reach: bool) -> bool {
    match expr {
        Expr::FieldAccess(base, field) => {
            (field.eq_ignore_ascii_case("file")
                && !matches!(&**base, Expr::Identifier(name) if name.eq_ignore_ascii_case("this")))
                || reaches_other_file_objects(base, formula_refs_reach)
        }
        Expr::IndexAccess(base, key) => {
            !matches!(&**key, Expr::Number(_) | Expr::Str(_))
                || matches!(&**key, Expr::Str(name) if name.eq_ignore_ascii_case("file"))
                || reaches_other_file_objects(base, formula_refs_reach)
        }
        Expr::MethodCall(base, method, args) => {
            // `linksTo` reads the outgoing links of the note a link names.
            method.eq_ignore_ascii_case("asFile")
                || method.eq_ignore_ascii_case("linksTo")
                || reaches_other_file_objects(base, formula_refs_reach)
                || args
                    .iter()
                    .any(|arg| reaches_other_file_objects(arg, formula_refs_reach))
        }
        Expr::FunctionCall(_, args) | Expr::Array(args) => args
            .iter()
            .any(|arg| reaches_other_file_objects(arg, formula_refs_reach)),
        Expr::Object(fields) => fields
            .iter()
            .any(|(_, value)| reaches_other_file_objects(value, formula_refs_reach)),
        Expr::BinaryOp(left, _, right) => {
            reaches_other_file_objects(left, formula_refs_reach)
                || reaches_other_file_objects(right, formula_refs_reach)
        }
        Expr::UnaryOp(_, operand) => reaches_other_file_objects(operand, formula_refs_reach),
        Expr::Lambda(_, body) => reaches_other_file_objects(body, formula_refs_reach),
        Expr::FormulaRef(_) => formula_refs_reach,
        Expr::Null
        | Expr::Bool(_)
        | Expr::Number(_)
        | Expr::Str(_)
        | Expr::Regex { .. }
        | Expr::Identifier(_) => false,
    }
}
