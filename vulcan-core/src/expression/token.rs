#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    Null,
    True,
    False,
    Number(f64),
    Str(String),
    Wikilink(String),
    DateLiteral(String),
    DurationLiteral(String),
    Regex(String, String), // pattern, flags
    Ident(String),

    // Operators
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    FatArrow,
    EqEq,
    Ne,
    Gt,
    Lt,
    Ge,
    Le,
    AndAnd,
    OrOr,
    Bang,

    // Punctuation
    Dot,
    Comma,
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Colon,

    Eof,
}

#[derive(Clone)]
pub struct Tokenizer<'a> {
    source: &'a str,
    bytes: &'a [u8],
    pos: usize,
    /// Tracks whether the last meaningful token could end an expression
    /// (used to disambiguate `/` as division vs regex).
    last_was_value: bool,
}

impl<'a> Tokenizer<'a> {
    #[must_use]
    pub fn new(source: &'a str) -> Self {
        Self {
            source,
            bytes: source.as_bytes(),
            pos: 0,
            last_was_value: false,
        }
    }

    #[allow(clippy::too_many_lines)]
    pub fn next_token(&mut self) -> Result<Token, String> {
        self.skip_whitespace();

        if self.pos >= self.bytes.len() {
            return Ok(Token::Eof);
        }

        let ch = self.bytes[self.pos];
        let token = match ch {
            b'!' if self.peek_next() == Some(b'[')
                && self.bytes.get(self.pos + 2) == Some(&b'[')
                && self.looks_like_wikilink_literal(self.pos) =>
            {
                let token = self.read_wikilink()?;
                self.last_was_value = true;
                token
            }
            b'(' => {
                self.pos += 1;
                self.last_was_value = false;
                Token::LParen
            }
            b')' => {
                self.pos += 1;
                self.last_was_value = true;
                Token::RParen
            }
            b'[' if self.peek_next() == Some(b'[')
                && self.looks_like_wikilink_literal(self.pos) =>
            {
                let token = self.read_wikilink()?;
                self.last_was_value = true;
                token
            }
            b'[' => {
                self.pos += 1;
                self.last_was_value = false;
                Token::LBracket
            }
            b']' => {
                self.pos += 1;
                self.last_was_value = true;
                Token::RBracket
            }
            b'{' => {
                self.pos += 1;
                self.last_was_value = false;
                Token::LBrace
            }
            b'}' => {
                self.pos += 1;
                self.last_was_value = true;
                Token::RBrace
            }
            b'.' => {
                self.pos += 1;
                self.last_was_value = false;
                Token::Dot
            }
            b',' => {
                self.pos += 1;
                self.last_was_value = false;
                Token::Comma
            }
            b':' => {
                self.pos += 1;
                self.last_was_value = false;
                Token::Colon
            }
            b'+' => {
                self.pos += 1;
                self.last_was_value = false;
                Token::Plus
            }
            b'-' => {
                self.pos += 1;
                self.last_was_value = false;
                Token::Minus
            }
            b'*' => {
                self.pos += 1;
                self.last_was_value = false;
                Token::Star
            }
            b'%' => {
                self.pos += 1;
                self.last_was_value = false;
                Token::Percent
            }
            b'=' if self.peek_next() == Some(b'>') => {
                self.pos += 2;
                self.last_was_value = false;
                Token::FatArrow
            }
            b'/' if !self.last_was_value => {
                self.pos += 1;
                return self.read_regex();
            }
            b'/' => {
                self.pos += 1;
                self.last_was_value = false;
                Token::Slash
            }
            b'=' if self.peek_next() == Some(b'=') => {
                self.pos += 2;
                self.last_was_value = false;
                Token::EqEq
            }
            b'!' if self.peek_next() == Some(b'=') => {
                self.pos += 2;
                self.last_was_value = false;
                Token::Ne
            }
            b'!' => {
                self.pos += 1;
                self.last_was_value = false;
                Token::Bang
            }
            b'>' if self.peek_next() == Some(b'=') => {
                self.pos += 2;
                self.last_was_value = false;
                Token::Ge
            }
            b'>' => {
                self.pos += 1;
                self.last_was_value = false;
                Token::Gt
            }
            b'<' if self.peek_next() == Some(b'=') => {
                self.pos += 2;
                self.last_was_value = false;
                Token::Le
            }
            b'<' => {
                self.pos += 1;
                self.last_was_value = false;
                Token::Lt
            }
            b'&' if self.peek_next() == Some(b'&') => {
                self.pos += 2;
                self.last_was_value = false;
                Token::AndAnd
            }
            b'|' if self.peek_next() == Some(b'|') => {
                self.pos += 2;
                self.last_was_value = false;
                Token::OrOr
            }
            b'"' | b'\'' => {
                let token = self.read_string(ch)?;
                self.last_was_value = true;
                token
            }
            b'0'..=b'9' => {
                let token = self
                    .read_date_literal()
                    .or_else(|| self.read_duration_literal())
                    .unwrap_or_else(|| self.read_number());
                self.last_was_value = true;
                token
            }
            _ if is_ident_start(ch) => {
                let token = self.read_identifier();
                self.last_was_value = matches!(
                    token,
                    Token::Ident(_) | Token::Null | Token::True | Token::False
                );
                token
            }
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

    fn read_string(&mut self, quote: u8) -> Result<Token, String> {
        self.pos += 1; // skip opening quote
        let mut s = String::new();
        while self.pos < self.bytes.len() {
            let ch = self.bytes[self.pos];
            if ch == quote {
                self.pos += 1;
                return Ok(Token::Str(s));
            }
            if ch == b'\\' && self.pos + 1 < self.bytes.len() {
                self.pos += 1;
                let escaped = self.bytes[self.pos];
                match escaped {
                    b'n' => s.push('\n'),
                    b't' => s.push('\t'),
                    b'r' => s.push('\r'),
                    b'\\' => s.push('\\'),
                    _ if escaped == quote => s.push(char::from(quote)),
                    _ => {
                        s.push('\\');
                        s.push(char::from(escaped));
                    }
                }
            } else {
                s.push(char::from(ch));
            }
            self.pos += 1;
        }
        Err("unterminated string literal".to_string())
    }

    fn read_wikilink(&mut self) -> Result<Token, String> {
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
                return Ok(Token::Wikilink(self.source[start..self.pos].to_string()));
            }
            self.pos += 1;
        }

        Err("unterminated wikilink".to_string())
    }

    fn read_number(&mut self) -> Token {
        let start = self.pos;
        while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_digit() {
            self.pos += 1;
        }
        if self.pos < self.bytes.len() && self.bytes[self.pos] == b'.' {
            // Check next char is a digit (not a method call like `3.abs()`)
            if self.pos + 1 < self.bytes.len() && self.bytes[self.pos + 1].is_ascii_digit() {
                self.pos += 1;
                while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_digit() {
                    self.pos += 1;
                }
            }
        }
        let text = &self.source[start..self.pos];
        Token::Number(text.parse().unwrap_or(0.0))
    }

    fn read_date_literal(&mut self) -> Option<Token> {
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
            .is_some_and(|ch| is_ident_continue(*ch) || matches!(ch, b'.' | b':'))
        {
            return None;
        }

        self.pos = start + end;
        Some(Token::DateLiteral(self.source[start..self.pos].to_string()))
    }

    fn read_duration_literal(&mut self) -> Option<Token> {
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
            if unit_start == scan {
                return None;
            }

            if !is_duration_unit(&self.source[unit_start..scan]) {
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
            .is_some_and(|ch| is_ident_continue(*ch) || matches!(ch, b'.' | b':'))
        {
            return None;
        }

        self.pos = end;
        Some(Token::DurationLiteral(
            self.source[start..self.pos].to_string(),
        ))
    }

    fn read_identifier(&mut self) -> Token {
        let start = self.pos;
        while self.pos < self.bytes.len() && is_ident_continue(self.bytes[self.pos]) {
            // Allow hyphens only between alphanumeric chars (not trailing)
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
        match text {
            "null" => Token::Null,
            "true" => Token::True,
            "false" => Token::False,
            _ => Token::Ident(text.to_string()),
        }
    }

    fn read_regex(&mut self) -> Result<Token, String> {
        // pos is already past the opening `/`
        let mut pattern = String::new();
        while self.pos < self.bytes.len() {
            let ch = self.bytes[self.pos];
            if ch == b'/' {
                self.pos += 1;
                // read flags
                let mut flags = String::new();
                while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_alphabetic() {
                    flags.push(char::from(self.bytes[self.pos]));
                    self.pos += 1;
                }
                self.last_was_value = true;
                return Ok(Token::Regex(pattern, flags));
            }
            if ch == b'\\' && self.pos + 1 < self.bytes.len() {
                pattern.push(char::from(ch));
                self.pos += 1;
                pattern.push(char::from(self.bytes[self.pos]));
            } else {
                pattern.push(char::from(ch));
            }
            self.pos += 1;
        }
        Err("unterminated regex literal".to_string())
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

    fn tokenize(input: &str) -> Vec<Token> {
        let mut t = Tokenizer::new(input);
        let mut tokens = Vec::new();
        loop {
            let tok = t.next_token().unwrap();
            if tok == Token::Eof {
                break;
            }
            tokens.push(tok);
        }
        tokens
    }

    #[test]
    fn simple_arithmetic() {
        assert_eq!(
            tokenize("1 + 2 * 3"),
            vec![
                Token::Number(1.0),
                Token::Plus,
                Token::Number(2.0),
                Token::Star,
                Token::Number(3.0),
            ]
        );
    }

    #[test]
    fn modulo_operator() {
        assert_eq!(
            tokenize("7 % 4"),
            vec![Token::Number(7.0), Token::Percent, Token::Number(4.0),]
        );
    }

    #[test]
    fn string_literals() {
        assert_eq!(
            tokenize(r#""hello" + 'world'"#),
            vec![
                Token::Str("hello".to_string()),
                Token::Plus,
                Token::Str("world".to_string()),
            ]
        );
    }

    #[test]
    fn identifiers_with_hyphens() {
        assert_eq!(
            tokenize("my-property"),
            vec![Token::Ident("my-property".to_string())]
        );
    }

    #[test]
    fn comparison_operators() {
        assert_eq!(
            tokenize("a == b != c >= d <= e > f < g"),
            vec![
                Token::Ident("a".to_string()),
                Token::EqEq,
                Token::Ident("b".to_string()),
                Token::Ne,
                Token::Ident("c".to_string()),
                Token::Ge,
                Token::Ident("d".to_string()),
                Token::Le,
                Token::Ident("e".to_string()),
                Token::Gt,
                Token::Ident("f".to_string()),
                Token::Lt,
                Token::Ident("g".to_string()),
            ]
        );
    }

    #[test]
    fn boolean_operators() {
        assert_eq!(
            tokenize("!a && b || c"),
            vec![
                Token::Bang,
                Token::Ident("a".to_string()),
                Token::AndAnd,
                Token::Ident("b".to_string()),
                Token::OrOr,
                Token::Ident("c".to_string()),
            ]
        );
    }

    #[test]
    fn keywords() {
        assert_eq!(
            tokenize("null true false"),
            vec![Token::Null, Token::True, Token::False]
        );
    }

    #[test]
    fn method_call() {
        assert_eq!(
            tokenize("file.name.lower()"),
            vec![
                Token::Ident("file".to_string()),
                Token::Dot,
                Token::Ident("name".to_string()),
                Token::Dot,
                Token::Ident("lower".to_string()),
                Token::LParen,
                Token::RParen,
            ]
        );
    }

    #[test]
    fn regex_literal() {
        assert_eq!(
            tokenize("/abc/g"),
            vec![Token::Regex("abc".to_string(), "g".to_string())]
        );
    }

    #[test]
    fn division_not_regex() {
        assert_eq!(
            tokenize("a / b"),
            vec![
                Token::Ident("a".to_string()),
                Token::Slash,
                Token::Ident("b".to_string()),
            ]
        );
    }

    #[test]
    #[allow(clippy::approx_constant)]
    fn decimal_number() {
        assert_eq!(tokenize("3.14"), vec![Token::Number(3.14)]);
    }

    #[test]
    fn date_literals() {
        assert_eq!(
            tokenize("date(2026-04-18)"),
            vec![
                Token::Ident("date".to_string()),
                Token::LParen,
                Token::DateLiteral("2026-04-18".to_string()),
                Token::RParen,
            ]
        );
        assert_eq!(
            tokenize("date(2026-04)"),
            vec![
                Token::Ident("date".to_string()),
                Token::LParen,
                Token::DateLiteral("2026-04".to_string()),
                Token::RParen,
            ]
        );
    }

    #[test]
    fn duration_literals() {
        assert_eq!(
            tokenize("dur(1d 3h 20m)"),
            vec![
                Token::Ident("dur".to_string()),
                Token::LParen,
                Token::DurationLiteral("1d 3h 20m".to_string()),
                Token::RParen,
            ]
        );
        assert_eq!(
            tokenize("dur(3 hours, 20 minutes)"),
            vec![
                Token::Ident("dur".to_string()),
                Token::LParen,
                Token::DurationLiteral("3 hours, 20 minutes".to_string()),
                Token::RParen,
            ]
        );
    }

    #[test]
    fn wikilink_literals() {
        assert_eq!(
            tokenize("[[alice]].role"),
            vec![
                Token::Wikilink("[[alice]]".to_string()),
                Token::Dot,
                Token::Ident("role".to_string()),
            ]
        );
        assert_eq!(
            tokenize("meta(![[alice#^bio|Bio]])"),
            vec![
                Token::Ident("meta".to_string()),
                Token::LParen,
                Token::Wikilink("![[alice#^bio|Bio]]".to_string()),
                Token::RParen,
            ]
        );
    }

    #[test]
    fn nested_arrays_are_not_wikilinks() {
        assert_eq!(
            tokenize("[[1, 2], [3]]"),
            vec![
                Token::LBracket,
                Token::LBracket,
                Token::Number(1.0),
                Token::Comma,
                Token::Number(2.0),
                Token::RBracket,
                Token::Comma,
                Token::LBracket,
                Token::Number(3.0),
                Token::RBracket,
                Token::RBracket,
            ]
        );
    }

    #[test]
    fn number_method_call() {
        // `3.abs()` should tokenize as number 3, dot, abs, parens
        // (not as 3.0 followed by ident)
        assert_eq!(
            tokenize("3.abs()"),
            vec![
                Token::Number(3.0),
                Token::Dot,
                Token::Ident("abs".to_string()),
                Token::LParen,
                Token::RParen,
            ]
        );
    }

    #[test]
    fn lambda_tokens() {
        assert_eq!(
            tokenize("(x, y) => x + y"),
            vec![
                Token::LParen,
                Token::Ident("x".to_string()),
                Token::Comma,
                Token::Ident("y".to_string()),
                Token::RParen,
                Token::FatArrow,
                Token::Ident("x".to_string()),
                Token::Plus,
                Token::Ident("y".to_string()),
            ]
        );
    }
}
