use crate::expression::ast::Expr;

use super::token::DqlToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DqlQueryType {
    Table,
    List,
    Task,
    Calendar,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DqlQuery {
    pub query_type: DqlQueryType,
    pub without_id: bool,
    pub table_columns: Vec<DqlProjection>,
    pub list_expression: Option<Expr>,
    pub calendar_expression: Option<Expr>,
    pub commands: Vec<DqlDataCommand>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DqlProjection {
    pub expr: Expr,
    pub alias: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DqlDataCommand {
    From(DqlSourceExpr),
    Where(Expr),
    Sort(Vec<DqlSortKey>),
    GroupBy(DqlNamedExpr),
    Flatten(DqlNamedExpr),
    Limit(usize),
}

#[derive(Debug, Clone, PartialEq)]
pub struct DqlSourceExpr {
    pub tokens: Vec<DqlToken>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DqlSortKey {
    pub expr: Expr,
    pub direction: DqlSortDirection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DqlSortDirection {
    Asc,
    Desc,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DqlNamedExpr {
    pub expr: Expr,
    pub alias: Option<String>,
}
