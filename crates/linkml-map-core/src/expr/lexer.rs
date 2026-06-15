//! Tokenizer for the restricted expression language.
//!
//! Produces a flat token stream consumed by the Pratt parser in
//! [`crate::expr::parser`]. The grammar is a Python-expression subset, so the
//! lexer recognizes Python operators, numeric/string literals, names, and the
//! `{` `}` braces used for the `{x}` variable-reference syntax.

use super::error::{ExprError, ExprResult};

/// A lexical token.
#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    // Literals
    Int(i64),
    Float(f64),
    Str(String),

    // Names / keywords
    Name(String),
    True,
    False,
    None,
    And,
    Or,
    Not,
    In,

    // Punctuation / grouping
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Comma,
    Dot,

    // Operators
    Plus,
    Minus,
    Star,
    Slash,       // /
    DoubleSlash, // //
    Percent,     // %
    DoubleStar,  // **  (power)
    Caret,       // ^   (bitwise xor, per doctests)
    Amp,         // &
    Pipe,        // |
    Tilde,       // ~
    LShift,      // <<
    RShift,      // >>

    // Comparisons
    Eq,    // ==
    NotEq, // !=
    Lt,    // <
    LtE,   // <=
    Gt,    // >
    GtE,   // >=

    Eof,
}

/// Tokenize `src` into a token stream terminated by [`Token::Eof`].
pub fn lex(src: &str) -> ExprResult<Vec<Token>> {
    let chars: Vec<char> = src.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0usize;
    let n = chars.len();

    while i < n {
        let c = chars[i];

        // Whitespace
        if c.is_whitespace() {
            i += 1;
            continue;
        }

        // String literal: ' or "
        if c == '\'' || c == '"' {
            let quote = c;
            i += 1;
            let mut s = String::new();
            let mut closed = false;
            while i < n {
                let ch = chars[i];
                if ch == '\\' && i + 1 < n {
                    // Minimal escape handling (Python-ish).
                    let next = chars[i + 1];
                    let mapped = match next {
                        'n' => '\n',
                        't' => '\t',
                        'r' => '\r',
                        '\\' => '\\',
                        '\'' => '\'',
                        '"' => '"',
                        '0' => '\0',
                        other => other,
                    };
                    s.push(mapped);
                    i += 2;
                    continue;
                }
                if ch == quote {
                    closed = true;
                    i += 1;
                    break;
                }
                s.push(ch);
                i += 1;
            }
            if !closed {
                return Err(ExprError::Lex("unterminated string literal".into()));
            }
            tokens.push(Token::Str(s));
            continue;
        }

        // Number literal
        if c.is_ascii_digit() || (c == '.' && i + 1 < n && chars[i + 1].is_ascii_digit()) {
            let start = i;
            let mut is_float = false;
            while i < n {
                let ch = chars[i];
                if ch.is_ascii_digit() {
                    i += 1;
                } else if ch == '.' {
                    is_float = true;
                    i += 1;
                } else if ch == 'e' || ch == 'E' {
                    is_float = true;
                    i += 1;
                    if i < n && (chars[i] == '+' || chars[i] == '-') {
                        i += 1;
                    }
                } else if ch == '_' {
                    // Python numeric underscore separators.
                    i += 1;
                } else {
                    break;
                }
            }
            let raw: String = chars[start..i].iter().filter(|&&ch| ch != '_').collect();
            if is_float {
                let f = raw
                    .parse::<f64>()
                    .map_err(|_| ExprError::Lex(format!("invalid float literal: {raw}")))?;
                tokens.push(Token::Float(f));
            } else {
                match raw.parse::<i64>() {
                    Ok(v) => tokens.push(Token::Int(v)),
                    Err(_) => {
                        let f = raw
                            .parse::<f64>()
                            .map_err(|_| ExprError::Lex(format!("invalid int literal: {raw}")))?;
                        tokens.push(Token::Float(f));
                    }
                }
            }
            continue;
        }

        // Identifier / keyword
        if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < n && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let tok = match word.as_str() {
                "True" => Token::True,
                "False" => Token::False,
                "None" => Token::None,
                "and" => Token::And,
                "or" => Token::Or,
                "not" => Token::Not,
                "in" => Token::In,
                _ => Token::Name(word),
            };
            tokens.push(tok);
            continue;
        }

        // Operators / punctuation (multi-char first)
        let two: Option<(Token, usize)> = if i + 1 < n {
            match (c, chars[i + 1]) {
                ('*', '*') => Some((Token::DoubleStar, 2)),
                ('/', '/') => Some((Token::DoubleSlash, 2)),
                ('<', '<') => Some((Token::LShift, 2)),
                ('>', '>') => Some((Token::RShift, 2)),
                ('=', '=') => Some((Token::Eq, 2)),
                ('!', '=') => Some((Token::NotEq, 2)),
                ('<', '=') => Some((Token::LtE, 2)),
                ('>', '=') => Some((Token::GtE, 2)),
                _ => None,
            }
        } else {
            None
        };

        if let Some((tok, len)) = two {
            tokens.push(tok);
            i += len;
            continue;
        }

        let single = match c {
            '+' => Token::Plus,
            '-' => Token::Minus,
            '*' => Token::Star,
            '/' => Token::Slash,
            '%' => Token::Percent,
            '^' => Token::Caret,
            '&' => Token::Amp,
            '|' => Token::Pipe,
            '~' => Token::Tilde,
            '<' => Token::Lt,
            '>' => Token::Gt,
            '(' => Token::LParen,
            ')' => Token::RParen,
            '[' => Token::LBracket,
            ']' => Token::RBracket,
            '{' => Token::LBrace,
            '}' => Token::RBrace,
            ',' => Token::Comma,
            '.' => Token::Dot,
            other => {
                return Err(ExprError::Lex(format!("unexpected character: {other:?}")));
            }
        };
        tokens.push(single);
        i += 1;
    }

    tokens.push(Token::Eof);
    Ok(tokens)
}
