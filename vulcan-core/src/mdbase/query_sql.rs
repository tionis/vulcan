//! Conservative scalar candidate predicates over rebuildable record metadata.
//!
//! This is not permission, snapshot, or CEL resource-limit evidence. In particular,
//! callers cannot skip CEL input checks just because a row cannot match a filter.

use crate::predicate::{
    Atom, Comparison, Decision, Dialect, Field, Literal, Predicate, RecordValues,
};
use cel_parser::ast::{operators, Expr, IdedExpr};
use cel_parser::reference::Val;
use rusqlite::types::Value;
use serde::Serialize;

/// An internal physical predicate retained with its canonical CEL program.
/// Unknown/missing/wrong-typed values remain candidates for residual evaluation.
/// No limit, ordering, projection, or diagnostic semantics are applied here.
/// Decisions and SQL come from the shared CEL-dialect atoms (QRY.1).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MdbaseSqlPredicate {
    predicate: Predicate,
}

pub(super) fn prepare(expression: &IdedExpr) -> Option<MdbaseSqlPredicate> {
    let mut atoms = Vec::new();
    collect(expression, &mut atoms)?;
    Some(MdbaseSqlPredicate {
        predicate: Predicate::All(atoms.into_iter().map(Predicate::Atom).collect()),
    })
}

fn collect(expression: &IdedExpr, atoms: &mut Vec<Atom>) -> Option<()> {
    let Expr::Call(call) = &expression.expr else {
        return None;
    };
    if call.func_name == operators::LOGICAL_AND && call.target.is_none() && call.args.len() == 2 {
        collect(&call.args[0], atoms)?;
        return collect(&call.args[1], atoms);
    }
    if atoms.len() >= 16 {
        return None;
    }
    let atom = if call.func_name == "startsWith" && call.args.len() == 1 {
        let field = field(call.target.as_deref()?)?;
        let literal = literal(&call.args[0])?;
        if !matches!(literal, Literal::Text(_)) {
            return None;
        }
        Atom {
            field,
            comparison: Comparison::StartsWith,
            literal,
        }
    } else {
        if call.target.is_some() || call.args.len() != 2 {
            return None;
        }
        let comparison = match call.func_name.as_str() {
            operators::EQUALS => Comparison::Equal,
            operators::NOT_EQUALS => Comparison::NotEqual,
            operators::LESS => Comparison::Less,
            operators::LESS_EQUALS => Comparison::LessEqual,
            operators::GREATER => Comparison::Greater,
            operators::GREATER_EQUALS => Comparison::GreaterEqual,
            _ => return None,
        };
        let (field, literal, comparison) =
            if let (Some(field), Some(literal)) = (field(&call.args[0]), literal(&call.args[1])) {
                (field, literal, comparison)
            } else {
                (
                    field(&call.args[1])?,
                    literal(&call.args[0])?,
                    reverse(comparison),
                )
            };
        if matches!(literal, Literal::Bool(_))
            && !matches!(comparison, Comparison::Equal | Comparison::NotEqual)
        {
            return None;
        }
        Atom {
            field,
            comparison,
            literal,
        }
    };
    atoms.push(atom);
    Some(())
}

fn field(expression: &IdedExpr) -> Option<Field> {
    match &expression.expr {
        Expr::Ident(name) if !super::MDBASE_CEL_RESERVED_BINDINGS.contains(&name.as_str()) => {
            simple_field(name)
        }
        Expr::Select(select) if !select.test => match &select.operand.expr {
            Expr::Ident(name) if name == "file" && select.field == "path" => Some(Field::FilePath),
            Expr::Ident(name) if name == "record" || name == "note" => simple_field(&select.field),
            _ => None,
        },
        _ => None,
    }
}

fn simple_field(name: &str) -> Option<Field> {
    (!name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'))
    .then(|| Field::Property(name.to_string()))
}

fn literal(expression: &IdedExpr) -> Option<Literal> {
    match &expression.expr {
        Expr::Literal(Val::String(value)) if !value.contains('\0') => {
            Some(Literal::Text(value.clone()))
        }
        Expr::Literal(Val::Boolean(value)) => Some(Literal::Bool(*value)),
        Expr::Literal(Val::Int(value)) => Some(Literal::Integer(*value)),
        // Numeric coercion, null/presence, unsigned values and dynamic expressions
        // require separate equivalence evidence; unsupported syntax stays residual.
        _ => None,
    }
}

fn reverse(comparison: Comparison) -> Comparison {
    match comparison {
        Comparison::Less => Comparison::Greater,
        Comparison::LessEqual => Comparison::GreaterEqual,
        Comparison::Greater => Comparison::Less,
        Comparison::GreaterEqual => Comparison::LessEqual,
        other => other,
    }
}

impl MdbaseSqlPredicate {
    /// Append bound values to an existing statement's numbered parameters.
    /// The statement must name its cache row `record`. SQL identifiers/operators
    /// come only from this closed implementation, never from query source text.
    pub(super) fn render(&self, parameters: &mut Vec<Value>) -> String {
        let (valid, matches) = self.render_parts(parameters);
        // An uncertain atom must survive even if another conjunct is false: CEL
        // diagnostics/short-circuit behavior remain the residual evaluator's job.
        format!("CASE WHEN {valid} THEN ({matches}) ELSE 1 END")
    }

    /// Render separate SQL expressions for "every atom has a type this lowering
    /// decides exactly" and "every atom matches". When the first is true, the
    /// second equals the CEL filter result with no diagnostics; otherwise the
    /// row needs residual CEL evaluation.
    pub(super) fn render_parts(&self, parameters: &mut Vec<Value>) -> (String, String) {
        self.predicate
            .render_cel_parts(parameters, |_, field, parameters| match field {
                Field::Property(name) => {
                    let path = bind(parameters, Value::Text(format!("$.{name}")));
                    (
                        format!("json_type(record.effective_frontmatter_json, {path})"),
                        format!("json_extract(record.effective_frontmatter_json, {path})"),
                    )
                }
                _ => ("'text'".to_string(), "record.path".to_string()),
            })
    }

    /// Like [`Self::render_parts`], but each atom's JSON type and value are
    /// computed once as named columns (`atom{i}_kind`, `atom{i}_value`) of an
    /// inner query over `record`, reading `frontmatter` (a JSON or JSONB
    /// expression). The returned expressions reference those columns.
    pub(super) fn render_columns(
        &self,
        parameters: &mut Vec<Value>,
        frontmatter: &str,
    ) -> (Vec<String>, String, String) {
        let mut columns = Vec::new();
        let (valid, matches) =
            self.predicate
                .render_cel_parts(parameters, |index, field, parameters| {
                    let (kind, value) = match field {
                        Field::Property(name) => {
                            let path = bind(parameters, Value::Text(format!("$.{name}")));
                            (
                                format!("json_type({frontmatter}, {path})"),
                                format!("json_extract({frontmatter}, {path})"),
                            )
                        }
                        _ => ("'text'".to_string(), "record.path".to_string()),
                    };
                    columns.push(format!("{kind} AS atom{index}_kind"));
                    columns.push(format!("{value} AS atom{index}_value"));
                    (format!("atom{index}_kind"), format!("atom{index}_value"))
                });
        (columns, valid, matches)
    }

    /// Decide the predicate for one record exactly as the rendered SQL does:
    /// `Some(result)` when every atom's value has a type this lowering decides
    /// exactly (the CEL result, with no diagnostics), `None` when the record
    /// needs residual CEL evaluation. `effective` is the record's effective
    /// frontmatter object.
    #[must_use]
    pub fn decide(&self, path: &str, effective: &serde_json::Value) -> Option<bool> {
        match self.predicate.decide(
            Dialect::Cel,
            &RecordValues {
                properties: effective,
                path,
                name: "",
                ext: "",
            },
        ) {
            Decision::Match => Some(true),
            Decision::NoMatch => Some(false),
            Decision::Undecided => None,
        }
    }
}

fn bind(parameters: &mut Vec<Value>, value: Value) -> String {
    parameters.push(value);
    format!("?{}", parameters.len())
}

#[cfg(test)]
mod tests;
