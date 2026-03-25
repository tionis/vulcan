#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    Null,
    True,
    False,
    Number(f64),
    Str(String),
    Regex(String, String), // pattern, flags
    Ident(String),

    // Operators
    Plus,
    Minus,
    Star,
    Slash,
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
                let token = self.read_number();
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
}
