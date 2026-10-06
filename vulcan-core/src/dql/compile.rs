use crate::expression::ast::Expr;
use crate::predicate::Predicate;

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
    /// The shared Dataview predicate lowered from `expr` (QRY.1).
    pub predicate: Predicate,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CompiledDqlSourceExpr {
    /// A tag without `#`, including nested tags.
    Tag(String),
    /// A folder or file, resolved against the vault when evaluated.
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
                    predicate: Predicate::lower_dataview(expr),
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
            CompiledDqlSourceExpr::Tag(tag.strip_prefix('#').unwrap_or(tag.as_str()).to_string())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dql::parse_dql;
    use crate::predicate::{Atom, Comparison, Field, Literal};

    #[test]
    fn compiles_tag_and_boolean_from_sources() {
        let query = parse_dql(r#"LIST FROM (#project AND "Projects") OR outgoing([[Home]])"#)
            .expect("query should parse");
        let compiled = compile_dql(&query);

        assert_eq!(
            compiled.commands,
            vec![CompiledDqlCommand::From(CompiledDqlSourceExpr::Or(
                Box::new(CompiledDqlSourceExpr::And(
                    Box::new(CompiledDqlSourceExpr::Tag("project".to_string())),
                    Box::new(CompiledDqlSourceExpr::Path("Projects".to_string())),
                )),
                Box::new(CompiledDqlSourceExpr::OutgoingLink(
                    DqlLinkTarget::Wikilink("[[Home]]".to_string(),)
                )),
            ))]
        );
    }

    #[test]
    fn lowers_where_expressions_to_shared_predicates() {
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
                predicate: Predicate::All(vec![
                    Predicate::Atom(Atom {
                        field: Field::Property("status".to_string()),
                        comparison: Comparison::Equal,
                        literal: Literal::Text("open".to_string()),
                    }),
                    Predicate::Atom(Atom {
                        field: Field::FilePath,
                        comparison: Comparison::NotEqual,
                        literal: Literal::Text("Archive.md".to_string()),
                    }),
                    Predicate::Unknown,
                ]),
            })
        );
    }

    #[test]
    fn leaves_unlowered_where_parts_to_the_evaluator() {
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
                predicate: Predicate::Any(vec![
                    Predicate::Atom(Atom {
                        field: Field::Property("priority".to_string()),
                        comparison: Comparison::Greater,
                        literal: Literal::Number(1.0),
                    }),
                    Predicate::Unknown,
                ]),
            })]
        );
    }
}
