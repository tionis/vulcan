//! Static analysis of Vulcan expressions for query planning (QRY.3).

use super::ast::Expr;
use super::eval::{canonical_file_field_name, normalize_field_name};

/// Whether evaluating `expr` can build the file object, and so read the
/// hydrated fields (tags, links, inlinks, tasks, lists), of a note other
/// than the current row and `this`: `.file` on anything but `this`,
/// `asFile()`, `linksTo()`, or indexing that could name `file`.
/// Conservative. A formula
/// reference reaches whatever its formula reaches; callers that analyze
/// every formula themselves pass `formula_refs_reach = false`.
#[must_use]
pub fn reaches_other_file_objects(expr: &Expr, formula_refs_reach: bool) -> bool {
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

/// File-object fields a stored note record carries; the rest (tags, links,
/// inlinks, tasks, lists) need hydration.
const STORED_FILE_FIELDS: &[&str] = &[
    "path",
    "name",
    "basename",
    "ext",
    "folder",
    "link",
    "size",
    "mtime",
    "ctime",
    "mday",
    "cday",
    "day",
    "frontmatter",
    "properties",
    "starred",
    "aliases",
];

/// File methods that read stored fields only.
const STORED_FILE_METHODS: &[&str] = &["asLink", "hasProperty", "inFolder"];

/// How a frontend binds the current row in expressions.
#[derive(Debug, Clone, Copy)]
pub struct RowBindings<'a> {
    /// Identifiers bound to whole rows (DQL `row` and grouped `rows`).
    pub whole_rows: &'a [&'a str],
    /// Whether `this` may be the current row (Bases without an embedding
    /// note) rather than a separately hydrated note (DQL).
    pub this_is_row: bool,
}

/// Whether evaluating `expr` may read the current row's hydrated
/// file-object fields (tags, links, inlinks, tasks, lists), or its whole
/// file object or row; otherwise the row needs stored fields only. Notes
/// reached through links hydrate on demand through the note lookup, but
/// any `.file` whose base might be a row counts, as does anything not
/// recognized as stored. Formula references count as nothing: callers
/// analyze every formula.
#[must_use]
pub fn reads_row_file_fields(expr: &Expr, rows: RowBindings<'_>) -> bool {
    let is_this = |expr: &Expr| matches!(expr, Expr::Identifier(name) if normalize_field_name(name) == "this");
    let is_row_file = |expr: &Expr| match expr {
        Expr::Identifier(name) => normalize_field_name(name) == "file",
        Expr::FieldAccess(base, field) => {
            normalize_field_name(field) == "file" && (rows.this_is_row || !is_this(base))
        }
        _ => false,
    };
    let is_whole_row = |expr: &Expr| match expr {
        Expr::Identifier(name) => {
            let name = normalize_field_name(name);
            rows.whole_rows.contains(&name.as_str()) || (rows.this_is_row && name == "this")
        }
        _ => false,
    };
    let reads = |expr: &Expr| reads_row_file_fields(expr, rows);
    // The base of a row's file object: identifiers name rows, `this`, or
    // link-valued fields, none of which is a file object itself.
    let base_reads = |file: &Expr| match file {
        Expr::FieldAccess(base, _) if !matches!(**base, Expr::Identifier(_)) => reads(base),
        _ => false,
    };
    match expr {
        Expr::FieldAccess(file, field) if is_row_file(file) => {
            !STORED_FILE_FIELDS.contains(&canonical_file_field_name(field).as_str())
                || base_reads(file)
        }
        Expr::MethodCall(file, method, args) if is_row_file(file) => {
            !STORED_FILE_METHODS.contains(&method.as_str())
                || args.iter().any(reads)
                || base_reads(file)
        }
        expr if is_row_file(expr) || is_whole_row(expr) => true,
        // `row.status` reads a field, not the whole row.
        Expr::FieldAccess(base, _) if is_whole_row(base) => false,
        Expr::FieldAccess(base, _) => reads(base),
        Expr::IndexAccess(base, index) => {
            is_row_file(base) || is_whole_row(base) || reads(base) || reads(index)
        }
        Expr::Array(items) | Expr::FunctionCall(_, items) => items.iter().any(reads),
        Expr::Object(fields) => fields.iter().any(|(_, value)| reads(value)),
        Expr::BinaryOp(left, _, right) => reads(left) || reads(right),
        Expr::UnaryOp(_, operand) => reads(operand),
        Expr::MethodCall(base, _, args) => reads(base) || args.iter().any(reads),
        Expr::Lambda(_, body) => reads(body),
        Expr::Null
        | Expr::Bool(_)
        | Expr::Number(_)
        | Expr::Str(_)
        | Expr::Regex { .. }
        | Expr::Identifier(_)
        | Expr::FormulaRef(_) => false,
    }
}
