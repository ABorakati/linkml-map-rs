//! A port of `asteval` (1.0.x), the interpreter behind linkml-map's
//! `unrestricted_eval`: Python 3.13's `ast.parse`, then a tree walk with
//! `asteval`'s symbol table, error list and control-flow quirks.
//!
//! [`super::stmt`] wraps it as [`Program`](super::stmt::Program) and
//! [`super::gate`] decides when it runs.

pub(crate) mod ast;
mod dirs;
mod fmt;
mod funcs;
mod interp;
mod lex;
mod methods;
mod ops;
pub(crate) mod parse;
mod value;

#[cfg(test)]
mod tests;

use std::rc::Rc;

use indexmap::IndexMap;

use self::ast::{Expr, Module, Stmt};
use self::interp::Interp;
pub(crate) use self::parse::{Parsed, parses_as_expression};
pub(crate) type PySrc = Py;
use self::value::{Py, from_value, rf, to_value};
use crate::value::Value;

/// `Interpreter(max_statement_length=50000)`.
const MAX_STATEMENT_LENGTH: usize = 50_000;

/// `Interpreter.parse`: `Err` is any failure `asteval` turns into a silent
/// `None` (a `SyntaxError`, `IndentationError`, or an over-long text).
pub(crate) fn compile(text: &str) -> Result<Parsed, String> {
    if text.chars().count() > MAX_STATEMENT_LENGTH {
        return Err(format!(
            "length of text exceeds {MAX_STATEMENT_LENGTH} characters"
        ));
    }
    parse::parse_module(text)
}

/// How a run failed in a way `_unrestricted_eval` does not swallow.
#[derive(Debug, PartialEq)]
pub(crate) enum ExecError {
    /// `NameError` first in the error list with `raise_undefined`: upstream's
    /// `ValueError("Unrestricted expression uses an undefined name: ...")`.
    Undefined { name: String, message: String },
    /// An exception that escapes `_unrestricted_eval` (`KeyError` from
    /// `aeval.symtable["target"]` after `del target`) or the step limit.
    Uncaught(String),
}

/// The attributes of `src` a module reads, when every use of `src` is
/// `src.<name>`; `None` when `src` is used any other way.
pub(crate) fn src_attributes(module: &Module) -> Option<Vec<String>> {
    let mut attrs: Vec<String> = Vec::new();
    let mut whole = false;
    for s in &module.body {
        walk_stmt(s, &mut |e| scan_src(e, &mut attrs, &mut whole));
    }
    if whole { None } else { Some(attrs) }
}

fn scan_src(e: &Expr, attrs: &mut Vec<String>, whole: &mut bool) -> bool {
    match e {
        Expr::Attribute(inner, attr) if matches!(inner.as_ref(), Expr::Name(n, _) if n == "src") => {
            if !attrs.contains(attr) {
                attrs.push(attr.clone());
            }
            false
        }
        Expr::Name(n, _) if n == "src" => {
            *whole = true;
            false
        }
        _ => true,
    }
}

/// Visit every expression (pre-order, iteratively); `f` returns whether to
/// descend.
pub(crate) fn walk_expr(e: &Expr, f: &mut dyn FnMut(&Expr) -> bool) {
    let mut stack: Vec<&Expr> = vec![e];
    while let Some(x) = stack.pop() {
        if f(x) {
            let kids = x.children();
            stack.extend(kids.into_iter().rev());
        }
    }
}

/// Visit every expression in a statement (pre-order), descending into bodies.
pub(crate) fn walk_stmt(s: &Stmt, f: &mut dyn FnMut(&Expr) -> bool) {
    let body =
        |b: &[Stmt], f: &mut dyn FnMut(&Expr) -> bool| b.iter().for_each(|x| walk_stmt(x, f));
    match s {
        Stmt::Expr(e) => walk_expr(e, f),
        Stmt::Assign(ts, v) => {
            ts.iter().for_each(|t| walk_expr(t, f));
            walk_expr(v, f);
        }
        Stmt::AugAssign(t, _, v) => {
            walk_expr(t, f);
            walk_expr(v, f);
        }
        Stmt::Return(v) => {
            if let Some(v) = v {
                walk_expr(v, f);
            }
        }
        Stmt::Delete(ts) => ts.iter().for_each(|t| walk_expr(t, f)),
        Stmt::Pass | Stmt::Break | Stmt::Continue | Stmt::Unsupported(_) => {}
        Stmt::If(t, b, o) | Stmt::While(t, b, o) => {
            walk_expr(t, f);
            body(b, f);
            body(o, f);
        }
        Stmt::For(t, i, b, o) => {
            walk_expr(t, f);
            walk_expr(i, f);
            body(b, f);
            body(o, f);
        }
        Stmt::With(items, b) => {
            for it in items {
                walk_expr(&it.context_expr, f);
                if let Some(v) = &it.optional_vars {
                    walk_expr(v, f);
                }
            }
            body(b, f);
        }
        Stmt::Raise(a, b) => {
            for x in [a, b].into_iter().flatten() {
                walk_expr(x, f);
            }
        }
        Stmt::Try(b, hs, o, fin) => {
            body(b, f);
            for h in hs {
                if let Some(t) = &h.type_ {
                    walk_expr(t, f);
                }
                body(&h.body, f);
            }
            body(o, f);
            body(fin, f);
        }
        Stmt::Assert(t, m) => {
            walk_expr(t, f);
            if let Some(m) = m {
                walk_expr(m, f);
            }
        }
        Stmt::FunctionDef(def) => {
            def.params.defaults.iter().for_each(|x| walk_expr(x, f));
            body(&def.body, f);
        }
    }
}

/// The source object as a `DynObj`, restricted to the attributes the program
/// reads when `attrs` says so.
pub(crate) fn build_src<'v>(
    entries: impl Iterator<Item = (&'v String, &'v Value)>,
    attrs: Option<&[String]>,
) -> Py {
    let mut m: IndexMap<String, Py> = IndexMap::new();
    for (k, v) in entries {
        if matches!(k.as_str(), "NULL" | "__slot_values") {
            continue;
        }
        if let Some(a) = attrs
            && !a.iter().any(|x| x == k)
        {
            continue;
        }
        m.insert(k.clone(), from_value(v));
    }
    Py::Obj(rf(m))
}

/// The source object for a non-map `src` (the enum-expression fallback).
pub(crate) fn src_from_value(v: &Value) -> Py {
    value::plain_from_value(v)
}

/// Run a compiled module: `_unrestricted_eval`. Returns `target`'s final value.
pub(crate) fn execute(
    parsed: &Parsed,
    text: &str,
    src: Py,
    slot_values: Option<&IndexMap<String, Value>>,
    raise_undefined: bool,
) -> Result<Value, ExecError> {
    let mut interp = Interp::new(text, src, slot_values);
    interp.eval_module(&parsed.module);
    if interp.aborted {
        return Err(ExecError::Uncaught(
            "unrestricted expression exceeded the evaluation step limit".into(),
        ));
    }
    if raise_undefined
        && let Some(first) = interp.error.first()
        && first.exc == Some("NameError")
    {
        let detail = interp.error_detail(first);
        let name = first.msg.split('\'').nth(1).unwrap_or_default().to_string();
        return Err(ExecError::Undefined {
            name,
            message: format!(
                "Unrestricted expression uses an undefined name: {detail}. \
                         Statements read slots as src.<slot>."
            ),
        });
    }
    match interp.symbol("target") {
        Some(v) => Ok(to_value(&v, 0)),
        None => Err(ExecError::Uncaught("KeyError: 'target'".into())),
    }
}

#[allow(dead_code)]
fn _unused(_: Rc<()>) {}
