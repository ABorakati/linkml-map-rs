//! The restricted / unrestricted evaluation split.
//!
//! Upstream evaluates an `expr:` with one of two engines, chosen per
//! transformer by `unrestricted_eval` (default `False`; the CLI flag is
//! `--unrestricted-eval`):
//!
//! * the **restricted** evaluator, `eval_utils.eval_expr_with_mapping`, built on
//!   `simpleeval`. It evaluates one expression against the source slots,
//!   `NULL`, join aliases and the built-in functions. It has no `src`, no
//!   statements (only the first of a block runs) and refuses attribute access on
//!   a comprehension-bound name.
//! * the **unrestricted** evaluator, `asteval`, which runs a statement block with
//!   `src`, `target` and `uuid5` in scope ([`super::stmt`]).
//!
//! `ObjectTransformer._eval_expr` and `transform_enum` choose between them; this
//! module is their port. [`eval_restricted`] is the restricted path,
//! [`eval_parsed_gated`] the slot-expression gate and [`eval_enum_parsed`] the
//! enum-expression gate. The restricted evaluator proper is the rest of
//! [`super::eval`], whose attribute arm consults [`comprehension_root`].

use std::cell::{Cell, RefCell};

use super::error::ExprResult;
use super::eval::{Bindings, ParsedExpr, ParsedInner, with_strict_mode};
use crate::value::Value;

/// How an expression is evaluated: upstream's `strict` and `unrestricted_eval`
/// transformer settings.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EvalMode {
    /// Unbound names and unresolved `slot()` references raise instead of
    /// resolving to null.
    pub strict: bool,
    /// Allow the asteval-style evaluator: statement blocks, and the fallback
    /// for an expression the restricted evaluator rejects. Off by default, as
    /// upstream.
    pub unrestricted: bool,
}

impl EvalMode {
    /// Restricted and lax: upstream's defaults.
    pub const RESTRICTED: Self = Self {
        strict: false,
        unrestricted: false,
    };
    /// Lax with `unrestricted_eval` on.
    pub const UNRESTRICTED: Self = Self {
        strict: false,
        unrestricted: true,
    };
}

/// The restricted evaluator: `eval_expr_with_mapping`.
///
/// PARITY: `simpleeval` evaluates only the first statement of a block (see
/// [`super::stmt::Program`]), and the bindings carry no `src`.
pub(super) fn eval_restricted(parsed: &ParsedExpr, vars: &Bindings) -> ExprResult<Value> {
    match &parsed.inner {
        ParsedInner::NoneLiteral => Ok(Value::Null),
        ParsedInner::Single(ast) => {
            super::eval::eval_ast_public(ast, vars).map(Value::into_record_form)
        }
        ParsedInner::Program(program) => program
            .eval_first_statement(vars)
            .map(Value::into_record_form),
    }
}

/// Evaluate a slot (or expression-mapping) `expr:` under `mode`: upstream
/// `ObjectTransformer._eval_expr`.
///
/// `vars` are the restricted bindings: the source slots, `NULL` and join
/// aliases. With `unrestricted_eval` a statement block runs in the asteval
/// interpreter with every binding but `NULL` and `__slot_values` exposed as the
/// source object `src`; it never sees the bare slot names.
///
/// Anything else goes to the restricted evaluator first. A rejection
/// ([`ExprError::is_restricted_rejection`](super::error::ExprError::is_restricted_rejection),
/// upstream's `InvalidExpression`/`TypeError`/`ValueError`) is re-raised, or
/// with `unrestricted_eval` retried by asteval, which for a single expression
/// leaves `target` unset and so yields null (see [`unrestricted_fallback`]).
/// Every other error propagates in both modes.
pub fn eval_parsed_gated(
    parsed: &ParsedExpr,
    vars: &Bindings,
    mode: EvalMode,
) -> ExprResult<Value> {
    if mode.unrestricted
        && let ParsedInner::Program(program) = &parsed.inner
    {
        return program.eval_vars(vars, true);
    }
    match with_strict_mode(mode.strict, || eval_restricted(parsed, vars)) {
        Err(e) if mode.unrestricted && e.is_restricted_rejection() => {
            unrestricted_fallback(parsed, Fallback::Vars(vars))
        }
        other => other,
    }
}

/// Evaluate an enum derivation's `expr:` under `mode`: the `expr` branch of
/// upstream `transform_enum`.
///
/// `vars` are the restricted bindings (`{**source_obj, "NULL": None}` for a
/// mapping source, else `{"src": source_obj, "NULL": None}`) and `src` is the
/// source object itself, which the asteval fallback exposes as `src` whatever
/// shape it has. Unlike the slot gate there is no statement-block shortcut: the
/// restricted evaluator always runs first and the fallback is reached only on a
/// rejection, with undefined names swallowed rather than raised.
pub fn eval_enum_parsed(
    parsed: &ParsedExpr,
    vars: &Bindings,
    src: &Value,
    mode: EvalMode,
) -> ExprResult<Value> {
    match with_strict_mode(mode.strict, || eval_restricted(parsed, vars)) {
        Err(e) if mode.unrestricted && e.is_restricted_rejection() => {
            unrestricted_fallback(parsed, Fallback::Src(src))
        }
        other => other,
    }
}

/// What the fallback exposes as `src`.
enum Fallback<'a> {
    /// The restricted bindings (the slot gate).
    Vars(&'a Bindings),
    /// The source object itself (the enum gate).
    Src(&'a Value),
}

/// `ObjectTransformer._unrestricted_eval(expr, src)` reached as the fallback
/// (`raise_undefined` false).
///
/// A statement block runs with an undefined name swallowed like any other
/// error. Anything that parsed as one expression leaves `target` at its
/// pre-seeded `None`: asteval evaluates it for nothing and the result is null.
fn unrestricted_fallback(parsed: &ParsedExpr, src: Fallback<'_>) -> ExprResult<Value> {
    match &parsed.inner {
        ParsedInner::Program(program) => match src {
            Fallback::Vars(vars) => program.eval_vars(vars, false),
            Fallback::Src(src) => program.eval_src(src, None, false),
        },
        ParsedInner::NoneLiteral | ParsedInner::Single(_) => Ok(Value::Null),
    }
}

// === comprehension-bound names ============================================

/// A name bound by an enclosing comprehension, with the binding (if any) the
/// expression's own bindings give the same name.
struct CompTarget {
    name: String,
    outer: Option<Value>,
}

thread_local! {
    /// The comprehension targets in scope, outermost first. Only maintained by
    /// the restricted evaluator (see [`enter_comprehension`]).
    static TARGETS: RefCell<Vec<CompTarget>> = const { RefCell::new(Vec::new()) };
    /// `TARGETS.len()`, readable without borrowing: nearly every attribute
    /// access happens outside any comprehension.
    static DEPTH: Cell<usize> = const { Cell::new(0) };
}

/// Scope guard returned by [`enter_comprehension`]; pops the target on drop.
pub(super) struct ComprehensionScope {
    pushed: bool,
}

impl Drop for ComprehensionScope {
    fn drop(&mut self) {
        if self.pushed {
            TARGETS.with(|t| t.borrow_mut().pop());
            DEPTH.with(|d| d.set(d.get() - 1));
        }
    }
}

/// Note that a comprehension binding `var` is being evaluated, mirroring
/// `LinkMLEvaluator._eval_comprehension`'s `_comprehension_targets`.
///
/// `vars` are the bindings in force at the comprehension, from which the
/// binding the *expression* gave `var` is recovered: an enclosing comprehension
/// that binds the same name has already shadowed it in `vars`, so its record
/// wins.
pub(super) fn enter_comprehension(var: &str, vars: &Bindings) -> ComprehensionScope {
    TARGETS.with(|t| {
        let mut targets = t.borrow_mut();
        let outer = targets
            .iter()
            .find(|c| c.name == var)
            .map_or_else(|| vars.get(var).cloned(), |c| c.outer.clone());
        targets.push(CompTarget {
            name: var.to_string(),
            outer,
        });
    });
    DEPTH.with(|d| d.set(d.get() + 1));
    ComprehensionScope { pushed: true }
}

/// What a root name of a qualified `root.attr` access refers to, as far as
/// comprehensions are concerned.
pub(super) enum ComprehensionRoot {
    /// Not bound by an enclosing comprehension: resolve it as usual.
    NotATarget,
    /// Bound by a comprehension, but the expression's own bindings also hold
    /// the name: that value is the root (upstream resolves the root outside the
    /// comprehension scope).
    Outer(Value),
    /// Bound by a comprehension and by nothing else, so upstream cannot find
    /// the root and reports attribute access on a comprehension-bound name.
    Unbound,
}

/// Classify `root` for `LinkMLEvaluator._eval_attribute`.
pub(super) fn comprehension_root(root: &str) -> ComprehensionRoot {
    if DEPTH.with(Cell::get) == 0 {
        return ComprehensionRoot::NotATarget;
    }
    TARGETS.with(|t| match t.borrow().iter().rev().find(|c| c.name == root) {
        None => ComprehensionRoot::NotATarget,
        Some(CompTarget { outer: Some(v), .. }) => ComprehensionRoot::Outer(v.clone()),
        Some(CompTarget { outer: None, .. }) => ComprehensionRoot::Unbound,
    })
}
