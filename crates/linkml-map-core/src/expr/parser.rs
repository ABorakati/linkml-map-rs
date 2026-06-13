//! Pratt parser producing an [`Ast`] for the restricted expression language.
//!
//! Operator precedence and associativity follow Python's grammar (which is
//! what simpleeval inherits). Comparison chaining (`a < b < c`) is supported.

use super::error::{ExprError, ExprResult};
use super::lexer::{lex, Token};

/// Binary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    FloorDiv,
    Mod,
    Pow,
    LShift,
    RShift,
    BitAnd,
    BitOr,
    BitXor,
}

/// Comparison operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    NotEq,
    Lt,
    LtE,
    Gt,
    GtE,
    In,
    NotIn,
}

/// Unary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,    // -x
    Pos,    // +x
    Invert, // ~x
    Not,    // not x
}

/// Boolean operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoolOp {
    And,
    Or,
}

/// Abstract syntax tree node.
#[derive(Debug, Clone, PartialEq)]
pub enum Ast {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    None,
    /// Bare name reference (`x`).
    Name(String),
    /// `{ ... }` brace reference — must enclose exactly one Name/Attribute.
    Brace(Box<Ast>),
    /// Attribute access `obj.attr`.
    Attribute {
        value: Box<Ast>,
        attr: String,
    },
    /// List literal `[a, b, c]`.
    List(Vec<Ast>),
    /// Tuple literal `(a, b)` — used for `case((cond, val), ...)` pairs.
    Tuple(Vec<Ast>),
    /// Function call `name(args...)`.
    Call {
        func: String,
        args: Vec<Ast>,
    },
    Unary {
        op: UnOp,
        operand: Box<Ast>,
    },
    Binary {
        op: BinOp,
        left: Box<Ast>,
        right: Box<Ast>,
    },
    /// Chained comparison `a < b <= c`.
    Compare {
        left: Box<Ast>,
        ops: Vec<CmpOp>,
        comparators: Vec<Ast>,
    },
    BoolOp {
        op: BoolOp,
        values: Vec<Ast>,
    },
}

/// Parse an expression string into an [`Ast`].
pub fn parse(src: &str) -> ExprResult<Ast> {
    let tokens = lex(src)?;
    let mut p = Parser { tokens, pos: 0 };
    let ast = p.parse_expr()?;
    p.expect(Token::Eof)?;
    Ok(ast)
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> &Token {
        &self.tokens[self.pos]
    }

    fn next(&mut self) -> Token {
        let t = self.tokens[self.pos].clone();
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        t
    }

    fn expect(&mut self, tok: Token) -> ExprResult<()> {
        if *self.peek() == tok {
            self.next();
            Ok(())
        } else {
            Err(ExprError::Parse(format!(
                "expected {:?}, found {:?}",
                tok,
                self.peek()
            )))
        }
    }

    // expr := or_expr
    fn parse_expr(&mut self) -> ExprResult<Ast> {
        self.parse_or()
    }

    // or := and ('or' and)*
    fn parse_or(&mut self) -> ExprResult<Ast> {
        let mut values = vec![self.parse_and()?];
        while *self.peek() == Token::Or {
            self.next();
            values.push(self.parse_and()?);
        }
        if values.len() == 1 {
            Ok(values.pop().unwrap())
        } else {
            Ok(Ast::BoolOp {
                op: BoolOp::Or,
                values,
            })
        }
    }

    // and := not ('and' not)*
    fn parse_and(&mut self) -> ExprResult<Ast> {
        let mut values = vec![self.parse_not()?];
        while *self.peek() == Token::And {
            self.next();
            values.push(self.parse_not()?);
        }
        if values.len() == 1 {
            Ok(values.pop().unwrap())
        } else {
            Ok(Ast::BoolOp {
                op: BoolOp::And,
                values,
            })
        }
    }

    // not := 'not' not | comparison
    fn parse_not(&mut self) -> ExprResult<Ast> {
        if *self.peek() == Token::Not {
            self.next();
            let operand = self.parse_not()?;
            Ok(Ast::Unary {
                op: UnOp::Not,
                operand: Box::new(operand),
            })
        } else {
            self.parse_comparison()
        }
    }

    // comparison := bitor (cmp_op bitor)*
    fn parse_comparison(&mut self) -> ExprResult<Ast> {
        let left = self.parse_bitor()?;
        let mut ops = Vec::new();
        let mut comparators = Vec::new();
        loop {
            let op = match self.peek() {
                Token::Eq => Some(CmpOp::Eq),
                Token::NotEq => Some(CmpOp::NotEq),
                Token::Lt => Some(CmpOp::Lt),
                Token::LtE => Some(CmpOp::LtE),
                Token::Gt => Some(CmpOp::Gt),
                Token::GtE => Some(CmpOp::GtE),
                Token::In => Some(CmpOp::In),
                Token::Not => {
                    // 'not in'
                    if self.tokens.get(self.pos + 1) == Some(&Token::In) {
                        Some(CmpOp::NotIn)
                    } else {
                        None
                    }
                }
                _ => None,
            };
            match op {
                Some(CmpOp::NotIn) => {
                    self.next(); // not
                    self.next(); // in
                    comparators.push(self.parse_bitor()?);
                    ops.push(CmpOp::NotIn);
                }
                Some(o) => {
                    self.next();
                    comparators.push(self.parse_bitor()?);
                    ops.push(o);
                }
                None => break,
            }
        }
        if ops.is_empty() {
            Ok(left)
        } else {
            Ok(Ast::Compare {
                left: Box::new(left),
                ops,
                comparators,
            })
        }
    }

    // bitor := bitxor ('|' bitxor)*
    fn parse_bitor(&mut self) -> ExprResult<Ast> {
        let mut left = self.parse_bitxor()?;
        while *self.peek() == Token::Pipe {
            self.next();
            let right = self.parse_bitxor()?;
            left = Ast::Binary {
                op: BinOp::BitOr,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    // bitxor := bitand ('^' bitand)*
    fn parse_bitxor(&mut self) -> ExprResult<Ast> {
        let mut left = self.parse_bitand()?;
        while *self.peek() == Token::Caret {
            self.next();
            let right = self.parse_bitand()?;
            left = Ast::Binary {
                op: BinOp::BitXor,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    // bitand := shift ('&' shift)*
    fn parse_bitand(&mut self) -> ExprResult<Ast> {
        let mut left = self.parse_shift()?;
        while *self.peek() == Token::Amp {
            self.next();
            let right = self.parse_shift()?;
            left = Ast::Binary {
                op: BinOp::BitAnd,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    // shift := arith (('<<'|'>>') arith)*
    fn parse_shift(&mut self) -> ExprResult<Ast> {
        let mut left = self.parse_arith()?;
        loop {
            let op = match self.peek() {
                Token::LShift => BinOp::LShift,
                Token::RShift => BinOp::RShift,
                _ => break,
            };
            self.next();
            let right = self.parse_arith()?;
            left = Ast::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    // arith := term (('+'|'-') term)*
    fn parse_arith(&mut self) -> ExprResult<Ast> {
        let mut left = self.parse_term()?;
        loop {
            let op = match self.peek() {
                Token::Plus => BinOp::Add,
                Token::Minus => BinOp::Sub,
                _ => break,
            };
            self.next();
            let right = self.parse_term()?;
            left = Ast::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    // term := factor (('*'|'/'|'//'|'%') factor)*
    fn parse_term(&mut self) -> ExprResult<Ast> {
        let mut left = self.parse_factor()?;
        loop {
            let op = match self.peek() {
                Token::Star => BinOp::Mul,
                Token::Slash => BinOp::Div,
                Token::DoubleSlash => BinOp::FloorDiv,
                Token::Percent => BinOp::Mod,
                _ => break,
            };
            self.next();
            let right = self.parse_factor()?;
            left = Ast::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    // factor := ('+'|'-'|'~') factor | power
    fn parse_factor(&mut self) -> ExprResult<Ast> {
        let op = match self.peek() {
            Token::Plus => Some(UnOp::Pos),
            Token::Minus => Some(UnOp::Neg),
            Token::Tilde => Some(UnOp::Invert),
            _ => None,
        };
        if let Some(op) = op {
            self.next();
            let operand = self.parse_factor()?;
            Ok(Ast::Unary {
                op,
                operand: Box::new(operand),
            })
        } else {
            self.parse_power()
        }
    }

    // power := postfix ('**' factor)?   (right-assoc; binds tighter than unary
    //          on the right, looser on the left — matches Python)
    fn parse_power(&mut self) -> ExprResult<Ast> {
        let base = self.parse_postfix()?;
        if *self.peek() == Token::DoubleStar {
            self.next();
            let exp = self.parse_factor()?;
            Ok(Ast::Binary {
                op: BinOp::Pow,
                left: Box::new(base),
                right: Box::new(exp),
            })
        } else {
            Ok(base)
        }
    }

    // postfix := atom ('.' NAME | call_args)*
    // (call args are only attached directly to a bare name -> function call)
    fn parse_postfix(&mut self) -> ExprResult<Ast> {
        let mut node = self.parse_atom()?;
        loop {
            match self.peek() {
                Token::Dot => {
                    self.next();
                    let attr = match self.next() {
                        Token::Name(n) => n,
                        other => {
                            return Err(ExprError::Parse(format!(
                                "expected attribute name after '.', found {other:?}"
                            )))
                        }
                    };
                    node = Ast::Attribute {
                        value: Box::new(node),
                        attr,
                    };
                }
                _ => break,
            }
        }
        Ok(node)
    }

    fn parse_atom(&mut self) -> ExprResult<Ast> {
        match self.next() {
            Token::Int(i) => Ok(Ast::Int(i)),
            Token::Float(f) => Ok(Ast::Float(f)),
            Token::Str(s) => Ok(Ast::Str(s)),
            Token::True => Ok(Ast::Bool(true)),
            Token::False => Ok(Ast::Bool(false)),
            Token::None => Ok(Ast::None),
            Token::Name(name) => {
                // Function call?
                if *self.peek() == Token::LParen {
                    self.next();
                    let args = self.parse_arg_list(Token::RParen)?;
                    self.expect(Token::RParen)?;
                    Ok(Ast::Call { func: name, args })
                } else {
                    Ok(Ast::Name(name))
                }
            }
            Token::LParen => {
                // Parenthesized expression or tuple.
                if *self.peek() == Token::RParen {
                    self.next();
                    return Ok(Ast::Tuple(vec![]));
                }
                let first = self.parse_expr()?;
                if *self.peek() == Token::Comma {
                    let mut elts = vec![first];
                    while *self.peek() == Token::Comma {
                        self.next();
                        if *self.peek() == Token::RParen {
                            break;
                        }
                        elts.push(self.parse_expr()?);
                    }
                    self.expect(Token::RParen)?;
                    Ok(Ast::Tuple(elts))
                } else {
                    self.expect(Token::RParen)?;
                    Ok(first)
                }
            }
            Token::LBracket => {
                let elts = self.parse_arg_list(Token::RBracket)?;
                self.expect(Token::RBracket)?;
                Ok(Ast::List(elts))
            }
            Token::LBrace => {
                // {x} variable reference: must enclose exactly one element,
                // and that element must be a Name or Attribute (validated at
                // eval time to mirror Python's ValueError/TypeError messages).
                if *self.peek() == Token::RBrace {
                    self.next();
                    return Err(ExprError::Parse(
                        "The {} must enclose a single variable".into(),
                    ));
                }
                let mut elts = vec![self.parse_expr()?];
                while *self.peek() == Token::Comma {
                    self.next();
                    if *self.peek() == Token::RBrace {
                        break;
                    }
                    elts.push(self.parse_expr()?);
                }
                self.expect(Token::RBrace)?;
                if elts.len() != 1 {
                    return Err(ExprError::Parse(
                        "The {} must enclose a single variable".into(),
                    ));
                }
                let inner = elts.pop().unwrap();
                match &inner {
                    Ast::Name(_) | Ast::Attribute { .. } => {}
                    _ => {
                        return Err(ExprError::Parse("The {} must enclose a variable".into()));
                    }
                }
                Ok(Ast::Brace(Box::new(inner)))
            }
            other => Err(ExprError::Parse(format!("unexpected token: {other:?}"))),
        }
    }

    fn parse_arg_list(&mut self, close: Token) -> ExprResult<Vec<Ast>> {
        let mut args = Vec::new();
        if *self.peek() == close {
            return Ok(args);
        }
        args.push(self.parse_expr()?);
        while *self.peek() == Token::Comma {
            self.next();
            if *self.peek() == close {
                break;
            }
            args.push(self.parse_expr()?);
        }
        Ok(args)
    }
}
