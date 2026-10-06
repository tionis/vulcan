//! Shared predicate atoms (QRY.1, `docs/specs/query-architecture.md`).
//!
//! Frontends lower the parts of their filter expressions that a store can
//! decide exactly into atoms carrying the frontend's comparison dialect. Each
//! atom decides a record as a match, a non-match, or undecided; undecided
//! records, and every record a frontend chooses to re-check, go to the
//! frontend's own evaluator. A lowering may never turn a record the frontend
//! would match into a non-match.
//!
//! The in-memory decider and the SQL rendering implement one decision table
//! per dialect and are differentially tested against each other and against
//! the frontend evaluator.

use crate::expression::ast::{BinOp, Expr};
use crate::expression::functions::{parse_date_like_string, parse_duration_string};
use rusqlite::types::Value as SqlValue;
use serde_json::Value;
use std::fmt::Write as _;

/// Whose comparison semantics an atom implements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Dialect {
    /// Dataview and Bases expressions, as `expression::eval` evaluates them.
    Dataview,
    /// mdbase CEL. An atom is decided only when the value has exactly the
    /// literal's type, and a conjunction only when every atom is decided, so
    /// CEL keeps reporting every diagnostic and resource-limit error itself.
    Cel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub(crate) enum Comparison {
    Equal,
    NotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    /// Byte-wise string prefix (CEL `startsWith`).
    StartsWith,
    /// Dataview `contains(field, "text")`: a substring of text, recursively
    /// in lists, or a key of an object. Decided in memory only.
    Contains,
}

impl Comparison {
    fn from_binop(operator: BinOp) -> Option<Self> {
        Some(match operator {
            BinOp::Eq => Self::Equal,
            BinOp::Ne => Self::NotEqual,
            BinOp::Lt => Self::Less,
            BinOp::Le => Self::LessEqual,
            BinOp::Gt => Self::Greater,
            BinOp::Ge => Self::GreaterEqual,
            _ => return None,
        })
    }

    /// The comparison with its operands swapped.
    fn reversed(self) -> Self {
        match self {
            Self::Less => Self::Greater,
            Self::LessEqual => Self::GreaterEqual,
            Self::Greater => Self::Less,
            Self::GreaterEqual => Self::LessEqual,
            other => other,
        }
    }

    /// `StartsWith` and `Contains` are not orderings; callers decide them
    /// separately.
    fn holds(self, ordering: std::cmp::Ordering) -> bool {
        use std::cmp::Ordering::{Equal, Greater, Less};
        match self {
            Self::StartsWith | Self::Contains => false,
            Self::Equal => ordering == Equal,
            Self::NotEqual => ordering != Equal,
            Self::Less => ordering == Less,
            Self::LessEqual => ordering != Greater,
            Self::Greater => ordering == Greater,
            Self::GreaterEqual => ordering != Less,
        }
    }

    fn sql(self) -> &'static str {
        match self {
            Self::StartsWith => unreachable!("prefixes render separately"),
            Self::Contains => unreachable!("contains is decided in memory only"),
            Self::Equal => "=",
            Self::NotEqual => "!=",
            Self::Less => "<",
            Self::LessEqual => "<=",
            Self::Greater => ">",
            Self::GreaterEqual => ">=",
        }
    }
}

/// A literal operand. Strings are only lowered when the dialect compares them
/// as plain strings (never date- or duration-like).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub(crate) enum Literal {
    Null,
    Bool(bool),
    Number(f64),
    /// A CEL `int`.
    Integer(i64),
    Text(String),
}

/// The record value an atom compares.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) enum Field {
    /// A top-level property by its exact key. A record without that exact
    /// key is undecided: the evaluator may resolve a differently spelled key.
    Property(String),
    FilePath,
    FileName,
    FileExt,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub(crate) struct Atom {
    pub field: Field,
    pub comparison: Comparison,
    pub literal: Literal,
}

/// A lowered filter. Children of `All` are in evaluation order.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub(crate) enum Predicate {
    Atom(Atom),
    /// Short-circuit conjunction.
    All(Vec<Predicate>),
    /// Short-circuit disjunction.
    Any(Vec<Predicate>),
    /// Not lowered; only the frontend evaluator can decide it, and it may
    /// produce diagnostics when evaluated.
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Decision {
    Match,
    NoMatch,
    Undecided,
}

/// Values the decider reads for one record.
pub(crate) struct RecordValues<'a> {
    pub properties: &'a Value,
    pub path: &'a str,
    pub name: &'a str,
    pub ext: &'a str,
}

/// Columns the SQL rendering reads.
pub(crate) struct SqlColumns<'a> {
    /// A JSON (text or JSONB) column holding the record's properties object.
    pub properties: &'a str,
    pub path: &'a str,
    pub name: &'a str,
    pub ext: &'a str,
}

impl Predicate {
    /// Lower a Dataview/Bases filter expression whose truthiness selects rows.
    pub(crate) fn lower_dataview(expr: &Expr) -> Self {
        match expr {
            Expr::BinaryOp(left, BinOp::And, right) => {
                let mut children = Vec::new();
                for side in [left, right] {
                    match Self::lower_dataview(side) {
                        Self::All(nested) => children.extend(nested),
                        other => children.push(other),
                    }
                }
                Self::All(children)
            }
            Expr::BinaryOp(left, BinOp::Or, right) => {
                let mut children = Vec::new();
                for side in [left, right] {
                    match Self::lower_dataview(side) {
                        Self::Any(nested) => children.extend(nested),
                        other => children.push(other),
                    }
                }
                Self::Any(children)
            }
            // `startswith(field, "prefix")`: byte-exact on strings, applied
            // element-wise to lists, and null (falsy) for other kinds.
            Expr::FunctionCall(name, args) if name == "startswith" && args.len() == 2 => {
                match (dataview_field(&args[0]), &args[1]) {
                    (Some(field), Expr::Str(prefix)) => Self::Atom(Atom {
                        field,
                        comparison: Comparison::StartsWith,
                        literal: Literal::Text(prefix.clone()),
                    }),
                    _ => Self::Unknown,
                }
            }
            Expr::FunctionCall(name, args) if name == "contains" && args.len() == 2 => {
                match (dataview_field(&args[0]), &args[1]) {
                    (Some(field @ Field::Property(_)), Expr::Str(needle)) => Self::Atom(Atom {
                        field,
                        comparison: Comparison::Contains,
                        literal: Literal::Text(needle.clone()),
                    }),
                    _ => Self::Unknown,
                }
            }
            Expr::BinaryOp(left, operator, right) => {
                let Some(comparison) = Comparison::from_binop(*operator) else {
                    return Self::Unknown;
                };
                if let (Some(field), Some(literal)) =
                    (dataview_field(left), dataview_literal(right))
                {
                    return Self::Atom(Atom {
                        field,
                        comparison,
                        literal,
                    });
                }
                if let (Some(literal), Some(field)) =
                    (dataview_literal(left), dataview_field(right))
                {
                    return Self::Atom(Atom {
                        field,
                        comparison: comparison.reversed(),
                        literal,
                    });
                }
                Self::Unknown
            }
            _ => Self::Unknown,
        }
    }

    /// Whether anything can be decided at all.
    pub(crate) fn is_useful(&self) -> bool {
        match self {
            Self::Atom(_) => true,
            Self::All(children) => children
                .iter()
                .take_while(|child| !matches!(child, Self::Unknown))
                .any(Self::is_useful),
            Self::Any(children) => children.iter().all(Self::is_useful),
            Self::Unknown => false,
        }
    }

    /// Decide one record in memory.
    pub(crate) fn decide(&self, dialect: Dialect, record: &RecordValues<'_>) -> Decision {
        match self {
            Self::Atom(atom) => atom.decide(dialect, record),
            Self::All(children) if dialect == Dialect::Cel => {
                let mut matched = true;
                for child in children {
                    match child.decide(dialect, record) {
                        Decision::Undecided => return Decision::Undecided,
                        decision => matched &= decision == Decision::Match,
                    }
                }
                if matched {
                    Decision::Match
                } else {
                    Decision::NoMatch
                }
            }
            Self::All(children) => {
                let mut decision = Decision::Match;
                for child in children {
                    // A later child is evaluated only after an earlier one; an
                    // unlowered child may report diagnostics, so nothing after
                    // it may exclude the record.
                    if matches!(child, Self::Unknown) {
                        return if decision == Decision::Match {
                            Decision::Undecided
                        } else {
                            decision
                        };
                    }
                    match child.decide(dialect, record) {
                        Decision::NoMatch => return Decision::NoMatch,
                        Decision::Undecided => decision = Decision::Undecided,
                        Decision::Match => {}
                    }
                }
                decision
            }
            Self::Any(children) => {
                let mut decision = Decision::NoMatch;
                for child in children {
                    match child.decide(dialect, record) {
                        Decision::Match => return Decision::Match,
                        Decision::Undecided => decision = Decision::Undecided,
                        Decision::NoMatch => {}
                    }
                }
                decision
            }
            Self::Unknown => Decision::Undecided,
        }
    }

    /// Render a SQL boolean that is false exactly for records this predicate
    /// decides as non-matches. Literals are bound parameters.
    pub(crate) fn render_possible_match(
        &self,
        dialect: Dialect,
        columns: &SqlColumns<'_>,
        params: &mut Vec<SqlValue>,
    ) -> String {
        match self {
            Self::Atom(atom) => atom.render_possible_match(dialect, columns, params),
            Self::All(children) => {
                let clauses = children
                    .iter()
                    .take_while(|child| !matches!(child, Self::Unknown))
                    .map(|child| child.render_possible_match(dialect, columns, params))
                    .collect::<Vec<_>>();
                if clauses.is_empty() {
                    "1".to_string()
                } else {
                    format!("({})", clauses.join(" AND "))
                }
            }
            Self::Any(children) => {
                if children.iter().any(|child| matches!(child, Self::Unknown)) {
                    return "1".to_string();
                }
                let clauses = children
                    .iter()
                    .map(|child| child.render_possible_match(dialect, columns, params))
                    .collect::<Vec<_>>();
                format!("({})", clauses.join(" OR "))
            }
            Self::Unknown => "1".to_string(),
        }
    }
}

fn dataview_field(expr: &Expr) -> Option<Field> {
    match expr {
        Expr::Identifier(name) => {
            let normalized = crate::expression::eval::normalize_field_name(name);
            // Special identifiers do not resolve to properties.
            (!matches!(normalized.as_str(), "this" | "file" | "note"))
                .then(|| Field::Property(name.clone()))
        }
        // `note["key"]` resolves like the bare identifier `key`.
        Expr::IndexAccess(receiver, index) => match (receiver.as_ref(), index.as_ref()) {
            (Expr::Identifier(name), Expr::Str(key))
                if crate::expression::eval::normalize_field_name(name) == "note" =>
            {
                Some(Field::Property(key.clone()))
            }
            _ => None,
        },
        Expr::FieldAccess(receiver, field) => {
            let Expr::Identifier(name) = receiver.as_ref() else {
                return None;
            };
            if crate::expression::eval::normalize_field_name(name) != "file" {
                return None;
            }
            match crate::expression::eval::canonical_file_field_name(field).as_str() {
                "path" => Some(Field::FilePath),
                "name" | "basename" => Some(Field::FileName),
                "ext" => Some(Field::FileExt),
                _ => None,
            }
        }
        _ => None,
    }
}

fn dataview_literal(expr: &Expr) -> Option<Literal> {
    match expr {
        Expr::Null => Some(Literal::Null),
        Expr::Bool(value) => Some(Literal::Bool(*value)),
        Expr::Number(value) if value.is_finite() => Some(Literal::Number(*value)),
        Expr::Str(text) if plain_dataview_string(text) => Some(Literal::Text(text.clone())),
        _ => None,
    }
}

/// Strings the Dataview evaluator compares as plain strings in every
/// operand position (never as dates or durations).
fn plain_dataview_string(text: &str) -> bool {
    parse_date_like_string(text).is_none() && parse_duration_string(text).is_none()
}

/// The value the Dataview evaluator reads for a property: the exact key,
/// else a key with the same normalized name, else null. SQL reads only the
/// exact key, so it treats a missing exact key as possible.
fn dataview_property<'a>(properties: &'a Value, key: &str) -> Option<&'a Value> {
    static NULL: Value = Value::Null;
    let map = properties.as_object()?;
    Some(crate::expression::eval::normalized_object_field(map, key).unwrap_or(&NULL))
}

/// Integers beyond this are compared through `f64` by the evaluator and
/// exactly by SQL; such values are left undecided.
const EXACT_INTEGER_BOUND: f64 = 9_007_199_254_740_992.0;

impl Atom {
    fn decide(&self, dialect: Dialect, record: &RecordValues<'_>) -> Decision {
        if dialect == Dialect::Cel {
            return self.decide_cel(record);
        }
        if matches!(self.literal, Literal::Integer(_)) {
            return Decision::Undecided;
        }
        if self.comparison == Comparison::StartsWith {
            return self.decide_dataview_prefix(record);
        }
        if self.comparison == Comparison::Contains {
            return self.decide_dataview_contains(record);
        }
        if matches!(self.literal, Literal::Number(literal) if literal.abs() >= EXACT_INTEGER_BOUND)
        {
            return Decision::Undecided;
        }
        let value = match &self.field {
            Field::Property(key) => match dataview_property(record.properties, key) {
                Some(value) => value,
                None => return Decision::Undecided,
            },
            Field::FilePath => return self.decide_text(record.path),
            Field::FileName => return self.decide_text(record.name),
            Field::FileExt => return self.decide_text(record.ext),
        };
        let decided = match (&self.literal, value) {
            // Null orders before every value and equals only null.
            (Literal::Null, Value::Null) => self.comparison.holds(std::cmp::Ordering::Equal),
            (Literal::Null, _) => self.comparison.holds(std::cmp::Ordering::Greater),
            (_, Value::Null) => self.comparison.holds(std::cmp::Ordering::Less),
            (Literal::Text(literal), Value::String(text)) => {
                self.comparison.holds(text.as_str().cmp(literal.as_str()))
            }
            // Date- and duration-like strings compare with integers; other
            // strings are a different kind.
            (Literal::Number(_), Value::String(text)) if !plain_dataview_string(text) => {
                return Decision::Undecided
            }
            (Literal::Number(literal), Value::Number(number)) => {
                let Some(number) = number.as_f64() else {
                    return Decision::Undecided;
                };
                if number.abs() >= EXACT_INTEGER_BOUND || literal.abs() >= EXACT_INTEGER_BOUND {
                    return Decision::Undecided;
                }
                match number.partial_cmp(literal) {
                    Some(ordering) => self.comparison.holds(ordering),
                    None => return Decision::Undecided,
                }
            }
            (Literal::Bool(literal), Value::Bool(value)) => {
                self.comparison.holds(value.cmp(literal))
            }
            // Values of different kinds are unequal and unordered.
            _ => self.comparison == Comparison::NotEqual,
        };
        if decided {
            Decision::Match
        } else {
            Decision::NoMatch
        }
    }

    fn decide_text(&self, text: &str) -> Decision {
        let decided = match &self.literal {
            Literal::Text(literal) => self.comparison.holds(text.cmp(literal.as_str())),
            Literal::Null => self.comparison.holds(std::cmp::Ordering::Greater),
            Literal::Bool(_) => self.comparison == Comparison::NotEqual,
            // A file name or path may itself be date-like.
            Literal::Number(_) | Literal::Integer(_) => return Decision::Undecided,
        };
        if decided {
            Decision::Match
        } else {
            Decision::NoMatch
        }
    }

    /// The evaluator's own `contains` on the value it would read.
    fn decide_dataview_contains(&self, record: &RecordValues<'_>) -> Decision {
        let (Field::Property(key), Literal::Text(needle)) = (&self.field, &self.literal) else {
            return Decision::Undecided;
        };
        let Some(value) = dataview_property(record.properties, key) else {
            return Decision::Undecided;
        };
        if crate::expression::functions::contains_value(
            value,
            &Value::String(needle.clone()),
            crate::expression::functions::ContainsMode::Recursive,
        ) {
            Decision::Match
        } else {
            Decision::NoMatch
        }
    }

    fn decide_dataview_prefix(&self, record: &RecordValues<'_>) -> Decision {
        let Literal::Text(prefix) = &self.literal else {
            return Decision::Undecided;
        };
        let text = match &self.field {
            Field::FilePath => record.path,
            Field::FileName => record.name,
            Field::FileExt => record.ext,
            Field::Property(key) => match dataview_property(record.properties, key) {
                None | Some(Value::Array(_)) => return Decision::Undecided,
                Some(Value::String(text)) => text,
                Some(_) => return Decision::NoMatch,
            },
        };
        if text.as_bytes().starts_with(prefix.as_bytes()) {
            Decision::Match
        } else {
            Decision::NoMatch
        }
    }

    /// CEL: decided only for a value of exactly the literal's type; strings
    /// containing NUL stay with CEL.
    fn decide_cel(&self, record: &RecordValues<'_>) -> Decision {
        enum Scalar<'a> {
            Text(&'a str),
            Boolean(bool),
            Integer(i64),
        }
        // Only the Dataview frontend lowers `contains`.
        if self.comparison == Comparison::Contains {
            return Decision::Undecided;
        }
        let value = match &self.field {
            Field::FilePath => Scalar::Text(record.path),
            Field::Property(key) => match record.properties.get(key) {
                Some(Value::String(text)) => Scalar::Text(text),
                Some(Value::Bool(boolean)) => Scalar::Boolean(*boolean),
                Some(number @ Value::Number(_)) => match number.as_i64() {
                    Some(integer) => Scalar::Integer(integer),
                    None => return Decision::Undecided,
                },
                _ => return Decision::Undecided,
            },
            Field::FileName | Field::FileExt => return Decision::Undecided,
        };
        let matched = match (&self.literal, value) {
            (Literal::Text(literal), Scalar::Text(text)) if !text.contains('\0') => {
                if self.comparison == Comparison::StartsWith {
                    text.as_bytes().starts_with(literal.as_bytes())
                } else {
                    self.comparison
                        .holds(text.as_bytes().cmp(literal.as_bytes()))
                }
            }
            (Literal::Bool(literal), Scalar::Boolean(boolean))
                if self.comparison != Comparison::StartsWith =>
            {
                self.comparison.holds(boolean.cmp(literal))
            }
            (Literal::Integer(literal), Scalar::Integer(integer))
                if self.comparison != Comparison::StartsWith =>
            {
                self.comparison.holds(integer.cmp(literal))
            }
            _ => return Decision::Undecided,
        };
        if matched {
            Decision::Match
        } else {
            Decision::NoMatch
        }
    }

    fn render_possible_match(
        &self,
        dialect: Dialect,
        columns: &SqlColumns<'_>,
        params: &mut Vec<SqlValue>,
    ) -> String {
        assert_eq!(dialect, Dialect::Dataview, "CEL renders decision parts");
        if matches!(self.literal, Literal::Integer(_)) {
            return "1".to_string();
        }
        if self.comparison == Comparison::StartsWith {
            return self.render_dataview_prefix(columns, params);
        }
        // Recursive substring and key semantics stay with the decider.
        if self.comparison == Comparison::Contains {
            return "1".to_string();
        }
        let column = match &self.field {
            Field::Property(key) => return self.render_property(columns.properties, key, params),
            Field::FilePath => columns.path,
            Field::FileName => columns.name,
            Field::FileExt => columns.ext,
        };
        match &self.literal {
            Literal::Text(literal) => {
                params.push(SqlValue::Text(literal.clone()));
                format!("({column} {} ?)", self.comparison.sql())
            }
            Literal::Null => bool_sql(self.comparison.holds(std::cmp::Ordering::Greater)),
            Literal::Bool(_) => bool_sql(self.comparison == Comparison::NotEqual),
            Literal::Number(_) | Literal::Integer(_) => "1".to_string(),
        }
    }

    /// Plain `?` placeholders in textual order, like [`Self::render_property`].
    fn render_dataview_prefix(
        &self,
        columns: &SqlColumns<'_>,
        params: &mut Vec<SqlValue>,
    ) -> String {
        let Literal::Text(prefix) = &self.literal else {
            return "1".to_string();
        };
        let prefix_of = |text: &str, params: &mut Vec<SqlValue>| {
            // `substr` of an empty blob is NULL; every string has this prefix.
            if prefix.is_empty() {
                return "1".to_string();
            }
            params.push(SqlValue::Text(prefix.clone()));
            params.push(SqlValue::Text(prefix.clone()));
            // `substr` of an empty blob is NULL, so compare NULL-safely.
            format!("(substr(CAST({text} AS BLOB), 1, length(CAST(? AS BLOB))) IS CAST(? AS BLOB))")
        };
        let column = match &self.field {
            Field::FilePath => columns.path,
            Field::FileName => columns.name,
            Field::FileExt => columns.ext,
            Field::Property(key) => {
                let json = columns.properties;
                let path = json_path(key);
                for _ in 0..3 {
                    params.push(SqlValue::Text(path.clone()));
                }
                let prefix_sql = if prefix.is_empty() {
                    "1".to_string()
                } else {
                    params.push(SqlValue::Text(path));
                    prefix_of(&format!("json_extract({json}, ?)"), params)
                };
                // Missing key or list: undecided; text: the prefix; else null.
                return format!(
                    "(json_type({json}, ?) IS NULL OR json_type({json}, ?) = 'array' \
                     OR (json_type({json}, ?) = 'text' AND {prefix_sql}))"
                );
            }
        };
        prefix_of(column, params)
    }

    /// Renders with plain `?` placeholders, pushing each parameter in the
    /// order it appears so callers can splice the clause anywhere.
    fn render_property(&self, json: &str, key: &str, params: &mut Vec<SqlValue>) -> String {
        if matches!(self.literal, Literal::Number(literal) if literal.abs() >= EXACT_INTEGER_BOUND)
        {
            return "1".to_string();
        }
        let path = json_path(key);
        let kind = |params: &mut Vec<SqlValue>| {
            params.push(SqlValue::Text(path.clone()));
            format!("json_type({json}, ?)")
        };
        // Missing key: undecided. Then null, the literal's own kind, and
        // every other kind.
        let mut sql = format!(
            "({} IS NULL OR CASE WHEN {} = 'null' THEN ",
            kind(params),
            kind(params)
        );
        if self.literal == Literal::Null {
            sql.push_str(&bool_sql(self.comparison.holds(std::cmp::Ordering::Equal)));
            sql.push_str(" ELSE ");
            sql.push_str(&bool_sql(
                self.comparison.holds(std::cmp::Ordering::Greater),
            ));
            sql.push_str(" END)");
            return sql;
        }
        sql.push_str(&bool_sql(self.comparison.holds(std::cmp::Ordering::Less)));
        let operator = self.comparison.sql();
        match &self.literal {
            Literal::Text(literal) => {
                let _ = write!(sql, " WHEN {} = 'text' THEN ", kind(params));
                params.push(SqlValue::Text(path.clone()));
                params.push(SqlValue::Text(literal.clone()));
                let _ = write!(sql, "json_extract({json}, ?) {operator} ?");
            }
            Literal::Number(literal) => {
                // Strings may be dates or durations compared with integers.
                let _ = write!(sql, " WHEN {} = 'text' THEN 1", kind(params));
                let _ = write!(sql, " WHEN {} IN ('integer', 'real') THEN ", kind(params));
                params.push(SqlValue::Text(path.clone()));
                params.push(SqlValue::Text(path.clone()));
                params.push(SqlValue::Real(*literal));
                let _ = write!(sql,
                    "(abs(json_extract({json}, ?)) >= {EXACT_INTEGER_BOUND} OR json_extract({json}, ?) {operator} ?)"
                );
            }
            Literal::Bool(literal) => {
                let _ = write!(sql, " WHEN {} IN ('true', 'false') THEN ", kind(params));
                let _ = write!(
                    sql,
                    "({} = 'true') {operator} {}",
                    kind(params),
                    i32::from(*literal)
                );
            }
            Literal::Null | Literal::Integer(_) => unreachable!("handled above"),
        }
        sql.push_str(" ELSE ");
        sql.push_str(&bool_sql(self.comparison == Comparison::NotEqual));
        sql.push_str(" END)");
        sql
    }
}

impl Predicate {
    /// CEL conjunction SQL as two expressions: `valid`, true when every atom's
    /// value has the type its literal needs, and `matches`, which then equals
    /// the CEL result. `operand` supplies each atom's JSON type and value
    /// expressions. Literals bind as numbered `?N` parameters after any
    /// already in `params`.
    pub(crate) fn render_cel_parts(
        &self,
        params: &mut Vec<SqlValue>,
        mut operand: impl FnMut(usize, &Field, &mut Vec<SqlValue>) -> (String, String),
    ) -> (String, String) {
        let atoms = match self {
            Self::All(children) => children
                .iter()
                .map(|child| match child {
                    Self::Atom(atom) => atom,
                    _ => unreachable!("CEL lowers conjunctions of atoms"),
                })
                .collect::<Vec<_>>(),
            Self::Atom(atom) => vec![atom],
            _ => unreachable!("CEL lowers conjunctions of atoms"),
        };
        let mut valid = Vec::new();
        let mut matches = Vec::new();
        for (index, atom) in atoms.into_iter().enumerate() {
            let (kind, value) = operand(index, &atom.field, params);
            let (guard, literal) = match &atom.literal {
                Literal::Text(text) => (
                    format!("({kind} = 'text' AND instr({value}, char(0)) = 0)"),
                    SqlValue::Text(text.clone()),
                ),
                Literal::Bool(boolean) => (
                    format!("{kind} IN ('true', 'false')"),
                    SqlValue::Integer(i64::from(*boolean)),
                ),
                Literal::Integer(integer) => (
                    format!("({kind} = 'integer' AND typeof({value}) = 'integer')"),
                    SqlValue::Integer(*integer),
                ),
                Literal::Null | Literal::Number(_) => {
                    unreachable!("CEL lowers text, boolean, and integer literals")
                }
            };
            valid.push(format!("({guard})"));
            if atom.comparison == Comparison::StartsWith
                && matches!(&atom.literal, Literal::Text(prefix) if prefix.is_empty())
            {
                // Every string has the empty prefix (and `substr` of an empty
                // blob is NULL).
                matches.push("(1)".to_string());
                continue;
            }
            params.push(literal);
            let parameter = format!("?{}", params.len());
            let sql = if atom.comparison == Comparison::StartsWith {
                format!(
                    "substr(CAST({value} AS BLOB), 1, length(CAST({parameter} AS BLOB))) IS CAST({parameter} AS BLOB)"
                )
            } else {
                format!(
                    "{value} COLLATE BINARY {} {parameter}",
                    atom.comparison.sql()
                )
            };
            matches.push(format!("({sql})"));
        }
        (
            format!("({})", valid.join(" AND ")),
            format!("({})", matches.join(" AND ")),
        )
    }
}

fn bool_sql(value: bool) -> String {
    if value { "1" } else { "0" }.to_string()
}

/// A JSON path selecting exactly one top-level key, whatever its spelling.
fn json_path(key: &str) -> String {
    format!("$.\"{}\"", key.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests;
