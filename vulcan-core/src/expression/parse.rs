use crate::expression::ast::{BinOp, Expr, UnOp};
use crate::expression::token::{Token, Tokenizer};

pub struct Parser<'a> {
    tokenizer: Tokenizer<'a>,
    current: Token,
}

impl<'a> Parser<'a> {
    pub fn new(source: &'a str) -> Result<Self, String> {
        let mut tokenizer = Tokenizer::new(source);
        let current = tokenizer.next_token()?;
        Ok(Self { tokenizer, current })
    }

    pub fn parse(mut self) -> Result<Expr, String> {
        let expr = self.parse_or()?;
        if self.current != Token::Eof {
            return Err(format!(
                "unexpected token {:?} after expression",
                self.current
            ));
        }
        Ok(expr)
    }

    fn advance(&mut self) -> Result<Token, String> {
        let prev = std::mem::replace(&mut self.current, Token::Eof);
        self.current = self.tokenizer.next_token()?;
        Ok(prev)
    }

    fn expect(&mut self, expected: &Token) -> Result<(), String> {
        if &self.current == expected {
            self.advance()?;
            Ok(())
        } else {
            Err(format!("expected {:?}, got {:?}", expected, self.current))
        }
    }

    // ── Precedence levels (lowest to highest) ────────────────────────

    fn parse_or(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_and()?;
        while self.current == Token::OrOr {
            self.advance()?;
            let right = self.parse_and()?;
            left = Expr::BinaryOp(Box::new(left), BinOp::Or, Box::new(right));
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_equality()?;
        while self.current == Token::AndAnd {
            self.advance()?;
            let right = self.parse_equality()?;
            left = Expr::BinaryOp(Box::new(left), BinOp::And, Box::new(right));
        }
        Ok(left)
    }

    fn parse_equality(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_comparison()?;
        loop {
            let op = match self.current {
                Token::EqEq => BinOp::Eq,
                Token::Ne => BinOp::Ne,
                _ => break,
            };
            self.advance()?;
            let right = self.parse_comparison()?;
            left = Expr::BinaryOp(Box::new(left), op, Box::new(right));
        }
        Ok(left)
    }

    fn parse_comparison(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_additive()?;
        loop {
            let op = match self.current {
                Token::Gt => BinOp::Gt,
                Token::Lt => BinOp::Lt,
                Token::Ge => BinOp::Ge,
                Token::Le => BinOp::Le,
                _ => break,
            };
            self.advance()?;
            let right = self.parse_additive()?;
            left = Expr::BinaryOp(Box::new(left), op, Box::new(right));
        }
        Ok(left)
    }

    fn parse_additive(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_multiplicative()?;
        loop {
            let op = match self.current {
                Token::Plus => BinOp::Add,
                Token::Minus => BinOp::Sub,
                _ => break,
            };
            self.advance()?;
            let right = self.parse_multiplicative()?;
            left = Expr::BinaryOp(Box::new(left), op, Box::new(right));
        }
        Ok(left)
    }

    fn parse_multiplicative(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_unary()?;
        loop {
            let op = match self.current {
                Token::Star => BinOp::Mul,
                Token::Slash => BinOp::Div,
                _ => break,
            };
            self.advance()?;
            let right = self.parse_unary()?;
            left = Expr::BinaryOp(Box::new(left), op, Box::new(right));
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Expr, String> {
        match self.current {
            Token::Bang => {
                self.advance()?;
                let expr = self.parse_unary()?;
                Ok(Expr::UnaryOp(UnOp::Not, Box::new(expr)))
            }
            Token::Minus => {
                self.advance()?;
                let expr = self.parse_unary()?;
                Ok(Expr::UnaryOp(UnOp::Neg, Box::new(expr)))
            }
            _ => self.parse_postfix(),
        }
    }

    fn parse_postfix(&mut self) -> Result<Expr, String> {
        let mut expr = self.parse_primary()?;

        loop {
            if self.current != Token::Dot {
                break;
            }
            self.advance()?; // consume `.`

            let Token::Ident(name) = self.advance()? else {
                return Err("expected identifier after '.'".to_string());
            };

            if self.current == Token::LParen {
                // Method call: expr.method(args...)
                self.advance()?; // consume `(`
                let args = self.parse_args()?;
                self.expect(&Token::RParen)?;
                expr = Expr::MethodCall(Box::new(expr), name, args);
            } else {
                // Field access: expr.field
                expr = Expr::FieldAccess(Box::new(expr), name);
            }
        }

        Ok(expr)
    }

    fn parse_primary(&mut self) -> Result<Expr, String> {
        match &self.current {
            Token::Null => {
                self.advance()?;
                Ok(Expr::Null)
            }
            Token::True => {
                self.advance()?;
                Ok(Expr::Bool(true))
            }
            Token::False => {
                self.advance()?;
                Ok(Expr::Bool(false))
            }
            Token::Number(n) => {
                let n = *n;
                self.advance()?;
                Ok(Expr::Number(n))
            }
            Token::Str(s) => {
                let s = s.clone();
                self.advance()?;
                Ok(Expr::Str(s))
            }
            Token::DateLiteral(s) | Token::DurationLiteral(s) => {
                let s = s.clone();
                self.advance()?;
                Ok(Expr::Str(s))
            }
            Token::Regex(pattern, flags) => {
                let pattern = pattern.clone();
                let flags = flags.clone();
                self.advance()?;
                Ok(Expr::Regex { pattern, flags })
            }
            Token::Ident(name) => {
                let name = name.clone();
                self.advance()?;

                // Check for `formula.X`
                if name == "formula" && self.current == Token::Dot {
                    self.advance()?; // consume `.`
                    let Token::Ident(formula_name) = self.advance()? else {
                        return Err("expected formula name after 'formula.'".to_string());
                    };
                    return Ok(Expr::FormulaRef(formula_name));
                }

                if self.current == Token::LParen {
                    // Function call: name(args...)
                    self.advance()?; // consume `(`
                    let args = self.parse_args()?;
                    self.expect(&Token::RParen)?;
                    Ok(Expr::FunctionCall(name, args))
                } else {
                    Ok(Expr::Identifier(name))
                }
            }
            Token::LParen => {
                self.advance()?;
                let expr = self.parse_or()?;
                self.expect(&Token::RParen)?;
                Ok(expr)
            }
            Token::LBracket => {
                self.advance()?;
                let elements = self.parse_args()?;
                self.expect(&Token::RBracket)?;
                Ok(Expr::Array(elements))
            }
            Token::LBrace => {
                self.advance()?;
                let mut entries = Vec::new();
                if self.current != Token::RBrace {
                    loop {
                        let key = match self.advance()? {
                            Token::Str(s) | Token::Ident(s) => s,
                            other => return Err(format!("expected object key, got {other:?}")),
                        };
                        self.expect(&Token::Colon)?;
                        let value = self.parse_or()?;
                        entries.push((key, value));
                        if self.current != Token::Comma {
                            break;
                        }
                        self.advance()?;
                    }
                }
                self.expect(&Token::RBrace)?;
                Ok(Expr::Object(entries))
            }
            other => Err(format!("unexpected token {other:?}")),
        }
    }

    fn parse_args(&mut self) -> Result<Vec<Expr>, String> {
        let mut args = Vec::new();
        if self.current == Token::RParen || self.current == Token::RBracket {
            return Ok(args);
        }
        loop {
            args.push(self.parse_or()?);
            if self.current != Token::Comma {
                break;
            }
            self.advance()?;
        }
        Ok(args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(input: &str) -> Expr {
        Parser::new(input).unwrap().parse().unwrap()
    }

    #[test]
    fn parse_number() {
        assert_eq!(parse("42"), Expr::Number(42.0));
    }

    #[test]
    fn parse_string() {
        assert_eq!(parse(r#""hello""#), Expr::Str("hello".to_string()));
    }

    #[test]
    fn parse_null_true_false() {
        assert_eq!(parse("null"), Expr::Null);
        assert_eq!(parse("true"), Expr::Bool(true));
        assert_eq!(parse("false"), Expr::Bool(false));
    }

    #[test]
    fn parse_identifier() {
        assert_eq!(parse("status"), Expr::Identifier("status".to_string()));
    }

    #[test]
    fn parse_hyphenated_property() {
        assert_eq!(
            parse("my-property"),
            Expr::Identifier("my-property".to_string())
        );
    }

    #[test]
    fn parse_arithmetic_precedence() {
        // 1 + 2 * 3 should parse as 1 + (2 * 3)
        assert_eq!(
            parse("1 + 2 * 3"),
            Expr::BinaryOp(
                Box::new(Expr::Number(1.0)),
                BinOp::Add,
                Box::new(Expr::BinaryOp(
                    Box::new(Expr::Number(2.0)),
                    BinOp::Mul,
                    Box::new(Expr::Number(3.0)),
                ))
            )
        );
    }

    #[test]
    fn parse_boolean_precedence() {
        // a || b && c should parse as a || (b && c)
        assert_eq!(
            parse("a || b && c"),
            Expr::BinaryOp(
                Box::new(Expr::Identifier("a".to_string())),
                BinOp::Or,
                Box::new(Expr::BinaryOp(
                    Box::new(Expr::Identifier("b".to_string())),
                    BinOp::And,
                    Box::new(Expr::Identifier("c".to_string())),
                ))
            )
        );
    }

    #[test]
    fn parse_comparison() {
        assert_eq!(
            parse("a > 5"),
            Expr::BinaryOp(
                Box::new(Expr::Identifier("a".to_string())),
                BinOp::Gt,
                Box::new(Expr::Number(5.0)),
            )
        );
    }

    #[test]
    fn parse_not_equals() {
        assert_eq!(
            parse(r#"status != "Done""#),
            Expr::BinaryOp(
                Box::new(Expr::Identifier("status".to_string())),
                BinOp::Ne,
                Box::new(Expr::Str("Done".to_string())),
            )
        );
    }

    #[test]
    fn parse_unary() {
        assert_eq!(
            parse("!completed"),
            Expr::UnaryOp(
                UnOp::Not,
                Box::new(Expr::Identifier("completed".to_string()))
            )
        );
        assert_eq!(
            parse("-5"),
            Expr::UnaryOp(UnOp::Neg, Box::new(Expr::Number(5.0)))
        );
    }

    #[test]
    fn parse_field_access() {
        assert_eq!(
            parse("file.name"),
            Expr::FieldAccess(
                Box::new(Expr::Identifier("file".to_string())),
                "name".to_string(),
            )
        );
    }

    #[test]
    fn parse_chained_field_access() {
        assert_eq!(
            parse("file.mtime.year"),
            Expr::FieldAccess(
                Box::new(Expr::FieldAccess(
                    Box::new(Expr::Identifier("file".to_string())),
                    "mtime".to_string(),
                )),
                "year".to_string(),
            )
        );
    }

    #[test]
    fn parse_method_call() {
        assert_eq!(
            parse(r#""hello".contains("ell")"#),
            Expr::MethodCall(
                Box::new(Expr::Str("hello".to_string())),
                "contains".to_string(),
                vec![Expr::Str("ell".to_string())],
            )
        );
    }

    #[test]
    fn parse_chained_method() {
        assert_eq!(
            parse("file.name.lower()"),
            Expr::MethodCall(
                Box::new(Expr::FieldAccess(
                    Box::new(Expr::Identifier("file".to_string())),
                    "name".to_string(),
                )),
                "lower".to_string(),
                vec![],
            )
        );
    }

    #[test]
    fn parse_function_call() {
        assert_eq!(
            parse("now()"),
            Expr::FunctionCall("now".to_string(), vec![])
        );
    }

    #[test]
    fn parse_unquoted_date_literal_argument() {
        assert_eq!(
            parse("date(2026-04-18)"),
            Expr::FunctionCall(
                "date".to_string(),
                vec![Expr::Str("2026-04-18".to_string())],
            )
        );
    }

    #[test]
    fn parse_unquoted_duration_literal_argument() {
        assert_eq!(
            parse("dur(1d 3h 20m)"),
            Expr::FunctionCall("dur".to_string(), vec![Expr::Str("1d 3h 20m".to_string())],)
        );
    }

    #[test]
    fn parse_if_function() {
        assert_eq!(
            parse(r#"if(done, "yes", "no")"#),
            Expr::FunctionCall(
                "if".to_string(),
                vec![
                    Expr::Identifier("done".to_string()),
                    Expr::Str("yes".to_string()),
                    Expr::Str("no".to_string()),
                ],
            )
        );
    }

    #[test]
    fn parse_formula_ref() {
        assert_eq!(
            parse("formula.price_per_unit"),
            Expr::FormulaRef("price_per_unit".to_string())
        );
    }

    #[test]
    fn parse_array_literal() {
        assert_eq!(
            parse("[1, 2, 3]"),
            Expr::Array(vec![
                Expr::Number(1.0),
                Expr::Number(2.0),
                Expr::Number(3.0),
            ])
        );
    }

    #[test]
    fn parse_object_literal() {
        assert_eq!(
            parse(r#"{"a": 1, "b": 2}"#),
            Expr::Object(vec![
                ("a".to_string(), Expr::Number(1.0)),
                ("b".to_string(), Expr::Number(2.0)),
            ])
        );
    }

    #[test]
    fn parse_parenthesized() {
        assert_eq!(
            parse("(1 + 2) * 3"),
            Expr::BinaryOp(
                Box::new(Expr::BinaryOp(
                    Box::new(Expr::Number(1.0)),
                    BinOp::Add,
                    Box::new(Expr::Number(2.0)),
                )),
                BinOp::Mul,
                Box::new(Expr::Number(3.0)),
            )
        );
    }

    #[test]
    fn parse_complex_formula() {
        // if(price > 0 && quantity > 0, price * quantity, 0)
        let expr = parse("if(price > 0 && quantity > 0, price * quantity, 0)");
        assert!(matches!(expr, Expr::FunctionCall(name, args) if name == "if" && args.len() == 3));
    }

    #[test]
    fn parse_regex() {
        assert_eq!(
            parse("/abc/g"),
            Expr::Regex {
                pattern: "abc".to_string(),
                flags: "g".to_string(),
            }
        );
    }

    #[test]
    fn parse_string_concatenation() {
        assert_eq!(
            parse(r#"file.name + " - " + description"#),
            Expr::BinaryOp(
                Box::new(Expr::BinaryOp(
                    Box::new(Expr::FieldAccess(
                        Box::new(Expr::Identifier("file".to_string())),
                        "name".to_string(),
                    )),
                    BinOp::Add,
                    Box::new(Expr::Str(" - ".to_string())),
                )),
                BinOp::Add,
                Box::new(Expr::Identifier("description".to_string())),
            )
        );
    }
}
