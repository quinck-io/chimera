#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Token {
    Ident(String),
    Str(String),
    Num(f64),
    True,
    False,
    Null,
    Dot,
    LParen,
    RParen,
    Comma,
    And,
    Or,
    Not,
    Eq,
    Neq,
    Lt,
    Gt,
    Le,
    Ge,
}

pub(crate) fn tokenize(input: &str) -> Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    let mut chars = input.chars().peekable();

    while let Some(&ch) = chars.peek() {
        match ch {
            ' ' | '\t' | '\n' | '\r' => {
                chars.next();
            }
            '.' => {
                chars.next();
                tokens.push(Token::Dot);
            }
            '(' => {
                chars.next();
                tokens.push(Token::LParen);
            }
            ')' => {
                chars.next();
                tokens.push(Token::RParen);
            }
            ',' => {
                chars.next();
                tokens.push(Token::Comma);
            }
            '!' => {
                chars.next();
                if chars.peek() == Some(&'=') {
                    chars.next();
                    tokens.push(Token::Neq);
                } else {
                    tokens.push(Token::Not);
                }
            }
            '=' => {
                chars.next();
                if chars.next() == Some('=') {
                    tokens.push(Token::Eq);
                } else {
                    return Err("expected '==' (single '=' is not valid)".into());
                }
            }
            '<' => {
                chars.next();
                if chars.peek() == Some(&'=') {
                    chars.next();
                    tokens.push(Token::Le);
                } else {
                    tokens.push(Token::Lt);
                }
            }
            '>' => {
                chars.next();
                if chars.peek() == Some(&'=') {
                    chars.next();
                    tokens.push(Token::Ge);
                } else {
                    tokens.push(Token::Gt);
                }
            }
            '&' => {
                chars.next();
                if chars.next() == Some('&') {
                    tokens.push(Token::And);
                } else {
                    return Err("expected '&&' (single '&' is not valid)".into());
                }
            }
            '|' => {
                chars.next();
                if chars.next() == Some('|') {
                    tokens.push(Token::Or);
                } else {
                    return Err("expected '||' (single '|' is not valid)".into());
                }
            }
            '\'' => {
                chars.next();
                let mut s = String::new();
                loop {
                    match chars.next() {
                        Some('\'') => {
                            if chars.peek() == Some(&'\'') {
                                chars.next();
                                s.push('\'');
                            } else {
                                break;
                            }
                        }
                        Some(c) => s.push(c),
                        None => return Err("unterminated string literal".into()),
                    }
                }
                tokens.push(Token::Str(s));
            }
            '0'..='9' => {
                let mut n = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_ascii_digit() || c == '.' {
                        n.push(c);
                        chars.next();
                    } else {
                        break;
                    }
                }
                tokens.push(Token::Num(
                    n.parse().map_err(|_| format!("invalid number: {n}"))?,
                ));
            }
            c if c.is_ascii_alphabetic() || c == '_' => {
                let mut ident = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                        ident.push(c);
                        chars.next();
                    } else {
                        break;
                    }
                }
                tokens.push(match ident.as_str() {
                    "true" => Token::True,
                    "false" => Token::False,
                    "null" => Token::Null,
                    _ => Token::Ident(ident),
                });
            }
            _ => return Err(format!("unexpected character: '{ch}'")),
        }
    }

    Ok(tokens)
}
