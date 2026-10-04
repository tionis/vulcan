//! Conservative scalar candidate predicates over rebuildable record metadata.
//!
//! This is not permission, snapshot, or CEL resource-limit evidence. In particular,
//! callers cannot skip CEL input checks just because a row cannot match a filter.

use cel_parser::ast::{operators, Expr, IdedExpr};
use cel_parser::reference::Val;
use rusqlite::types::Value;
use serde::Serialize;

/// An internal physical predicate retained with its canonical CEL program.
/// Unknown/missing/wrong-typed values remain candidates for residual evaluation.
/// No limit, ordering, projection, or diagnostic semantics are applied here.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MdbaseSqlPredicate {
    atoms: Vec<Atom>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct Atom {
    field: Field,
    comparison: Comparison,
    literal: Literal,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
enum Field {
    Effective(String),
    Path,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
enum Comparison {
    Equal,
    NotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    StartsWith,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
enum Literal {
    String(String),
    Boolean(bool),
    Integer(i64),
}

pub(super) fn prepare(expression: &IdedExpr) -> Option<MdbaseSqlPredicate> {
    let mut atoms = Vec::new();
    collect(expression, &mut atoms)?;
    Some(MdbaseSqlPredicate { atoms })
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
        if !matches!(literal, Literal::String(_)) {
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
        if matches!(literal, Literal::Boolean(_))
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
            Expr::Ident(name) if name == "file" && select.field == "path" => Some(Field::Path),
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
    .then(|| Field::Effective(name.to_string()))
}

fn literal(expression: &IdedExpr) -> Option<Literal> {
    match &expression.expr {
        Expr::Literal(Val::String(value)) if !value.contains('\0') => {
            Some(Literal::String(value.clone()))
        }
        Expr::Literal(Val::Boolean(value)) => Some(Literal::Boolean(*value)),
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
        let mut valid = Vec::new();
        let mut matches = Vec::new();
        for atom in &self.atoms {
            let (kind, value) = match &atom.field {
                Field::Path => ("'text'".to_string(), "record.path".to_string()),
                Field::Effective(name) => {
                    let path = bind(parameters, Value::Text(format!("$.{name}")));
                    (
                        format!("json_type(record.effective_frontmatter_json, {path})"),
                        format!("json_extract(record.effective_frontmatter_json, {path})"),
                    )
                }
            };
            let (guard, literal) = match &atom.literal {
                Literal::String(text) => (
                    format!("({kind} = 'text' AND instr({value}, char(0)) = 0)"),
                    Value::Text(text.clone()),
                ),
                Literal::Boolean(boolean) => (
                    format!("{kind} IN ('true', 'false')"),
                    Value::Integer(i64::from(*boolean)),
                ),
                Literal::Integer(integer) => (
                    format!("({kind} = 'integer' AND typeof({value}) = 'integer')"),
                    Value::Integer(*integer),
                ),
            };
            valid.push(format!("({guard})"));
            let parameter = bind(parameters, literal);
            let sql = match atom.comparison {
                Comparison::StartsWith => format!("substr(CAST({value} AS BLOB), 1, length(CAST({parameter} AS BLOB))) = CAST({parameter} AS BLOB)"),
                comparison => {
                    let operator = match comparison {
                        Comparison::Equal => "=", Comparison::NotEqual => "!=",
                        Comparison::Less => "<", Comparison::LessEqual => "<=",
                        Comparison::Greater => ">", Comparison::GreaterEqual => ">=",
                        Comparison::StartsWith => unreachable!(),
                    };
                    format!("{value} COLLATE BINARY {operator} {parameter}")
                }
            };
            matches.push(format!("({sql})"));
        }
        // An uncertain atom must survive even if another conjunct is false: CEL
        // diagnostics/short-circuit behavior remain the residual evaluator's job.
        format!(
            "CASE WHEN {} THEN ({}) ELSE 1 END",
            valid.join(" AND "),
            matches.join(" AND ")
        )
    }
}

fn bind(parameters: &mut Vec<Value>, value: Value) -> String {
    parameters.push(value);
    format!("?{}", parameters.len())
}

#[cfg(test)]
mod tests;
