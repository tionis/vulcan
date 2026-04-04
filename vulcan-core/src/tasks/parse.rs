use super::ast::{
    TasksDateField, TasksDateRelation, TasksFilter, TasksQuery, TasksQueryCommand, TasksTextField,
};

pub fn parse_tasks_query(source: &str) -> Result<TasksQuery, String> {
    let mut commands = Vec::new();

    for (index, line) in source.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let command = parse_tasks_query_line(trimmed)
            .map_err(|error| format!("line {}: {error}", index.saturating_add(1)))?;
        commands.push(command);
    }

    Ok(TasksQuery { commands })
}

fn parse_tasks_query_line(line: &str) -> Result<TasksQueryCommand, String> {
    if line.eq_ignore_ascii_case("short mode") {
        return Ok(TasksQueryCommand::ShortMode);
    }
    if line.eq_ignore_ascii_case("explain") {
        return Ok(TasksQueryCommand::Explain);
    }
    if let Some(rest) = strip_prefix_ci(line, "limit groups ") {
        return Ok(TasksQueryCommand::LimitGroups {
            value: parse_usize(rest, "limit groups")?,
        });
    }
    if let Some(rest) = strip_prefix_ci(line, "limit ") {
        return Ok(TasksQueryCommand::Limit {
            value: parse_usize(rest, "limit")?,
        });
    }
    if let Some(rest) = strip_prefix_ci(line, "sort by ") {
        let (field, reverse) = parse_field_with_optional_reverse(rest, "sort by")?;
        return Ok(TasksQueryCommand::Sort { field, reverse });
    }
    if let Some(rest) = strip_prefix_ci(line, "group by ") {
        let (field, reverse) = parse_field_with_optional_reverse(rest, "group by")?;
        return Ok(TasksQueryCommand::Group { field, reverse });
    }
    if let Some(rest) = strip_prefix_ci(line, "hide ") {
        return Ok(TasksQueryCommand::Hide {
            field: parse_field(rest, "hide")?,
        });
    }
    if let Some(rest) = strip_prefix_ci(line, "show ") {
        return Ok(TasksQueryCommand::Show {
            field: parse_field(rest, "show")?,
        });
    }

    Ok(TasksQueryCommand::Filter {
        filter: parse_filter_expression(line)?,
    })
}

fn parse_filter_expression(line: &str) -> Result<TasksFilter, String> {
    let tokens = tokenize_filter(line)?;
    if tokens.is_empty() {
        return Err("expected a filter expression".to_string());
    }

    let mut parser = FilterParser::new(tokens);
    let filter = parser.parse_expression()?;
    if parser.peek().is_some() {
        return Err(format!(
            "unexpected trailing token {}",
            parser.peek().map_or(String::new(), FilterToken::display)
        ));
    }
    Ok(filter)
}

fn parse_usize(value: &str, clause_name: &str) -> Result<usize, String> {
    value
        .trim()
        .parse::<usize>()
        .map_err(|_| format!("{clause_name} requires a positive integer"))
}

fn parse_field(rest: &str, clause_name: &str) -> Result<String, String> {
    let field = unquote(rest.trim());
    if field.is_empty() {
        return Err(format!("{clause_name} requires a field name"));
    }
    Ok(field)
}

fn parse_field_with_optional_reverse(
    rest: &str,
    clause_name: &str,
) -> Result<(String, bool), String> {
    let trimmed = rest.trim();
    if let Some(field) = strip_suffix_ci(trimmed, " reverse") {
        return Ok((parse_field(field, clause_name)?, true));
    }

    Ok((parse_field(trimmed, clause_name)?, false))
}

fn strip_prefix_ci<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    value
        .get(..prefix.len())
        .filter(|candidate| candidate.eq_ignore_ascii_case(prefix))
        .map(|_| &value[prefix.len()..])
}

fn strip_suffix_ci<'a>(value: &'a str, suffix: &str) -> Option<&'a str> {
    let start = value.len().checked_sub(suffix.len())?;
    value
        .get(start..)
        .filter(|candidate| candidate.eq_ignore_ascii_case(suffix))
        .map(|_| &value[..start])
}

fn unquote(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.len() >= 2
        && ((trimmed.starts_with('"') && trimmed.ends_with('"'))
            || (trimmed.starts_with('\'') && trimmed.ends_with('\'')))
    {
        trimmed[1..trimmed.len() - 1].to_string()
    } else {
        trimmed.to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FilterToken {
    Word(String),
    LParen,
    RParen,
}

impl FilterToken {
    fn display(&self) -> String {
        match self {
            Self::Word(word) => word.clone(),
            Self::LParen => "(".to_string(),
            Self::RParen => ")".to_string(),
        }
    }
}

fn tokenize_filter(source: &str) -> Result<Vec<FilterToken>, String> {
    let mut tokens = Vec::new();
    let mut chars = source.chars().peekable();

    while let Some(ch) = chars.peek().copied() {
        if ch.is_whitespace() {
            chars.next();
            continue;
        }
        if ch == '(' {
            chars.next();
            tokens.push(FilterToken::LParen);
            continue;
        }
        if ch == ')' {
            chars.next();
            tokens.push(FilterToken::RParen);
            continue;
        }
        if ch == '"' || ch == '\'' {
            let quote = ch;
            chars.next();
            let mut value = String::new();
            let mut closed = false;
            for next in chars.by_ref() {
                if next == quote {
                    closed = true;
                    break;
                }
                value.push(next);
            }
            if !closed {
                return Err("unterminated quoted string".to_string());
            }
            tokens.push(FilterToken::Word(value));
            continue;
        }

        let mut value = String::new();
        while let Some(next) = chars.peek().copied() {
            if next.is_whitespace() || matches!(next, '(' | ')') {
                break;
            }
            value.push(next);
            chars.next();
        }
        tokens.push(FilterToken::Word(value));
    }

    Ok(tokens)
}

struct FilterParser {
    tokens: Vec<FilterToken>,
    pos: usize,
}

impl FilterParser {
    fn new(tokens: Vec<FilterToken>) -> Self {
        Self { tokens, pos: 0 }
    }

    fn parse_expression(&mut self) -> Result<TasksFilter, String> {
        self.parse_or()
    }

    fn parse_or(&mut self) -> Result<TasksFilter, String> {
        let mut filters = vec![self.parse_and()?];
        while self.consume_word_ci("or") {
            filters.push(self.parse_and()?);
        }
        if filters.len() == 1 {
            Ok(filters.remove(0))
        } else {
            Ok(TasksFilter::Or { filters })
        }
    }

    fn parse_and(&mut self) -> Result<TasksFilter, String> {
        let mut filters = vec![self.parse_not()?];
        while self.consume_word_ci("and") {
            filters.push(self.parse_not()?);
        }
        if filters.len() == 1 {
            Ok(filters.remove(0))
        } else {
            Ok(TasksFilter::And { filters })
        }
    }

    fn parse_not(&mut self) -> Result<TasksFilter, String> {
        if self.peek_word_ci("not") && matches!(self.peek_n(1), Some(FilterToken::LParen)) {
            self.pos += 1;
            return Ok(TasksFilter::Not {
                filter: Box::new(self.parse_primary()?),
            });
        }

        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<TasksFilter, String> {
        if matches!(self.peek(), Some(FilterToken::LParen)) {
            self.pos += 1;
            let filter = self.parse_expression()?;
            match self.advance() {
                Some(FilterToken::RParen) => Ok(filter),
                Some(token) => Err(format!("expected ')', got {}", token.display())),
                None => Err("expected ')'".to_string()),
            }
        } else {
            self.parse_primitive()
        }
    }

    fn parse_primitive(&mut self) -> Result<TasksFilter, String> {
        let start = self.pos;
        while let Some(token) = self.peek() {
            match token {
                FilterToken::RParen => break,
                FilterToken::Word(word)
                    if word.eq_ignore_ascii_case("and") || word.eq_ignore_ascii_case("or") =>
                {
                    break;
                }
                _ => self.pos += 1,
            }
        }

        let tokens = &self.tokens[start..self.pos];
        if tokens.is_empty() {
            return Err("expected a filter".to_string());
        }
        parse_primitive_tokens(tokens)
    }

    fn consume_word_ci(&mut self, expected: &str) -> bool {
        if self.peek_word_ci(expected) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn peek_word_ci(&self, expected: &str) -> bool {
        matches!(
            self.peek(),
            Some(FilterToken::Word(word)) if word.eq_ignore_ascii_case(expected)
        )
    }

    fn peek(&self) -> Option<&FilterToken> {
        self.tokens.get(self.pos)
    }

    fn peek_n(&self, offset: usize) -> Option<&FilterToken> {
        self.tokens.get(self.pos.saturating_add(offset))
    }

    fn advance(&mut self) -> Option<&FilterToken> {
        let token = self.tokens.get(self.pos);
        if token.is_some() {
            self.pos += 1;
        }
        token
    }
}

#[allow(clippy::too_many_lines)]
fn parse_primitive_tokens(tokens: &[FilterToken]) -> Result<TasksFilter, String> {
    let words = tokens
        .iter()
        .map(|token| match token {
            FilterToken::Word(word) => Ok(word.clone()),
            FilterToken::LParen | FilterToken::RParen => {
                Err("parentheses must wrap full filter expressions".to_string())
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    let lower = words
        .iter()
        .map(|word| word.to_ascii_lowercase())
        .collect::<Vec<_>>();

    match lower.as_slice() {
        [done] if done == "done" => return Ok(TasksFilter::Done { value: true }),
        [not, done] if not == "not" && done == "done" => {
            return Ok(TasksFilter::Done { value: false });
        }
        [is, archived] if is == "is" && archived == "archived" => {
            return Ok(TasksFilter::Archived { value: true });
        }
        [is, not, archived] if is == "is" && not == "not" && archived == "archived" => {
            return Ok(TasksFilter::Archived { value: false });
        }
        [has, id] if has == "has" && id == "id" => return Ok(TasksFilter::HasId),
        [is, recurring] if is == "is" && recurring == "recurring" => {
            return Ok(TasksFilter::Recurring { value: true });
        }
        [is, not, recurring] if is == "is" && not == "not" && recurring == "recurring" => {
            return Ok(TasksFilter::Recurring { value: false });
        }
        [is, blocked] if is == "is" && blocked == "blocked" => {
            return Ok(TasksFilter::Blocked { value: true });
        }
        [is, not, blocked] if is == "is" && not == "not" && blocked == "blocked" => {
            return Ok(TasksFilter::Blocked { value: false });
        }
        _ => {}
    }

    if lower.len() >= 3 && lower[0] == "status" && lower[1] == "is" {
        return Ok(TasksFilter::StatusIs {
            value: join_value(&words[2..], "status is")?,
        });
    }
    if lower.len() >= 3 && lower[0] == "status.name" && lower[1] == "includes" {
        return Ok(TasksFilter::StatusNameIncludes {
            value: join_value(&words[2..], "status.name includes")?,
        });
    }
    if lower.len() >= 3 && lower[0] == "status.type" && lower[1] == "is" {
        return Ok(TasksFilter::StatusTypeIs {
            value: normalize_status_type(&join_value(&words[2..], "status.type is")?),
        });
    }
    if lower.len() >= 3 {
        if let Some(field) = parse_date_field(&lower[0]) {
            if let Some(relation) = parse_date_relation(&lower[1]) {
                return Ok(TasksFilter::Date {
                    field,
                    relation,
                    value: join_value(&words[2..], "date filter")?,
                });
            }
        }
    }
    if lower.len() == 3 && lower[0] == "has" && lower[2] == "date" {
        if let Some(field) = parse_date_field(&lower[1]) {
            return Ok(TasksFilter::HasDate { field, value: true });
        }
    }
    if lower.len() == 3 && lower[0] == "no" && lower[2] == "date" {
        if let Some(field) = parse_date_field(&lower[1]) {
            return Ok(TasksFilter::HasDate {
                field,
                value: false,
            });
        }
    }
    if lower.len() >= 3 && lower[1] == "includes" {
        if let Some(field) = parse_text_field(&lower[0]) {
            return Ok(TasksFilter::TextIncludes {
                field,
                value: join_value(&words[2..], "includes filter")?,
            });
        }
        if lower[0] == "tag" {
            return Ok(TasksFilter::TagIncludes {
                value: join_value(&words[2..], "tag includes")?,
            });
        }
        if lower[0] == "context" {
            return Ok(TasksFilter::ContextIncludes {
                value: join_value(&words[2..], "context includes")?,
            });
        }
        if lower[0] == "project" {
            return Ok(TasksFilter::ProjectIncludes {
                value: join_value(&words[2..], "project includes")?,
            });
        }
    }
    if lower.len() >= 3 && lower[0] == "priority" && lower[1] == "is" {
        return Ok(TasksFilter::PriorityIs {
            value: join_value(&words[2..], "priority is")?,
        });
    }
    if lower.len() >= 3 && lower[0] == "source" && lower[1] == "is" {
        return Ok(TasksFilter::SourceIs {
            value: join_value(&words[2..], "source is")?,
        });
    }

    Err(format!("unsupported tasks filter `{}`", words.join(" ")))
}

fn join_value(words: &[String], context: &str) -> Result<String, String> {
    let value = words.join(" ");
    let value = unquote(value.trim());
    if value.is_empty() {
        return Err(format!("{context} requires a value"));
    }
    Ok(value)
}

fn parse_date_field(value: &str) -> Option<TasksDateField> {
    match value {
        "due" => Some(TasksDateField::Due),
        "created" => Some(TasksDateField::Created),
        "start" => Some(TasksDateField::Start),
        "scheduled" => Some(TasksDateField::Scheduled),
        "done" => Some(TasksDateField::Done),
        _ => None,
    }
}

fn parse_date_relation(value: &str) -> Option<TasksDateRelation> {
    match value {
        "before" => Some(TasksDateRelation::Before),
        "after" => Some(TasksDateRelation::After),
        "on" => Some(TasksDateRelation::On),
        _ => None,
    }
}

fn parse_text_field(value: &str) -> Option<TasksTextField> {
    match value {
        "description" => Some(TasksTextField::Description),
        "path" => Some(TasksTextField::Path),
        "heading" => Some(TasksTextField::Heading),
        _ => None,
    }
}

fn normalize_status_type(value: &str) -> String {
    value.trim().to_ascii_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_status_filters() {
        let query = parse_tasks_query(
            "status is in-progress\n\
             not done\n\
             done\n\
             status.name includes \"Waiting review\"\n\
             status.type is in_progress\n",
        )
        .expect("query should parse");

        assert_eq!(
            query.commands,
            vec![
                TasksQueryCommand::Filter {
                    filter: TasksFilter::StatusIs {
                        value: "in-progress".to_string(),
                    },
                },
                TasksQueryCommand::Filter {
                    filter: TasksFilter::Done { value: false },
                },
                TasksQueryCommand::Filter {
                    filter: TasksFilter::Done { value: true },
                },
                TasksQueryCommand::Filter {
                    filter: TasksFilter::StatusNameIncludes {
                        value: "Waiting review".to_string(),
                    },
                },
                TasksQueryCommand::Filter {
                    filter: TasksFilter::StatusTypeIs {
                        value: "IN_PROGRESS".to_string(),
                    },
                },
            ]
        );
    }

    #[test]
    fn parses_date_and_property_filters() {
        let query = parse_tasks_query(
            "due before 2026-04-01\n\
             has due date\n\
             no scheduled date\n\
             description includes release notes\n\
             path includes Projects\n\
             heading includes \"Sprint Board\"\n\
             tag includes #work\n\
             context includes @desk\n\
             project includes [[Projects/Website]]\n\
             priority is high\n\
             source is file\n\
             is archived\n\
             is recurring\n\
             is not blocked\n\
             has id\n",
        )
        .expect("query should parse");

        assert_eq!(
            query.commands,
            vec![
                TasksQueryCommand::Filter {
                    filter: TasksFilter::Date {
                        field: TasksDateField::Due,
                        relation: TasksDateRelation::Before,
                        value: "2026-04-01".to_string(),
                    },
                },
                TasksQueryCommand::Filter {
                    filter: TasksFilter::HasDate {
                        field: TasksDateField::Due,
                        value: true,
                    },
                },
                TasksQueryCommand::Filter {
                    filter: TasksFilter::HasDate {
                        field: TasksDateField::Scheduled,
                        value: false,
                    },
                },
                TasksQueryCommand::Filter {
                    filter: TasksFilter::TextIncludes {
                        field: TasksTextField::Description,
                        value: "release notes".to_string(),
                    },
                },
                TasksQueryCommand::Filter {
                    filter: TasksFilter::TextIncludes {
                        field: TasksTextField::Path,
                        value: "Projects".to_string(),
                    },
                },
                TasksQueryCommand::Filter {
                    filter: TasksFilter::TextIncludes {
                        field: TasksTextField::Heading,
                        value: "Sprint Board".to_string(),
                    },
                },
                TasksQueryCommand::Filter {
                    filter: TasksFilter::TagIncludes {
                        value: "#work".to_string(),
                    },
                },
                TasksQueryCommand::Filter {
                    filter: TasksFilter::ContextIncludes {
                        value: "@desk".to_string(),
                    },
                },
                TasksQueryCommand::Filter {
                    filter: TasksFilter::ProjectIncludes {
                        value: "[[Projects/Website]]".to_string(),
                    },
                },
                TasksQueryCommand::Filter {
                    filter: TasksFilter::PriorityIs {
                        value: "high".to_string(),
                    },
                },
                TasksQueryCommand::Filter {
                    filter: TasksFilter::SourceIs {
                        value: "file".to_string(),
                    },
                },
                TasksQueryCommand::Filter {
                    filter: TasksFilter::Archived { value: true },
                },
                TasksQueryCommand::Filter {
                    filter: TasksFilter::Recurring { value: true },
                },
                TasksQueryCommand::Filter {
                    filter: TasksFilter::Blocked { value: false },
                },
                TasksQueryCommand::Filter {
                    filter: TasksFilter::HasId,
                },
            ]
        );
    }

    #[test]
    fn parses_boolean_composition() {
        let query = parse_tasks_query(
            "NOT (done OR status.type is cancelled)\n\
             (not done AND due after 2026-03-31)\n",
        )
        .expect("query should parse");

        assert_eq!(
            query.commands,
            vec![
                TasksQueryCommand::Filter {
                    filter: TasksFilter::Not {
                        filter: Box::new(TasksFilter::Or {
                            filters: vec![
                                TasksFilter::Done { value: true },
                                TasksFilter::StatusTypeIs {
                                    value: "CANCELLED".to_string(),
                                },
                            ],
                        }),
                    },
                },
                TasksQueryCommand::Filter {
                    filter: TasksFilter::And {
                        filters: vec![
                            TasksFilter::Done { value: false },
                            TasksFilter::Date {
                                field: TasksDateField::Due,
                                relation: TasksDateRelation::After,
                                value: "2026-03-31".to_string(),
                            },
                        ],
                    },
                },
            ]
        );
    }

    #[test]
    fn parses_sort_group_limit_and_display_commands() {
        let query = parse_tasks_query(
            "sort by due reverse\n\
             group by status.name\n\
             limit 10\n\
             limit groups 2\n\
             hide backlink\n\
             show urgency\n\
             short mode\n\
             explain\n",
        )
        .expect("query should parse");

        assert_eq!(
            query.commands,
            vec![
                TasksQueryCommand::Sort {
                    field: "due".to_string(),
                    reverse: true,
                },
                TasksQueryCommand::Group {
                    field: "status.name".to_string(),
                    reverse: false,
                },
                TasksQueryCommand::Limit { value: 10 },
                TasksQueryCommand::LimitGroups { value: 2 },
                TasksQueryCommand::Hide {
                    field: "backlink".to_string(),
                },
                TasksQueryCommand::Show {
                    field: "urgency".to_string(),
                },
                TasksQueryCommand::ShortMode,
                TasksQueryCommand::Explain,
            ]
        );
    }

    #[test]
    fn rejects_unknown_filters() {
        let error = parse_tasks_query("mystery filter").expect_err("query should fail");
        assert_eq!(error, "line 1: unsupported tasks filter `mystery filter`");
    }
}
