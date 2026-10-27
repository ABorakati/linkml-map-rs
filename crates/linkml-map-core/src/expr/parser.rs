//! Pratt parser producing an [`Ast`] for the restricted expression language.
//!
//! Operator precedence and associativity follow Python's grammar (which is
//! what simpleeval inherits). Comparison chaining (`a < b < c`) is supported, as
//! are the conditional expression (`a if c else b`, right-associative, lowest
//! precedence), dict/set displays and comprehensions, generator expressions and
//! keyword arguments.
//!
//! # Braces
//!
//! Upstream does not pre-process `{x}`: Python's own parser reads every `{...}`
//! as a dict (`{}`, `{k: v}`, `{k: v for ...}`), a set (`{a}`, `{a, b}`,
//! `{a for ...}`) or a set comprehension, and the LinkML evaluator then gives
//! the *set display* `{x}` its reference meaning (`LinkMLEvaluator._eval_set`).
//! This parser makes the same syntactic choice, so the brace form is decided by
//! what follows the first element: a `:` makes a dict, anything else a set. A set
//! display of exactly one name or attribute (`{x}`, `{Table.col}`, `{ x }`, `{x,}`)
//! becomes [`Ast::Brace`]; every other set display is [`Ast::Set`].

use super::error::{ExprError, ExprResult};
use super::lexer::{Token, lex};

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
    /// `{ ... }` brace reference: a set display of exactly one Name/Attribute
    /// (`{x}`, `{Table.col}`), which `_eval_set` resolves to the variable.
    Brace(Box<Ast>),
    /// Any other set display (`{1, 2}`, `{x, y}`, `{'a'}`, `{x + 1}`).
    ///
    /// Upstream's restricted evaluator rejects these (`_eval_set` raises
    /// `ValueError`/`TypeError`); see [`super::compound::with_restricted_syntax`].
    Set(Vec<Ast>),
    /// Dict display `{k: v, ...}` (`{}` is the empty dict).
    Dict(Vec<(Ast, Ast)>),
    /// Attribute access `obj.attr`.
    Attribute {
        value: Box<Ast>,
        attr: String,
    },
    /// List literal `[a, b, c]`.
    List(Vec<Ast>),
    /// Subscript `value[index]`.
    Subscript {
        value: Box<Ast>,
        index: Box<Ast>,
    },
    /// A comprehension or generator expression (`ast.ListComp`, `ast.SetComp`,
    /// `ast.DictComp`, `ast.GeneratorExp`).
    Comp(Box<Comprehension>),
    /// Conditional expression `body if test else orelse` (`ast.IfExp`).
    IfExp {
        test: Box<Ast>,
        body: Box<Ast>,
        orelse: Box<Ast>,
    },
    /// Tuple literal `(a, b)` — used for `case((cond, val), ...)` pairs.
    Tuple(Vec<Ast>),
    /// Function call `name(args..., key=value...)`.
    Call {
        func: String,
        args: Vec<Ast>,
        /// Keyword arguments, in source order (`ast.Call.keywords`).
        keywords: Vec<(String, Ast)>,
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

/// Which display a [`Comprehension`] builds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompKind {
    /// `[elt for ...]`
    List,
    /// `{elt for ...}`
    Set,
    /// `{key: value for ...}`
    Dict,
    /// `(elt for ...)` or a bare generator argument, `f(elt for ...)`. Like
    /// simpleeval's `_eval_comprehension`, it evaluates to a list.
    Generator,
}

/// The target of a `for` clause: one name, or several unpacked from each item
/// (`for k, v in ...`).
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    Name(String),
    Tuple(Vec<String>),
}

impl Target {
    /// The names this target binds.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        match self {
            Self::Name(name) => std::slice::from_ref(name),
            Self::Tuple(names) => names.as_slice(),
        }
        .iter()
        .map(String::as_str)
    }
}

/// One `for target in iter if cond ...` clause (`ast.comprehension`).
#[derive(Debug, Clone, PartialEq)]
pub struct Generator {
    pub target: Target,
    pub iter: Ast,
    pub ifs: Vec<Ast>,
}

/// A comprehension: the produced element (and, for a dict, its value) and the
/// `for` clauses it ranges over, outermost first.
#[derive(Debug, Clone, PartialEq)]
pub struct Comprehension {
    pub kind: CompKind,
    /// The element; the key of a [`CompKind::Dict`].
    pub elt: Ast,
    /// The value of a [`CompKind::Dict`] comprehension, `None` otherwise.
    pub value: Option<Ast>,
    pub generators: Vec<Generator>,
}

/// Parse an expression string into an [`Ast`].
pub fn parse(src: &str) -> ExprResult<Ast> {
    let tokens = lex(src)?;
    let mut p = Parser { tokens, pos: 0 };
    let ast = p.parse_expr()?;
    p.expect(Token::Eof)?;
    Ok(ast)
}

/// Positional and keyword arguments of a call.
type CallArgs = (Vec<Ast>, Vec<(String, Ast)>);

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

    /// Is the next token the keyword `kw`? Keywords other than the operators
    /// (`if`, `else`, `for`) are lexed as names.
    fn peek_keyword(&self, kw: &str) -> bool {
        matches!(self.peek(), Token::Name(n) if n == kw)
    }

    // expr := or_expr ('if' or_expr 'else' expr)?
    //
    // Python's `expression` rule: the conditional expression has the lowest
    // precedence and chains to the right (`a if b else c if d else e` is
    // `a if b else (c if d else e)`).
    fn parse_expr(&mut self) -> ExprResult<Ast> {
        let body = self.parse_or()?;
        if !self.peek_keyword("if") {
            return Ok(body);
        }
        self.next(); // if
        let test = self.parse_or()?;
        if !self.peek_keyword("else") {
            return Err(ExprError::Parse(
                "expected 'else' after the condition of a conditional expression".into(),
            ));
        }
        self.next(); // else
        let orelse = self.parse_expr()?;
        Ok(Ast::IfExp {
            test: Box::new(test),
            body: Box::new(body),
            orelse: Box::new(orelse),
        })
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
                            )));
                        }
                    };
                    node = Ast::Attribute {
                        value: Box::new(node),
                        attr,
                    };
                }
                Token::LBracket => {
                    // Subscript: value[index].
                    self.next();
                    let index = self.parse_expr()?;
                    self.expect(Token::RBracket)?;
                    node = Ast::Subscript {
                        value: Box::new(node),
                        index: Box::new(index),
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
                if matches!(name.as_str(), "if" | "else" | "for") {
                    return Err(ExprError::Parse(format!("unexpected keyword: {name:?}")));
                }
                // Function call?
                if *self.peek() == Token::LParen {
                    self.next();
                    let (args, keywords) = self.parse_call_args()?;
                    self.expect(Token::RParen)?;
                    Ok(Ast::Call {
                        func: name,
                        args,
                        keywords,
                    })
                } else {
                    Ok(Ast::Name(name))
                }
            }
            Token::LParen => {
                // Parenthesized expression, tuple or generator expression.
                if *self.peek() == Token::RParen {
                    self.next();
                    return Ok(Ast::Tuple(vec![]));
                }
                let first = self.parse_expr()?;
                if self.peek_keyword("for") {
                    let generators = self.parse_for_clauses()?;
                    self.expect(Token::RParen)?;
                    return Ok(comprehension(CompKind::Generator, first, None, generators));
                }
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
                // Empty list.
                if *self.peek() == Token::RBracket {
                    self.next();
                    return Ok(Ast::List(vec![]));
                }
                let first = self.parse_expr()?;
                // List comprehension: `[elt for target in iter (if cond)*]`.
                if self.peek_keyword("for") {
                    let generators = self.parse_for_clauses()?;
                    self.expect(Token::RBracket)?;
                    return Ok(comprehension(CompKind::List, first, None, generators));
                }
                // Plain list literal: collect remaining comma-separated elements.
                let mut elts = vec![first];
                while *self.peek() == Token::Comma {
                    self.next();
                    if *self.peek() == Token::RBracket {
                        break;
                    }
                    elts.push(self.parse_expr()?);
                }
                self.expect(Token::RBracket)?;
                Ok(Ast::List(elts))
            }
            Token::LBrace => self.parse_brace(),
            other => Err(ExprError::Parse(format!("unexpected token: {other:?}"))),
        }
    }

    /// The display after a `{`: Python reads it as a dict, a set or a
    /// comprehension of either, decided by what follows the first element.
    /// A set of exactly one name or attribute is the `{x}` reference syntax.
    fn parse_brace(&mut self) -> ExprResult<Ast> {
        if *self.peek() == Token::RBrace {
            self.next();
            return Ok(Ast::Dict(vec![]));
        }
        let first = self.parse_expr()?;
        if *self.peek() == Token::Colon {
            self.next();
            let value = self.parse_expr()?;
            if self.peek_keyword("for") {
                let generators = self.parse_for_clauses()?;
                self.expect(Token::RBrace)?;
                return Ok(comprehension(
                    CompKind::Dict,
                    first,
                    Some(value),
                    generators,
                ));
            }
            let mut pairs = vec![(first, value)];
            while *self.peek() == Token::Comma {
                self.next();
                if *self.peek() == Token::RBrace {
                    break;
                }
                let key = self.parse_expr()?;
                self.expect(Token::Colon)?;
                pairs.push((key, self.parse_expr()?));
            }
            self.expect(Token::RBrace)?;
            return Ok(Ast::Dict(pairs));
        }
        if self.peek_keyword("for") {
            let generators = self.parse_for_clauses()?;
            self.expect(Token::RBrace)?;
            return Ok(comprehension(CompKind::Set, first, None, generators));
        }
        let mut elts = vec![first];
        while *self.peek() == Token::Comma {
            self.next();
            if *self.peek() == Token::RBrace {
                break;
            }
            elts.push(self.parse_expr()?);
        }
        self.expect(Token::RBrace)?;
        // `{x}`, `{ x }`, `{x,}` and `{Table.col}`: the reference syntax
        // (`_eval_set`); every other set display is a genuine set.
        if let [Ast::Name(_) | Ast::Attribute { .. }] = elts.as_slice() {
            return Ok(Ast::Brace(Box::new(elts.pop().expect("one element"))));
        }
        Ok(Ast::Set(elts))
    }

    /// `for target in iter (if cond)*` clauses, one or more
    /// (`for_if_clauses` in Python's grammar). The iterable and the conditions
    /// are `disjunction`s, so a conditional expression there needs parentheses.
    fn parse_for_clauses(&mut self) -> ExprResult<Vec<Generator>> {
        let mut generators = Vec::new();
        while self.peek_keyword("for") {
            self.next(); // for
            let target = self.parse_for_target()?;
            self.expect(Token::In)?;
            let iter = self.parse_or()?;
            let mut ifs = Vec::new();
            while self.peek_keyword("if") {
                self.next(); // if
                ifs.push(self.parse_or()?);
            }
            generators.push(Generator { target, iter, ifs });
        }
        Ok(generators)
    }

    /// A `for` target: a name, or names to unpack (`k, v` or `(k, v)`).
    fn parse_for_target(&mut self) -> ExprResult<Target> {
        let parenthesised = *self.peek() == Token::LParen;
        if parenthesised {
            self.next();
        }
        let mut names = vec![self.parse_target_name()?];
        let mut tuple = false;
        while *self.peek() == Token::Comma {
            tuple = true;
            self.next();
            if matches!(self.peek(), Token::In | Token::RParen) {
                break;
            }
            names.push(self.parse_target_name()?);
        }
        if parenthesised {
            self.expect(Token::RParen)?;
        }
        Ok(if tuple {
            Target::Tuple(names)
        } else {
            Target::Name(names.pop().expect("one name"))
        })
    }

    fn parse_target_name(&mut self) -> ExprResult<String> {
        match self.next() {
            Token::Name(n) if !matches!(n.as_str(), "if" | "else" | "for") => Ok(n),
            other => Err(ExprError::Parse(format!(
                "expected loop variable after 'for', found {other:?}"
            ))),
        }
    }

    /// The arguments of a call, up to (not including) the closing `)`:
    /// positional arguments, then `name=value` keywords. A lone positional
    /// argument followed by `for` is a generator expression (`sum(x for x in xs)`).
    fn parse_call_args(&mut self) -> ExprResult<CallArgs> {
        let mut args = Vec::new();
        let mut keywords: Vec<(String, Ast)> = Vec::new();
        while *self.peek() != Token::RParen {
            if let (Token::Name(name), Some(Token::Assign)) =
                (self.peek(), self.tokens.get(self.pos + 1))
            {
                let name = name.clone();
                self.next(); // name
                self.next(); // =
                keywords.push((name, self.parse_expr()?));
            } else {
                if !keywords.is_empty() {
                    return Err(ExprError::Parse(
                        "positional argument follows keyword argument".into(),
                    ));
                }
                let arg = self.parse_expr()?;
                if args.is_empty() && self.peek_keyword("for") {
                    let generators = self.parse_for_clauses()?;
                    args.push(comprehension(CompKind::Generator, arg, None, generators));
                    break;
                }
                args.push(arg);
            }
            if *self.peek() != Token::Comma {
                break;
            }
            self.next();
        }
        Ok((args, keywords))
    }
}

fn comprehension(kind: CompKind, elt: Ast, value: Option<Ast>, generators: Vec<Generator>) -> Ast {
    Ast::Comp(Box::new(Comprehension {
        kind,
        elt,
        value,
        generators,
    }))
}
