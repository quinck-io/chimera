use super::token::Token;
use super::value::Value;

#[derive(Debug)]
pub(crate) enum Expr {
    Literal(Value),
    Property(Vec<String>),
    Call(String, Vec<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Not(Box<Expr>),
}

#[derive(Debug)]
pub(crate) enum BinOp {
    And,
    Or,
    Eq,
    Neq,
    Lt,
    Gt,
    Le,
    Ge,
}

pub(crate) struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
}

impl<'a> Parser<'a> {
    pub(crate) fn new(tokens: &'a [Token]) -> Self {
        Self { tokens, pos: 0 }
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn advance(&mut self) -> Option<&Token> {
        let tok = self.tokens.get(self.pos);
        self.pos += 1;
        tok
    }

    fn expect(&mut self, expected: &Token) -> Result<(), String> {
        match self.advance() {
            Some(tok) if tok == expected => Ok(()),
            Some(tok) => Err(format!("expected {expected:?}, got {tok:?}")),
            None => Err(format!("expected {expected:?}, got end of input")),
        }
    }

    pub(crate) fn parse(&mut self) -> Result<Expr, String> {
        let expr = self.parse_or()?;
        if self.pos < self.tokens.len() {
            return Err(format!("unexpected token: {:?}", self.tokens[self.pos]));
        }
        Ok(expr)
    }

    fn parse_or(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_and()?;
        while self.peek() == Some(&Token::Or) {
            self.advance();
            let right = self.parse_and()?;
            left = Expr::Binary(BinOp::Or, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_compare()?;
        while self.peek() == Some(&Token::And) {
            self.advance();
            let right = self.parse_compare()?;
            left = Expr::Binary(BinOp::And, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_compare(&mut self) -> Result<Expr, String> {
        let left = self.parse_unary()?;
        let op = match self.peek() {
            Some(Token::Eq) => BinOp::Eq,
            Some(Token::Neq) => BinOp::Neq,
            Some(Token::Lt) => BinOp::Lt,
            Some(Token::Gt) => BinOp::Gt,
            Some(Token::Le) => BinOp::Le,
            Some(Token::Ge) => BinOp::Ge,
            _ => return Ok(left),
        };
        self.advance();
        let right = self.parse_unary()?;
        Ok(Expr::Binary(op, Box::new(left), Box::new(right)))
    }

    fn parse_unary(&mut self) -> Result<Expr, String> {
        if self.peek() == Some(&Token::Not) {
            self.advance();
            let inner = self.parse_unary()?;
            return Ok(Expr::Not(Box::new(inner)));
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<Expr, String> {
        match self.peek().cloned() {
            Some(Token::Str(s)) => {
                self.advance();
                Ok(Expr::Literal(Value::String(s)))
            }
            Some(Token::Num(n)) => {
                self.advance();
                Ok(Expr::Literal(Value::Number(n)))
            }
            Some(Token::True) => {
                self.advance();
                Ok(Expr::Literal(Value::Bool(true)))
            }
            Some(Token::False) => {
                self.advance();
                Ok(Expr::Literal(Value::Bool(false)))
            }
            Some(Token::Null) => {
                self.advance();
                Ok(Expr::Literal(Value::Null))
            }
            Some(Token::LParen) => {
                self.advance();
                let expr = self.parse_or()?;
                self.expect(&Token::RParen)?;
                Ok(expr)
            }
            Some(Token::Ident(_)) => self.parse_property_or_call(),
            Some(tok) => Err(format!("unexpected token: {tok:?}")),
            None => Err("unexpected end of expression".into()),
        }
    }

    fn parse_property_or_call(&mut self) -> Result<Expr, String> {
        let mut parts = Vec::new();
        if let Some(Token::Ident(name)) = self.advance().cloned() {
            parts.push(name);
        } else {
            return Err("expected identifier".into());
        }

        while self.peek() == Some(&Token::Dot) {
            self.advance();
            match self.advance().cloned() {
                Some(Token::Ident(name)) => parts.push(name),
                Some(Token::Num(n)) => {
                    parts.push(if n == (n as i64) as f64 {
                        (n as i64).to_string()
                    } else {
                        n.to_string()
                    });
                }
                other => return Err(format!("expected identifier after '.', got {other:?}")),
            }
        }

        if self.peek() == Some(&Token::LParen) {
            self.advance();
            let func_name = parts.join(".");
            let mut args = Vec::new();
            if self.peek() != Some(&Token::RParen) {
                args.push(self.parse_or()?);
                while self.peek() == Some(&Token::Comma) {
                    self.advance();
                    args.push(self.parse_or()?);
                }
            }
            self.expect(&Token::RParen)?;
            Ok(Expr::Call(func_name, args))
        } else {
            Ok(Expr::Property(parts))
        }
    }
}
