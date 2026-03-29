use crate::expression::{ast::Expr, parse_expression};

use super::ast::{
    DqlDataCommand, DqlNamedExpr, DqlProjection, DqlQuery, DqlQueryType, DqlSortDirection,
    DqlSortKey, DqlSourceExpr,
};
use super::token::{DqlToken, DqlTokenizer};

pub fn parse_dql(source: &str) -> Result<DqlQuery, String> {
    DqlParser::new(source)?.parse()
}

struct DqlParser {
    tokens: Vec<DqlToken>,
    pos: usize,
}

impl DqlParser {
    fn new(source: &str) -> Result<Self, String> {
        let mut tokenizer = DqlTokenizer::new(source);
        let mut tokens = Vec::new();
        loop {
            let token = tokenizer.next_token()?;
            if token == DqlToken::Eof {
                break;
            }
            tokens.push(token);
        }
        Ok(Self { tokens, pos: 0 })
    }

    fn parse(mut self) -> Result<DqlQuery, String> {
        let query_type = self.parse_query_type()?;
        let without_id = self.parse_without_id(query_type)?;
        let (table_columns, list_expression, calendar_expression) = match query_type {
            DqlQueryType::Table => (self.parse_table_columns()?, None, None),
            DqlQueryType::List => (Vec::new(), self.parse_optional_list_expression()?, None),
            DqlQueryType::Calendar => (Vec::new(), None, self.parse_optional_list_expression()?),
            DqlQueryType::Task => (Vec::new(), None, None),
        };

        let mut commands = Vec::new();
        while self.peek().is_some() {
            commands.push(self.parse_command()?);
        }

        Ok(DqlQuery {
            query_type,
            without_id,
            table_columns,
            list_expression,
            calendar_expression,
            commands,
        })
    }

    fn parse_query_type(&mut self) -> Result<DqlQueryType, String> {
        match self
            .advance()
            .ok_or_else(|| "expected query type".to_string())?
        {
            DqlToken::Table => Ok(DqlQueryType::Table),
            DqlToken::List => Ok(DqlQueryType::List),
            DqlToken::Task => Ok(DqlQueryType::Task),
            DqlToken::Calendar => Ok(DqlQueryType::Calendar),
            token => Err(format!("expected query type, got {token:?}")),
        }
    }

    fn parse_without_id(&mut self, query_type: DqlQueryType) -> Result<bool, String> {
        if !matches!(query_type, DqlQueryType::Table | DqlQueryType::List) {
            return Ok(false);
        }
        if !matches!(self.peek(), Some(DqlToken::Without)) {
            return Ok(false);
        }
        self.advance();
        match self.advance() {
            Some(DqlToken::Id) => Ok(true),
            Some(token) => Err(format!("expected ID after WITHOUT, got {token:?}")),
            None => Err("expected ID after WITHOUT".to_string()),
        }
    }

    fn parse_table_columns(&mut self) -> Result<Vec<DqlProjection>, String> {
        let mut columns = Vec::new();
        while self.peek().is_some() && !self.at_clause_start() {
            let tokens = self.take_top_level_until(|token, depth| {
                depth == 0
                    && (matches!(token, DqlToken::Comma | DqlToken::Eof) || is_clause_start(token))
            });
            if tokens.is_empty() {
                return Err("expected TABLE column expression".to_string());
            }
            columns.push(parse_projection(&tokens)?);
            if matches!(self.peek(), Some(DqlToken::Comma)) {
                self.advance();
            }
        }
        if columns.is_empty() {
            return Err("TABLE queries require at least one column expression".to_string());
        }
        Ok(columns)
    }

    fn parse_optional_list_expression(&mut self) -> Result<Option<Expr>, String> {
        if self.peek().is_none() || self.at_clause_start() {
            return Ok(None);
        }
        let tokens = self.take_top_level_until(|token, depth| depth == 0 && is_clause_start(token));
        if tokens.is_empty() {
            return Ok(None);
        }
        Ok(Some(parse_expression_tokens(&tokens)?))
    }

    fn parse_command(&mut self) -> Result<DqlDataCommand, String> {
        match self
            .advance()
            .ok_or_else(|| "expected data command".to_string())?
        {
            DqlToken::From => {
                let tokens = self.take_clause_body_tokens();
                if tokens.is_empty() {
                    return Err("FROM requires a source expression".to_string());
                }
                Ok(DqlDataCommand::From(DqlSourceExpr { tokens }))
            }
            DqlToken::Where => {
                let tokens = self.take_clause_body_tokens();
                if tokens.is_empty() {
                    return Err("WHERE requires an expression".to_string());
                }
                Ok(DqlDataCommand::Where(parse_expression_tokens(&tokens)?))
            }
            DqlToken::Sort => {
                let keys = self.parse_sort_keys()?;
                if keys.is_empty() {
                    return Err("SORT requires at least one sort key".to_string());
                }
                Ok(DqlDataCommand::Sort(keys))
            }
            DqlToken::Group => {
                self.expect(&DqlToken::By, "expected BY after GROUP")?;
                Ok(DqlDataCommand::GroupBy(
                    self.parse_named_expression("GROUP BY")?,
                ))
            }
            DqlToken::Flatten => Ok(DqlDataCommand::Flatten(
                self.parse_named_expression("FLATTEN")?,
            )),
            DqlToken::Limit => Ok(DqlDataCommand::Limit(self.parse_limit()?)),
            token => Err(format!("unexpected token {token:?} at top level")),
        }
    }

    fn parse_sort_keys(&mut self) -> Result<Vec<DqlSortKey>, String> {
        let mut keys = Vec::new();
        while self.peek().is_some() && !self.at_clause_start() {
            let tokens = self.take_top_level_until(|token, depth| {
                depth == 0
                    && (matches!(token, DqlToken::Comma | DqlToken::Eof) || is_clause_start(token))
            });
            if tokens.is_empty() {
                return Err("expected SORT key expression".to_string());
            }
            keys.push(parse_sort_key(&tokens)?);
            if matches!(self.peek(), Some(DqlToken::Comma)) {
                self.advance();
            }
        }
        Ok(keys)
    }

    fn parse_named_expression(&mut self, clause_name: &str) -> Result<DqlNamedExpr, String> {
        let tokens = self.take_clause_body_tokens();
        if tokens.is_empty() {
            return Err(format!("{clause_name} requires an expression"));
        }
        parse_named_expression_tokens(&tokens)
    }

    fn parse_limit(&mut self) -> Result<usize, String> {
        match self.advance() {
            Some(DqlToken::Number(value))
                if value.is_finite() && value >= 0.0 && value.fract() == 0.0 =>
            {
                value
                    .to_string()
                    .parse::<usize>()
                    .map_err(|_| "LIMIT is too large".to_string())
            }
            Some(token) => Err(format!(
                "LIMIT expects a non-negative integer, got {token:?}"
            )),
            None => Err("LIMIT expects a non-negative integer".to_string()),
        }
    }

    fn take_clause_body_tokens(&mut self) -> Vec<DqlToken> {
        self.take_top_level_until(|token, depth| depth == 0 && is_clause_start(token))
    }

    fn take_top_level_until<F>(&mut self, mut should_stop: F) -> Vec<DqlToken>
    where
        F: FnMut(&DqlToken, usize) -> bool,
    {
        let mut tokens = Vec::new();
        let mut depth = 0_usize;

        while let Some(token) = self.peek() {
            if should_stop(token, depth) {
                break;
            }

            let token = self.advance().expect("peek confirmed a token exists");
            match token {
                DqlToken::LParen | DqlToken::LBracket => {
                    depth += 1;
                    tokens.push(token);
                }
                DqlToken::RParen | DqlToken::RBracket => {
                    depth = depth.saturating_sub(1);
                    tokens.push(token);
                }
                _ => tokens.push(token),
            }
        }

        tokens
    }

    fn expect(&mut self, expected: &DqlToken, message: &str) -> Result<(), String> {
        match self.advance() {
            Some(token) if token == *expected => Ok(()),
            Some(token) => Err(format!("{message}, got {token:?}")),
            None => Err(message.to_string()),
        }
    }

    fn at_clause_start(&self) -> bool {
        self.peek().is_some_and(is_clause_start)
    }

    fn peek(&self) -> Option<&DqlToken> {
        self.tokens.get(self.pos)
    }

    fn advance(&mut self) -> Option<DqlToken> {
        let token = self.tokens.get(self.pos).cloned()?;
        self.pos += 1;
        Some(token)
    }
}

fn is_clause_start(token: &DqlToken) -> bool {
    matches!(
        token,
        DqlToken::From
            | DqlToken::Where
            | DqlToken::Sort
            | DqlToken::Group
            | DqlToken::Flatten
            | DqlToken::Limit
    )
}

fn parse_projection(tokens: &[DqlToken]) -> Result<DqlProjection, String> {
    let (expr_tokens, alias) = split_alias(tokens)?;
    Ok(DqlProjection {
        expr: parse_expression_tokens(expr_tokens)?,
        alias,
    })
}

fn parse_named_expression_tokens(tokens: &[DqlToken]) -> Result<DqlNamedExpr, String> {
    let (expr_tokens, alias) = split_alias(tokens)?;
    Ok(DqlNamedExpr {
        expr: parse_expression_tokens(expr_tokens)?,
        alias,
    })
}

fn parse_sort_key(tokens: &[DqlToken]) -> Result<DqlSortKey, String> {
    let (expr_tokens, direction) = match tokens.last() {
        Some(DqlToken::Asc | DqlToken::Ascending) => {
            (&tokens[..tokens.len() - 1], DqlSortDirection::Asc)
        }
        Some(DqlToken::Desc | DqlToken::Descending) => {
            (&tokens[..tokens.len() - 1], DqlSortDirection::Desc)
        }
        _ => (tokens, DqlSortDirection::Asc),
    };

    if expr_tokens.is_empty() {
        return Err("SORT key is missing an expression".to_string());
    }

    Ok(DqlSortKey {
        expr: parse_expression_tokens(expr_tokens)?,
        direction,
    })
}

fn split_alias(tokens: &[DqlToken]) -> Result<(&[DqlToken], Option<String>), String> {
    let mut depth = 0_usize;
    let mut alias_index = None;

    for (index, token) in tokens.iter().enumerate() {
        match token {
            DqlToken::LParen | DqlToken::LBracket => depth += 1,
            DqlToken::RParen | DqlToken::RBracket => depth = depth.saturating_sub(1),
            DqlToken::As if depth == 0 => alias_index = Some(index),
            _ => {}
        }
    }

    let Some(alias_index) = alias_index else {
        return Ok((tokens, None));
    };
    if alias_index == 0 {
        return Err("alias is missing an expression".to_string());
    }
    if alias_index + 2 != tokens.len() {
        return Err("AS aliases must appear at the end of an expression".to_string());
    }

    let alias = alias_from_token(&tokens[alias_index + 1])
        .ok_or_else(|| "aliases must be identifiers or strings".to_string())?;
    Ok((&tokens[..alias_index], Some(alias)))
}

fn alias_from_token(token: &DqlToken) -> Option<String> {
    match token {
        DqlToken::Str(value) | DqlToken::Ident(value) => Some(value.clone()),
        _ => keyword_token_name(token).map(ToOwned::to_owned),
    }
}

fn parse_expression_tokens(tokens: &[DqlToken]) -> Result<Expr, String> {
    parse_expression(&render_expression_tokens(tokens))
}

fn render_expression_tokens(tokens: &[DqlToken]) -> String {
    tokens
        .iter()
        .map(render_expression_token)
        .collect::<Vec<_>>()
        .join(" ")
}

fn render_expression_token(token: &DqlToken) -> String {
    match token {
        DqlToken::Number(value) => value.to_string(),
        DqlToken::Str(value) => serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string()),
        DqlToken::Ident(value)
        | DqlToken::Wikilink(value)
        | DqlToken::DateLiteral(value)
        | DqlToken::DurationLiteral(value) => value.clone(),
        DqlToken::Tag(value) => serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string()),
        DqlToken::Plus => "+".to_string(),
        DqlToken::Minus => "-".to_string(),
        DqlToken::Star => "*".to_string(),
        DqlToken::Slash => "/".to_string(),
        DqlToken::Percent => "%".to_string(),
        DqlToken::Eq | DqlToken::EqEq => "=".to_string(),
        DqlToken::Ne => "!=".to_string(),
        DqlToken::Gt => ">".to_string(),
        DqlToken::Lt => "<".to_string(),
        DqlToken::Ge => ">=".to_string(),
        DqlToken::Le => "<=".to_string(),
        DqlToken::And => "&&".to_string(),
        DqlToken::Or => "||".to_string(),
        DqlToken::Not | DqlToken::Bang => "!".to_string(),
        DqlToken::Dot => ".".to_string(),
        DqlToken::Comma => ",".to_string(),
        DqlToken::LParen => "(".to_string(),
        DqlToken::RParen => ")".to_string(),
        DqlToken::LBracket => "[".to_string(),
        DqlToken::RBracket => "]".to_string(),
        DqlToken::Null => "null".to_string(),
        DqlToken::True => "true".to_string(),
        DqlToken::False => "false".to_string(),
        token => keyword_token_name(token).unwrap_or("").to_string(),
    }
}

fn keyword_token_name(token: &DqlToken) -> Option<&'static str> {
    match token {
        DqlToken::Table => Some("table"),
        DqlToken::List => Some("list"),
        DqlToken::Task => Some("task"),
        DqlToken::Calendar => Some("calendar"),
        DqlToken::From => Some("from"),
        DqlToken::Where => Some("where"),
        DqlToken::Sort => Some("sort"),
        DqlToken::Group => Some("group"),
        DqlToken::By => Some("by"),
        DqlToken::Flatten => Some("flatten"),
        DqlToken::Limit => Some("limit"),
        DqlToken::Asc | DqlToken::Ascending => Some("asc"),
        DqlToken::Desc | DqlToken::Descending => Some("desc"),
        DqlToken::Without => Some("without"),
        DqlToken::Id => Some("id"),
        DqlToken::As => Some("as"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use crate::expression::ast::BinOp;

    use super::*;

    #[test]
    fn parses_table_query_shape_and_sequential_commands() {
        let query = parse_dql(
            r#"TABLE WITHOUT ID file.name AS "Name", choice(priority > 1, "hi", "lo") AS Score
FROM #project
WHERE reviewed = true
SORT due DESC, file.name ASC
LIMIT 5"#,
        )
        .expect("DQL should parse");

        assert_eq!(query.query_type, DqlQueryType::Table);
        assert!(query.without_id);
        assert_eq!(query.table_columns.len(), 2);
        assert_eq!(query.table_columns[0].alias.as_deref(), Some("Name"));
        assert_eq!(query.table_columns[1].alias.as_deref(), Some("Score"));
        assert_eq!(query.calendar_expression, None);
        assert_eq!(
            query.commands,
            vec![
                DqlDataCommand::From(DqlSourceExpr {
                    tokens: vec![DqlToken::Tag("#project".to_string())],
                }),
                DqlDataCommand::Where(Expr::BinaryOp(
                    Box::new(Expr::Identifier("reviewed".to_string())),
                    BinOp::Eq,
                    Box::new(Expr::Bool(true)),
                )),
                DqlDataCommand::Sort(vec![
                    DqlSortKey {
                        expr: Expr::Identifier("due".to_string()),
                        direction: DqlSortDirection::Desc,
                    },
                    DqlSortKey {
                        expr: Expr::FieldAccess(
                            Box::new(Expr::Identifier("file".to_string())),
                            "name".to_string(),
                        ),
                        direction: DqlSortDirection::Asc,
                    },
                ]),
                DqlDataCommand::Limit(5),
            ]
        );
    }

    #[test]
    fn parses_list_task_and_calendar_query_types() {
        let list_query = parse_dql(r#"LIST choice(done, "yes", "no") FROM "Projects" LIMIT 10"#)
            .expect("LIST DQL should parse");
        assert_eq!(list_query.query_type, DqlQueryType::List);
        assert!(list_query.table_columns.is_empty());
        assert!(list_query.list_expression.is_some());
        assert_eq!(list_query.calendar_expression, None);
        assert_eq!(
            list_query.commands,
            vec![
                DqlDataCommand::From(DqlSourceExpr {
                    tokens: vec![DqlToken::Str("Projects".to_string())],
                }),
                DqlDataCommand::Limit(10),
            ]
        );

        let task_query = parse_dql("TASK FROM (#project OR [[]])").expect("TASK DQL should parse");
        assert_eq!(task_query.query_type, DqlQueryType::Task);
        assert_eq!(task_query.list_expression, None);
        assert_eq!(task_query.calendar_expression, None);
        assert_eq!(
            task_query.commands,
            vec![DqlDataCommand::From(DqlSourceExpr {
                tokens: vec![
                    DqlToken::LParen,
                    DqlToken::Tag("#project".to_string()),
                    DqlToken::Or,
                    DqlToken::Wikilink("[[]]".to_string()),
                    DqlToken::RParen,
                ],
            })]
        );

        let calendar_query = parse_dql("CALENDAR file.day WHERE file.day != null")
            .expect("CALENDAR DQL should parse");
        assert_eq!(calendar_query.query_type, DqlQueryType::Calendar);
        assert_eq!(
            calendar_query.calendar_expression,
            Some(parse_expression("file.day").expect("expression should parse"))
        );
        assert_eq!(
            calendar_query.commands,
            vec![DqlDataCommand::Where(Expr::BinaryOp(
                Box::new(Expr::FieldAccess(
                    Box::new(Expr::Identifier("file".to_string())),
                    "day".to_string(),
                )),
                BinOp::Ne,
                Box::new(Expr::Null),
            ))]
        );
    }

    #[test]
    fn parses_group_by_and_flatten_named_expressions() {
        let query = parse_dql(
            r#"TABLE file.name
GROUP BY (choice(priority > 1, "hot", "cold")) AS "Bucket"
FLATTEN file.tasks AS task"#,
        )
        .expect("DQL should parse");

        assert_eq!(query.calendar_expression, None);
        assert_eq!(
            query.commands,
            vec![
                DqlDataCommand::GroupBy(DqlNamedExpr {
                    expr: parse_expression(r#"choice(priority > 1, "hot", "cold")"#)
                        .expect("expression should parse"),
                    alias: Some("Bucket".to_string()),
                }),
                DqlDataCommand::Flatten(DqlNamedExpr {
                    expr: parse_expression("file.tasks").expect("expression should parse"),
                    alias: Some("task".to_string()),
                }),
            ]
        );
    }
}
