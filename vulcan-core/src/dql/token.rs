#[derive(Debug, Clone, PartialEq)]
pub enum DqlToken {
    Table,
    List,
    Task,
    Calendar,
    From,
    Where,
    Sort,
    Group,
    By,
    Flatten,
    Limit,
    Asc,
    Desc,
    Ascending,
    Descending,
    And,
    Or,
    Not,
    Without,
    Id,
    As,
    Null,
    True,
    False,
    Number(f64),
    Str(String),
    Ident(String),
    Tag(String),
    Wikilink(String),
    DateLiteral(String),
    DurationLiteral(String),
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    FatArrow,
    Eq,
    EqEq,
    Ne,
    Gt,
    Lt,
    Ge,
    Le,
    Bang,
    Dot,
    Comma,
    LParen,
    RParen,
    LBracket,
    RBracket,
    Eof,
}

#[derive(Clone)]
pub struct DqlTokenizer<'a> {
    source: &'a str,
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> DqlTokenizer<'a> {
    #[must_use]
    pub fn new(source: &'a str) -> Self {
        Self {
            source,
            bytes: source.as_bytes(),
            pos: 0,
        }
    }

    #[allow(clippy::too_many_lines)]
    pub fn next_token(&mut self) -> Result<DqlToken, String> {
        self.skip_whitespace();

        if self.pos >= self.bytes.len() {
            return Ok(DqlToken::Eof);
        }

        let ch = self.bytes[self.pos];
        let token = match ch {
            b'!' if self.peek_next() == Some(b'[')
                && self.bytes.get(self.pos + 2) == Some(&b'[')
                && self.looks_like_wikilink_literal(self.pos) =>
            {
                self.read_wikilink()?
            }
            b'(' => {
                self.pos += 1;
                DqlToken::LParen
            }
            b')' => {
                self.pos += 1;
                DqlToken::RParen
            }
            b'[' if self.peek_next() == Some(b'[')
                && self.looks_like_wikilink_literal(self.pos) =>
            {
                self.read_wikilink()?
            }
            b'[' => {
                self.pos += 1;
                DqlToken::LBracket
            }
            b']' => {
                self.pos += 1;
                DqlToken::RBracket
            }
            b'.' => {
                self.pos += 1;
                DqlToken::Dot
            }
            b',' => {
                self.pos += 1;
                DqlToken::Comma
            }
            b'+' => {
                self.pos += 1;
                DqlToken::Plus
            }
            b'-' => {
                self.pos += 1;
                DqlToken::Minus
            }
            b'*' => {
                self.pos += 1;
                DqlToken::Star
            }
            b'/' => {
                self.pos += 1;
                DqlToken::Slash
            }
            b'%' => {
                self.pos += 1;
                DqlToken::Percent
            }
            b'=' if self.peek_next() == Some(b'>') => {
                self.pos += 2;
                DqlToken::FatArrow
            }
            b'=' if self.peek_next() == Some(b'=') => {
                self.pos += 2;
                DqlToken::EqEq
            }
            b'=' => {
                self.pos += 1;
                DqlToken::Eq
            }
            b'!' if self.peek_next() == Some(b'=') => {
                self.pos += 2;
                DqlToken::Ne
            }
            b'!' => {
                self.pos += 1;
                DqlToken::Bang
            }
            b'>' if self.peek_next() == Some(b'=') => {
                self.pos += 2;
                DqlToken::Ge
            }
            b'>' => {
                self.pos += 1;
                DqlToken::Gt
            }
            b'<' if self.peek_next() == Some(b'=') => {
                self.pos += 2;
                DqlToken::Le
            }
            b'<' => {
                self.pos += 1;
                DqlToken::Lt
            }
            b'"' | b'\'' => self.read_string(ch)?,
            b'#' => self.read_tag()?,
            b'0'..=b'9' => self
                .read_date_literal()
                .or_else(|| self.read_duration_literal())
                .unwrap_or_else(|| self.read_number()),
            _ if is_ident_start(ch) => self.read_identifier(),
            _ => {
                return Err(format!(
                    "unexpected character '{}' at position {}",
                    char::from(ch),
                    self.pos
                ));
            }
        };

        Ok(token)
    }

    fn skip_whitespace(&mut self) {
        while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
    }

    fn peek_next(&self) -> Option<u8> {
        self.bytes.get(self.pos + 1).copied()
    }

    fn looks_like_wikilink_literal(&self, start: usize) -> bool {
        let mut pos = start;
        if self.bytes.get(pos) == Some(&b'!') {
            pos += 1;
        }
        if self.bytes.get(pos) != Some(&b'[') || self.bytes.get(pos + 1) != Some(&b'[') {
            return false;
        }

        pos += 2;
        while pos + 1 < self.bytes.len() {
            if self.bytes[pos] == b']' {
                return self.bytes[pos + 1] == b']';
            }
            pos += 1;
        }
        false
    }

    fn read_string(&mut self, quote: u8) -> Result<DqlToken, String> {
        self.pos += 1;
        let mut value = String::new();
        while self.pos < self.bytes.len() {
            let ch = self.bytes[self.pos];
            if ch == quote {
                self.pos += 1;
                return Ok(DqlToken::Str(value));
            }
            if ch == b'\\' && self.pos + 1 < self.bytes.len() {
                self.pos += 1;
                let escaped = self.bytes[self.pos];
                match escaped {
                    b'n' => value.push('\n'),
                    b't' => value.push('\t'),
                    b'r' => value.push('\r'),
                    b'\\' => value.push('\\'),
                    _ if escaped == quote => value.push(char::from(quote)),
                    _ => {
                        value.push('\\');
                        value.push(char::from(escaped));
                    }
                }
            } else {
                value.push(char::from(ch));
            }
            self.pos += 1;
        }
        Err("unterminated string literal".to_string())
    }

    fn read_tag(&mut self) -> Result<DqlToken, String> {
        let start = self.pos;
        self.pos += 1;
        while self.pos < self.bytes.len()
            && matches!(
                self.bytes[self.pos],
                b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-' | b'/'
            )
        {
            self.pos += 1;
        }

        if self.pos == start + 1 {
            return Err(format!("invalid tag at position {start}"));
        }

        Ok(DqlToken::Tag(self.source[start..self.pos].to_string()))
    }

    fn read_wikilink(&mut self) -> Result<DqlToken, String> {
        let start = self.pos;
        if self.bytes[self.pos] == b'!' {
            self.pos += 1;
        }

        if self.bytes.get(self.pos) != Some(&b'[') || self.bytes.get(self.pos + 1) != Some(&b'[') {
            return Err(format!("invalid wikilink at position {start}"));
        }

        self.pos += 2;
        while self.pos + 1 < self.bytes.len() {
            if self.bytes[self.pos] == b']' && self.bytes[self.pos + 1] == b']' {
                self.pos += 2;
                return Ok(DqlToken::Wikilink(self.source[start..self.pos].to_string()));
            }
            self.pos += 1;
        }

        Err("unterminated wikilink".to_string())
    }

    fn read_number(&mut self) -> DqlToken {
        let start = self.pos;
        while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_digit() {
            self.pos += 1;
        }
        if self.pos < self.bytes.len()
            && self.bytes[self.pos] == b'.'
            && self.pos + 1 < self.bytes.len()
            && self.bytes[self.pos + 1].is_ascii_digit()
        {
            self.pos += 1;
            while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_digit() {
                self.pos += 1;
            }
        }

        DqlToken::Number(self.source[start..self.pos].parse().unwrap_or(0.0))
    }

    fn read_date_literal(&mut self) -> Option<DqlToken> {
        let start = self.pos;
        let bytes = &self.bytes[start..];

        if bytes.len() < 7
            || !bytes[0..4].iter().all(u8::is_ascii_digit)
            || bytes[4] != b'-'
            || !bytes[5].is_ascii_digit()
            || !bytes[6].is_ascii_digit()
        {
            return None;
        }

        let mut end = 7;
        if bytes.len() >= 10
            && bytes[7] == b'-'
            && bytes[8].is_ascii_digit()
            && bytes[9].is_ascii_digit()
        {
            end = 10;
        }

        if matches!(bytes.get(end), Some(b'T')) {
            let mut time_end = end + 1;
            let mut saw_digit = false;
            while time_end < bytes.len()
                && (bytes[time_end].is_ascii_digit()
                    || matches!(bytes[time_end], b':' | b'.' | b'Z' | b'+' | b'-'))
            {
                saw_digit |= bytes[time_end].is_ascii_digit();
                time_end += 1;
            }
            if saw_digit {
                end = time_end;
            }
        }

        if bytes
            .get(end)
            .is_some_and(|ch| is_ident_continue(*ch) || matches!(ch, b'.' | b':' | b'/'))
        {
            return None;
        }

        self.pos = start + end;
        Some(DqlToken::DateLiteral(
            self.source[start..self.pos].to_string(),
        ))
    }

    fn read_duration_literal(&mut self) -> Option<DqlToken> {
        let start = self.pos;
        let mut scan = start;
        let end = loop {
            let number_start = scan;
            while scan < self.bytes.len()
                && (self.bytes[scan].is_ascii_digit() || self.bytes[scan] == b'.')
            {
                scan += 1;
            }
            if number_start == scan {
                return None;
            }

            while scan < self.bytes.len() && self.bytes[scan].is_ascii_whitespace() {
                scan += 1;
            }

            let unit_start = scan;
            while scan < self.bytes.len() && self.bytes[scan].is_ascii_alphabetic() {
                scan += 1;
            }
            if unit_start == scan || !is_duration_unit(&self.source[unit_start..scan]) {
                return None;
            }
            let unit_end = scan;

            let mut separator_scan = scan;
            while separator_scan < self.bytes.len()
                && self.bytes[separator_scan].is_ascii_whitespace()
            {
                separator_scan += 1;
            }
            if separator_scan < self.bytes.len() && self.bytes[separator_scan] == b',' {
                separator_scan += 1;
                while separator_scan < self.bytes.len()
                    && self.bytes[separator_scan].is_ascii_whitespace()
                {
                    separator_scan += 1;
                }
            }

            if separator_scan < self.bytes.len() && self.bytes[separator_scan].is_ascii_digit() {
                scan = separator_scan;
                continue;
            }

            break unit_end;
        };

        if self
            .bytes
            .get(end)
            .is_some_and(|ch| is_ident_continue(*ch) || matches!(ch, b'.' | b':' | b'/'))
        {
            return None;
        }

        self.pos = end;
        Some(DqlToken::DurationLiteral(
            self.source[start..self.pos].to_string(),
        ))
    }

    fn read_identifier(&mut self) -> DqlToken {
        let start = self.pos;
        while self.pos < self.bytes.len() && is_ident_continue(self.bytes[self.pos]) {
            if self.bytes[self.pos] == b'-' {
                if self.pos + 1 < self.bytes.len()
                    && is_ident_continue(self.bytes[self.pos + 1])
                    && self.bytes[self.pos + 1] != b'-'
                {
                    self.pos += 1;
                } else {
                    break;
                }
            } else {
                self.pos += 1;
            }
        }

        let text = &self.source[start..self.pos];
        match text.to_ascii_uppercase().as_str() {
            "TABLE" => DqlToken::Table,
            "LIST" => DqlToken::List,
            "TASK" => DqlToken::Task,
            "CALENDAR" => DqlToken::Calendar,
            "FROM" => DqlToken::From,
            "WHERE" => DqlToken::Where,
            "SORT" => DqlToken::Sort,
            "GROUP" => DqlToken::Group,
            "BY" => DqlToken::By,
            "FLATTEN" => DqlToken::Flatten,
            "LIMIT" => DqlToken::Limit,
            "ASC" => DqlToken::Asc,
            "DESC" => DqlToken::Desc,
            "ASCENDING" => DqlToken::Ascending,
            "DESCENDING" => DqlToken::Descending,
            "AND" => DqlToken::And,
            "OR" => DqlToken::Or,
            "NOT" => DqlToken::Not,
            "WITHOUT" => DqlToken::Without,
            "ID" => DqlToken::Id,
            "AS" => DqlToken::As,
            "NULL" => DqlToken::Null,
            "TRUE" => DqlToken::True,
            "FALSE" => DqlToken::False,
            _ => DqlToken::Ident(text.to_string()),
        }
    }
}

fn is_ident_start(ch: u8) -> bool {
    ch.is_ascii_alphabetic() || ch == b'_'
}

fn is_ident_continue(ch: u8) -> bool {
    ch.is_ascii_alphanumeric() || ch == b'_' || ch == b'-'
}

fn is_duration_unit(unit: &str) -> bool {
    matches!(
        unit.to_ascii_lowercase().as_str(),
        "ms" | "msec"
            | "msecs"
            | "millisecond"
            | "milliseconds"
            | "s"
            | "sec"
            | "secs"
            | "second"
            | "seconds"
            | "m"
            | "min"
            | "mins"
            | "minute"
            | "minutes"
            | "h"
            | "hr"
            | "hrs"
            | "hour"
            | "hours"
            | "d"
            | "day"
            | "days"
            | "w"
            | "wk"
            | "wks"
            | "week"
            | "weeks"
            | "mo"
            | "mos"
            | "month"
            | "months"
            | "y"
            | "yr"
            | "yrs"
            | "year"
            | "years"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokenize(input: &str) -> Vec<DqlToken> {
        let mut tokenizer = DqlTokenizer::new(input);
        let mut tokens = Vec::new();
        loop {
            let token = tokenizer.next_token().unwrap();
            if token == DqlToken::Eof {
                break;
            }
            tokens.push(token);
        }
        tokens
    }

    #[test]
    fn tokenizes_query_keywords_case_insensitively() {
        assert_eq!(
            tokenize("table without id from #project sort file.name ascending limit 10"),
            vec![
                DqlToken::Table,
                DqlToken::Without,
                DqlToken::Id,
                DqlToken::From,
                DqlToken::Tag("#project".to_string()),
                DqlToken::Sort,
                DqlToken::Ident("file".to_string()),
                DqlToken::Dot,
                DqlToken::Ident("name".to_string()),
                DqlToken::Ascending,
                DqlToken::Limit,
                DqlToken::Number(10.0),
            ]
        );
    }

    #[test]
    fn tokenizes_group_by_flatten_and_aliases() {
        assert_eq!(
            tokenize("GROUP BY (status) AS \"State\" FLATTEN file.tasks AS task"),
            vec![
                DqlToken::Group,
                DqlToken::By,
                DqlToken::LParen,
                DqlToken::Ident("status".to_string()),
                DqlToken::RParen,
                DqlToken::As,
                DqlToken::Str("State".to_string()),
                DqlToken::Flatten,
                DqlToken::Ident("file".to_string()),
                DqlToken::Dot,
                DqlToken::Ident("tasks".to_string()),
                DqlToken::As,
                DqlToken::Task,
            ]
        );
    }

    #[test]
    fn tokenizes_sources_with_tags_links_and_functions() {
        assert_eq!(
            tokenize("FROM (#project/sub OR [[]]) AND outgoing([[Alpha]])"),
            vec![
                DqlToken::From,
                DqlToken::LParen,
                DqlToken::Tag("#project/sub".to_string()),
                DqlToken::Or,
                DqlToken::Wikilink("[[]]".to_string()),
                DqlToken::RParen,
                DqlToken::And,
                DqlToken::Ident("outgoing".to_string()),
                DqlToken::LParen,
                DqlToken::Wikilink("[[Alpha]]".to_string()),
                DqlToken::RParen,
            ]
        );
    }

    #[test]
    fn tokenizes_where_expressions_and_literals() {
        assert_eq!(
            tokenize("WHERE due <= date(2026-04-18) AND effort > dur(1d 3h)"),
            vec![
                DqlToken::Where,
                DqlToken::Ident("due".to_string()),
                DqlToken::Le,
                DqlToken::Ident("date".to_string()),
                DqlToken::LParen,
                DqlToken::DateLiteral("2026-04-18".to_string()),
                DqlToken::RParen,
                DqlToken::And,
                DqlToken::Ident("effort".to_string()),
                DqlToken::Gt,
                DqlToken::Ident("dur".to_string()),
                DqlToken::LParen,
                DqlToken::DurationLiteral("1d 3h".to_string()),
                DqlToken::RParen,
            ]
        );
    }

    #[test]
    fn tokenizes_lambda_expressions_in_where_clauses() {
        assert_eq!(
            tokenize("WHERE length(filter(file.tasks, (task) => task.completed)) > 0"),
            vec![
                DqlToken::Where,
                DqlToken::Ident("length".to_string()),
                DqlToken::LParen,
                DqlToken::Ident("filter".to_string()),
                DqlToken::LParen,
                DqlToken::Ident("file".to_string()),
                DqlToken::Dot,
                DqlToken::Ident("tasks".to_string()),
                DqlToken::Comma,
                DqlToken::LParen,
                DqlToken::Task,
                DqlToken::RParen,
                DqlToken::FatArrow,
                DqlToken::Task,
                DqlToken::Dot,
                DqlToken::Ident("completed".to_string()),
                DqlToken::RParen,
                DqlToken::RParen,
                DqlToken::Gt,
                DqlToken::Number(0.0),
            ]
        );
    }

    #[test]
    fn tokenizes_strings_numbers_and_operators() {
        assert_eq!(
            tokenize(r#"WHERE score != 3.5 AND name = "Alpha" OR !done"#),
            vec![
                DqlToken::Where,
                DqlToken::Ident("score".to_string()),
                DqlToken::Ne,
                DqlToken::Number(3.5),
                DqlToken::And,
                DqlToken::Ident("name".to_string()),
                DqlToken::Eq,
                DqlToken::Str("Alpha".to_string()),
                DqlToken::Or,
                DqlToken::Bang,
                DqlToken::Ident("done".to_string()),
            ]
        );
    }

    #[test]
    fn tokenizes_embedded_wikilinks_and_special_identifiers() {
        assert_eq!(
            tokenize("LIST meta(![[alpha#^bio|Bio]]).path, file.name"),
            vec![
                DqlToken::List,
                DqlToken::Ident("meta".to_string()),
                DqlToken::LParen,
                DqlToken::Wikilink("![[alpha#^bio|Bio]]".to_string()),
                DqlToken::RParen,
                DqlToken::Dot,
                DqlToken::Ident("path".to_string()),
                DqlToken::Comma,
                DqlToken::Ident("file".to_string()),
                DqlToken::Dot,
                DqlToken::Ident("name".to_string()),
            ]
        );
    }
}
