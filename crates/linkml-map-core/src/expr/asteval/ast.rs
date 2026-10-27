//! The Python syntax tree `asteval` walks.
//!
//! Upstream parses the text with `ast.parse` (CPython 3.13) and dispatches on
//! `node.__class__.__name__.lower()`. This is that tree, restricted to the
//! shapes `asteval` can act on; syntax it refuses at run time (`import`,
//! `class`, `global`, `yield`, `:=`, `match`, ...) is kept as a node of the
//! right class name so the same `NotImplementedError` is raised when, and only
//! when, evaluation reaches it.

use std::sync::Arc;

/// Where a `Name` node sits, for `asteval`'s error text (`ExceptionHolder`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Pos {
    /// 1-based first line.
    pub line: u32,
    /// 1-based last line.
    pub end_line: u32,
    /// UTF-8 byte offset of the node in its first line (`ast` `col_offset`).
    pub col: u32,
}

/// A constant (`ast.Constant`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Const {
    None,
    Bool(bool),
    Int(i64),
    /// An integer literal beyond `i64`; its decimal digits.
    BigInt(String),
    Float(f64),
    Str(Arc<str>),
    Bytes(Arc<[u8]>),
    Ellipsis,
    /// `1j`: complex numbers are not modelled.
    Complex(f64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BinOp {
    Add,
    Sub,
    Mult,
    MatMult,
    Div,
    Mod,
    Pow,
    LShift,
    RShift,
    BitOr,
    BitXor,
    BitAnd,
    FloorDiv,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnaryOp {
    Invert,
    Not,
    UAdd,
    USub,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BoolOp {
    And,
    Or,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CmpOp {
    Eq,
    NotEq,
    Lt,
    LtE,
    Gt,
    GtE,
    Is,
    IsNot,
    In,
    NotIn,
}

/// `ast.keyword`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Keyword {
    /// `None` for `**mapping`.
    pub arg: Option<String>,
    pub value: Expr,
}

/// `ast.comprehension`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Comprehension {
    pub target: Expr,
    pub iter: Expr,
    pub ifs: Vec<Expr>,
}

/// One part of an f-string (`JoinedStr.values`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum FPart {
    Lit(String),
    /// `FormattedValue`.
    Field {
        value: Box<Expr>,
        /// `s`, `r` or `a`.
        conversion: Option<char>,
        /// The format spec: itself a `JoinedStr`.
        spec: Option<Vec<FPart>>,
    },
}

/// A function parameter list, reduced to what `Procedure` keeps.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct Params {
    /// `args.args` (positional-only and ordinary parameters).
    pub args: Vec<String>,
    /// `args.defaults`, matching the last `defaults.len()` of `args`.
    pub defaults: Vec<Expr>,
    pub vararg: Option<String>,
    pub kwarg: Option<String>,
    /// Keyword-only parameters exist in the tree but `asteval` ignores them.
    pub kwonly: usize,
}

/// A `def` or `lambda`.
#[derive(Debug, PartialEq)]
pub(crate) struct FuncDef {
    pub name: String,
    pub params: Params,
    pub body: Vec<Stmt>,
    /// A lambda's body is one expression.
    pub lambda_body: Option<Expr>,
    pub has_decorators: bool,
}

/// An `ast.expr` node.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Expr {
    Constant(Const),
    Name(String, Pos),
    List(Vec<Expr>),
    Tuple(Vec<Expr>),
    Set(Vec<Expr>),
    /// `None` key is `**mapping`.
    Dict(Vec<(Option<Expr>, Expr)>),
    Attribute(Box<Expr>, String),
    Subscript(Box<Expr>, Box<Expr>),
    Slice(Option<Box<Expr>>, Option<Box<Expr>>, Option<Box<Expr>>),
    Call(Box<Expr>, Vec<Expr>, Vec<Keyword>),
    BinOp(Box<Expr>, BinOp, Box<Expr>),
    UnaryOp(UnaryOp, Box<Expr>),
    BoolOp(BoolOp, Vec<Expr>),
    Compare(Box<Expr>, Vec<(CmpOp, Expr)>),
    IfExp(Box<Expr>, Box<Expr>, Box<Expr>),
    Lambda(Arc<FuncDef>),
    ListComp(Box<Expr>, Vec<Comprehension>),
    SetComp(Box<Expr>, Vec<Comprehension>),
    DictComp(Box<Expr>, Box<Expr>, Vec<Comprehension>),
    JoinedStr(Vec<FPart>),
    /// Syntax that parses but that `asteval` has no handler for: the class
    /// name (`GeneratorExp`, `Starred`, `NamedExpr`, `Await`, `Yield`, ...).
    Starred(Box<Expr>),
    Unsupported(&'static str),
}

/// One `except` clause.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Handler {
    /// The exception expression, if any.
    pub type_: Option<Expr>,
    pub name: Option<String>,
    pub body: Vec<Stmt>,
}

/// One `with` item.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WithItem {
    pub context_expr: Expr,
    pub optional_vars: Option<Expr>,
}

/// An `ast.stmt` node.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Stmt {
    Expr(Expr),
    Assign(Vec<Expr>, Expr),
    AugAssign(Expr, BinOp, Expr),
    Return(Option<Expr>),
    Delete(Vec<Expr>),
    Pass,
    Break,
    Continue,
    If(Expr, Vec<Stmt>, Vec<Stmt>),
    While(Expr, Vec<Stmt>, Vec<Stmt>),
    For(Expr, Expr, Vec<Stmt>, Vec<Stmt>),
    With(Vec<WithItem>, Vec<Stmt>),
    Raise(Option<Expr>, Option<Expr>),
    Try(Vec<Stmt>, Vec<Handler>, Vec<Stmt>, Vec<Stmt>),
    Assert(Expr, Option<Expr>),
    FunctionDef(Arc<FuncDef>),
    /// A statement `asteval` cannot run: its `ast` class name.
    Unsupported(&'static str),
}

/// A parsed module with the source spans of its top-level expressions, kept so
/// reference scanning can re-parse them with the expression parser.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Module {
    pub body: Vec<Stmt>,
}

impl Expr {
    /// The direct sub-expressions, in source order.
    pub(crate) fn children(&self) -> Vec<&Expr> {
        let mut out: Vec<&Expr> = Vec::new();
        match self {
            Expr::Constant(_) | Expr::Name(..) | Expr::Unsupported(_) => {}
            Expr::List(v) | Expr::Tuple(v) | Expr::Set(v) | Expr::BoolOp(_, v) => out.extend(v),
            Expr::Dict(items) => {
                for (k, v) in items {
                    out.extend(k.as_ref());
                    out.push(v);
                }
            }
            Expr::Attribute(v, _) | Expr::Starred(v) | Expr::UnaryOp(_, v) => out.push(v),
            Expr::Subscript(a, b) | Expr::BinOp(a, _, b) => {
                out.push(a);
                out.push(b);
            }
            Expr::Slice(a, b, c) => {
                for x in [a, b, c].into_iter().flatten() {
                    out.push(x);
                }
            }
            Expr::Call(func, args, kws) => {
                out.push(func);
                out.extend(args);
                out.extend(kws.iter().map(|k| &k.value));
            }
            Expr::Compare(l, rest) => {
                out.push(l);
                out.extend(rest.iter().map(|(_, x)| x));
            }
            Expr::IfExp(a, b, c) => {
                out.push(a);
                out.push(b);
                out.push(c);
            }
            Expr::Lambda(def) => {
                out.extend(def.params.defaults.iter());
                out.extend(def.lambda_body.iter());
            }
            Expr::ListComp(elt, gens) | Expr::SetComp(elt, gens) => {
                out.push(elt);
                comp_children(gens, &mut out);
            }
            Expr::DictComp(k, v, gens) => {
                out.push(k);
                out.push(v);
                comp_children(gens, &mut out);
            }
            Expr::JoinedStr(parts) => fpart_children(parts, &mut out),
        }
        out
    }

    /// Move the boxed sub-expressions out, leaving cheap placeholders, so that
    /// dropping a very deep tree does not recurse.
    fn take_children(&mut self, out: &mut Vec<Expr>) {
        let take = |e: &mut Expr| std::mem::replace(e, Expr::Constant(Const::None));
        match self {
            Expr::Constant(_) | Expr::Name(..) | Expr::Unsupported(_) | Expr::Lambda(_) => {}
            Expr::List(v) | Expr::Tuple(v) | Expr::Set(v) | Expr::BoolOp(_, v) => {
                out.append(v);
            }
            Expr::Dict(items) => {
                for (k, v) in items.drain(..) {
                    out.extend(k);
                    out.push(v);
                }
            }
            Expr::Attribute(v, _) | Expr::Starred(v) | Expr::UnaryOp(_, v) => out.push(take(v)),
            Expr::Subscript(a, b) | Expr::BinOp(a, _, b) => {
                out.push(take(a));
                out.push(take(b));
            }
            Expr::Slice(a, b, c) => {
                for x in [a, b, c].into_iter().flatten() {
                    out.push(take(x));
                }
            }
            Expr::Call(func, args, kws) => {
                out.push(take(func));
                out.append(args);
                out.extend(kws.drain(..).map(|k| k.value));
            }
            Expr::Compare(l, rest) => {
                out.push(take(l));
                out.extend(rest.drain(..).map(|(_, x)| x));
            }
            Expr::IfExp(a, b, c) => {
                out.push(take(a));
                out.push(take(b));
                out.push(take(c));
            }
            Expr::ListComp(elt, gens) | Expr::SetComp(elt, gens) => {
                out.push(take(elt));
                for g in gens.drain(..) {
                    out.push(g.target);
                    out.push(g.iter);
                    out.extend(g.ifs);
                }
            }
            Expr::DictComp(k, v, gens) => {
                out.push(take(k));
                out.push(take(v));
                for g in gens.drain(..) {
                    out.push(g.target);
                    out.push(g.iter);
                    out.extend(g.ifs);
                }
            }
            Expr::JoinedStr(parts) => {
                for p in parts.drain(..) {
                    if let FPart::Field { value, spec, .. } = p {
                        out.push(*value);
                        if let Some(s) = spec {
                            fpart_values(s, out);
                        }
                    }
                }
            }
        }
    }
}

fn fpart_values(parts: Vec<FPart>, out: &mut Vec<Expr>) {
    for p in parts {
        if let FPart::Field { value, spec, .. } = p {
            out.push(*value);
            if let Some(s) = spec {
                fpart_values(s, out);
            }
        }
    }
}

fn comp_children<'a>(gens: &'a [Comprehension], out: &mut Vec<&'a Expr>) {
    for g in gens {
        out.push(&g.target);
        out.push(&g.iter);
        out.extend(g.ifs.iter());
    }
}

fn fpart_children<'a>(parts: &'a [FPart], out: &mut Vec<&'a Expr>) {
    for p in parts {
        if let FPart::Field { value, spec, .. } = p {
            out.push(value);
            if let Some(s) = spec {
                fpart_children(s, out);
            }
        }
    }
}

impl Drop for Expr {
    fn drop(&mut self) {
        let mut stack: Vec<Expr> = Vec::new();
        self.take_children(&mut stack);
        while let Some(mut e) = stack.pop() {
            e.take_children(&mut stack);
        }
    }
}
