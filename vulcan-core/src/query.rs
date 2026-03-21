/// Canonical query AST shared by `notes`, `search`, saved reports, serve handlers, and the
/// human DSL / JSON query payloads.
///
/// Design constraints:
/// - Do not expose raw SQLite schema or SQL as the long-term public contract.
/// - Machine-readable JSON must round-trip cleanly with the AST.
/// - The AST must be a superset of the existing `NoteQuery` filter string format.
use std::fmt::{self, Display, Formatter};

use serde::{Deserialize, Serialize};

use crate::properties::{NoteQuery, NotesReport, PropertyError};
use crate::{CacheError, VaultPaths};

// ── Error ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryError {
    InvalidDsl(String),
    InvalidJson(String),
    Property(String),
    CacheMissing,
}

impl Display for QueryError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDsl(msg) => write!(f, "query DSL error: {msg}"),
            Self::InvalidJson(msg) => write!(f, "query JSON error: {msg}"),
            Self::Property(msg) => write!(f, "query error: {msg}"),
            Self::CacheMissing => {
                f.write_str("cache is missing; run `vulcan scan` before querying notes")
            }
        }
    }
}

impl std::error::Error for QueryError {}

impl From<PropertyError> for QueryError {
    fn from(e: PropertyError) -> Self {
        match e {
            PropertyError::CacheMissing => Self::CacheMissing,
            other => Self::Property(other.to_string()),
        }
    }
}

impl From<CacheError> for QueryError {
    fn from(e: CacheError) -> Self {
        Self::Property(e.to_string())
    }
}

// ── AST types ─────────────────────────────────────────────────────────────────

/// The data source for a query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuerySource {
    Notes,
}

impl Display for QuerySource {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Notes => f.write_str("notes"),
        }
    }
}

/// A filter operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryOperator {
    Eq,
    Gt,
    Gte,
    Lt,
    Lte,
    StartsWith,
    Contains,
}

impl Display for QueryOperator {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Eq => "=",
            Self::Gt => ">",
            Self::Gte => ">=",
            Self::Lt => "<",
            Self::Lte => "<=",
            Self::StartsWith => "starts_with",
            Self::Contains => "contains",
        })
    }
}

/// A typed filter value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum QueryValue {
    Null,
    Bool(bool),
    Number(f64),
    Text(String),
}

impl Display for QueryValue {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Null => f.write_str("null"),
            Self::Bool(b) => write!(f, "{b}"),
            Self::Number(n) => write!(f, "{n}"),
            Self::Text(s) => write!(f, "{s:?}"),
        }
    }
}

/// A single typed predicate (WHERE condition).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryPredicate {
    pub field: String,
    pub operator: QueryOperator,
    pub value: QueryValue,
}

impl QueryPredicate {
    /// Render back to the legacy filter string format understood by `build_note_filter_clause`.
    pub fn to_filter_string(&self) -> String {
        let value_str = match &self.value {
            QueryValue::Null => "null".to_string(),
            QueryValue::Bool(b) => b.to_string(),
            QueryValue::Number(n) => n.to_string(),
            QueryValue::Text(s) => {
                if s.contains('"') {
                    format!("'{s}'")
                } else {
                    format!("\"{s}\"")
                }
            }
        };
        format!("{} {} {}", self.field, self.operator, value_str)
    }
}

/// Sort specification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuerySort {
    pub field: String,
    #[serde(default)]
    pub descending: bool,
}

/// Field projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum QueryProjection {
    All,
    Fields(Vec<String>),
}

impl Default for QueryProjection {
    fn default() -> Self {
        Self::All
    }
}

/// The canonical query AST.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryAst {
    pub source: QuerySource,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub predicates: Vec<QueryPredicate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort: Option<QuerySort>,
    #[serde(default)]
    pub projection: QueryProjection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub offset: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

impl QueryAst {
    /// Build from the existing `NoteQuery` filter string format.
    pub fn from_note_query(query: &NoteQuery) -> Result<Self, QueryError> {
        let predicates = query
            .filters
            .iter()
            .map(|f| parse_predicate_from_filter_string(f))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self {
            source: QuerySource::Notes,
            predicates,
            sort: query.sort_by.as_deref().map(|field| QuerySort {
                field: field.to_string(),
                descending: query.sort_descending,
            }),
            projection: QueryProjection::All,
            limit: None,
            offset: 0,
        })
    }

    /// Convert this AST back to a `NoteQuery` compatible form.
    ///
    /// The filter strings produced are compatible with `build_note_filter_clause`.
    pub fn to_note_query(&self) -> NoteQuery {
        NoteQuery {
            filters: self
                .predicates
                .iter()
                .map(QueryPredicate::to_filter_string)
                .collect(),
            sort_by: self.sort.as_ref().map(|s| s.field.clone()),
            sort_descending: self.sort.as_ref().is_some_and(|s| s.descending),
        }
    }

    /// Parse from the human DSL:
    ///
    /// ```text
    /// from notes
    ///   [where <field> <op> <value> [and <field> <op> <value>...]]
    ///   [select <field>[,<field>...]]
    ///   [order by <field> [desc|asc]]
    ///   [limit <n>]
    ///   [offset <n>]
    /// ```
    pub fn from_dsl(input: &str) -> Result<Self, QueryError> {
        DslParser::new(input).parse()
    }

    /// Deserialize from a JSON payload.
    ///
    /// Expected shape:
    /// ```json
    /// {
    ///   "source": "notes",
    ///   "predicates": [{"field":"status","operator":"eq","value":"done"}],
    ///   "sort": {"field":"file.mtime","descending":true},
    ///   "limit": 10
    /// }
    /// ```
    pub fn from_json(input: &str) -> Result<Self, QueryError> {
        serde_json::from_str(input).map_err(|e| QueryError::InvalidJson(e.to_string()))
    }
}

// ── Execution ─────────────────────────────────────────────────────────────────

/// Execute a `QueryAst` against the vault and return a `NotesReport`.
pub fn execute_query(paths: &VaultPaths, ast: &QueryAst) -> Result<NotesReport, QueryError> {
    let note_query = ast.to_note_query();

    if !paths.cache_db().exists() {
        return Err(QueryError::CacheMissing);
    }

    crate::properties::query_notes(paths, &note_query).map_err(QueryError::from)
}

/// Execute a `QueryAst` from a DSL string.
pub fn execute_query_dsl(paths: &VaultPaths, dsl: &str) -> Result<NotesReport, QueryError> {
    let ast = QueryAst::from_dsl(dsl)?;
    execute_query(paths, &ast)
}

/// Execute a `QueryAst` from a JSON payload string.
pub fn execute_query_json(paths: &VaultPaths, json: &str) -> Result<NotesReport, QueryError> {
    let ast = QueryAst::from_json(json)?;
    execute_query(paths, &ast)
}

// ── Predicate parser ──────────────────────────────────────────────────────────

/// Parse a predicate from the existing filter string format, e.g. `"status = done"`.
fn parse_predicate_from_filter_string(filter: &str) -> Result<QueryPredicate, QueryError> {
    for (separator, operator) in [
        (" contains ", QueryOperator::Contains),
        (" starts_with ", QueryOperator::StartsWith),
        (" >= ", QueryOperator::Gte),
        (" <= ", QueryOperator::Lte),
        (" = ", QueryOperator::Eq),
        (" > ", QueryOperator::Gt),
        (" < ", QueryOperator::Lt),
    ] {
        if let Some((field, value)) = filter.split_once(separator) {
            return Ok(QueryPredicate {
                field: field.trim().to_string(),
                operator,
                value: parse_query_value(value.trim()),
            });
        }
    }

    Err(QueryError::InvalidDsl(format!(
        "cannot parse predicate from filter string: {filter:?}"
    )))
}

fn parse_query_value(value: &str) -> QueryValue {
    // strip quotes
    if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        return QueryValue::Text(value[1..value.len() - 1].to_string());
    }

    match value.trim().to_ascii_lowercase().as_str() {
        "null" => QueryValue::Null,
        "true" => QueryValue::Bool(true),
        "false" => QueryValue::Bool(false),
        _ => {
            if let Ok(n) = value.trim().parse::<f64>() {
                return QueryValue::Number(n);
            }
            QueryValue::Text(value.trim().to_string())
        }
    }
}

// ── DSL parser ────────────────────────────────────────────────────────────────

struct DslParser<'a> {
    tokens: Vec<&'a str>,
    pos: usize,
}

impl<'a> DslParser<'a> {
    fn new(input: &'a str) -> Self {
        // Tokenize: split on whitespace but keep quoted strings together
        let mut tokens = Vec::new();
        let mut chars = input.char_indices().peekable();
        while let Some((start, ch)) = chars.next() {
            if ch.is_whitespace() {
                continue;
            }
            if ch == '"' || ch == '\'' {
                let quote = ch;
                let mut end = start + 1;
                for (i, c) in chars.by_ref() {
                    end = i + c.len_utf8();
                    if c == quote {
                        break;
                    }
                }
                tokens.push(&input[start..end]);
            } else if ch == ',' {
                tokens.push(",");
            } else {
                let mut end = start + ch.len_utf8();
                while let Some(&(i, c)) = chars.peek() {
                    if c.is_whitespace() || c == ',' {
                        break;
                    }
                    end = i + c.len_utf8();
                    chars.next();
                }
                tokens.push(&input[start..end]);
            }
        }
        Self { tokens, pos: 0 }
    }

    fn peek(&self) -> Option<&str> {
        self.tokens.get(self.pos).copied()
    }

    fn peek_lower(&self) -> Option<String> {
        self.peek().map(|s| s.to_ascii_lowercase())
    }

    fn consume(&mut self) -> Option<&str> {
        let tok = self.tokens.get(self.pos).copied();
        self.pos += 1;
        tok
    }

    fn expect(&mut self, expected: &str) -> Result<(), QueryError> {
        match self.consume() {
            Some(tok) if tok.eq_ignore_ascii_case(expected) => Ok(()),
            Some(tok) => Err(QueryError::InvalidDsl(format!(
                "expected {expected:?}, got {tok:?}"
            ))),
            None => Err(QueryError::InvalidDsl(format!(
                "expected {expected:?} but reached end of input"
            ))),
        }
    }

    fn parse(mut self) -> Result<QueryAst, QueryError> {
        self.expect("from")?;
        let source_token = self
            .consume()
            .ok_or_else(|| QueryError::InvalidDsl("expected source after 'from'".to_string()))?;
        let source = match source_token.to_ascii_lowercase().as_str() {
            "notes" => QuerySource::Notes,
            other => {
                return Err(QueryError::InvalidDsl(format!(
                    "unknown source {other:?}; expected 'notes'"
                )));
            }
        };

        let mut predicates = Vec::new();
        let mut sort: Option<QuerySort> = None;
        let mut projection = QueryProjection::All;
        let mut limit: Option<usize> = None;
        let mut offset: usize = 0;

        while let Some(keyword) = self.peek_lower() {
            match keyword.as_str() {
                "where" => {
                    self.consume();
                    predicates = self.parse_predicate_list()?;
                }
                "select" => {
                    self.consume();
                    projection = self.parse_select()?;
                }
                "order" => {
                    self.consume();
                    self.expect("by")?;
                    sort = Some(self.parse_sort()?);
                }
                "limit" => {
                    self.consume();
                    limit = Some(self.parse_usize("limit")?);
                }
                "offset" => {
                    self.consume();
                    offset = self.parse_usize("offset")?;
                }
                other => {
                    return Err(QueryError::InvalidDsl(format!(
                        "unexpected token {other:?}; expected 'where', 'select', 'order', 'limit', or 'offset'"
                    )));
                }
            }
        }

        Ok(QueryAst {
            source,
            predicates,
            sort,
            projection,
            limit,
            offset,
        })
    }

    fn parse_predicate_list(&mut self) -> Result<Vec<QueryPredicate>, QueryError> {
        let mut predicates = Vec::new();
        predicates.push(self.parse_predicate()?);
        loop {
            match self.peek_lower().as_deref() {
                Some("and") => {
                    self.consume();
                    predicates.push(self.parse_predicate()?);
                }
                _ => break,
            }
        }
        Ok(predicates)
    }

    fn parse_predicate(&mut self) -> Result<QueryPredicate, QueryError> {
        let field = self
            .consume()
            .ok_or_else(|| QueryError::InvalidDsl("expected field name".to_string()))?
            .to_string();

        let op_str = self
            .consume()
            .ok_or_else(|| QueryError::InvalidDsl("expected operator".to_string()))?;
        let operator = match op_str.to_ascii_lowercase().as_str() {
            "=" => QueryOperator::Eq,
            ">" => QueryOperator::Gt,
            ">=" => QueryOperator::Gte,
            "<" => QueryOperator::Lt,
            "<=" => QueryOperator::Lte,
            "starts_with" => QueryOperator::StartsWith,
            "contains" => QueryOperator::Contains,
            other => {
                return Err(QueryError::InvalidDsl(format!(
                    "unknown operator {other:?}; expected =, >, >=, <, <=, starts_with, or contains"
                )));
            }
        };

        let value_tok = self
            .consume()
            .ok_or_else(|| QueryError::InvalidDsl("expected value after operator".to_string()))?;
        let value = parse_query_value(value_tok);

        Ok(QueryPredicate {
            field,
            operator,
            value,
        })
    }

    fn parse_select(&mut self) -> Result<QueryProjection, QueryError> {
        let mut fields = Vec::new();
        loop {
            match self.peek() {
                Some(tok)
                    if !tok.eq_ignore_ascii_case("where")
                        && !tok.eq_ignore_ascii_case("order")
                        && !tok.eq_ignore_ascii_case("limit")
                        && !tok.eq_ignore_ascii_case("offset")
                        && tok != "," =>
                {
                    fields.push(self.consume().unwrap().to_string());
                }
                Some(",") => {
                    self.consume();
                }
                _ => break,
            }
        }
        if fields.is_empty() {
            return Err(QueryError::InvalidDsl(
                "expected at least one field after 'select'".to_string(),
            ));
        }
        Ok(QueryProjection::Fields(fields))
    }

    fn parse_sort(&mut self) -> Result<QuerySort, QueryError> {
        let field = self
            .consume()
            .ok_or_else(|| QueryError::InvalidDsl("expected field after 'order by'".to_string()))?
            .to_string();
        let descending = matches!(
            self.peek_lower().as_deref(),
            Some("desc") | Some("descending")
        );
        if descending {
            self.consume();
        } else if matches!(
            self.peek_lower().as_deref(),
            Some("asc") | Some("ascending")
        ) {
            self.consume();
        }
        Ok(QuerySort { field, descending })
    }

    fn parse_usize(&mut self, label: &str) -> Result<usize, QueryError> {
        let tok = self.consume().ok_or_else(|| {
            QueryError::InvalidDsl(format!("expected number after '{label}'"))
        })?;
        tok.parse::<usize>().map_err(|_| {
            QueryError::InvalidDsl(format!("expected positive integer for '{label}', got {tok:?}"))
        })
    }
}

// ── QueryReport ───────────────────────────────────────────────────────────────

/// Result of executing a `QueryAst`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QueryReport {
    /// The serialized AST that was executed.
    pub query: QueryAst,
    /// Matched notes.
    pub notes: Vec<crate::properties::NoteRecord>,
}

/// Execute a `QueryAst` and return a `QueryReport` (AST + results).
pub fn execute_query_report(paths: &VaultPaths, ast: QueryAst) -> Result<QueryReport, QueryError> {
    let report = execute_query(paths, &ast)?;
    Ok(QueryReport {
        query: ast,
        notes: report.notes,
    })
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── DSL parsing tests ──────────────────────────────────────────────────────

    #[test]
    fn dsl_from_notes() {
        let ast = QueryAst::from_dsl("from notes").unwrap();
        assert_eq!(ast.source, QuerySource::Notes);
        assert!(ast.predicates.is_empty());
        assert!(ast.sort.is_none());
        assert_eq!(ast.limit, None);
        assert_eq!(ast.offset, 0);
    }

    #[test]
    fn dsl_where_eq() {
        let ast = QueryAst::from_dsl("from notes where status = done").unwrap();
        assert_eq!(ast.predicates.len(), 1);
        assert_eq!(ast.predicates[0].field, "status");
        assert_eq!(ast.predicates[0].operator, QueryOperator::Eq);
        assert_eq!(ast.predicates[0].value, QueryValue::Text("done".to_string()));
    }

    #[test]
    fn dsl_where_quoted_value() {
        let ast = QueryAst::from_dsl("from notes where status = \"In Progress\"").unwrap();
        assert_eq!(
            ast.predicates[0].value,
            QueryValue::Text("In Progress".to_string())
        );
    }

    #[test]
    fn dsl_where_multiple_and() {
        let ast =
            QueryAst::from_dsl("from notes where status = done and priority > 2").unwrap();
        assert_eq!(ast.predicates.len(), 2);
        assert_eq!(ast.predicates[1].field, "priority");
        assert_eq!(ast.predicates[1].operator, QueryOperator::Gt);
        assert_eq!(ast.predicates[1].value, QueryValue::Number(2.0));
    }

    #[test]
    fn dsl_order_by_desc() {
        let ast =
            QueryAst::from_dsl("from notes where status = done order by file.mtime desc").unwrap();
        let sort = ast.sort.unwrap();
        assert_eq!(sort.field, "file.mtime");
        assert!(sort.descending);
    }

    #[test]
    fn dsl_order_by_asc() {
        let ast = QueryAst::from_dsl("from notes order by title asc").unwrap();
        let sort = ast.sort.unwrap();
        assert_eq!(sort.field, "title");
        assert!(!sort.descending);
    }

    #[test]
    fn dsl_select_fields() {
        let ast =
            QueryAst::from_dsl("from notes select file.path, status").unwrap();
        assert_eq!(
            ast.projection,
            QueryProjection::Fields(vec!["file.path".to_string(), "status".to_string()])
        );
    }

    #[test]
    fn dsl_limit_and_offset() {
        let ast = QueryAst::from_dsl("from notes limit 10 offset 5").unwrap();
        assert_eq!(ast.limit, Some(10));
        assert_eq!(ast.offset, 5);
    }

    #[test]
    fn dsl_boolean_value() {
        let ast = QueryAst::from_dsl("from notes where reviewed = true").unwrap();
        assert_eq!(ast.predicates[0].value, QueryValue::Bool(true));
    }

    #[test]
    fn dsl_null_value() {
        let ast = QueryAst::from_dsl("from notes where due = null").unwrap();
        assert_eq!(ast.predicates[0].value, QueryValue::Null);
    }

    #[test]
    fn dsl_contains_operator() {
        let ast = QueryAst::from_dsl("from notes where tags contains sprint").unwrap();
        assert_eq!(ast.predicates[0].operator, QueryOperator::Contains);
        assert_eq!(
            ast.predicates[0].value,
            QueryValue::Text("sprint".to_string())
        );
    }

    #[test]
    fn dsl_starts_with_operator() {
        let ast =
            QueryAst::from_dsl("from notes where file.path starts_with \"Projects/\"").unwrap();
        assert_eq!(ast.predicates[0].operator, QueryOperator::StartsWith);
        assert_eq!(
            ast.predicates[0].value,
            QueryValue::Text("Projects/".to_string())
        );
    }

    #[test]
    fn dsl_unknown_source_errors() {
        let err = QueryAst::from_dsl("from tags").unwrap_err();
        assert!(matches!(err, QueryError::InvalidDsl(_)));
    }

    #[test]
    fn dsl_unknown_operator_errors() {
        let err = QueryAst::from_dsl("from notes where status like done").unwrap_err();
        assert!(matches!(err, QueryError::InvalidDsl(_)));
    }

    // ── JSON round-trip tests ──────────────────────────────────────────────────

    #[test]
    fn json_round_trip_simple() {
        let ast = QueryAst {
            source: QuerySource::Notes,
            predicates: vec![QueryPredicate {
                field: "status".to_string(),
                operator: QueryOperator::Eq,
                value: QueryValue::Text("done".to_string()),
            }],
            sort: Some(QuerySort {
                field: "file.mtime".to_string(),
                descending: true,
            }),
            projection: QueryProjection::All,
            limit: Some(10),
            offset: 0,
        };
        let json = serde_json::to_string(&ast).unwrap();
        let roundtripped: QueryAst = serde_json::from_str(&json).unwrap();
        assert_eq!(ast, roundtripped);
    }

    #[test]
    fn json_from_str_minimal() {
        let ast = QueryAst::from_json(r#"{"source":"notes"}"#).unwrap();
        assert_eq!(ast.source, QuerySource::Notes);
        assert!(ast.predicates.is_empty());
    }

    #[test]
    fn json_from_str_with_predicates() {
        let ast = QueryAst::from_json(
            r#"{"source":"notes","predicates":[{"field":"status","operator":"eq","value":"done"}]}"#,
        )
        .unwrap();
        assert_eq!(ast.predicates.len(), 1);
        assert_eq!(ast.predicates[0].field, "status");
        assert_eq!(ast.predicates[0].operator, QueryOperator::Eq);
        assert_eq!(
            ast.predicates[0].value,
            QueryValue::Text("done".to_string())
        );
    }

    // ── NoteQuery conversion tests ─────────────────────────────────────────────

    #[test]
    fn from_note_query_roundtrip() {
        let nq = NoteQuery {
            filters: vec!["status = done".to_string(), "priority >= 2".to_string()],
            sort_by: Some("file.mtime".to_string()),
            sort_descending: true,
        };
        let ast = QueryAst::from_note_query(&nq).unwrap();
        assert_eq!(ast.predicates.len(), 2);
        assert_eq!(ast.sort.as_ref().unwrap().field, "file.mtime");
        assert!(ast.sort.as_ref().unwrap().descending);

        let back = ast.to_note_query();
        // Filter strings may not be byte-for-byte identical (values get re-quoted)
        // but they must parse to the same predicates
        let ast2 = QueryAst::from_note_query(&back).unwrap();
        assert_eq!(ast.predicates, ast2.predicates);
        assert_eq!(ast.sort, ast2.sort);
    }

    #[test]
    fn predicate_to_filter_string_roundtrip() {
        let cases = [
            "status = done",
            "priority >= 2",
            "reviewed = true",
            "due = null",
            "tags contains sprint",
            "file.path starts_with \"Projects/\"",
        ];
        for case in cases {
            let pred = parse_predicate_from_filter_string(case)
                .unwrap_or_else(|_| panic!("should parse: {case}"));
            let rendered = pred.to_filter_string();
            let pred2 = parse_predicate_from_filter_string(&rendered)
                .unwrap_or_else(|_| panic!("should re-parse: {rendered}"));
            assert_eq!(pred, pred2, "round-trip failed for: {case}");
        }
    }
}
