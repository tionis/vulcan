use serde_json::Value;

use crate::expression::ast::{BinOp, Expr};
use crate::expression::eval::{canonical_file_field_name, normalize_field_name};
use crate::expression::functions::parse_date_like_string;
use crate::properties::{FilterExpression, FilterField, FilterOperator, FilterValue, ParsedFilter};

use super::ast::{
    DqlDataCommand, DqlLinkTarget, DqlNamedExpr, DqlProjection, DqlQuery, DqlQueryType, DqlSortKey,
    DqlSourceExpr,
};

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CompiledDqlQuery {
    pub query_type: DqlQueryType,
    pub without_id: bool,
    pub table_columns: Vec<DqlProjection>,
    pub list_expression: Option<Expr>,
    pub calendar_expression: Option<Expr>,
    pub commands: Vec<CompiledDqlCommand>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CompiledDqlCommand {
    From(CompiledDqlSourceExpr),
    Where(CompiledWhereClause),
    Sort(Vec<DqlSortKey>),
    GroupBy(DqlNamedExpr),
    Flatten(DqlNamedExpr),
    Limit(usize),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CompiledWhereClause {
    pub expr: Expr,
    pub filters: Option<Vec<FilterExpression>>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CompiledDqlSourceExpr {
    Filter(FilterExpression),
    Path(String),
    IncomingLink(DqlLinkTarget),
    OutgoingLink(DqlLinkTarget),
    Not(Box<CompiledDqlSourceExpr>),
    And(Box<CompiledDqlSourceExpr>, Box<CompiledDqlSourceExpr>),
    Or(Box<CompiledDqlSourceExpr>, Box<CompiledDqlSourceExpr>),
}

pub(crate) fn compile_dql(query: &DqlQuery) -> CompiledDqlQuery {
    CompiledDqlQuery {
        query_type: query.query_type,
        without_id: query.without_id,
        table_columns: query.table_columns.clone(),
        list_expression: query.list_expression.clone(),
        calendar_expression: query.calendar_expression.clone(),
        commands: query
            .commands
            .iter()
            .map(|command| match command {
                DqlDataCommand::From(source) => CompiledDqlCommand::From(compile_source(source)),
                DqlDataCommand::Where(expr) => CompiledDqlCommand::Where(CompiledWhereClause {
                    expr: expr.clone(),
                    filters: compile_where_filters(expr),
                }),
                DqlDataCommand::Sort(keys) => CompiledDqlCommand::Sort(keys.clone()),
                DqlDataCommand::GroupBy(named_expr) => {
                    CompiledDqlCommand::GroupBy(named_expr.clone())
                }
                DqlDataCommand::Flatten(named_expr) => {
                    CompiledDqlCommand::Flatten(named_expr.clone())
                }
                DqlDataCommand::Limit(limit) => CompiledDqlCommand::Limit(*limit),
            })
            .collect(),
    }
}

fn compile_source(source: &DqlSourceExpr) -> CompiledDqlSourceExpr {
    match source {
        DqlSourceExpr::Tag(tag) => {
            CompiledDqlSourceExpr::Filter(FilterExpression::Condition(ParsedFilter {
                field: FilterField::FileTags,
                operator: FilterOperator::HasTag,
                value: FilterValue::Text(tag.strip_prefix('#').unwrap_or(tag.as_str()).to_string()),
            }))
        }
        DqlSourceExpr::Path(path) => CompiledDqlSourceExpr::Path(path.clone()),
        DqlSourceExpr::IncomingLink(target) => CompiledDqlSourceExpr::IncomingLink(target.clone()),
        DqlSourceExpr::OutgoingLink(target) => CompiledDqlSourceExpr::OutgoingLink(target.clone()),
        DqlSourceExpr::Not(inner) => CompiledDqlSourceExpr::Not(Box::new(compile_source(inner))),
        DqlSourceExpr::And(left, right) => CompiledDqlSourceExpr::And(
            Box::new(compile_source(left)),
            Box::new(compile_source(right)),
        ),
        DqlSourceExpr::Or(left, right) => CompiledDqlSourceExpr::Or(
            Box::new(compile_source(left)),
            Box::new(compile_source(right)),
        ),
    }
}

pub(crate) fn compile_where_filters(expr: &Expr) -> Option<Vec<FilterExpression>> {
    match expr {
        Expr::BinaryOp(left, BinOp::And, right) => {
            let mut filters = compile_where_filters(left)?;
            filters.extend(compile_where_filters(right)?);
            Some(filters)
        }
        Expr::BinaryOp(left, BinOp::Or, right) => {
            let mut filters = compile_any_filters(left)?;
            filters.extend(compile_any_filters(right)?);
            Some(vec![FilterExpression::Any(filters)])
        }
        _ => compile_single_filter(expr).map(|filter| vec![filter]),
    }
}

fn compile_any_filters(expr: &Expr) -> Option<Vec<ParsedFilter>> {
    match expr {
        Expr::BinaryOp(left, BinOp::Or, right) => {
            let mut filters = compile_any_filters(left)?;
            filters.extend(compile_any_filters(right)?);
            Some(filters)
        }
        _ => match compile_single_filter(expr)? {
            FilterExpression::Condition(condition) => Some(vec![condition]),
            FilterExpression::Any(filters) => Some(filters),
        },
    }
}

fn compile_single_filter(expr: &Expr) -> Option<FilterExpression> {
    match expr {
        Expr::BinaryOp(left, operator, right)
            if matches!(
                operator,
                BinOp::Eq | BinOp::Ne | BinOp::Gt | BinOp::Ge | BinOp::Lt | BinOp::Le
            ) =>
        {
            compile_comparison_filter(left, *operator, right)
        }
        Expr::FunctionCall(name, args)
            if name.eq_ignore_ascii_case("contains") && args.len() == 2 =>
        {
            compile_contains_filter(&args[0], &args[1])
        }
        _ => None,
    }
}

fn compile_comparison_filter(
    left: &Expr,
    operator: BinOp,
    right: &Expr,
) -> Option<FilterExpression> {
    if let (Some(field), Some(value)) = (compile_filter_field(left), compile_filter_value(right)) {
        return Some(FilterExpression::Condition(ParsedFilter {
            field,
            operator: comparison_operator(operator),
            value,
        }));
    }

    let (Some(field), Some(value)) = (compile_filter_field(right), compile_filter_value(left))
    else {
        return None;
    };
    Some(FilterExpression::Condition(ParsedFilter {
        field,
        operator: comparison_operator(reverse_comparison(operator)),
        value,
    }))
}

fn compile_contains_filter(container: &Expr, needle: &Expr) -> Option<FilterExpression> {
    let field = compile_filter_field(container)?;
    let value = compile_filter_value(needle)?;
    Some(FilterExpression::Condition(ParsedFilter {
        field,
        operator: FilterOperator::Contains,
        value,
    }))
}

fn compile_filter_field(expr: &Expr) -> Option<FilterField> {
    match expr {
        Expr::Identifier(name)
            if name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')) =>
        {
            Some(FilterField::Property(normalize_field_name(name)))
        }
        Expr::FieldAccess(receiver, field) if matches!(receiver.as_ref(), Expr::Identifier(name) if name.eq_ignore_ascii_case("file")) => {
            match canonical_file_field_name(field).as_str() {
                "path" => Some(FilterField::FilePath),
                "name" | "basename" => Some(FilterField::FileName),
                "ext" => Some(FilterField::FileExt),
                "mtime" => Some(FilterField::FileMtime),
                "tags" => Some(FilterField::FileTags),
                _ => None,
            }
        }
        _ => None,
    }
}

fn compile_filter_value(expr: &Expr) -> Option<FilterValue> {
    match expr {
        Expr::Null => Some(FilterValue::Null),
        Expr::Bool(value) => Some(FilterValue::Bool(*value)),
        Expr::Number(value) => Some(FilterValue::Number(*value)),
        Expr::Str(text) => compile_string_filter_value(text),
        Expr::FunctionCall(name, args) if name.eq_ignore_ascii_case("date") && args.len() == 1 => {
            match args.first()? {
                Expr::Str(text) => {
                    parse_date_like_string(text).map(|_| FilterValue::Date(text.trim().to_string()))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

fn compile_string_filter_value(text: &str) -> Option<FilterValue> {
    if parse_date_like_string(text).is_some() {
        return Some(FilterValue::Date(text.trim().to_string()));
    }

    match Value::String(text.to_string()) {
        Value::String(value) => Some(FilterValue::Text(value)),
        _ => None,
    }
}

fn comparison_operator(operator: BinOp) -> FilterOperator {
    match operator {
        BinOp::Eq => FilterOperator::Eq,
        BinOp::Ne => FilterOperator::Ne,
        BinOp::Gt => FilterOperator::Gt,
        BinOp::Ge => FilterOperator::Gte,
        BinOp::Lt => FilterOperator::Lt,
        BinOp::Le => FilterOperator::Lte,
        BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod | BinOp::And | BinOp::Or => {
            unreachable!("comparison operator mapping only accepts comparison operators")
        }
    }
}

fn reverse_comparison(operator: BinOp) -> BinOp {
    match operator {
        BinOp::Gt => BinOp::Lt,
        BinOp::Ge => BinOp::Le,
        BinOp::Lt => BinOp::Gt,
        BinOp::Le => BinOp::Ge,
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dql::parse_dql;

    #[test]
    fn compiles_tag_and_boolean_from_sources() {
        let query = parse_dql(r#"LIST FROM (#project AND "Projects") OR outgoing([[Home]])"#)
            .expect("query should parse");
        let compiled = compile_dql(&query);

        assert_eq!(
            compiled.commands,
            vec![CompiledDqlCommand::From(CompiledDqlSourceExpr::Or(
                Box::new(CompiledDqlSourceExpr::And(
                    Box::new(CompiledDqlSourceExpr::Filter(FilterExpression::Condition(
                        ParsedFilter {
                            field: FilterField::FileTags,
                            operator: FilterOperator::HasTag,
                            value: FilterValue::Text("project".to_string()),
                        }
                    ))),
                    Box::new(CompiledDqlSourceExpr::Path("Projects".to_string())),
                )),
                Box::new(CompiledDqlSourceExpr::OutgoingLink(
                    DqlLinkTarget::Wikilink("[[Home]]".to_string(),)
                )),
            ))]
        );
    }

    #[test]
    fn compiles_simple_where_expressions_to_shared_filters() {
        let query = parse_dql(
            r##"TABLE file.name
WHERE status = "open" AND file.path != "Archive.md" AND contains(file.tags, "#project")"##,
        )
        .expect("query should parse");
        let compiled = compile_dql(&query);

        assert_eq!(
            compiled.commands[0],
            CompiledDqlCommand::Where(CompiledWhereClause {
                expr: crate::expression::parse_expression(
                    r##"status = "open" && file.path != "Archive.md" && contains(file.tags, "#project")"##,
                )
                .expect("expression should parse"),
                filters: Some(vec![
                    FilterExpression::Condition(ParsedFilter {
                        field: FilterField::Property("status".to_string()),
                        operator: FilterOperator::Eq,
                        value: FilterValue::Text("open".to_string()),
                    }),
                    FilterExpression::Condition(ParsedFilter {
                        field: FilterField::FilePath,
                        operator: FilterOperator::Ne,
                        value: FilterValue::Text("Archive.md".to_string()),
                    }),
                    FilterExpression::Condition(ParsedFilter {
                        field: FilterField::FileTags,
                        operator: FilterOperator::Contains,
                        value: FilterValue::Text("#project".to_string()),
                    }),
                ]),
            })
        );
    }

    #[test]
    fn leaves_non_sql_where_expressions_as_expression_only() {
        let query = parse_dql(r"TABLE file.name WHERE priority > 1 OR choice(done, 1, 0) = 1")
            .expect("query should parse");
        let compiled = compile_dql(&query);

        assert_eq!(
            compiled.commands,
            vec![CompiledDqlCommand::Where(CompiledWhereClause {
                expr: crate::expression::parse_expression(
                    r"priority > 1 || choice(done, 1, 0) = 1",
                )
                .expect("expression should parse"),
                filters: None,
            })]
        );
    }
}
