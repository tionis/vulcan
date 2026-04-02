use serde::{Deserialize, Serialize};

use crate::expression::ast::Expr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DqlSourceExpr {
    Tag(String),
    Path(String),
    IncomingLink(DqlLinkTarget),
    OutgoingLink(DqlLinkTarget),
    Not(Box<DqlSourceExpr>),
    And(Box<DqlSourceExpr>, Box<DqlSourceExpr>),
    Or(Box<DqlSourceExpr>, Box<DqlSourceExpr>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DqlLinkTarget {
    Wikilink(String),
    SelfReference,
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
