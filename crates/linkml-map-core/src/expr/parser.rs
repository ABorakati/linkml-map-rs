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
use super::lexer::{FStrField, FStrPart, Token, lex};

/// The deepest expression tree the parser accepts, counted in nodes along the
/// longest path (a leaf is 1, `1 + 1` is 2).
///
/// PARITY: upstream evaluates the `ast` CPython built with a recursive
/// `_eval`, so a tree deeper than the interpreter's recursion limit raises
/// `RecursionError`, which `_eval_expr` does not catch. `simpleeval` costs two
/// frames per level of a binary operation, unary operation, attribute or
/// subscript, so with the default limit of 1000 a chain fails at about 496
/// levels (measured against v0.6.0-rc1: 495 chained `-` evaluate, 496 do not).
/// The real threshold also depends on how deep the caller already is; this is
/// the figure at a shallow stack. Upstream raises at evaluation time, so a
/// conditional expression whose deep branch is never taken evaluates there but
/// is refused here at parse time. Bounding the depth also bounds the stack the
/// evaluator, `Clone`, `Drop` and the AST walkers use.
pub const MAX_AST_DEPTH: usize = 496;

/// How many lambdas may nest (`lambda: lambda: ...`), which recurses in the
/// parser without any bracket for the lexer's limit to count.
///
/// PARITY: CPython's parser accepts far deeper chains (and upstream evaluates
/// them, or raises `RecursionError`); a hundred nested lambdas is not a real
/// expression, and each level costs several parser frames.
const MAX_LAMBDA_NESTING: usize = 100;

/// An error for a tree deeper than [`MAX_AST_DEPTH`].
fn too_deep() -> ExprError {
    ExprError::recursion()
}

/// Whether `ast` is deeper than [`MAX_AST_DEPTH`]. Iterative, so a tree too deep
/// to recurse over is still safe to measure.
fn exceeds_max_depth(ast: &Ast) -> bool {
    let mut stack: Vec<(&Ast, usize)> = vec![(ast, 1)];
    while let Some((node, depth)) = stack.pop() {
        if depth > MAX_AST_DEPTH {
            return true;
        }
        node.for_each_child(&mut |child| stack.push((child, depth + 1)));
    }
    false
}

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
    /// `@`: lexed and parsed, refused (`OperatorNotDefined`) when evaluated.
    MatMul,
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
    Is,
    IsNot,
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
    /// An imaginary literal (`1j`); evaluating it reports an unrepresentable
    /// value, as [`Value`](crate::value::Value) has no complex numbers.
    Imag(f64),
    Str(String),
    /// A `b'..'` literal (unrepresentable when evaluated).
    Bytes(Vec<u8>),
    Bool(bool),
    None,
    /// `...`
    Ellipsis,
    /// An f-string (`ast.JoinedStr`).
    FString(Vec<FPart>),
    /// A slice `lower:upper:step`, only ever the index of a [`Ast::Subscript`]
    /// (or an element of a tuple index).
    Slice {
        lower: Option<Box<Ast>>,
        upper: Option<Box<Ast>>,
        step: Option<Box<Ast>>,
    },
    /// `*value` in a call, list or tuple display, or subscript.
    Starred(Box<Ast>),
    /// The key of a `**mapping` entry of a dict display (Python's `None` key).
    DictUnpack,
    /// `lambda params: body`.
    Lambda(Box<Lambda>),
    /// A call whose callee is not a bare name: `x.method(..)`, `f(..)(..)`,
    /// `(lambda: 1)()`.
    CallValue {
        func: Box<Ast>,
        args: Vec<Ast>,
        /// As [`Ast::Call::keywords`].
        keywords: Vec<(String, Ast)>,
    },
    /// `name := value`.
    NamedExpr {
        target: String,
        value: Box<Ast>,
    },
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
        /// Keyword arguments, in source order (`ast.Call.keywords`); an empty
        /// name is a `**mapping` unpacking.
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

impl Ast {
    /// Call `f` on each direct child expression, in source order: the nodes
    /// `ast.iter_child_nodes` would give for this node (f-string fields, call
    /// arguments, keyword values, comprehension parts, lambda defaults and
    /// body).
    pub fn for_each_child<'a>(&'a self, f: &mut impl FnMut(&'a Ast)) {
        fn fstring<'a>(parts: &'a [FPart], f: &mut impl FnMut(&'a Ast)) {
            for part in parts {
                if let FPart::Field(field) = part {
                    f(&field.value);
                    if let Some(spec) = &field.spec {
                        fstring(spec, f);
                    }
                }
            }
        }
        match self {
            Ast::Int(_)
            | Ast::Float(_)
            | Ast::Imag(_)
            | Ast::Str(_)
            | Ast::Bytes(_)
            | Ast::Bool(_)
            | Ast::None
            | Ast::Ellipsis
            | Ast::DictUnpack
            | Ast::Name(_) => {}
            Ast::Brace(inner) | Ast::Starred(inner) => f(inner),
            Ast::NamedExpr { value, .. } => f(value),
            Ast::FString(parts) => fstring(parts, f),
            Ast::Slice { lower, upper, step } => {
                for bound in [lower, upper, step].into_iter().flatten() {
                    f(bound);
                }
            }
            Ast::Lambda(lambda) => {
                lambda.defaults().for_each(&mut *f);
                f(&lambda.body);
            }
            Ast::CallValue {
                func,
                args,
                keywords,
            } => {
                f(func);
                args.iter().for_each(&mut *f);
                keywords.iter().for_each(|(_, value)| f(value));
            }
            Ast::Attribute { value, .. } => f(value),
            Ast::Set(items) | Ast::List(items) | Ast::Tuple(items) => {
                items.iter().for_each(&mut *f);
            }
            Ast::Dict(pairs) => {
                for (key, value) in pairs {
                    f(key);
                    f(value);
                }
            }
            Ast::Subscript { value, index } => {
                f(value);
                f(index);
            }
            Ast::Comp(comp) => {
                f(&comp.elt);
                if let Some(value) = &comp.value {
                    f(value);
                }
                for generator in &comp.generators {
                    f(&generator.iter);
                    generator.ifs.iter().for_each(&mut *f);
                }
            }
            Ast::IfExp { test, body, orelse } => {
                f(test);
                f(body);
                f(orelse);
            }
            Ast::Call { args, keywords, .. } => {
                args.iter().for_each(&mut *f);
                keywords.iter().for_each(|(_, value)| f(value));
            }
            Ast::Unary { operand, .. } => f(operand),
            Ast::Binary { left, right, .. } => {
                f(left);
                f(right);
            }
            Ast::Compare {
                left, comparators, ..
            } => {
                f(left);
                comparators.iter().for_each(&mut *f);
            }
            Ast::BoolOp { values, .. } => values.iter().for_each(&mut *f),
        }
    }
}

/// A piece of an f-string.
#[derive(Debug, Clone, PartialEq)]
pub enum FPart {
    /// Literal text.
    Lit(String),
    /// A replacement field.
    Field(Box<FField>),
}

/// A replacement field of an f-string (`ast.FormattedValue`). The text of a
/// self-documenting `{x=}` is a literal part before it.
#[derive(Debug, Clone, PartialEq)]
pub struct FField {
    pub value: Ast,
    /// `Some('s' | 'r' | 'a')` for `!s`, `!r`, `!a` (and the implicit `!r` of
    /// `{x=}` without a format spec).
    pub conversion: Option<char>,
    /// The format spec, an f-string body (`ast.JoinedStr`).
    pub spec: Option<Vec<FPart>>,
}

/// A lambda's parameters and body.
#[derive(Debug, Clone, PartialEq)]
pub struct Lambda {
    /// Positional parameters (positional-only ones included) with defaults.
    pub params: Vec<(String, Option<Ast>)>,
    /// `*args`.
    pub vararg: Option<String>,
    /// Keyword-only parameters (after `*` or `*args`) with defaults.
    pub kwonly: Vec<(String, Option<Ast>)>,
    /// `**kwargs`.
    pub kwarg: Option<String>,
    pub body: Ast,
}

impl Lambda {
    /// The default-value expressions.
    pub fn defaults(&self) -> impl Iterator<Item = &Ast> {
        self.params
            .iter()
            .chain(&self.kwonly)
            .filter_map(|(_, default)| default.as_ref())
    }
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
    let mut p = Parser {
        tokens,
        pos: 0,
        lambda_depth: 0,
    };
    let ast = p.parse_expr()?;
    // A trailing newline ends the expression, as in `ast.parse(.., mode="eval")`.
    while *p.peek() == Token::Newline {
        p.next();
    }
    p.expect(Token::Eof)?;
    Ok(ast)
}

/// Positional and keyword arguments of a call.
type CallArgs = (Vec<Ast>, Vec<(String, Ast)>);

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    /// Lambdas being parsed: `lambda: lambda: ...` recurses without a bracket,
    /// which is all the lexer's bracket limit bounds.
    lambda_depth: usize,
}

impl Parser {
    fn peek(&self) -> &Token {
        &self.tokens[self.pos]
    }

    /// Consume the next token (moving it out: nothing is cloned). The final
    /// [`Token::Eof`] is never consumed.
    fn next(&mut self) -> Token {
        if self.pos + 1 < self.tokens.len() {
            let token = std::mem::replace(&mut self.tokens[self.pos], Token::Eof);
            self.pos += 1;
            token
        } else {
            Token::Eof
        }
    }

    /// Count one more link of a left-nested chain (`a + b + c`, `x.a.b`, `x[0][1]`),
    /// whose tree is as deep as the chain is long.
    fn chain_link(links: &mut usize) -> ExprResult<()> {
        *links += 1;
        if *links >= MAX_AST_DEPTH {
            return Err(too_deep());
        }
        Ok(())
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
    //
    // Iterative over the chain, so a long `a if b else c if d else ...` costs no
    // stack; the tree it builds is bounded by [`MAX_AST_DEPTH`] (checked here,
    // once per expression, so every nested expression is checked too).
    fn parse_expr(&mut self) -> ExprResult<Ast> {
        let mut arms: Vec<(Ast, Ast)> = Vec::new();
        let mut result = loop {
            // `lambdef` is an `expression` too, and swallows the rest.
            if *self.peek() == Token::Lambda {
                break self.parse_lambda()?;
            }
            let body = self.parse_or()?;
            if !self.peek_keyword("if") {
                break body;
            }
            self.next(); // if
            let test = self.parse_or()?;
            if !self.peek_keyword("else") {
                return Err(ExprError::Parse(
                    "expected 'else' after the condition of a conditional expression".into(),
                ));
            }
            self.next(); // else
            arms.push((body, test));
            if arms.len() >= MAX_AST_DEPTH {
                return Err(too_deep());
            }
        };
        while let Some((body, test)) = arms.pop() {
            result = Ast::IfExp {
                test: Box::new(test),
                body: Box::new(body),
                orelse: Box::new(result),
            };
        }
        if exceeds_max_depth(&result) {
            return Err(too_deep());
        }
        Ok(result)
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

    // not := 'not' not | comparison   (iterative: a run of `not`s costs no stack)
    fn parse_not(&mut self) -> ExprResult<Ast> {
        let mut nots = 0usize;
        while *self.peek() == Token::Not {
            self.next();
            nots += 1;
            if nots >= MAX_AST_DEPTH {
                return Err(too_deep());
            }
        }
        let mut node = self.parse_comparison()?;
        for _ in 0..nots {
            node = Ast::Unary {
                op: UnOp::Not,
                operand: Box::new(node),
            };
        }
        Ok(node)
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
                Token::Is => {
                    // 'is not'
                    if self.tokens.get(self.pos + 1) == Some(&Token::Not) {
                        Some(CmpOp::IsNot)
                    } else {
                        Some(CmpOp::Is)
                    }
                }
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
                Some(CmpOp::NotIn | CmpOp::IsNot) => {
                    self.next(); // not / is
                    self.next(); // in / not
                    comparators.push(self.parse_bitor()?);
                    ops.push(op.expect("matched Some"));
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
        let mut links = 0;
        while *self.peek() == Token::Pipe {
            self.next();
            Self::chain_link(&mut links)?;
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
        let mut links = 0;
        while *self.peek() == Token::Caret {
            self.next();
            Self::chain_link(&mut links)?;
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
        let mut links = 0;
        while *self.peek() == Token::Amp {
            self.next();
            Self::chain_link(&mut links)?;
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
        let mut links = 0;
        loop {
            let op = match self.peek() {
                Token::LShift => BinOp::LShift,
                Token::RShift => BinOp::RShift,
                _ => break,
            };
            self.next();
            Self::chain_link(&mut links)?;
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
        let mut links = 0;
        loop {
            let op = match self.peek() {
                Token::Plus => BinOp::Add,
                Token::Minus => BinOp::Sub,
                _ => break,
            };
            self.next();
            Self::chain_link(&mut links)?;
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
        let mut links = 0;
        loop {
            let op = match self.peek() {
                Token::Star => BinOp::Mul,
                Token::Slash => BinOp::Div,
                Token::DoubleSlash => BinOp::FloorDiv,
                Token::Percent => BinOp::Mod,
                Token::At => BinOp::MatMul,
                _ => break,
            };
            self.next();
            Self::chain_link(&mut links)?;
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
    // power := postfix ('**' factor)?   (right-assoc; binds tighter than unary
    //          on the right, looser on the left — matches Python)
    //
    // Iterative: `- - - x` and `a ** b ** c ** ...` cost no stack. Each pending
    // frame is a run of unary operators and a power base that still waits for
    // its exponent; the frames are folded once the innermost factor is known.
    fn parse_factor(&mut self) -> ExprResult<Ast> {
        let mut pending: Vec<(Vec<UnOp>, Ast)> = Vec::new();
        loop {
            let mut ops = Vec::new();
            while let Some(op) = match self.peek() {
                Token::Plus => Some(UnOp::Pos),
                Token::Minus => Some(UnOp::Neg),
                Token::Tilde => Some(UnOp::Invert),
                _ => None,
            } {
                self.next();
                ops.push(op);
                if ops.len() >= MAX_AST_DEPTH {
                    return Err(too_deep());
                }
            }
            let base = self.parse_postfix()?;
            if *self.peek() == Token::DoubleStar {
                self.next();
                pending.push((ops, base));
                if pending.len() >= MAX_AST_DEPTH {
                    return Err(too_deep());
                }
                continue;
            }
            let mut result = apply_unary(ops, base);
            while let Some((ops, base)) = pending.pop() {
                result = apply_unary(
                    ops,
                    Ast::Binary {
                        op: BinOp::Pow,
                        left: Box::new(base),
                        right: Box::new(result),
                    },
                );
            }
            return Ok(result);
        }
    }

    // postfix := atom ('.' NAME | call_args | '[' subscript ']')*
    fn parse_postfix(&mut self) -> ExprResult<Ast> {
        let mut node = self.parse_atom()?;
        let mut links = 0;
        loop {
            match self.peek() {
                Token::Dot => {
                    self.next();
                    Self::chain_link(&mut links)?;
                    let attr = match self.next() {
                        Token::Name(n) if !is_hard_keyword(&n) => n,
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
                    // Subscript: value[index], value[lower:upper:step].
                    self.next();
                    Self::chain_link(&mut links)?;
                    let index = self.parse_subscript()?;
                    self.expect(Token::RBracket)?;
                    node = Ast::Subscript {
                        value: Box::new(node),
                        index: Box::new(index),
                    };
                }
                Token::LParen => {
                    self.next();
                    Self::chain_link(&mut links)?;
                    let (args, keywords) = self.parse_call_args()?;
                    self.expect(Token::RParen)?;
                    node = match node {
                        Ast::Name(func) => Ast::Call {
                            func,
                            args,
                            keywords,
                        },
                        func => Ast::CallValue {
                            func: Box::new(func),
                            args,
                            keywords,
                        },
                    };
                }
                _ => break,
            }
        }
        Ok(node)
    }

    // subscript := slice_item (',' slice_item)* [',']   (several items are a tuple)
    fn parse_subscript(&mut self) -> ExprResult<Ast> {
        let first = self.parse_slice_item()?;
        if *self.peek() != Token::Comma {
            return Ok(match first {
                starred @ Ast::Starred(_) => Ast::Tuple(vec![starred]),
                other => other,
            });
        }
        let mut elts = vec![first];
        while *self.peek() == Token::Comma {
            self.next();
            if *self.peek() == Token::RBracket {
                break;
            }
            elts.push(self.parse_slice_item()?);
        }
        Ok(Ast::Tuple(elts))
    }

    // slice_item := slice | star_named_expression
    // slice := [expression] ':' [expression] [':' [expression]]
    fn parse_slice_item(&mut self) -> ExprResult<Ast> {
        let lower = if *self.peek() == Token::Colon {
            None
        } else {
            let first = self.parse_star_named()?;
            if *self.peek() != Token::Colon {
                return Ok(first);
            }
            Some(Box::new(first))
        };
        self.next(); // :
        let bound = |p: &mut Self| -> ExprResult<Option<Box<Ast>>> {
            if matches!(p.peek(), Token::Colon | Token::RBracket | Token::Comma) {
                Ok(None)
            } else {
                Ok(Some(Box::new(p.parse_expr()?)))
            }
        };
        let upper = bound(self)?;
        let step = if *self.peek() == Token::Colon {
            self.next();
            bound(self)?
        } else {
            None
        };
        Ok(Ast::Slice { lower, upper, step })
    }

    // star_named_expression := '*' bitwise_or | named_expression
    fn parse_star_named(&mut self) -> ExprResult<Ast> {
        if *self.peek() == Token::Star {
            self.next();
            return Ok(Ast::Starred(Box::new(self.parse_bitor()?)));
        }
        self.parse_named_expr()
    }

    // named_expression := NAME ':=' expression | expression
    fn parse_named_expr(&mut self) -> ExprResult<Ast> {
        if let (Token::Name(name), Some(Token::ColonEq)) =
            (self.peek(), self.tokens.get(self.pos + 1))
        {
            if is_hard_keyword(name) {
                return Err(ExprError::Parse(format!("unexpected keyword: {name:?}")));
            }
            let target = name.clone();
            self.next(); // name
            self.next(); // :=
            let value = Box::new(self.parse_expr()?);
            return Ok(Ast::NamedExpr { target, value });
        }
        self.parse_expr()
    }

    // lambdef := 'lambda' [parameters] ':' expression
    fn parse_lambda(&mut self) -> ExprResult<Ast> {
        self.lambda_depth += 1;
        if self.lambda_depth > MAX_LAMBDA_NESTING {
            return Err(too_deep());
        }
        let lambda = self.parse_lambda_inner();
        self.lambda_depth -= 1;
        lambda
    }

    fn parse_lambda_inner(&mut self) -> ExprResult<Ast> {
        self.next(); // lambda
        let mut lambda = Lambda {
            params: Vec::new(),
            vararg: None,
            kwonly: Vec::new(),
            kwarg: None,
            body: Ast::None,
        };
        let mut after_star = false;
        let mut seen: Vec<String> = Vec::new();
        while *self.peek() != Token::Colon {
            match self.next() {
                Token::Star => {
                    if after_star {
                        return Err(ExprError::Parse("* argument may appear only once".into()));
                    }
                    after_star = true;
                    if let Token::Name(_) = self.peek() {
                        let name = self.parse_param_name(&mut seen)?;
                        lambda.vararg = Some(name);
                    }
                }
                Token::DoubleStar => {
                    lambda.kwarg = Some(self.parse_param_name(&mut seen)?);
                    if *self.peek() == Token::Comma {
                        self.next();
                    }
                    break;
                }
                Token::Slash => {}
                Token::Name(name) => {
                    if is_hard_keyword(&name) || seen.contains(&name) {
                        return Err(ExprError::Parse(format!(
                            "invalid lambda parameter {name:?}"
                        )));
                    }
                    seen.push(name.clone());
                    let default = if *self.peek() == Token::Assign {
                        self.next();
                        Some(self.parse_expr()?)
                    } else {
                        None
                    };
                    if after_star {
                        lambda.kwonly.push((name, default));
                    } else {
                        lambda.params.push((name, default));
                    }
                }
                other => {
                    return Err(ExprError::Parse(format!(
                        "invalid lambda parameter, found {other:?}"
                    )));
                }
            }
            if *self.peek() != Token::Comma {
                break;
            }
            self.next();
        }
        self.expect(Token::Colon)?;
        lambda.body = self.parse_expr()?;
        Ok(Ast::Lambda(Box::new(lambda)))
    }

    fn parse_param_name(&mut self, seen: &mut Vec<String>) -> ExprResult<String> {
        match self.next() {
            Token::Name(name) if !is_hard_keyword(&name) && !seen.contains(&name) => {
                seen.push(name.clone());
                Ok(name)
            }
            other => Err(ExprError::Parse(format!(
                "invalid lambda parameter, found {other:?}"
            ))),
        }
    }

    fn parse_atom(&mut self) -> ExprResult<Ast> {
        match self.next() {
            Token::Int(i) => Ok(Ast::Int(i)),
            Token::Float(f) => Ok(Ast::Float(f)),
            Token::Imag(f) => Ok(Ast::Imag(f)),
            first @ (Token::Str(_) | Token::Bytes(_) | Token::FStr(_)) => self.parse_strings(first),
            Token::True => Ok(Ast::Bool(true)),
            Token::False => Ok(Ast::Bool(false)),
            Token::None => Ok(Ast::None),
            Token::Ellipsis => Ok(Ast::Ellipsis),
            Token::Name(name) => {
                if is_hard_keyword(&name) {
                    return Err(ExprError::Parse(format!("unexpected keyword: {name:?}")));
                }
                Ok(Ast::Name(name))
            }
            Token::LParen => {
                // Parenthesized expression, tuple or generator expression.
                if *self.peek() == Token::RParen {
                    self.next();
                    return Ok(Ast::Tuple(vec![]));
                }
                let first = self.parse_star_named()?;
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
                        elts.push(self.parse_star_named()?);
                    }
                    self.expect(Token::RParen)?;
                    Ok(Ast::Tuple(elts))
                } else {
                    self.expect(Token::RParen)?;
                    if matches!(first, Ast::Starred(_)) {
                        return Err(ExprError::Parse(
                            "cannot use starred expression here".into(),
                        ));
                    }
                    Ok(first)
                }
            }
            Token::LBracket => {
                // Empty list.
                if *self.peek() == Token::RBracket {
                    self.next();
                    return Ok(Ast::List(vec![]));
                }
                let first = self.parse_star_named()?;
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
                    elts.push(self.parse_star_named()?);
                }
                self.expect(Token::RBracket)?;
                Ok(Ast::List(elts))
            }
            Token::LBrace => self.parse_brace(),
            other => Err(ExprError::Parse(format!("unexpected token: {other:?}"))),
        }
    }

    /// One literal or a run of adjacent literals (`'a' 'b'`, `'a' f'{x}'`),
    /// starting with the already consumed `first`. Str and f-string parts merge;
    /// bytes and non-bytes do not mix.
    fn parse_strings(&mut self, first: Token) -> ExprResult<Ast> {
        enum Acc {
            Str(String),
            F(Vec<FPart>),
            Bytes(Vec<u8>),
        }
        let mixed = || ExprError::Parse("cannot mix bytes and nonbytes literals".into());
        let mut acc: Option<Acc> = None;
        let mut token = first;
        loop {
            acc = Some(match (acc, token) {
                (None, Token::Str(s)) => Acc::Str(s),
                (None, Token::Bytes(b)) => Acc::Bytes(b),
                (None, Token::FStr(parts)) => Acc::F(fstring_parts(parts)?),
                (Some(Acc::Str(mut text)), Token::Str(s)) => {
                    text.push_str(&s);
                    Acc::Str(text)
                }
                (Some(Acc::Str(text)), Token::FStr(parts)) => {
                    let mut merged = Vec::new();
                    push_lit(&mut merged, text);
                    extend_parts(&mut merged, fstring_parts(parts)?);
                    Acc::F(merged)
                }
                (Some(Acc::F(mut merged)), Token::Str(s)) => {
                    push_lit(&mut merged, s);
                    Acc::F(merged)
                }
                (Some(Acc::F(mut merged)), Token::FStr(parts)) => {
                    extend_parts(&mut merged, fstring_parts(parts)?);
                    Acc::F(merged)
                }
                (Some(Acc::Bytes(mut bytes)), Token::Bytes(b)) => {
                    bytes.extend(b);
                    Acc::Bytes(bytes)
                }
                _ => return Err(mixed()),
            });
            if matches!(
                self.peek(),
                Token::Str(_) | Token::Bytes(_) | Token::FStr(_)
            ) {
                token = self.next();
            } else {
                break;
            }
        }
        Ok(match acc.expect("at least one literal") {
            Acc::Str(text) => Ast::Str(text),
            Acc::Bytes(bytes) => Ast::Bytes(bytes),
            Acc::F(mut parts) => match parts.as_mut_slice() {
                [FPart::Lit(text)] => Ast::Str(std::mem::take(text)),
                _ => Ast::FString(parts),
            },
        })
    }

    /// The display after a `{`: Python reads it as a dict, a set or a
    /// comprehension of either, decided by what follows the first element.
    /// A set of exactly one name or attribute is the `{x}` reference syntax.
    fn parse_brace(&mut self) -> ExprResult<Ast> {
        if *self.peek() == Token::RBrace {
            self.next();
            return Ok(Ast::Dict(vec![]));
        }
        if *self.peek() == Token::DoubleStar {
            return self.parse_dict_tail(Vec::new());
        }
        let first = self.parse_star_named()?;
        if *self.peek() == Token::Colon && !matches!(first, Ast::Starred(_) | Ast::NamedExpr { .. })
        {
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
            return self.parse_dict_tail(vec![(first, value)]);
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
            elts.push(self.parse_star_named()?);
        }
        self.expect(Token::RBrace)?;
        // `{x}`, `{ x }`, `{x,}` and `{Table.col}`: the reference syntax
        // (`_eval_set`); every other set display is a genuine set.
        if let [Ast::Name(_) | Ast::Attribute { .. }] = elts.as_slice() {
            return Ok(Ast::Brace(Box::new(elts.pop().expect("one element"))));
        }
        Ok(Ast::Set(elts))
    }

    /// The rest of a dict display after the entries in `pairs` (none, when it
    /// opens with `**`): `, key: value` and `, **mapping` entries up to the `}`.
    fn parse_dict_tail(&mut self, mut pairs: Vec<(Ast, Ast)>) -> ExprResult<Ast> {
        loop {
            if !pairs.is_empty() {
                if *self.peek() != Token::Comma {
                    break;
                }
                self.next();
                if *self.peek() == Token::RBrace {
                    break;
                }
            }
            if *self.peek() == Token::DoubleStar {
                self.next();
                pairs.push((Ast::DictUnpack, self.parse_bitor()?));
                continue;
            }
            let key = self.parse_expr()?;
            self.expect(Token::Colon)?;
            pairs.push((key, self.parse_expr()?));
        }
        self.expect(Token::RBrace)?;
        Ok(Ast::Dict(pairs))
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
            Token::Name(n) if !is_hard_keyword(&n) => Ok(n),
            other => Err(ExprError::Parse(format!(
                "expected loop variable after 'for', found {other:?}"
            ))),
        }
    }

    /// The arguments of a call, up to (not including) the closing `)`:
    /// positional arguments (`*iterable` ones included) and `name=value` /
    /// `**mapping` keywords. A lone positional argument followed by `for` is a
    /// generator expression (`sum(x for x in xs)`).
    fn parse_call_args(&mut self) -> ExprResult<CallArgs> {
        let mut args = Vec::new();
        let mut keywords: Vec<(String, Ast)> = Vec::new();
        let mut double_star = false;
        while *self.peek() != Token::RParen {
            if *self.peek() == Token::DoubleStar {
                self.next();
                keywords.push((String::new(), self.parse_expr()?));
                double_star = true;
            } else if *self.peek() == Token::Star {
                if double_star {
                    return Err(ExprError::Parse(
                        "iterable argument unpacking follows keyword argument unpacking".into(),
                    ));
                }
                self.next();
                args.push(Ast::Starred(Box::new(self.parse_expr()?)));
            } else if let (Token::Name(name), Some(Token::Assign)) =
                (self.peek(), self.tokens.get(self.pos + 1))
            {
                if is_hard_keyword(name) {
                    return Err(ExprError::Parse(format!("unexpected keyword: {name:?}")));
                }
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
                let arg = self.parse_named_expr()?;
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

/// Python's reserved words: none of them is an identifier. (`match`, `case`,
/// `type` and `_` are soft keywords and stay usable as names.)
fn is_hard_keyword(name: &str) -> bool {
    matches!(
        name,
        "False"
            | "None"
            | "True"
            | "and"
            | "as"
            | "assert"
            | "async"
            | "await"
            | "break"
            | "class"
            | "continue"
            | "def"
            | "del"
            | "elif"
            | "else"
            | "except"
            | "finally"
            | "for"
            | "from"
            | "global"
            | "if"
            | "import"
            | "in"
            | "is"
            | "lambda"
            | "nonlocal"
            | "not"
            | "or"
            | "pass"
            | "raise"
            | "return"
            | "try"
            | "while"
            | "with"
            | "yield"
    )
}

/// Append literal text to an f-string's parts, joining it to a literal before it.
fn push_lit(parts: &mut Vec<FPart>, text: String) {
    if text.is_empty() {
        return;
    }
    match parts.last_mut() {
        Some(FPart::Lit(last)) => last.push_str(&text),
        _ => parts.push(FPart::Lit(text)),
    }
}

fn extend_parts(parts: &mut Vec<FPart>, more: Vec<FPart>) {
    for part in more {
        match part {
            FPart::Lit(text) => push_lit(parts, text),
            field @ FPart::Field(_) => parts.push(field),
        }
    }
}

/// The f-string pieces the lexer found, with each replacement field's tokens
/// parsed into an expression.
fn fstring_parts(parts: Vec<FStrPart>) -> ExprResult<Vec<FPart>> {
    let mut out = Vec::with_capacity(parts.len());
    for part in parts {
        match part {
            FStrPart::Lit(text) => push_lit(&mut out, text),
            FStrPart::Field(field) => {
                let FStrField {
                    tokens,
                    conversion,
                    spec,
                    debug,
                } = *field;
                let mut parser = Parser {
                    tokens,
                    pos: 0,
                    lambda_depth: 0,
                };
                // `star_expressions`: `f"{1, 2}"` is a tuple.
                let first = parser.parse_star_named()?;
                let value = if *parser.peek() == Token::Comma {
                    let mut elts = vec![first];
                    while *parser.peek() == Token::Comma {
                        parser.next();
                        if *parser.peek() == Token::Eof {
                            break;
                        }
                        elts.push(parser.parse_star_named()?);
                    }
                    Ast::Tuple(elts)
                } else {
                    first
                };
                parser.expect(Token::Eof)?;
                let spec = spec.map(fstring_parts).transpose()?;
                let conversion = match (conversion, &debug, &spec) {
                    // `{x=}` shows `repr(x)` unless a format spec is given.
                    (None, Some(_), None) => Some('r'),
                    (conversion, _, _) => conversion,
                };
                if let Some(text) = debug {
                    push_lit(&mut out, text);
                }
                out.push(FPart::Field(Box::new(FField {
                    value,
                    conversion,
                    spec,
                })));
            }
        }
    }
    Ok(out)
}

/// Wrap `operand` in `ops`, the innermost operator last (`-~x` is `[Neg, Invert]`).
fn apply_unary(ops: Vec<UnOp>, operand: Ast) -> Ast {
    ops.into_iter()
        .rev()
        .fold(operand, |operand, op| Ast::Unary {
            op,
            operand: Box::new(operand),
        })
}

fn comprehension(kind: CompKind, elt: Ast, value: Option<Ast>, generators: Vec<Generator>) -> Ast {
    Ast::Comp(Box::new(Comprehension {
        kind,
        elt,
        value,
        generators,
    }))
}
