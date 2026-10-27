//! A recursive-descent parser for the Python 3.13 grammar, producing the
//! subset of `ast` that `asteval` evaluates (see [`super::ast`]).
//!
//! It accepts and rejects the same texts as `ast.parse` (CPython's PEG
//! parser) for every construct implemented here: the tokenizer's indentation
//! rules, assignment-target validation, argument ordering, parameter ordering,
//! f-string syntax, and so on. Errors carry only a message: `asteval` turns
//! every one into a silent `None`.

use std::sync::Arc;

use super::ast::*;
use super::lex::{Num, PResult, StrTok, Tok, Token, tokenize, translate_newlines};

const STACK_OVERFLOW: &str = "parser stack overflow";

/// Nesting limit of the recursive-descent parser, standing in for CPython's
/// `MAXSTACK` ("parser stack overflow") and recursion limit.
const MAX_DEPTH: usize = 400;

/// The `ast` class name of a statement.
fn stmt_kind(s: &Stmt) -> &'static str {
    match s {
        Stmt::Expr(_) => "Expr",
        Stmt::Assign(..) => "Assign",
        Stmt::AugAssign(..) => "AugAssign",
        Stmt::Return(_) => "Return",
        Stmt::Delete(_) => "Delete",
        Stmt::Pass => "Pass",
        Stmt::Break => "Break",
        Stmt::Continue => "Continue",
        Stmt::If(..) => "If",
        Stmt::While(..) => "While",
        Stmt::For(..) => "For",
        Stmt::With(..) => "With",
        Stmt::Raise(..) => "Raise",
        Stmt::Try(..) => "Try",
        Stmt::Assert(..) => "Assert",
        Stmt::FunctionDef(_) => "FunctionDef",
        Stmt::Unsupported(k) => k,
    }
}

/// A parsed module and the byte spans of the top-level expressions in it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Parsed {
    pub module: Module,
    /// Source (newline-translated) and spans of statement-level expressions.
    pub source: String,
    pub expr_spans: Vec<(usize, usize)>,
    pub first: Option<(&'static str, Option<(usize, usize)>)>,
}

/// `ast.parse(text)`.
pub(crate) fn parse_module(text: &str) -> PResult<Parsed> {
    let source = translate_newlines(text);
    let toks = tokenize(&source)?;
    let mut p = Parser {
        toks,
        pos: 0,
        src: &source,
        depth: 0,
        spans: Vec::new(),
        last_value_span: None,
        first: None,
    };
    let body = p.statements(false)?;
    if !matches!(p.peek(), Tok::End) {
        return Err("invalid syntax".into());
    }
    let spans = std::mem::take(&mut p.spans);
    let first = p.first;
    Ok(Parsed {
        module: Module { body },
        source: source.clone(),
        expr_spans: spans,
        first,
    })
}

/// `ast.parse(text, mode="eval")` succeeds: `expressions NEWLINE* ENDMARKER`.
pub(crate) fn parses_as_expression(text: &str) -> bool {
    let source = translate_newlines(text);
    let Ok(toks) = tokenize(&source) else {
        return false;
    };
    let mut p = Parser {
        toks,
        pos: 0,
        src: &source,
        depth: 0,
        spans: Vec::new(),
        last_value_span: None,
        first: None,
    };
    let r = (|| -> PResult<()> {
        p.expression()?;
        while p.eat_op(",") {
            if matches!(p.peek(), Tok::Newline | Tok::End) {
                break;
            }
            p.expression()?;
        }
        p.eat_newline_opt();
        if matches!(p.peek(), Tok::End) {
            Ok(())
        } else {
            err("invalid syntax")
        }
    })();
    // Past the nesting limit this parser gives up where CPython still parses:
    // let the caller's own parser decide.
    match r {
        Ok(()) => true,
        Err(e) => e == STACK_OVERFLOW,
    }
}

/// Parse a lone expression list (an f-string replacement field's text).
fn parse_field_expr(text: &str) -> PResult<Expr> {
    let wrapped = format!("({text})");
    let toks = tokenize(&wrapped)?;
    let mut p = Parser {
        toks,
        pos: 0,
        src: &wrapped,
        depth: 0,
        spans: Vec::new(),
        last_value_span: None,
        first: None,
    };
    let e = p.star_expressions_or_yield()?;
    p.eat_newline_opt();
    if !matches!(p.peek(), Tok::End) {
        return Err("f-string: invalid syntax".into());
    }
    Ok(e)
}

struct Parser<'a> {
    toks: Vec<Token>,
    pos: usize,
    src: &'a str,
    depth: usize,
    spans: Vec<(usize, usize)>,
    /// Source span of the value of the last assignment / expression statement.
    last_value_span: Option<(usize, usize)>,
    /// What the first statement was: its kind and (for `Expr`/`Assign`) the
    /// span of its value.
    first: Option<(&'static str, Option<(usize, usize)>)>,
}

const KEYWORDS: [&str; 35] = [
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class",
    "continue", "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if",
    "import", "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try",
    "while", "with", "yield",
];

fn is_keyword(s: &str) -> bool {
    KEYWORDS.contains(&s)
}

fn err<T>(msg: &str) -> PResult<T> {
    Err(msg.to_string())
}

impl Parser<'_> {
    // --- token helpers ----------------------------------------------------

    fn peek(&self) -> &Tok {
        &self.toks[self.pos].tok
    }

    fn peek_at(&self, k: usize) -> &Tok {
        let i = (self.pos + k).min(self.toks.len() - 1);
        &self.toks[i].tok
    }

    fn advance(&mut self) -> &Token {
        let t = &self.toks[self.pos];
        if self.pos + 1 < self.toks.len() {
            self.pos += 1;
        }
        t
    }

    fn is_op(&self, op: &str) -> bool {
        matches!(self.peek(), Tok::Op(o) if *o == op)
    }

    fn is_op_at(&self, k: usize, op: &str) -> bool {
        matches!(self.peek_at(k), Tok::Op(o) if *o == op)
    }

    fn eat_op(&mut self, op: &str) -> bool {
        if self.is_op(op) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn expect_op(&mut self, op: &str) -> PResult<()> {
        if self.eat_op(op) {
            Ok(())
        } else {
            Err(format!("invalid syntax: expected '{op}'"))
        }
    }

    fn is_kw(&self, kw: &str) -> bool {
        matches!(self.peek(), Tok::Name(n) if n == kw)
    }

    fn is_kw_at(&self, k: usize, kw: &str) -> bool {
        matches!(self.peek_at(k), Tok::Name(n) if n == kw)
    }

    fn eat_kw(&mut self, kw: &str) -> bool {
        if self.is_kw(kw) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn expect_kw(&mut self, kw: &str) -> PResult<()> {
        if self.eat_kw(kw) {
            Ok(())
        } else {
            Err(format!("invalid syntax: expected '{kw}'"))
        }
    }

    fn eat_newline_opt(&mut self) {
        while matches!(self.peek(), Tok::Newline) {
            self.advance();
        }
    }

    /// An identifier that is not a hard keyword.
    fn name(&mut self) -> PResult<String> {
        match self.peek() {
            Tok::Name(n) if !is_keyword(n) => {
                let n = n.clone();
                self.advance();
                Ok(n)
            }
            _ => err("invalid syntax: expected a name"),
        }
    }

    fn enter(&mut self) -> PResult<()> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return err(STACK_OVERFLOW);
        }
        Ok(())
    }

    fn leave(&mut self) {
        self.depth -= 1;
    }

    fn record_span(&mut self, first_tok: usize) {
        let last = self.pos.saturating_sub(1).max(first_tok);
        let (a, b) = (self.toks[first_tok].start, self.toks[last].end);
        if a < b {
            self.spans.push((a, b));
        }
    }

    // --- statements -------------------------------------------------------

    /// `statements` until a dedent (in a block) or the end marker.
    fn statements(&mut self, in_block: bool) -> PResult<Vec<Stmt>> {
        let mut out = Vec::new();
        loop {
            match self.peek() {
                Tok::End => break,
                Tok::Dedent if in_block => break,
                Tok::Newline => {
                    self.advance();
                }
                Tok::Indent => return err("unexpected indent"),
                Tok::Dedent => return err("unexpected dedent"),
                _ => {
                    let before = out.len();
                    let was_first = self.first.is_none() && !in_block;
                    self.last_value_span = None;
                    self.statement(&mut out)?;
                    if was_first && out.len() > before {
                        let kind = stmt_kind(&out[before]);
                        let span = match &out[before] {
                            Stmt::Expr(_) | Stmt::Assign(..) | Stmt::AugAssign(..) => self.last_value_span,
                            _ => None,
                        };
                        self.first = Some((kind, span));
                    }
                }
            }
        }
        Ok(out)
    }

    fn statement(&mut self, out: &mut Vec<Stmt>) -> PResult<()> {
        self.enter()?;
        let r = self.statement_inner(out);
        self.leave();
        r
    }

    fn statement_inner(&mut self, out: &mut Vec<Stmt>) -> PResult<()> {
        if let Tok::Name(n) = self.peek().clone() {
            match n.as_str() {
                "if" => {
                    out.push(self.if_stmt()?);
                    return Ok(());
                }
                "while" => {
                    out.push(self.while_stmt()?);
                    return Ok(());
                }
                "for" => {
                    out.push(self.for_stmt()?);
                    return Ok(());
                }
                "try" => {
                    out.push(self.try_stmt()?);
                    return Ok(());
                }
                "with" => {
                    out.push(self.with_stmt(false)?);
                    return Ok(());
                }
                "def" => {
                    out.push(self.funcdef(false)?);
                    return Ok(());
                }
                "class" => {
                    out.push(self.classdef()?);
                    return Ok(());
                }
                "async" => {
                    out.push(self.async_stmt()?);
                    return Ok(());
                }
                "match" => {
                    if let Some(s) = self.try_match()? {
                        out.push(s);
                        return Ok(());
                    }
                }
                _ => {}
            }
        }
        if self.is_op("@") {
            out.push(self.decorated()?);
            return Ok(());
        }
        self.simple_stmts(out)
    }

    fn simple_stmts(&mut self, out: &mut Vec<Stmt>) -> PResult<()> {
        loop {
            out.push(self.simple_stmt()?);
            if self.eat_op(";") {
                if matches!(self.peek(), Tok::Newline) {
                    break;
                }
                continue;
            }
            break;
        }
        match self.peek() {
            Tok::Newline => {
                self.advance();
                Ok(())
            }
            Tok::End => Ok(()),
            _ => err("invalid syntax"),
        }
    }

    fn block(&mut self) -> PResult<Vec<Stmt>> {
        self.expect_op(":")?;
        if matches!(self.peek(), Tok::Newline) {
            self.advance();
            if !matches!(self.peek(), Tok::Indent) {
                return err("expected an indented block");
            }
            self.advance();
            let body = self.statements(true)?;
            if body.is_empty() {
                return err("expected an indented block");
            }
            if !matches!(self.peek(), Tok::Dedent) {
                return err("invalid syntax");
            }
            self.advance();
            Ok(body)
        } else {
            let mut out = Vec::new();
            self.simple_stmts(&mut out)?;
            Ok(out)
        }
    }

    fn simple_stmt(&mut self) -> PResult<Stmt> {
        let first = self.pos;
        if let Tok::Name(n) = self.peek().clone() {
            match n.as_str() {
                "pass" => {
                    self.advance();
                    return Ok(Stmt::Pass);
                }
                "break" => {
                    self.advance();
                    return Ok(Stmt::Break);
                }
                "continue" => {
                    self.advance();
                    return Ok(Stmt::Continue);
                }
                "return" => {
                    self.advance();
                    let v = if self.at_stmt_end() {
                        None
                    } else {
                        let f = self.pos;
                        let e = self.star_expressions()?;
                        self.record_span(f);
                        Some(e)
                    };
                    return Ok(Stmt::Return(v));
                }
                "raise" => {
                    self.advance();
                    if self.at_stmt_end() {
                        return Ok(Stmt::Raise(None, None));
                    }
                    let f = self.pos;
                    let e = self.expression()?;
                    self.record_span(f);
                    let cause = if self.eat_kw("from") {
                        let f = self.pos;
                        let c = self.expression()?;
                        self.record_span(f);
                        Some(c)
                    } else {
                        None
                    };
                    return Ok(Stmt::Raise(Some(e), cause));
                }
                "global" | "nonlocal" => {
                    let which = if n.as_str() == "global" { "Global" } else { "Nonlocal" };
                    self.advance();
                    self.name()?;
                    while self.eat_op(",") {
                        self.name()?;
                    }
                    return Ok(Stmt::Unsupported(which));
                }
                "del" => {
                    self.advance();
                    return self.del_stmt();
                }
                "assert" => {
                    self.advance();
                    let f = self.pos;
                    let test = self.expression()?;
                    self.record_span(f);
                    let msg = if self.eat_op(",") {
                        let f = self.pos;
                        let m = self.expression()?;
                        self.record_span(f);
                        Some(m)
                    } else {
                        None
                    };
                    return Ok(Stmt::Assert(test, msg));
                }
                "import" => {
                    self.advance();
                    self.import_names()?;
                    return Ok(Stmt::Unsupported("Import"));
                }
                "from" => {
                    self.advance();
                    self.import_from()?;
                    return Ok(Stmt::Unsupported("ImportFrom"));
                }
                "type" => {
                    // `type X = ...` / `type X[T] = ...` (soft keyword).
                    if matches!(self.peek_at(1), Tok::Name(m) if !is_keyword(m))
                        && (self.is_op_at(2, "=") || self.is_op_at(2, "["))
                    {
                        self.advance();
                        self.advance();
                        if self.is_op("[") {
                            self.type_params()?;
                        }
                        self.expect_op("=")?;
                        self.expression()?;
                        return Ok(Stmt::Unsupported("TypeAlias"));
                    }
                }
                _ => {}
            }
        }
        // Assignment or expression statement.
        let lhs = self.star_expressions_or_yield()?;
        if self.is_op("=") {
            let mut targets = Vec::new();
            let mut cur = lhs;
            let mut cur_start = first;
            let mut vspan = None;
            while self.is_op("=") {
                check_target(&cur, Ctx::Store)?;
                if !matches!(cur, Expr::Name(..)) {
                    self.record_span_range(cur_start, self.pos - 1);
                }
                targets.push(cur);
                self.advance();
                let vf = self.pos;
                cur_start = vf;
                cur = self.star_expressions_or_yield()?;
                vspan = self.span_since(vf);
            }
            self.last_value_span = vspan;
            if let Some((a, b)) = vspan {
                self.spans.push((a, b));
            }
            return Ok(Stmt::Assign(targets, cur));
        }
        if let Some(op) = self.aug_op() {
            // single_target: Name / Attribute / Subscript
            match &lhs {
                Expr::Name(..) | Expr::Attribute(..) | Expr::Subscript(..) => {}
                _ => return err("illegal expression for augmented assignment"),
            }
            self.record_span_range(first, self.pos);
            self.advance();
            let vf = self.pos;
            let rhs = self.star_expressions_or_yield()?;
            self.last_value_span = self.span_since(vf);
            self.record_span(vf);
            return Ok(Stmt::AugAssign(lhs, op, rhs));
        }
        if self.is_op(":") {
            // Annotated assignment.
            match &lhs {
                Expr::Name(..) | Expr::Attribute(..) | Expr::Subscript(..) => {}
                Expr::Tuple(_) => return err("only single target (not tuple) can be annotated"),
                Expr::List(_) => return err("only single target (not list) can be annotated"),
                _ => return err("illegal target for annotation"),
            }
            self.advance();
            self.expression()?;
            if self.eat_op("=") {
                self.star_expressions_or_yield()?;
            }
            return Ok(Stmt::Unsupported("AnnAssign"));
        }
        self.last_value_span = self.span_since(first);
        self.record_span(first);
        Ok(Stmt::Expr(lhs))
    }

    /// Record the span of tokens `from..to` (exclusive of `to`).
    fn record_span_range(&mut self, from: usize, to: usize) {
        if to > from {
            let (a, b) = (self.toks[from].start, self.toks[to - 1].end);
            if a < b {
                self.spans.push((a, b));
            }
        }
    }

    fn span_since(&self, first_tok: usize) -> Option<(usize, usize)> {
        let last = self.pos.saturating_sub(1).max(first_tok);
        let (a, b) = (self.toks[first_tok].start, self.toks[last].end);
        (a < b).then_some((a, b))
    }

    fn at_stmt_end(&self) -> bool {
        matches!(self.peek(), Tok::Newline | Tok::End) || self.is_op(";")
    }

    fn aug_op(&self) -> Option<BinOp> {
        let Tok::Op(o) = self.peek() else { return None };
        Some(match *o {
            "+=" => BinOp::Add,
            "-=" => BinOp::Sub,
            "*=" => BinOp::Mult,
            "@=" => BinOp::MatMult,
            "/=" => BinOp::Div,
            "%=" => BinOp::Mod,
            "&=" => BinOp::BitAnd,
            "|=" => BinOp::BitOr,
            "^=" => BinOp::BitXor,
            "<<=" => BinOp::LShift,
            ">>=" => BinOp::RShift,
            "**=" => BinOp::Pow,
            "//=" => BinOp::FloorDiv,
            _ => return None,
        })
    }

    fn del_stmt(&mut self) -> PResult<Stmt> {
        let mut targets = Vec::new();
        loop {
            let e = self.bitwise_or_star()?;
            check_target(&e, Ctx::Del)?;
            targets.push(e);
            if self.eat_op(",") {
                if self.at_stmt_end() {
                    break;
                }
                continue;
            }
            break;
        }
        if !self.at_stmt_end() {
            return err("invalid syntax");
        }
        Ok(Stmt::Delete(targets))
    }

    fn dotted_name(&mut self) -> PResult<()> {
        self.name()?;
        while self.eat_op(".") {
            self.name()?;
        }
        Ok(())
    }

    fn import_names(&mut self) -> PResult<()> {
        loop {
            self.dotted_name()?;
            if self.eat_kw("as") {
                self.name()?;
            }
            if !self.eat_op(",") {
                break;
            }
        }
        Ok(())
    }

    fn import_from(&mut self) -> PResult<()> {
        let mut dots = 0;
        loop {
            if self.eat_op(".") {
                dots += 1;
            } else if self.eat_op("...") {
                dots += 3;
            } else {
                break;
            }
        }
        if !self.is_kw("import") {
            self.dotted_name()?;
        } else if dots == 0 {
            return err("invalid syntax");
        }
        self.expect_kw("import")?;
        if self.eat_op("*") {
            return Ok(());
        }
        let paren = self.eat_op("(");
        loop {
            self.name()?;
            if self.eat_kw("as") {
                self.name()?;
            }
            if !self.eat_op(",") {
                break;
            }
            if paren && self.is_op(")") {
                break;
            }
        }
        if paren {
            self.expect_op(")")?;
        }
        Ok(())
    }

    fn if_stmt(&mut self) -> PResult<Stmt> {
        self.advance(); // if / elif
        let f = self.pos;
        let test = self.named_expression()?;
        self.record_span(f);
        let body = self.block()?;
        let orelse = if self.is_kw("elif") {
            vec![self.if_stmt()?]
        } else if self.eat_kw("else") {
            self.block()?
        } else {
            Vec::new()
        };
        Ok(Stmt::If(test, body, orelse))
    }

    fn while_stmt(&mut self) -> PResult<Stmt> {
        self.advance();
        let f = self.pos;
        let test = self.named_expression()?;
        self.record_span(f);
        let body = self.block()?;
        let orelse = if self.eat_kw("else") {
            self.block()?
        } else {
            Vec::new()
        };
        Ok(Stmt::While(test, body, orelse))
    }

    fn for_stmt(&mut self) -> PResult<Stmt> {
        self.advance();
        let target = self.star_targets()?;
        self.expect_kw("in")?;
        let f = self.pos;
        let iter = self.star_expressions()?;
        self.record_span(f);
        let body = self.block()?;
        let orelse = if self.eat_kw("else") {
            self.block()?
        } else {
            Vec::new()
        };
        Ok(Stmt::For(target, iter, body, orelse))
    }

    /// `star_targets` for `for` and comprehensions: bitwise-or level
    /// expressions (so `in` is left for the caller) validated as targets.
    fn star_targets(&mut self) -> PResult<Expr> {
        let first = self.bitwise_or_star()?;
        let e = if self.is_op(",") {
            let mut elts = vec![first];
            while self.eat_op(",") {
                if self.is_kw("in") || self.is_op("=") {
                    break;
                }
                elts.push(self.bitwise_or_star()?);
            }
            Expr::Tuple(elts)
        } else {
            first
        };
        check_target(&e, Ctx::Store)?;
        Ok(e)
    }

    fn try_stmt(&mut self) -> PResult<Stmt> {
        self.advance();
        let body = self.block()?;
        let mut handlers = Vec::new();
        let mut star = None::<bool>;
        while self.is_kw("except") {
            self.advance();
            let is_star = self.eat_op("*");
            match star {
                None => star = Some(is_star),
                Some(s) if s != is_star => {
                    return err("cannot have both 'except' and 'except*' on the same 'try'");
                }
                _ => {}
            }
            let (type_, name) = if self.is_op(":") {
                if is_star {
                    return err("expected one or more exception types");
                }
                (None, None)
            } else {
                let t = self.expression()?;
                if self.is_op(",") {
                    return err("multiple exception types must be parenthesized");
                }
                let n = if self.eat_kw("as") {
                    Some(self.name()?)
                } else {
                    None
                };
                (Some(t), n)
            };
            let hbody = self.block()?;
            handlers.push(Handler {
                type_,
                name,
                body: hbody,
            });
        }
        let orelse = if !handlers.is_empty() && self.eat_kw("else") {
            self.block()?
        } else {
            Vec::new()
        };
        let finalbody = if self.eat_kw("finally") {
            self.block()?
        } else {
            Vec::new()
        };
        if handlers.is_empty() && finalbody.is_empty() {
            return err("expected 'except' or 'finally' block");
        }
        if star == Some(true) {
            return Ok(Stmt::Unsupported("TryStar"));
        }
        Ok(Stmt::Try(body, handlers, orelse, finalbody))
    }

    fn with_stmt(&mut self, is_async: bool) -> PResult<Stmt> {
        self.advance();
        let items = self.with_items()?;
        let body = self.block()?;
        if is_async {
            return Ok(Stmt::Unsupported("AsyncWith"));
        }
        Ok(Stmt::With(items, body))
    }

    fn with_items(&mut self) -> PResult<Vec<WithItem>> {
        // The parenthesised form first: `with (a as b, c):`.
        if self.is_op("(") {
            let save = self.pos;
            let attempt = (|| -> PResult<Vec<WithItem>> {
                self.advance();
                let mut items = Vec::new();
                loop {
                    items.push(self.with_item()?);
                    if !self.eat_op(",") {
                        break;
                    }
                    if self.is_op(")") {
                        break;
                    }
                }
                self.expect_op(")")?;
                if !self.is_op(":") {
                    return err("not a parenthesised with");
                }
                Ok(items)
            })();
            match attempt {
                Ok(items) => return Ok(items),
                Err(_) => self.pos = save,
            }
        }
        let mut items = vec![self.with_item()?];
        while self.eat_op(",") {
            items.push(self.with_item()?);
        }
        Ok(items)
    }

    fn with_item(&mut self) -> PResult<WithItem> {
        let f = self.pos;
        let context_expr = self.expression()?;
        self.record_span(f);
        let optional_vars = if self.eat_kw("as") {
            let t = self.star_target_single()?;
            Some(t)
        } else {
            None
        };
        Ok(WithItem {
            context_expr,
            optional_vars,
        })
    }

    fn star_target_single(&mut self) -> PResult<Expr> {
        let e = self.bitwise_or_star()?;
        check_target(&e, Ctx::Store)?;
        Ok(e)
    }

    fn async_stmt(&mut self) -> PResult<Stmt> {
        self.advance(); // async
        if self.is_kw("def") {
            self.funcdef(false)?;
            return Ok(Stmt::Unsupported("AsyncFunctionDef"));
        }
        if self.is_kw("for") {
            self.for_stmt()?;
            return Ok(Stmt::Unsupported("AsyncFor"));
        }
        if self.is_kw("with") {
            return self.with_stmt(true);
        }
        err("invalid syntax")
    }

    fn decorated(&mut self) -> PResult<Stmt> {
        while self.eat_op("@") {
            self.named_expression()?;
            if !matches!(self.peek(), Tok::Newline) {
                return err("invalid syntax");
            }
            self.advance();
        }
        match self.peek() {
            Tok::Name(n) if n == "def" => self.funcdef(true),
            Tok::Name(n) if n == "class" => self.classdef(),
            Tok::Name(n) if n == "async" => {
                self.advance();
                if !self.is_kw("def") {
                    return err("invalid syntax");
                }
                self.funcdef(true)?;
                Ok(Stmt::Unsupported("AsyncFunctionDef"))
            }
            _ => err("invalid syntax"),
        }
    }

    fn type_params(&mut self) -> PResult<()> {
        self.expect_op("[")?;
        loop {
            if self.eat_op("**") || self.eat_op("*") {
                self.name()?;
                if self.eat_op("=") {
                    self.expression()?;
                }
            } else {
                self.name()?;
                if self.eat_op(":") {
                    self.expression()?;
                }
                if self.eat_op("=") {
                    self.expression()?;
                }
            }
            if !self.eat_op(",") {
                break;
            }
            if self.is_op("]") {
                break;
            }
        }
        self.expect_op("]")
    }

    fn funcdef(&mut self, has_decorators: bool) -> PResult<Stmt> {
        self.advance(); // def
        let name = self.name()?;
        if self.is_op("[") {
            self.type_params()?;
        }
        self.expect_op("(")?;
        let params = self.parameters(true, ")")?;
        self.expect_op(")")?;
        if self.eat_op("->") {
            self.expression()?;
        }
        let body = self.block()?;
        Ok(Stmt::FunctionDef(Arc::new(FuncDef {
            name,
            params,
            body,
            lambda_body: None,
            has_decorators,
        })))
    }

    fn classdef(&mut self) -> PResult<Stmt> {
        self.advance();
        self.name()?;
        if self.is_op("[") {
            self.type_params()?;
        }
        if self.eat_op("(") {
            self.call_arguments()?;
            self.expect_op(")")?;
        }
        self.block()?;
        Ok(Stmt::Unsupported("ClassDef"))
    }

    /// Parameters up to (not including) the closing token. `annotations`
    /// allows `name: expr` (for `def`, not `lambda`).
    fn parameters(&mut self, annotations: bool, close: &str) -> PResult<Params> {
        let mut p = Params::default();
        let mut seen_default = false;
        let mut seen_star = false;
        let mut seen_slash = false;
        let mut seen_kwarg = false;
        loop {
            if self.is_op(close) {
                break;
            }
            if seen_kwarg {
                return err("arguments cannot follow var-keyword argument");
            }
            if self.eat_op("/") {
                if seen_slash || seen_star || p.args.is_empty() {
                    return err("invalid syntax: '/'");
                }
                seen_slash = true;
            } else if self.eat_op("**") {
                let n = self.name()?;
                if annotations && self.eat_op(":") {
                    self.expression()?;
                }
                if self.is_op("=") {
                    return err("var-keyword argument cannot have default value");
                }
                p.kwarg = Some(n);
                seen_kwarg = true;
            } else if self.eat_op("*") {
                if seen_star {
                    return err("* argument may appear only once");
                }
                seen_star = true;
                if self.is_op(",") {
                    // bare `*`: a named parameter must follow.
                    let next_is_name = matches!(self.peek_at(1), Tok::Name(n) if !is_keyword(n));
                    if !next_is_name {
                        return err("named arguments must follow bare *");
                    }
                } else if self.is_op(close) {
                    return err("named arguments must follow bare *");
                } else {
                    let n = self.name()?;
                    if annotations && self.eat_op(":") {
                        // `*args: *Ts` is allowed.
                        if self.eat_op("*") {
                            self.bitwise_or()?;
                        } else {
                            self.expression()?;
                        }
                    }
                    if self.is_op("=") {
                        return err("var-positional argument cannot have default value");
                    }
                    p.vararg = Some(n);
                }
            } else {
                let n = self.name()?;
                if annotations && self.eat_op(":") {
                    self.expression()?;
                }
                let has_default = if self.eat_op("=") {
                    let d = self.expression()?;
                    if seen_star {
                        // keyword-only default: ignored by Procedure.
                    } else {
                        p.defaults.push(d);
                    }
                    true
                } else {
                    false
                };
                if seen_star {
                    p.kwonly += 1;
                } else {
                    if !has_default && seen_default {
                        return err("non-default argument follows default argument");
                    }
                    seen_default |= has_default;
                    p.args.push(n);
                }
            }
            if !self.eat_op(",") {
                break;
            }
        }
        Ok(p)
    }

    // --- match ------------------------------------------------------------

    /// `match` is a soft keyword: try the statement, else give the position
    /// back so it parses as an ordinary expression statement.
    fn try_match(&mut self) -> PResult<Option<Stmt>> {
        let save = self.pos;
        let attempt = (|| -> PResult<()> {
            self.advance(); // match
            let first = self.star_named_expression()?;
            if self.is_op(",") {
                let _ = first;
                while self.eat_op(",") {
                    if self.is_op(":") {
                        break;
                    }
                    self.star_named_expression()?;
                }
            }
            self.expect_op(":")?;
            if !matches!(self.peek(), Tok::Newline) {
                return err("not a match");
            }
            self.advance();
            if !matches!(self.peek(), Tok::Indent) {
                return err("not a match");
            }
            self.advance();
            if !self.is_kw("case") {
                return err("not a match");
            }
            Ok(())
        })();
        if attempt.is_err() {
            self.pos = save;
            return Ok(None);
        }
        // Committed: parse the case blocks.
        while self.is_kw("case") {
            self.advance();
            self.patterns()?;
            if self.eat_kw("if") {
                self.named_expression()?;
            }
            self.block()?;
            self.eat_newline_opt();
        }
        if !matches!(self.peek(), Tok::Dedent) {
            return err("invalid syntax");
        }
        self.advance();
        Ok(Some(Stmt::Unsupported("Match")))
    }

    fn patterns(&mut self) -> PResult<()> {
        self.maybe_star_pattern()?;
        while self.eat_op(",") {
            if self.is_op(":") || self.is_kw("if") {
                break;
            }
            self.maybe_star_pattern()?;
        }
        Ok(())
    }

    fn maybe_star_pattern(&mut self) -> PResult<()> {
        if self.eat_op("*") {
            self.name()?;
            return Ok(());
        }
        self.as_pattern()
    }

    fn as_pattern(&mut self) -> PResult<()> {
        self.or_pattern()?;
        if self.eat_kw("as") {
            self.name()?;
        }
        Ok(())
    }

    fn or_pattern(&mut self) -> PResult<()> {
        self.closed_pattern()?;
        while self.eat_op("|") {
            self.closed_pattern()?;
        }
        Ok(())
    }

    fn closed_pattern(&mut self) -> PResult<()> {
        self.enter()?;
        let r = self.closed_pattern_inner();
        self.leave();
        r
    }

    fn closed_pattern_inner(&mut self) -> PResult<()> {
        match self.peek().clone() {
            Tok::Number(_) => {
                self.advance();
                if self.is_op("+") || self.is_op("-") {
                    self.advance();
                    if !matches!(self.peek(), Tok::Number(_)) {
                        return err("invalid pattern");
                    }
                    self.advance();
                }
                Ok(())
            }
            Tok::Op("-") => {
                self.advance();
                if !matches!(self.peek(), Tok::Number(_)) {
                    return err("invalid pattern");
                }
                self.advance();
                if self.is_op("+") || self.is_op("-") {
                    self.advance();
                    if !matches!(self.peek(), Tok::Number(_)) {
                        return err("invalid pattern");
                    }
                    self.advance();
                }
                Ok(())
            }
            Tok::Str(_) => {
                self.strings()?;
                Ok(())
            }
            Tok::Name(n) if matches!(n.as_str(), "None" | "True" | "False") => {
                self.advance();
                Ok(())
            }
            Tok::Name(n) if !is_keyword(&n) => {
                self.advance();
                let mut dotted = false;
                while self.eat_op(".") {
                    dotted = true;
                    self.name()?;
                }
                if self.eat_op("(") {
                    // class pattern
                    loop {
                        if self.is_op(")") {
                            break;
                        }
                        if matches!(self.peek(), Tok::Name(_)) && self.is_op_at(1, "=") {
                            self.advance();
                            self.advance();
                        }
                        self.as_pattern()?;
                        if !self.eat_op(",") {
                            break;
                        }
                    }
                    self.expect_op(")")?;
                } else if dotted {
                    // value pattern
                }
                Ok(())
            }
            Tok::Op("(") | Tok::Op("[") => {
                let close = if self.is_op("(") { ")" } else { "]" };
                self.advance();
                loop {
                    if self.is_op(close) {
                        break;
                    }
                    self.maybe_star_pattern()?;
                    if !self.eat_op(",") {
                        break;
                    }
                }
                self.expect_op(close)
            }
            Tok::Op("{") => {
                self.advance();
                loop {
                    if self.is_op("}") {
                        break;
                    }
                    if self.eat_op("**") {
                        self.name()?;
                    } else {
                        match self.peek().clone() {
                            Tok::Number(_) | Tok::Str(_) | Tok::Op("-") => self.closed_pattern()?,
                            Tok::Name(_) => {
                                self.advance();
                                while self.eat_op(".") {
                                    self.name()?;
                                }
                            }
                            _ => return err("invalid pattern"),
                        }
                        self.expect_op(":")?;
                        self.as_pattern()?;
                    }
                    if !self.eat_op(",") {
                        break;
                    }
                }
                self.expect_op("}")
            }
            _ => err("invalid pattern"),
        }
    }

    // --- expressions ------------------------------------------------------

    fn star_expressions_or_yield(&mut self) -> PResult<Expr> {
        if self.is_kw("yield") {
            return self.yield_expr();
        }
        self.star_expressions()
    }

    fn yield_expr(&mut self) -> PResult<Expr> {
        self.advance();
        if self.eat_kw("from") {
            self.expression()?;
            return Ok(Expr::Unsupported("YieldFrom"));
        }
        if !self.at_stmt_end() && !self.is_op(")") && !self.is_op("=") && !self.is_op("]") && !self.is_op("}") {
            self.star_expressions()?;
        }
        Ok(Expr::Unsupported("Yield"))
    }

    /// `star_expressions`: a comma-separated list becomes a tuple.
    fn star_expressions(&mut self) -> PResult<Expr> {
        let first = self.star_expression()?;
        if !self.is_op(",") {
            return Ok(first);
        }
        let mut elts = vec![first];
        while self.eat_op(",") {
            if !self.starts_expression() {
                break;
            }
            elts.push(self.star_expression()?);
        }
        Ok(Expr::Tuple(elts))
    }

    /// Whether the next token can start an expression (used after a comma).
    fn starts_expression(&self) -> bool {
        match self.peek() {
            Tok::Name(n) => !is_keyword(n) || matches!(n.as_str(), "None" | "True" | "False" | "not" | "lambda" | "await" | "yield"),
            Tok::Number(_) | Tok::Str(_) => true,
            Tok::Op(o) => matches!(*o, "(" | "[" | "{" | "-" | "+" | "~" | "..." | "*"),
            _ => false,
        }
    }

    fn star_expression(&mut self) -> PResult<Expr> {
        if self.eat_op("*") {
            let e = self.bitwise_or()?;
            return Ok(Expr::Starred(Box::new(e)));
        }
        self.expression()
    }

    fn star_named_expression(&mut self) -> PResult<Expr> {
        if self.eat_op("*") {
            let e = self.bitwise_or()?;
            return Ok(Expr::Starred(Box::new(e)));
        }
        self.named_expression()
    }

    /// `bitwise_or` or a starred one, for targets.
    fn bitwise_or_star(&mut self) -> PResult<Expr> {
        if self.eat_op("*") {
            let inner = self.bitwise_or()?;
            return Ok(Expr::Starred(Box::new(inner)));
        }
        self.bitwise_or()
    }

    fn named_expression(&mut self) -> PResult<Expr> {
        if matches!(self.peek(), Tok::Name(n) if !is_keyword(n)) && self.is_op_at(1, ":=") {
            self.advance();
            self.advance();
            self.expression()?;
            return Ok(Expr::Unsupported("NamedExpr"));
        }
        let e = self.expression()?;
        if self.is_op(":=") {
            return err("cannot use assignment expressions with this target");
        }
        Ok(e)
    }

    fn expression(&mut self) -> PResult<Expr> {
        self.enter()?;
        let r = self.expression_inner();
        self.leave();
        r
    }

    fn expression_inner(&mut self) -> PResult<Expr> {
        if self.is_kw("lambda") {
            return self.lambdef();
        }
        let body = self.disjunction()?;
        if self.is_kw("if") {
            self.advance();
            let test = self.disjunction()?;
            if !self.eat_kw("else") {
                return err("expected 'else' after 'if' expression");
            }
            let orelse = self.expression()?;
            return Ok(Expr::IfExp(Box::new(test), Box::new(body), Box::new(orelse)));
        }
        Ok(body)
    }

    fn lambdef(&mut self) -> PResult<Expr> {
        self.advance();
        let params = self.parameters(false, ":")?;
        self.expect_op(":")?;
        let body = self.expression()?;
        Ok(Expr::Lambda(Arc::new(FuncDef {
            name: "lambda".into(),
            params,
            body: Vec::new(),
            lambda_body: Some(body),
            has_decorators: false,
        })))
    }

    fn disjunction(&mut self) -> PResult<Expr> {
        let first = self.conjunction()?;
        if !self.is_kw("or") {
            return Ok(first);
        }
        let mut values = vec![first];
        while self.eat_kw("or") {
            values.push(self.conjunction()?);
        }
        Ok(Expr::BoolOp(BoolOp::Or, values))
    }

    fn conjunction(&mut self) -> PResult<Expr> {
        let first = self.inversion()?;
        if !self.is_kw("and") {
            return Ok(first);
        }
        let mut values = vec![first];
        while self.eat_kw("and") {
            values.push(self.inversion()?);
        }
        Ok(Expr::BoolOp(BoolOp::And, values))
    }

    fn inversion(&mut self) -> PResult<Expr> {
        if self.is_kw("not") {
            self.enter()?;
            self.advance();
            let e = self.inversion();
            self.leave();
            return Ok(Expr::UnaryOp(UnaryOp::Not, Box::new(e?)));
        }
        self.comparison()
    }

    fn cmp_op(&mut self) -> Option<CmpOp> {
        let op = match self.peek() {
            Tok::Op("==") => CmpOp::Eq,
            Tok::Op("!=") => CmpOp::NotEq,
            Tok::Op("<") => CmpOp::Lt,
            Tok::Op("<=") => CmpOp::LtE,
            Tok::Op(">") => CmpOp::Gt,
            Tok::Op(">=") => CmpOp::GtE,
            Tok::Name(n) if n == "in" => CmpOp::In,
            Tok::Name(n) if n == "is" => {
                if self.is_kw_at(1, "not") {
                    self.advance();
                    CmpOp::IsNot
                } else {
                    CmpOp::Is
                }
            }
            Tok::Name(n) if n == "not" && self.is_kw_at(1, "in") => {
                self.advance();
                CmpOp::NotIn
            }
            _ => return None,
        };
        self.advance();
        Some(op)
    }

    fn comparison(&mut self) -> PResult<Expr> {
        let left = self.bitwise_or()?;
        let mut rest = Vec::new();
        while let Some(op) = self.cmp_op() {
            rest.push((op, self.bitwise_or()?));
        }
        if rest.is_empty() {
            Ok(left)
        } else {
            Ok(Expr::Compare(Box::new(left), rest))
        }
    }

    fn bitwise_or(&mut self) -> PResult<Expr> {
        let mut left = self.bitwise_xor()?;
        while self.is_op("|") {
            self.advance();
            let r = self.bitwise_xor()?;
            left = Expr::BinOp(Box::new(left), BinOp::BitOr, Box::new(r));
        }
        Ok(left)
    }

    fn bitwise_xor(&mut self) -> PResult<Expr> {
        let mut left = self.bitwise_and()?;
        while self.is_op("^") {
            self.advance();
            let r = self.bitwise_and()?;
            left = Expr::BinOp(Box::new(left), BinOp::BitXor, Box::new(r));
        }
        Ok(left)
    }

    fn bitwise_and(&mut self) -> PResult<Expr> {
        let mut left = self.shift_expr()?;
        while self.is_op("&") {
            self.advance();
            let r = self.shift_expr()?;
            left = Expr::BinOp(Box::new(left), BinOp::BitAnd, Box::new(r));
        }
        Ok(left)
    }

    fn shift_expr(&mut self) -> PResult<Expr> {
        let mut left = self.sum()?;
        loop {
            let op = if self.is_op("<<") {
                BinOp::LShift
            } else if self.is_op(">>") {
                BinOp::RShift
            } else {
                break;
            };
            self.advance();
            let r = self.sum()?;
            left = Expr::BinOp(Box::new(left), op, Box::new(r));
        }
        Ok(left)
    }

    fn sum(&mut self) -> PResult<Expr> {
        let mut left = self.term()?;
        loop {
            let op = if self.is_op("+") {
                BinOp::Add
            } else if self.is_op("-") {
                BinOp::Sub
            } else {
                break;
            };
            self.advance();
            let r = self.term()?;
            left = Expr::BinOp(Box::new(left), op, Box::new(r));
        }
        Ok(left)
    }

    fn term(&mut self) -> PResult<Expr> {
        let mut left = self.factor()?;
        loop {
            let op = match self.peek() {
                Tok::Op("*") => BinOp::Mult,
                Tok::Op("/") => BinOp::Div,
                Tok::Op("//") => BinOp::FloorDiv,
                Tok::Op("%") => BinOp::Mod,
                Tok::Op("@") => BinOp::MatMult,
                _ => break,
            };
            self.advance();
            let r = self.factor()?;
            left = Expr::BinOp(Box::new(left), op, Box::new(r));
        }
        Ok(left)
    }

    fn factor(&mut self) -> PResult<Expr> {
        let op = match self.peek() {
            Tok::Op("+") => Some(UnaryOp::UAdd),
            Tok::Op("-") => Some(UnaryOp::USub),
            Tok::Op("~") => Some(UnaryOp::Invert),
            _ => None,
        };
        if let Some(op) = op {
            self.enter()?;
            self.advance();
            let e = self.factor();
            self.leave();
            return Ok(Expr::UnaryOp(op, Box::new(e?)));
        }
        self.power()
    }

    fn power(&mut self) -> PResult<Expr> {
        let base = self.await_primary()?;
        if self.is_op("**") {
            self.enter()?;
            self.advance();
            let exp = self.factor();
            self.leave();
            return Ok(Expr::BinOp(Box::new(base), BinOp::Pow, Box::new(exp?)));
        }
        Ok(base)
    }

    fn await_primary(&mut self) -> PResult<Expr> {
        if self.is_kw("await") {
            self.advance();
            self.primary()?;
            return Ok(Expr::Unsupported("Await"));
        }
        self.primary()
    }

    fn primary(&mut self) -> PResult<Expr> {
        let mut e = self.atom()?;
        loop {
            if self.is_op(".") {
                self.advance();
                let attr = self.name()?;
                e = Expr::Attribute(Box::new(e), attr);
            } else if self.is_op("(") {
                self.advance();
                let (args, kws) = self.call_arguments()?;
                self.expect_op(")")?;
                e = Expr::Call(Box::new(e), args, kws);
            } else if self.is_op("[") {
                self.advance();
                let s = self.slices()?;
                self.expect_op("]")?;
                e = Expr::Subscript(Box::new(e), Box::new(s));
            } else {
                break;
            }
        }
        Ok(e)
    }

    fn slices(&mut self) -> PResult<Expr> {
        let first = self.slice_or_star()?;
        if !self.is_op(",") {
            return Ok(first);
        }
        let mut elts = vec![first];
        while self.eat_op(",") {
            if self.is_op("]") {
                break;
            }
            elts.push(self.slice_or_star()?);
        }
        Ok(Expr::Tuple(elts))
    }

    fn slice_or_star(&mut self) -> PResult<Expr> {
        if self.eat_op("*") {
            let e = self.bitwise_or()?;
            return Ok(Expr::Starred(Box::new(e)));
        }
        let lower = if self.is_op(":") {
            None
        } else {
            let e = self.named_expression()?;
            if !self.is_op(":") {
                return Ok(e);
            }
            Some(Box::new(e))
        };
        self.expect_op(":")?;
        let upper = if self.is_op(":") || self.is_op(",") || self.is_op("]") {
            None
        } else {
            Some(Box::new(self.expression()?))
        };
        let step = if self.eat_op(":") {
            if self.is_op(",") || self.is_op("]") {
                None
            } else {
                Some(Box::new(self.expression()?))
            }
        } else {
            None
        };
        Ok(Expr::Slice(lower, upper, step))
    }

    /// Call arguments up to the `)`.
    fn call_arguments(&mut self) -> PResult<(Vec<Expr>, Vec<Keyword>)> {
        let mut args = Vec::new();
        let mut kws: Vec<Keyword> = Vec::new();
        let mut seen_kw = false;
        let mut seen_dstar = false;
        loop {
            if self.is_op(")") {
                break;
            }
            if self.eat_op("**") {
                let v = self.expression()?;
                kws.push(Keyword { arg: None, value: v });
                seen_kw = true;
                seen_dstar = true;
            } else if self.eat_op("*") {
                if seen_dstar {
                    return err("iterable argument unpacking follows keyword argument unpacking");
                }
                self.expression()?;
                args.push(Expr::Starred(Box::new(Expr::Constant(Const::None))));
            } else if matches!(self.peek(), Tok::Name(n) if !is_keyword(n)) && self.is_op_at(1, "=") {
                let Tok::Name(n) = self.peek().clone() else { unreachable!() };
                self.advance();
                self.advance();
                let v = self.expression()?;
                kws.push(Keyword { arg: Some(n), value: v });
                seen_kw = true;
            } else {
                let e = self.named_expression()?;
                if self.is_op("=") {
                    return err("expression cannot contain assignment, perhaps you meant \"==\"?");
                }
                // A bare generator expression argument.
                if self.is_kw("for") || (self.is_kw("async") && self.is_kw_at(1, "for")) {
                    let _ = self.comp_clauses()?;
                    if !self.is_op(")") || !args.is_empty() || seen_kw {
                        return err("Generator expression must be parenthesized");
                    }
                    args.push(Expr::Unsupported("GeneratorExp"));
                } else {
                    if seen_kw {
                        return err(if seen_dstar {
                            "positional argument follows keyword argument unpacking"
                        } else {
                            "positional argument follows keyword argument"
                        });
                    }
                    args.push(e);
                }
            }
            if !self.eat_op(",") {
                break;
            }
        }
        Ok((args, kws))
    }

    fn comp_clauses(&mut self) -> PResult<Vec<Comprehension>> {
        let mut gens = Vec::new();
        loop {
            let is_async = self.is_kw("async") && self.is_kw_at(1, "for");
            if is_async {
                self.advance();
            }
            if !self.is_kw("for") {
                break;
            }
            self.advance();
            let target = self.star_targets()?;
            self.expect_kw("in")?;
            // `iter` is a disjunction (no ternary, no lambda, no walrus).
            let iter = self.disjunction()?;
            let mut ifs = Vec::new();
            while self.is_kw("if") {
                self.advance();
                ifs.push(self.disjunction_or_named()?);
            }
            gens.push(Comprehension { target, iter, ifs });
        }
        if gens.is_empty() {
            return err("invalid syntax");
        }
        Ok(gens)
    }

    fn disjunction_or_named(&mut self) -> PResult<Expr> {
        if matches!(self.peek(), Tok::Name(n) if !is_keyword(n)) && self.is_op_at(1, ":=") {
            self.advance();
            self.advance();
            self.disjunction()?;
            return Ok(Expr::Unsupported("NamedExpr"));
        }
        self.disjunction()
    }

    fn atom(&mut self) -> PResult<Expr> {
        self.enter()?;
        let r = self.atom_inner();
        self.leave();
        r
    }

    fn atom_inner(&mut self) -> PResult<Expr> {
        let tok = self.toks[self.pos].clone();
        match &tok.tok {
            Tok::Name(n) => match n.as_str() {
                "None" => {
                    self.advance();
                    Ok(Expr::Constant(Const::None))
                }
                "True" => {
                    self.advance();
                    Ok(Expr::Constant(Const::Bool(true)))
                }
                "False" => {
                    self.advance();
                    Ok(Expr::Constant(Const::Bool(false)))
                }
                n if is_keyword(n) => err("invalid syntax"),
                n => {
                    self.advance();
                    Ok(Expr::Name(
                        n.to_string(),
                        Pos {
                            line: tok.line,
                            end_line: tok.end_line,
                            col: tok.col,
                        },
                    ))
                }
            },
            Tok::Number(num) => {
                self.advance();
                Ok(Expr::Constant(match num {
                    Num::Int(i) => Const::Int(*i),
                    Num::Big(s) => Const::BigInt(s.clone()),
                    Num::Float(f) => Const::Float(*f),
                    Num::Imag(f) => Const::Complex(*f),
                }))
            }
            Tok::Str(_) => self.strings(),
            Tok::Op("...") => {
                self.advance();
                Ok(Expr::Constant(Const::Ellipsis))
            }
            Tok::Op("(") => self.paren_atom(),
            Tok::Op("[") => self.list_atom(),
            Tok::Op("{") => self.brace_atom(),
            _ => err("invalid syntax"),
        }
    }

    fn paren_atom(&mut self) -> PResult<Expr> {
        self.advance();
        if self.eat_op(")") {
            return Ok(Expr::Tuple(Vec::new()));
        }
        if self.is_kw("yield") {
            let y = self.yield_expr()?;
            self.expect_op(")")?;
            return Ok(y);
        }
        let first = self.star_named_expression()?;
        if self.is_kw("for") || (self.is_kw("async") && self.is_kw_at(1, "for")) {
            if matches!(first, Expr::Starred(_)) {
                return err("iterable unpacking cannot be used in comprehension");
            }
            self.comp_clauses()?;
            self.expect_op(")")?;
            return Ok(Expr::Unsupported("GeneratorExp"));
        }
        if self.is_op(",") {
            let mut elts = vec![first];
            while self.eat_op(",") {
                if self.is_op(")") {
                    break;
                }
                elts.push(self.star_named_expression()?);
            }
            self.expect_op(")")?;
            return Ok(Expr::Tuple(elts));
        }
        self.expect_op(")")?;
        if matches!(first, Expr::Starred(_)) {
            return err("cannot use starred expression here");
        }
        Ok(first)
    }

    fn list_atom(&mut self) -> PResult<Expr> {
        self.advance();
        if self.eat_op("]") {
            return Ok(Expr::List(Vec::new()));
        }
        let first = self.star_named_expression()?;
        if self.is_kw("for") || (self.is_kw("async") && self.is_kw_at(1, "for")) {
            if matches!(first, Expr::Starred(_)) {
                return err("iterable unpacking cannot be used in comprehension");
            }
            let gens = self.comp_clauses()?;
            self.expect_op("]")?;
            return Ok(Expr::ListComp(Box::new(first), gens));
        }
        let mut elts = vec![first];
        while self.eat_op(",") {
            if self.is_op("]") {
                break;
            }
            elts.push(self.star_named_expression()?);
        }
        self.expect_op("]")?;
        Ok(Expr::List(elts))
    }

    fn brace_atom(&mut self) -> PResult<Expr> {
        self.advance();
        if self.eat_op("}") {
            return Ok(Expr::Dict(Vec::new()));
        }
        // `**mapping` first: a dict.
        if self.is_op("**") {
            return self.dict_rest(None);
        }
        let first = self.star_named_expression()?;
        if self.is_op(":") && !matches!(first, Expr::Starred(_)) {
            self.advance();
            let value = self.expression()?;
            if self.is_kw("for") || (self.is_kw("async") && self.is_kw_at(1, "for")) {
                let gens = self.comp_clauses()?;
                self.expect_op("}")?;
                return Ok(Expr::DictComp(Box::new(first), Box::new(value), gens));
            }
            return self.dict_rest(Some((first, value)));
        }
        // A set.
        if self.is_kw("for") || (self.is_kw("async") && self.is_kw_at(1, "for")) {
            if matches!(first, Expr::Starred(_)) {
                return err("iterable unpacking cannot be used in comprehension");
            }
            let gens = self.comp_clauses()?;
            self.expect_op("}")?;
            return Ok(Expr::SetComp(Box::new(first), gens));
        }
        let mut elts = vec![first];
        while self.eat_op(",") {
            if self.is_op("}") {
                break;
            }
            elts.push(self.star_named_expression()?);
        }
        self.expect_op("}")?;
        Ok(Expr::Set(elts))
    }

    fn dict_rest(&mut self, first: Option<(Expr, Expr)>) -> PResult<Expr> {
        let mut items: Vec<(Option<Expr>, Expr)> = Vec::new();
        let had_first = first.is_some();
        if let Some((k, v)) = first {
            items.push((Some(k), v));
        }
        let mut need_comma = had_first;
        loop {
            if need_comma {
                if !self.eat_op(",") {
                    break;
                }
                if self.is_op("}") {
                    break;
                }
            }
            need_comma = true;
            if self.eat_op("**") {
                let v = self.bitwise_or()?;
                items.push((None, v));
            } else {
                let k = self.expression()?;
                self.expect_op(":")?;
                let v = self.expression()?;
                items.push((Some(k), v));
            }
        }
        self.expect_op("}")?;
        Ok(Expr::Dict(items))
    }

    // --- strings ----------------------------------------------------------

    fn strings(&mut self) -> PResult<Expr> {
        let mut toks: Vec<StrTok> = Vec::new();
        while let Tok::Str(s) = self.peek() {
            toks.push(s.clone());
            self.advance();
        }
        let any_bytes = toks.iter().any(|t| t.bytes);
        if any_bytes {
            if !toks.iter().all(|t| t.bytes) {
                return err("cannot mix bytes and nonbytes literals");
            }
            let mut out = Vec::new();
            for t in &toks {
                out.extend(decode_bytes(&t.body, t.raw)?);
            }
            return Ok(Expr::Constant(Const::Bytes(out.into())));
        }
        if toks.iter().any(|t| t.fstring) {
            let mut parts: Vec<FPart> = Vec::new();
            for t in &toks {
                let these = if t.fstring {
                    parse_fstring(&t.body, t.raw)?
                } else {
                    vec![FPart::Lit(decode_str(&t.body, t.raw)?)]
                };
                for p in these {
                    match (parts.last_mut(), p) {
                        (Some(FPart::Lit(a)), FPart::Lit(b)) => a.push_str(&b),
                        (_, p) => parts.push(p),
                    }
                }
            }
            return Ok(Expr::JoinedStr(parts));
        }
        let mut out = String::new();
        for t in &toks {
            out.push_str(&decode_str(&t.body, t.raw)?);
        }
        Ok(Expr::Constant(Const::Str(out.into())))
    }
}

// --- target validation ----------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ctx {
    Store,
    Del,
}

/// The validity rules of `star_targets` / `del_targets`.
fn check_target(e: &Expr, ctx: Ctx) -> PResult<()> {
    match e {
        Expr::Name(..) | Expr::Attribute(..) | Expr::Subscript(..) => Ok(()),
        Expr::Tuple(elts) | Expr::List(elts) => {
            for el in elts {
                check_target(el, ctx)?;
            }
            Ok(())
        }
        Expr::Starred(inner) if ctx == Ctx::Store => match inner.as_ref() {
            Expr::Starred(_) => err("cannot assign to starred expression"),
            other => check_target(other, ctx),
        },
        Expr::Starred(_) => err("cannot delete starred"),
        Expr::Constant(Const::None | Const::Bool(_)) => err("cannot assign to a keyword"),
        _ => err("cannot assign to expression"),
    }
}

// --- string literals --------------------------------------------------------

fn hex_val(chars: &[char], i: usize, n: usize) -> PResult<u32> {
    if i + n > chars.len() {
        return err("truncated escape");
    }
    let s: String = chars[i..i + n].iter().collect();
    u32::from_str_radix(&s, 16).map_err(|_| "invalid escape".to_string())
}

/// Decode a (non-bytes) string literal body.
pub(super) fn decode_str(body: &str, raw: bool) -> PResult<String> {
    if raw || !body.contains('\\') {
        return Ok(body.to_string());
    }
    let chars: Vec<char> = body.chars().collect();
    let mut out = String::with_capacity(body.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c != '\\' {
            out.push(c);
            i += 1;
            continue;
        }
        i += 1;
        let Some(&e) = chars.get(i) else {
            return err("trailing backslash");
        };
        i += 1;
        match e {
            '\n' => {}
            '\\' => out.push('\\'),
            '\'' => out.push('\''),
            '"' => out.push('"'),
            'a' => out.push('\x07'),
            'b' => out.push('\x08'),
            'f' => out.push('\x0c'),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            'v' => out.push('\x0b'),
            '0'..='7' => {
                let mut v = e.to_digit(8).expect("octal");
                let mut n = 1;
                while n < 3 && chars.get(i).is_some_and(|d| d.is_digit(8)) {
                    v = v * 8 + chars[i].to_digit(8).expect("octal");
                    i += 1;
                    n += 1;
                }
                out.push(char::from_u32(v).ok_or("bad octal escape")?);
            }
            'x' => {
                let v = hex_val(&chars, i, 2)?;
                i += 2;
                out.push(char::from_u32(v).ok_or("bad escape")?);
            }
            'u' => {
                let v = hex_val(&chars, i, 4)?;
                i += 4;
                out.push(char::from_u32(v).ok_or("illegal Unicode character")?);
            }
            'U' => {
                let v = hex_val(&chars, i, 8)?;
                i += 8;
                out.push(char::from_u32(v).ok_or("illegal Unicode character")?);
            }
            'N' => return err("\\N{...} escapes are not supported"),
            other => {
                out.push('\\');
                out.push(other);
            }
        }
    }
    Ok(out)
}

fn decode_bytes(body: &str, raw: bool) -> PResult<Vec<u8>> {
    if let Some(c) = body.chars().find(|c| !c.is_ascii()) {
        let _ = c;
        return err("bytes can only contain ASCII literal characters");
    }
    if raw || !body.contains('\\') {
        return Ok(body.as_bytes().to_vec());
    }
    let chars: Vec<char> = body.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c != '\\' {
            out.push(c as u8);
            i += 1;
            continue;
        }
        i += 1;
        let Some(&e) = chars.get(i) else {
            return err("trailing backslash");
        };
        i += 1;
        match e {
            '\n' => {}
            '\\' => out.push(b'\\'),
            '\'' => out.push(b'\''),
            '"' => out.push(b'"'),
            'a' => out.push(7),
            'b' => out.push(8),
            'f' => out.push(12),
            'n' => out.push(b'\n'),
            'r' => out.push(b'\r'),
            't' => out.push(b'\t'),
            'v' => out.push(11),
            '0'..='7' => {
                let mut v = e.to_digit(8).expect("octal");
                let mut n = 1;
                while n < 3 && chars.get(i).is_some_and(|d| d.is_digit(8)) {
                    v = v * 8 + chars[i].to_digit(8).expect("octal");
                    i += 1;
                    n += 1;
                }
                out.push((v & 0xff) as u8);
            }
            'x' => {
                let v = hex_val(&chars, i, 2)?;
                i += 2;
                out.push(v as u8);
            }
            other => {
                out.push(b'\\');
                out.push(other as u8);
            }
        }
    }
    Ok(out)
}

// --- f-strings --------------------------------------------------------------

/// Split an f-string body into literal and replacement-field parts.
pub(super) fn parse_fstring(body: &str, raw: bool) -> PResult<Vec<FPart>> {
    let chars: Vec<char> = body.chars().collect();
    let (parts, end) = parse_fstring_chars(&chars, 0, raw, false)?;
    debug_assert_eq!(end, chars.len());
    Ok(parts)
}

/// Parse literal text and fields from `i`; stops at the end, or at a `}` when
/// `in_spec` (a nested format spec). Returns the parts and the stop index.
fn parse_fstring_chars(
    chars: &[char],
    mut i: usize,
    raw: bool,
    in_spec: bool,
) -> PResult<(Vec<FPart>, usize)> {
    let n = chars.len();
    let mut parts: Vec<FPart> = Vec::new();
    let mut lit = String::new();
    let flush = |lit: &mut String, parts: &mut Vec<FPart>| -> PResult<()> {
        if !lit.is_empty() {
            let text = std::mem::take(lit);
            let decoded = decode_str(&text, raw)?;
            match parts.last_mut() {
                Some(FPart::Lit(a)) => a.push_str(&decoded),
                _ => parts.push(FPart::Lit(decoded)),
            }
        }
        Ok(())
    };
    while i < n {
        let c = chars[i];
        match c {
            '{' => {
                if !in_spec && chars.get(i + 1) == Some(&'{') {
                    lit.push('{');
                    i += 2;
                    continue;
                }
                flush(&mut lit, &mut parts)?;
                let (field, next) = parse_field(chars, i + 1, raw)?;
                parts.extend(field);
                i = next;
            }
            '}' => {
                if in_spec {
                    break;
                }
                if chars.get(i + 1) == Some(&'}') {
                    lit.push('}');
                    i += 2;
                    continue;
                }
                return err("f-string: single '}' is not allowed");
            }
            '\\' if !raw => {
                // Keep the escape for decode_str, but a `\N{...}` consumes its braces.
                lit.push('\\');
                if let Some(&nx) = chars.get(i + 1) {
                    if nx == 'N' && chars.get(i + 2) == Some(&'{') {
                        return err("\\N{...} escapes are not supported");
                    }
                    lit.push(nx);
                    i += 2;
                } else {
                    i += 1;
                }
            }
            _ => {
                lit.push(c);
                i += 1;
            }
        }
    }
    flush(&mut lit, &mut parts)?;
    Ok((parts, i))
}

/// Parse one replacement field starting after its `{`; returns the parts it
/// contributes (one `Field`, preceded by the debug text for `{x=}`) and the
/// index after the closing `}`.
fn parse_field(chars: &[char], start: usize, raw: bool) -> PResult<(Vec<FPart>, usize)> {
    let n = chars.len();
    let mut i = start;
    let mut stack: Vec<char> = Vec::new();
    // Find the end of the expression part.
    while i < n {
        let c = chars[i];
        match c {
            '\'' | '"' => {
                let ntriple = chars.get(i + 1) == Some(&c) && chars.get(i + 2) == Some(&c);
                let mut j = i;
                let mut pfx = String::new();
                while j > start && chars[j - 1].is_ascii_alphabetic() && pfx.len() < 2 {
                    j -= 1;
                    pfx.push(chars[j]);
                }
                let pl = pfx.to_ascii_lowercase();
                let s = i + if ntriple { 3 } else { 1 };
                let e = scan_string_end(chars, s, c, ntriple, pl.contains('f'), pl.contains('r'))?;
                i = e + if ntriple { 3 } else { 1 };
            }
            '(' | '[' | '{' => {
                stack.push(c);
                i += 1;
            }
            ')' | ']' => {
                stack.pop();
                i += 1;
            }
            '}' => {
                if stack.is_empty() {
                    break;
                }
                stack.pop();
                i += 1;
            }
            '#' => {
                while i < n && chars[i] != '\n' {
                    i += 1;
                }
            }
            '!' if stack.is_empty() && chars.get(i + 1) != Some(&'=') => break,
            ':' if stack.is_empty() => break,
            '=' if stack.is_empty() => {
                let prev = if i > start { chars[i - 1] } else { ' ' };
                let next = chars.get(i + 1).copied().unwrap_or(' ');
                if next == '=' {
                    i += 2;
                } else if matches!(prev, '=' | '!' | '<' | '>') {
                    i += 1;
                } else {
                    break;
                }
            }
            _ => i += 1,
        }
    }
    if i >= n {
        return err("f-string: expecting '}'");
    }
    let expr_text: String = chars[start..i].iter().collect();
    if expr_text.trim().is_empty() {
        return err("f-string: valid expression required before '}'");
    }
    let value = parse_field_expr(&expr_text)?;
    let mut out = Vec::new();
    let mut debug = false;
    if chars[i] == '=' {
        debug = true;
        i += 1;
        let ws_start = i;
        while i < n && chars[i].is_whitespace() {
            i += 1;
        }
        let ws: String = chars[ws_start..i].iter().collect();
        out.push(FPart::Lit(format!("{expr_text}={ws}")));
    }
    let mut conversion = None;
    if i < n && chars[i] == '!' {
        i += 1;
        let c = chars.get(i).copied().ok_or("f-string: expecting '}'")?;
        if !matches!(c, 's' | 'r' | 'a') {
            return err("f-string: invalid conversion character");
        }
        conversion = Some(c);
        i += 1;
    }
    let mut spec = None;
    if i < n && chars[i] == ':' {
        let (sp, next) = parse_fstring_chars(chars, i + 1, raw, true)?;
        spec = Some(sp);
        i = next;
    }
    if i >= n || chars[i] != '}' {
        return err("f-string: expecting '}'");
    }
    i += 1;
    if debug && conversion.is_none() && spec.is_none() {
        conversion = Some('r');
    }
    out.push(FPart::Field {
        value: Box::new(value),
        conversion,
        spec,
    });
    Ok((out, i))
}

fn scan_string_end(
    chars: &[char],
    mut i: usize,
    q: char,
    triple: bool,
    fstring: bool,
    raw: bool,
) -> PResult<usize> {
    let n = chars.len();
    while i < n {
        let c = chars[i];
        if c == '\\' {
            i += 2;
            continue;
        }
        if c == q {
            if !triple || (chars.get(i + 1) == Some(&q) && chars.get(i + 2) == Some(&q)) {
                return Ok(i);
            }
            i += 1;
            continue;
        }
        if fstring && c == '{' {
            if chars.get(i + 1) == Some(&'{') {
                i += 2;
                continue;
            }
            // Skip a nested replacement field.
            let (_, next) = parse_field(chars, i + 1, raw)?;
            i = next;
            continue;
        }
        i += 1;
    }
    err("unterminated string literal")
}

#[allow(dead_code)]
fn _unused(_: &str) {}
