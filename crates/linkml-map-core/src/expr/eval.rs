//! Native AST evaluator for the restricted LinkML expression language.
//!
//! Semantics mirror `linkml_map.utils.eval_utils` exactly:
//!
//! - SQL-style NULL propagation through arithmetic, ordering comparisons,
//!   membership, unary ops, and function calls — but NOT through `==`/`!=`,
//!   which use native Python equality.
//! - Numeric-string coercion for comparisons; binary arithmetic tries native
//!   first (so `str + str` is concatenation), then retries by coercing both
//!   operands to float.
//! - `^` is bitwise XOR, `**` is power (per the Python doctests).
//! - Attribute access distributes over lists/dicts.
//! - A fixed function set with list-funcs (`max`/`min`/`len`) and distributing
//!   scalar funcs.

use indexmap::IndexMap;

use super::error::{ExprError, ExprResult};
use super::parser::{parse, Ast, BinOp, BoolOp, CmpOp, UnOp};
use super::stmt::{is_multi_statement, parse_program, Program};
use crate::value::Value;

/// Variable bindings: name → value mapping.
pub type Bindings = IndexMap<String, Value>;

/// A parsed expression, ready to evaluate many times without re-parsing.
///
/// This is the core performance lever for the row pipeline: an `expr:` string
/// is lexed + parsed exactly once into a [`ParsedExpr`], then evaluated against
/// fresh [`Bindings`] for every row via [`eval_parsed`].
///
/// `ParsedExpr` wraps a plain-data [`Ast`] (no interior mutability, no borrows),
/// so it is `Clone + Send + Sync` and can be shared across worker threads.
///
/// The `"None"` literal short-circuit of [`eval_expr_with_mapping`]
/// (`if expr == "None": return None`) is preserved here: parsing the literal
/// string `"None"` yields a [`ParsedExpr`] whose evaluation always returns
/// [`Value::Null`], identical to the per-call string path.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedExpr {
    inner: ParsedInner,
}

/// Internal representation: a single cached expression (the fast path), the
/// `"None"` literal short-circuit, or a multi-statement program (the
/// `asteval`-style block path).
#[derive(Debug, Clone, PartialEq)]
enum ParsedInner {
    /// `"None"` literal short-circuit — always evaluates to [`Value::Null`].
    NoneLiteral,
    /// A single parsed expression tree (the common case + fast path).
    Single(Ast),
    /// A multi-statement program (`target = ...` blocks, `if`, comprehensions).
    Program(Program),
}

impl ParsedExpr {
    /// Evaluate this pre-parsed expression against `vars`.
    ///
    /// Equivalent to [`eval_parsed`]; provided as a method for ergonomics.
    pub fn eval(&self, vars: &Bindings) -> ExprResult<Value> {
        eval_parsed(self, vars)
    }

    /// True when this was parsed as a multi-statement program (the
    /// `asteval`-style block path), as opposed to a single expression.
    pub fn is_program(&self) -> bool {
        matches!(self.inner, ParsedInner::Program(_))
    }
}

/// Evaluate a single AST node against `vars`. Exposed for the statement
/// interpreter (`super::stmt`), which reuses the full expression semantics for
/// every sub-expression in a program.
pub fn eval_ast_public(node: &Ast, vars: &Bindings) -> ExprResult<Value> {
    eval_ast(node, vars)
}

/// Lex + parse `expr` once into a reusable [`ParsedExpr`].
///
/// The returned value can be evaluated repeatedly with [`eval_parsed`] (or
/// [`ParsedExpr::eval`]) against different bindings, with no further parsing.
/// Preserves the `"None"` literal short-circuit of [`eval_expr_with_mapping`].
///
/// ```
/// use linkml_map_core::expr::{parse_expr, Bindings};
/// use linkml_map_core::value::Value;
///
/// let parsed = parse_expr("{x} + {y}").unwrap();
/// let mut vars = Bindings::new();
/// vars.insert("x".into(), Value::Int(1));
/// vars.insert("y".into(), Value::Int(2));
/// assert_eq!(parsed.eval(&vars).unwrap(), Value::Int(3));
///
/// // Re-evaluate the same AST against fresh bindings, no re-parsing.
/// vars.insert("x".into(), Value::Int(10));
/// assert_eq!(parsed.eval(&vars).unwrap(), Value::Int(12));
///
/// // The "None" literal short-circuit is preserved.
/// let none = parse_expr("None").unwrap();
/// assert_eq!(none.eval(&Bindings::new()).unwrap(), Value::Null);
/// ```
pub fn parse_expr(expr: &str) -> ExprResult<ParsedExpr> {
    if expr == "None" {
        return Ok(ParsedExpr {
            inner: ParsedInner::NoneLiteral,
        });
    }
    // Multi-statement (`asteval`-style) blocks route to the statement parser;
    // single expressions keep the cached single-AST fast path untouched.
    if is_multi_statement(expr) {
        let program = parse_program(expr)?;
        return Ok(ParsedExpr {
            inner: ParsedInner::Program(program),
        });
    }
    let ast = parse(expr)?;
    Ok(ParsedExpr {
        inner: ParsedInner::Single(ast),
    })
}

/// Evaluate a pre-parsed expression against the given `bindings`.
///
/// This performs no parsing — it walks the cached [`Ast`]/program directly.
/// Unbound names resolve to [`Value::Null`].
pub fn eval_parsed(parsed: &ParsedExpr, vars: &Bindings) -> ExprResult<Value> {
    match &parsed.inner {
        ParsedInner::NoneLiteral => Ok(Value::Null),
        ParsedInner::Single(ast) => eval_ast(ast, vars),
        ParsedInner::Program(program) => with_object_attr_mode(|| program.eval(vars)),
    }
}

/// Evaluate `expr` against the given variable `bindings`.
///
/// Mirrors Python `eval_expr_with_mapping`, including the `"None"` literal
/// short-circuit (`if expr == "None": return None`). Unbound names resolve to
/// [`Value::Null`].
///
/// This is the convenience string-in path; it is exactly equivalent to
/// `eval_parsed(&parse_expr(expr)?, vars)`. Hot paths that evaluate the same
/// expression many times should call [`parse_expr`] once and reuse the
/// resulting [`ParsedExpr`].
pub fn eval_expr_with_mapping(expr: &str, vars: &Bindings) -> ExprResult<Value> {
    let parsed = parse_expr(expr)?;
    eval_parsed(&parsed, vars)
}

/// Convenience: evaluate against an empty binding set.
pub fn eval_expr(expr: &str) -> ExprResult<Value> {
    let vars = Bindings::new();
    eval_expr_with_mapping(expr, &vars)
}

fn eval_ast(node: &Ast, vars: &Bindings) -> ExprResult<Value> {
    match node {
        Ast::Int(i) => Ok(Value::Int(*i)),
        Ast::Float(f) => Ok(Value::Float(*f)),
        Ast::Str(s) => Ok(Value::Str(s.clone())),
        Ast::Bool(b) => Ok(Value::Bool(*b)),
        Ast::None => Ok(Value::Null),

        Ast::Name(name) => Ok(vars.get(name).cloned().unwrap_or(Value::Null)),

        // {x} resolves identically to bare x.
        Ast::Brace(inner) => eval_ast(inner, vars),

        Ast::Attribute { value, attr } => {
            let obj = eval_ast(value, vars)?;
            distributed_getattr(&obj, attr)
        }

        Ast::List(elts) => {
            let mut out = Vec::with_capacity(elts.len());
            for e in elts {
                out.push(eval_ast(e, vars)?);
            }
            Ok(Value::List(out))
        }

        Ast::Subscript { value, index } => {
            let obj = eval_ast(value, vars)?;
            let idx = eval_ast(index, vars)?;
            eval_subscript(&obj, &idx)
        }

        Ast::ListComp {
            elt,
            var,
            iter,
            cond,
        } => {
            let iterable = eval_ast(iter, vars)?;
            // Python iterates lists by element, dicts by *keys*, None → empty.
            let items: Vec<Value> = match iterable {
                Value::List(items) => items,
                Value::Map(m) => m.into_iter().map(|(k, _)| Value::Str(k)).collect(),
                Value::Null => Vec::new(),
                other => {
                    return Err(ExprError::Eval(format!(
                        "'{}' object is not iterable",
                        type_name(&other)
                    )))
                }
            };
            // Evaluate against a child scope that shadows the loop variable.
            // The loop var binds a single element, so attribute access on it is
            // per-element (a scalar field read on a single Map), not the list
            // auto-distribution used in plain exprs.
            let mut scope = vars.clone();
            let mut out = Vec::new();
            for item in items {
                scope.insert(var.clone(), item);
                if let Some(c) = cond {
                    if !eval_ast(c, &scope)?.is_truthy() {
                        continue;
                    }
                }
                out.push(eval_ast(elt, &scope)?);
            }
            Ok(Value::List(out))
        }

        Ast::Tuple(elts) => {
            // Tuples are only meaningful as case() pair arguments; represent as
            // a list so the evaluator can index into them.
            let mut out = Vec::with_capacity(elts.len());
            for e in elts {
                out.push(eval_ast(e, vars)?);
            }
            Ok(Value::List(out))
        }

        Ast::Unary { op, operand } => {
            let v = eval_ast(operand, vars)?;
            eval_unary(*op, v)
        }

        Ast::Binary { op, left, right } => {
            let l = eval_ast(left, vars)?;
            let r = eval_ast(right, vars)?;
            eval_binary(*op, l, r)
        }

        Ast::Compare {
            left,
            ops,
            comparators,
        } => eval_compare(left, ops, comparators, vars),

        Ast::BoolOp { op, values } => eval_boolop(*op, values, vars),

        Ast::Call { func, args } => {
            // case() arguments are (cond, val) pairs that must be evaluated;
            // they go through the generic arg evaluation below.
            let mut evaled = Vec::with_capacity(args.len());
            for a in args {
                evaled.push(eval_ast(a, vars)?);
            }
            call_function(func, evaled)
        }
    }
}

// --- subscript (mirrors Python __getitem__ for list/str/dict) ---

fn eval_subscript(obj: &Value, idx: &Value) -> ExprResult<Value> {
    match obj {
        Value::List(items) => {
            let i = match idx {
                Value::Int(i) => *i,
                Value::Bool(b) => *b as i64,
                other => {
                    return Err(ExprError::Eval(format!(
                        "list indices must be integers, not {}",
                        type_name(other)
                    )))
                }
            };
            let len = items.len() as i64;
            let resolved = if i < 0 { len + i } else { i };
            if resolved < 0 || resolved >= len {
                return Err(ExprError::Eval("list index out of range".into()));
            }
            Ok(items[resolved as usize].clone())
        }
        Value::Str(s) => {
            let chars: Vec<char> = s.chars().collect();
            let i = match idx {
                Value::Int(i) => *i,
                Value::Bool(b) => *b as i64,
                other => {
                    return Err(ExprError::Eval(format!(
                        "string indices must be integers, not {}",
                        type_name(other)
                    )))
                }
            };
            let len = chars.len() as i64;
            let resolved = if i < 0 { len + i } else { i };
            if resolved < 0 || resolved >= len {
                return Err(ExprError::Eval("string index out of range".into()));
            }
            Ok(Value::Str(chars[resolved as usize].to_string()))
        }
        Value::Map(m) => {
            let key = match idx {
                Value::Str(k) => k.clone(),
                other => py_str(other),
            };
            match m.get(&key) {
                Some(v) => Ok(v.clone()),
                None => Err(ExprError::Eval(format!("KeyError: {key:?}"))),
            }
        }
        Value::Null => Err(ExprError::Eval(
            "'NoneType' object is not subscriptable".into(),
        )),
        other => Err(ExprError::Eval(format!(
            "'{}' object is not subscriptable",
            type_name(other)
        ))),
    }
}

// --- attribute distribution (mirrors _distributed_getattr) ---

thread_local! {
    /// When set, attribute access on a `Map` uses *object* semantics:
    /// a missing key returns [`Value::Null`] (Python `DynObj.__getattr__`),
    /// instead of distributing over the map's values.
    ///
    /// The multi-statement (`asteval`-style) program path binds `src` to a
    /// single object whose attributes are read via native `getattr`, where a
    /// missing attribute yields `None`. The single-expression path keeps the
    /// historical dict-value distribution (off by default). See
    /// [`with_object_attr_mode`].
    static OBJECT_ATTR_MODE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run `f` with object-attribute semantics enabled, restoring the prior mode
/// afterwards (nesting-safe).
pub(crate) fn with_object_attr_mode<T>(f: impl FnOnce() -> T) -> T {
    OBJECT_ATTR_MODE.with(|m| {
        let prev = m.get();
        m.set(true);
        let out = f();
        m.set(prev);
        out
    })
}

fn distributed_getattr(obj: &Value, attr: &str) -> ExprResult<Value> {
    if attr.starts_with('_') {
        return Err(ExprError::Eval(format!(
            "Access to private attribute '{attr}' is not allowed"
        )));
    }
    match obj {
        Value::List(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(distributed_getattr(item, attr)?);
            }
            Ok(Value::List(out))
        }
        Value::Map(m) => {
            // A present key resolves directly in both modes (object.attr).
            if let Some(v) = m.get(attr) {
                return Ok(v.clone());
            }
            // Missing key:
            //  * object mode (multi-statement `src.attr`): return None, exactly
            //    like Python's `DynObj.__getattr__` (vars(self).get(p, None)).
            //  * default (single-expr) mode: distribute over the map's values,
            //    matching `_distributed_getattr` over a bare dict.
            if OBJECT_ATTR_MODE.with(|c| c.get()) {
                return Ok(Value::Null);
            }
            let mut out = Vec::with_capacity(m.len());
            for v in m.values() {
                out.push(distributed_getattr(v, attr)?);
            }
            Ok(Value::List(out))
        }
        Value::Null => Ok(Value::Null),
        // A scalar has no attributes; Python getattr would raise AttributeError.
        other => Err(ExprError::Eval(format!(
            "value of type {} has no attribute '{attr}'",
            type_name(other)
        ))),
    }
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "None",
        Value::Bool(_) => "bool",
        Value::Int(_) => "int",
        Value::Float(_) => "float",
        Value::Str(_) => "str",
        Value::List(_) => "list",
        Value::Map(_) => "dict",
    }
}

// --- unary ops (null-propagating) ---

fn eval_unary(op: UnOp, v: Value) -> ExprResult<Value> {
    if op == UnOp::Not {
        // `not` is NOT null-propagating in Python: `not None` is True.
        return Ok(Value::Bool(!v.is_truthy()));
    }
    if v.is_null() {
        return Ok(Value::Null);
    }
    match op {
        UnOp::Neg => match v {
            Value::Int(i) => Ok(Value::Int(-i)),
            Value::Float(f) => Ok(Value::Float(-f)),
            Value::Bool(b) => Ok(Value::Int(-(b as i64))),
            other => Err(ExprError::Eval(format!(
                "bad operand type for unary -: '{}'",
                type_name(&other)
            ))),
        },
        UnOp::Pos => match v {
            Value::Int(_) | Value::Float(_) => Ok(v),
            Value::Bool(b) => Ok(Value::Int(b as i64)),
            other => Err(ExprError::Eval(format!(
                "bad operand type for unary +: '{}'",
                type_name(&other)
            ))),
        },
        UnOp::Invert => match v {
            Value::Int(i) => Ok(Value::Int(!i)),
            Value::Bool(b) => Ok(Value::Int(!(b as i64))),
            other => Err(ExprError::Eval(format!(
                "bad operand type for unary ~: '{}'",
                type_name(&other)
            ))),
        },
        UnOp::Not => unreachable!(),
    }
}

// --- boolean ops (short-circuit, Python truthiness, return operand value) ---

fn eval_boolop(op: BoolOp, values: &[Ast], vars: &Bindings) -> ExprResult<Value> {
    // Python `and`/`or` return one of the operands, not a coerced bool.
    let mut last = Value::Bool(matches!(op, BoolOp::And));
    for node in values {
        let v = eval_ast(node, vars)?;
        match op {
            BoolOp::And => {
                if !v.is_truthy() {
                    return Ok(v);
                }
                last = v;
            }
            BoolOp::Or => {
                if v.is_truthy() {
                    return Ok(v);
                }
                last = v;
            }
        }
    }
    Ok(last)
}

// --- comparisons ---

fn eval_compare(
    left: &Ast,
    ops: &[CmpOp],
    comparators: &[Ast],
    vars: &Bindings,
) -> ExprResult<Value> {
    // Python chained comparison: a OP1 b OP2 c == (a OP1 b) and (b OP2 c),
    // each adjacent pair evaluated independently, b evaluated once.
    let mut cur = eval_ast(left, vars)?;
    let mut result = Value::Bool(true);
    for (op, rhs_node) in ops.iter().zip(comparators) {
        let rhs = eval_ast(rhs_node, vars)?;
        let pair = eval_single_compare(*op, &cur, &rhs)?;
        // Short-circuit on a false/null result, like `and`.
        match &pair {
            Value::Bool(true) => {}
            other => return Ok(other.clone()),
        }
        result = pair;
        cur = rhs;
    }
    Ok(result)
}

fn eval_single_compare(op: CmpOp, left: &Value, right: &Value) -> ExprResult<Value> {
    match op {
        CmpOp::Eq | CmpOp::NotEq => {
            // _coercing applied, but NOT null-propagating: native equality.
            let (l, r) = maybe_coerce_numeric(left, right);
            let eq = l.py_eq(&r);
            Ok(Value::Bool(if op == CmpOp::Eq { eq } else { !eq }))
        }
        CmpOp::Lt | CmpOp::LtE | CmpOp::Gt | CmpOp::GtE => {
            // _coercing then _null_propagating.
            if left.is_null() || right.is_null() {
                return Ok(Value::Null);
            }
            let (l, r) = maybe_coerce_numeric(left, right);
            match ordering_compare(op, &l, &r) {
                Ok(v) => Ok(v),
                Err(_) => {
                    // Python `_null_propagating` retry path: the native compare
                    // raised TypeError (e.g. str < int after coercion failed to
                    // change the operands); retry by coercing both to float.
                    match (l.try_numeric(), r.try_numeric()) {
                        (Some(a), Some(b)) => {
                            ordering_compare(op, &Value::Float(a), &Value::Float(b))
                        }
                        // Non-numeric operands: warn + None (mirrors Python).
                        _ => Ok(Value::Null),
                    }
                }
            }
        }
        CmpOp::In | CmpOp::NotIn => {
            // _null_propagating (no coercion wrapper in Python for In/NotIn).
            if left.is_null() || right.is_null() {
                return Ok(Value::Null);
            }
            let contained = membership(left, right)?;
            Ok(Value::Bool(if op == CmpOp::In {
                contained
            } else {
                !contained
            }))
        }
    }
}

fn ordering_compare(op: CmpOp, l: &Value, r: &Value) -> ExprResult<Value> {
    use std::cmp::Ordering;
    let ord: Option<Ordering> = match (l, r) {
        // numeric comparisons (bool counts as numeric for ordering in Python)
        _ if numeric_for_ordering(l).is_some() && numeric_for_ordering(r).is_some() => {
            let a = numeric_for_ordering(l).unwrap();
            let b = numeric_for_ordering(r).unwrap();
            a.partial_cmp(&b)
        }
        (Value::Str(a), Value::Str(b)) => Some(a.cmp(b)),
        _ => {
            return Err(ExprError::Eval(format!(
                "'{}' not supported between instances of '{}' and '{}'",
                cmp_symbol(op),
                type_name(l),
                type_name(r)
            )));
        }
    };
    let ord = match ord {
        Some(o) => o,
        None => {
            // NaN: Python comparisons with NaN are all False.
            return Ok(Value::Bool(false));
        }
    };
    let res = match op {
        CmpOp::Lt => ord == Ordering::Less,
        CmpOp::LtE => ord != Ordering::Greater,
        CmpOp::Gt => ord == Ordering::Greater,
        CmpOp::GtE => ord != Ordering::Less,
        _ => unreachable!(),
    };
    Ok(Value::Bool(res))
}

/// Numeric view for ordering: bool, int, float are all numeric here.
fn numeric_for_ordering(v: &Value) -> Option<f64> {
    match v {
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::Int(i) => Some(*i as f64),
        Value::Float(f) => Some(*f),
        _ => None,
    }
}

fn cmp_symbol(op: CmpOp) -> &'static str {
    match op {
        CmpOp::Lt => "<",
        CmpOp::LtE => "<=",
        CmpOp::Gt => ">",
        CmpOp::GtE => ">=",
        CmpOp::Eq => "==",
        CmpOp::NotEq => "!=",
        CmpOp::In => "in",
        CmpOp::NotIn => "not in",
    }
}

fn membership(needle: &Value, haystack: &Value) -> ExprResult<bool> {
    match haystack {
        Value::Str(s) => match needle {
            Value::Str(sub) => Ok(s.contains(sub.as_str())),
            other => Err(ExprError::Eval(format!(
                "'in <string>' requires string as left operand, not {}",
                type_name(other)
            ))),
        },
        Value::List(items) => Ok(items.iter().any(|item| item.py_eq(needle))),
        Value::Map(m) => match needle {
            Value::Str(k) => Ok(m.contains_key(k)),
            _ => Ok(false),
        },
        other => Err(ExprError::Eval(format!(
            "argument of type '{}' is not iterable",
            type_name(other)
        ))),
    }
}

// --- numeric-string coercion (mirrors _maybe_coerce_numeric) ---

/// If one operand is a real number (not bool) and the other is a numeric
/// string, coerce the string to match the numeric operand's type. Matches
/// Python `_maybe_coerce_numeric`.
fn maybe_coerce_numeric(left: &Value, right: &Value) -> (Value, Value) {
    // type(left) is type(right) → unchanged
    if same_py_type(left, right) {
        return (left.clone(), right.clone());
    }
    // left numeric (non-bool), right str → coerce right to type(left)
    if is_real_number(left) {
        if let Value::Str(s) = right {
            if let Some(coerced) = coerce_str_to_type_of(s, left) {
                return (left.clone(), coerced);
            }
        }
    }
    // right numeric (non-bool), left str → coerce left to type(right)
    if is_real_number(right) {
        if let Value::Str(s) = left {
            if let Some(coerced) = coerce_str_to_type_of(s, right) {
                return (coerced, right.clone());
            }
        }
    }
    (left.clone(), right.clone())
}

fn is_real_number(v: &Value) -> bool {
    matches!(v, Value::Int(_) | Value::Float(_))
}

fn same_py_type(a: &Value, b: &Value) -> bool {
    matches!(
        (a, b),
        (Value::Null, Value::Null)
            | (Value::Bool(_), Value::Bool(_))
            | (Value::Int(_), Value::Int(_))
            | (Value::Float(_), Value::Float(_))
            | (Value::Str(_), Value::Str(_))
            | (Value::List(_), Value::List(_))
            | (Value::Map(_), Value::Map(_))
    )
}

/// Coerce string `s` to the same Python type as `target` (int or float).
/// `int("3.14")` raises ValueError in Python, so int coercion only succeeds
/// for integral strings.
fn coerce_str_to_type_of(s: &str, target: &Value) -> Option<Value> {
    match target {
        Value::Int(_) => parse_python_int(s).map(Value::Int),
        Value::Float(_) => crate::value::parse_python_float(s).map(Value::Float),
        _ => None,
    }
}

/// Parse a string as Python `int()` would (whitespace-trimmed, base-10, no
/// decimal point).
fn parse_python_int(s: &str) -> Option<i64> {
    s.trim().parse::<i64>().ok()
}

// --- binary arithmetic / bitwise (null-propagating with numeric retry) ---

fn eval_binary(op: BinOp, left: Value, right: Value) -> ExprResult<Value> {
    if left.is_null() || right.is_null() {
        return Ok(Value::Null);
    }
    // Try native first (so str+str is concatenation, int op int stays int).
    match try_native_binary(op, &left, &right) {
        Ok(Some(v)) => Ok(v),
        Ok(None) | Err(_) => {
            // TypeError/ValueError path: retry by coercing both to float.
            let ln = left.try_numeric();
            let rn = right.try_numeric();
            match (ln, rn) {
                (Some(a), Some(b)) => numeric_binary_f64(op, a, b),
                _ => {
                    // warn + return None (enables case() guards)
                    Ok(Value::Null)
                }
            }
        }
    }
}

/// Attempt the operation with native (non-coerced) semantics.
/// Returns `Ok(Some(v))` on success, `Ok(None)`/`Err` to signal the Python
/// `TypeError`/`ValueError` retry path.
fn try_native_binary(op: BinOp, left: &Value, right: &Value) -> ExprResult<Option<Value>> {
    use Value::*;

    // String concatenation / repetition.
    if let (Str(a), Str(b)) = (left, right) {
        return match op {
            BinOp::Add => Ok(Some(Str(format!("{a}{b}")))),
            // Other ops on two strings are TypeErrors → trigger numeric retry.
            _ => Ok(None),
        };
    }
    // List concatenation: list + list → joined list (Python `+` on lists).
    if let (List(a), List(b)) = (left, right) {
        return match op {
            BinOp::Add => {
                let mut out = a.clone();
                out.extend(b.iter().cloned());
                Ok(Some(List(out)))
            }
            _ => Ok(None),
        };
    }

    // str * int  / int * str  → repetition
    if op == BinOp::Mul {
        if let (Str(s), Int(n)) = (left, right) {
            return Ok(Some(repeat_str(s, *n)));
        }
        if let (Int(n), Str(s)) = (left, right) {
            return Ok(Some(repeat_str(s, *n)));
        }
    }

    // Numeric (int/float; bool participates as int in Python arithmetic).
    let li = as_int_like(left);
    let ri = as_int_like(right);
    if let (Some(a), Some(b)) = (li, ri) {
        // Both integral → integer arithmetic where Python keeps int.
        return int_binary(op, a, b);
    }

    let lf = as_float_like(left);
    let rf = as_float_like(right);
    if let (Some(a), Some(b)) = (lf, rf) {
        // At least one is float (or bitwise on float → TypeError).
        return float_binary(op, a, b);
    }

    // Non-numeric, non-string-handled combination → retry path.
    Ok(None)
}

fn repeat_str(s: &str, n: i64) -> Value {
    if n <= 0 {
        Value::Str(String::new())
    } else {
        Value::Str(s.repeat(n as usize))
    }
}

/// int-like: int or bool (Python treats bool as int in arithmetic), but NOT float.
fn as_int_like(v: &Value) -> Option<i64> {
    match v {
        Value::Int(i) => Some(*i),
        Value::Bool(b) => Some(*b as i64),
        _ => None,
    }
}

/// float-like for arithmetic: int/float/bool numeric value.
fn as_float_like(v: &Value) -> Option<f64> {
    match v {
        Value::Int(i) => Some(*i as f64),
        Value::Float(f) => Some(*f),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    }
}

fn int_binary(op: BinOp, a: i64, b: i64) -> ExprResult<Option<Value>> {
    let v = match op {
        BinOp::Add => Value::Int(a.wrapping_add(b)),
        BinOp::Sub => Value::Int(a.wrapping_sub(b)),
        BinOp::Mul => Value::Int(a.wrapping_mul(b)),
        BinOp::Div => {
            // Python `/` always yields float.
            if b == 0 {
                return Ok(None); // ZeroDivisionError → retry path (will also fail → None)
            }
            Value::Float(a as f64 / b as f64)
        }
        BinOp::FloorDiv => {
            if b == 0 {
                return Ok(None);
            }
            Value::Int(python_floordiv_int(a, b))
        }
        BinOp::Mod => {
            if b == 0 {
                return Ok(None);
            }
            Value::Int(python_mod_int(a, b))
        }
        BinOp::Pow => {
            if b < 0 {
                // Python returns a float for negative integer exponents.
                Value::Float((a as f64).powi(b as i32))
            } else {
                Value::Int(ipow(a, b as u32))
            }
        }
        BinOp::LShift => Value::Int(a.wrapping_shl(b as u32)),
        BinOp::RShift => Value::Int(a.wrapping_shr(b as u32)),
        BinOp::BitAnd => Value::Int(a & b),
        BinOp::BitOr => Value::Int(a | b),
        BinOp::BitXor => Value::Int(a ^ b),
    };
    Ok(Some(v))
}

fn float_binary(op: BinOp, a: f64, b: f64) -> ExprResult<Option<Value>> {
    let v = match op {
        BinOp::Add => Value::Float(a + b),
        BinOp::Sub => Value::Float(a - b),
        BinOp::Mul => Value::Float(a * b),
        BinOp::Div => {
            if b == 0.0 {
                return Ok(None);
            }
            Value::Float(a / b)
        }
        BinOp::FloorDiv => {
            if b == 0.0 {
                return Ok(None);
            }
            Value::Float((a / b).floor())
        }
        BinOp::Mod => {
            if b == 0.0 {
                return Ok(None);
            }
            Value::Float(python_mod_float(a, b))
        }
        BinOp::Pow => Value::Float(a.powf(b)),
        // Bitwise/shift on floats is a TypeError in Python → retry path.
        BinOp::LShift | BinOp::RShift | BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor => {
            return Ok(None);
        }
    };
    Ok(Some(v))
}

/// Numeric retry path: both operands coerced to f64.
fn numeric_binary_f64(op: BinOp, a: f64, b: f64) -> ExprResult<Value> {
    match float_binary(op, a, b)? {
        Some(v) => Ok(v),
        None => Ok(Value::Null), // e.g. div-by-zero or bitwise on floats
    }
}

fn ipow(mut base: i64, mut exp: u32) -> i64 {
    let mut acc: i64 = 1;
    while exp > 0 {
        if exp & 1 == 1 {
            acc = acc.wrapping_mul(base);
        }
        exp >>= 1;
        if exp > 0 {
            base = base.wrapping_mul(base);
        }
    }
    acc
}

/// Python floor division for ints (floors toward negative infinity).
fn python_floordiv_int(a: i64, b: i64) -> i64 {
    let q = a / b;
    let r = a % b;
    if (r != 0) && ((r < 0) != (b < 0)) {
        q - 1
    } else {
        q
    }
}

/// Python modulo for ints (result has sign of divisor).
fn python_mod_int(a: i64, b: i64) -> i64 {
    let r = a % b;
    if r != 0 && (r < 0) != (b < 0) {
        r + b
    } else {
        r
    }
}

/// Python modulo for floats (result has sign of divisor).
fn python_mod_float(a: f64, b: f64) -> f64 {
    let r = a % b;
    if r != 0.0 && (r < 0.0) != (b < 0.0) {
        r + b
    } else {
        r
    }
}

// --- function calls ---

fn call_function(name: &str, args: Vec<Value>) -> ExprResult<Value> {
    match name {
        // list-functions: take the list directly, null-safe (None arg → None).
        "max" => list_func_reduce(name, args, |items| reduce_minmax(items, true)),
        "min" => list_func_reduce(name, args, |items| reduce_minmax(items, false)),
        "len" => null_safe_unary(args, "len", |v| match v {
            Value::Str(s) => Ok(Value::Int(s.chars().count() as i64)),
            Value::List(l) => Ok(Value::Int(l.len() as i64)),
            Value::Map(m) => Ok(Value::Int(m.len() as i64)),
            other => Err(ExprError::Eval(format!(
                "object of type '{}' has no len()",
                type_name(&other)
            ))),
        }),

        // distributing scalar functions
        "str" => distributing(args, "str", |v| Ok(Value::Str(py_str(&v)))),
        "int" => distributing(args, "int", |v| py_int(&v)),
        "float" => distributing(args, "float", |v| py_float(&v)),
        "bool" => distributing(args, "bool", |v| Ok(Value::Bool(v.is_truthy()))),
        "abs" => distributing(args, "abs", |v| py_abs(&v)),
        "round" => distributing_round(args),
        "strlen" => distributing(args, "strlen", |v| match v {
            Value::Str(s) => Ok(Value::Int(s.chars().count() as i64)),
            Value::List(l) => Ok(Value::Int(l.len() as i64)),
            Value::Map(m) => Ok(Value::Int(m.len() as i64)),
            other => Err(ExprError::Eval(format!(
                "object of type '{}' has no len()",
                type_name(&other)
            ))),
        }),
        "uuid5" => distributing_uuid5(args),

        "case" => eval_conditional(args),
        "is_numeric" => {
            if args.len() != 1 {
                return Err(ExprError::Eval(format!(
                    "is_numeric() takes 1 argument ({} given)",
                    args.len()
                )));
            }
            Ok(Value::Bool(args[0].is_numeric()))
        }

        other => Err(ExprError::Eval(format!(
            "function '{other}' is not defined"
        ))),
    }
}

/// `case(*conds)`: first pair `(cond, val)` whose cond is truthy → val, else Null.
fn eval_conditional(args: Vec<Value>) -> ExprResult<Value> {
    for arg in &args {
        let pair = match arg {
            Value::List(items) if items.len() == 2 => items,
            _ => {
                return Err(ExprError::Eval(
                    "case() arguments must be (cond, value) pairs".into(),
                ))
            }
        };
        if pair[0].is_truthy() {
            return Ok(pair[1].clone());
        }
    }
    Ok(Value::Null)
}

fn list_func_reduce(
    name: &str,
    args: Vec<Value>,
    f: impl Fn(&[Value]) -> ExprResult<Value>,
) -> ExprResult<Value> {
    // _null_safe: any None arg → None.
    if args.iter().any(|a| a.is_null()) {
        return Ok(Value::Null);
    }
    // max/min/len accept the list as a single argument (no distribution).
    if args.len() == 1 {
        if let Value::List(items) = &args[0] {
            return f(items);
        }
        // max/min over multiple positional args also valid in Python, but the
        // single-list form is what these expressions use.
    }
    // Multiple positional args: treat them as the iterable.
    f(&args).map_err(|_| ExprError::Eval(format!("{name}() argument error")))
}

fn reduce_minmax(items: &[Value], want_max: bool) -> ExprResult<Value> {
    if items.is_empty() {
        return Err(ExprError::Eval(
            "max()/min() arg is an empty sequence".into(),
        ));
    }
    let mut best = items[0].clone();
    for item in &items[1..] {
        let cmp = compare_for_minmax(item, &best)?;
        let take = if want_max {
            cmp == std::cmp::Ordering::Greater
        } else {
            cmp == std::cmp::Ordering::Less
        };
        if take {
            best = item.clone();
        }
    }
    Ok(best)
}

fn compare_for_minmax(a: &Value, b: &Value) -> ExprResult<std::cmp::Ordering> {
    if let (Some(x), Some(y)) = (numeric_for_ordering(a), numeric_for_ordering(b)) {
        return Ok(x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal));
    }
    if let (Value::Str(x), Value::Str(y)) = (a, b) {
        return Ok(x.cmp(y));
    }
    Err(ExprError::Eval("unorderable types in max()/min()".into()))
}

fn null_safe_unary(
    args: Vec<Value>,
    name: &str,
    f: impl Fn(Value) -> ExprResult<Value>,
) -> ExprResult<Value> {
    if args.len() != 1 {
        return Err(ExprError::Eval(format!(
            "{name}() takes 1 argument ({} given)",
            args.len()
        )));
    }
    if args[0].is_null() {
        return Ok(Value::Null);
    }
    f(args.into_iter().next().unwrap())
}

/// Distribute a scalar unary function over a list first-arg, propagate None.
fn distributing(
    args: Vec<Value>,
    name: &str,
    f: impl Fn(Value) -> ExprResult<Value> + Copy,
) -> ExprResult<Value> {
    if args.len() != 1 {
        return Err(ExprError::Eval(format!(
            "{name}() takes 1 argument ({} given)",
            args.len()
        )));
    }
    let arg = args.into_iter().next().unwrap();
    if let Value::List(items) = arg {
        let mut out = Vec::with_capacity(items.len());
        for item in items {
            if item.is_null() {
                out.push(Value::Null);
            } else {
                out.push(f(item)?);
            }
        }
        return Ok(Value::List(out));
    }
    if arg.is_null() {
        return Ok(Value::Null);
    }
    f(arg)
}

fn distributing_round(args: Vec<Value>) -> ExprResult<Value> {
    // round(x) or round(x, ndigits). Distributes over a list first-arg.
    if args.is_empty() || args.len() > 2 {
        return Err(ExprError::Eval(format!(
            "round() takes 1 or 2 arguments ({} given)",
            args.len()
        )));
    }
    let tail: Vec<Value> = args[1..].to_vec();
    let first = args[0].clone();
    if let Value::List(items) = first {
        let mut out = Vec::with_capacity(items.len());
        for item in items {
            if item.is_null() || tail.iter().any(|t| t.is_null()) {
                out.push(Value::Null);
            } else {
                out.push(py_round(&item, tail.first())?);
            }
        }
        return Ok(Value::List(out));
    }
    if first.is_null() || tail.iter().any(|t| t.is_null()) {
        return Ok(Value::Null);
    }
    py_round(&first, tail.first())
}

fn distributing_uuid5(args: Vec<Value>) -> ExprResult<Value> {
    // uuid5(namespace, name). Distributes over a list first arg, null-safe.
    if args.len() != 2 {
        return Err(ExprError::Eval(format!(
            "uuid5() takes 2 arguments ({} given)",
            args.len()
        )));
    }
    let ns = args[0].clone();
    let name = args[1].clone();
    if let Value::List(items) = ns {
        let mut out = Vec::with_capacity(items.len());
        for item in items {
            if item.is_null() || name.is_null() {
                out.push(Value::Null);
            } else {
                out.push(Value::Str(uuid5_two_level(&item, &name)?));
            }
        }
        return Ok(Value::List(out));
    }
    if ns.is_null() || name.is_null() {
        return Ok(Value::Null);
    }
    Ok(Value::Str(uuid5_two_level(&ns, &name)?))
}

fn uuid5_two_level(namespace: &Value, name: &Value) -> ExprResult<String> {
    let ns_str = match namespace {
        Value::Str(s) => s.clone(),
        other => py_str(other),
    };
    let name_str = match name {
        Value::Str(s) => s.clone(),
        other => py_str(other),
    };
    let derived = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, ns_str.as_bytes());
    let result = uuid::Uuid::new_v5(&derived, name_str.as_bytes());
    Ok(result.to_string())
}

// --- scalar function implementations mirroring Python builtins ---

fn py_str(v: &Value) -> String {
    match v {
        Value::Null => "None".to_string(),
        Value::Bool(b) => if *b { "True" } else { "False" }.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => format_py_float(*f),
        Value::Str(s) => s.clone(),
        Value::List(items) => {
            let parts: Vec<String> = items.iter().map(py_repr).collect();
            format!("[{}]", parts.join(", "))
        }
        Value::Map(m) => {
            let parts: Vec<String> = m
                .iter()
                .map(|(k, v)| format!("{}: {}", py_repr(&Value::Str(k.clone())), py_repr(v)))
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
    }
}

fn py_repr(v: &Value) -> String {
    match v {
        Value::Str(s) => format!("'{s}'"),
        other => py_str(other),
    }
}

fn format_py_float(f: f64) -> String {
    if f.is_infinite() {
        return if f > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if f.is_nan() {
        return "nan".into();
    }
    if f == f.trunc() && f.abs() < 1e16 {
        format!("{f:.1}")
    } else {
        format!("{f}")
    }
}

fn py_int(v: &Value) -> ExprResult<Value> {
    match v {
        Value::Int(i) => Ok(Value::Int(*i)),
        Value::Bool(b) => Ok(Value::Int(*b as i64)),
        Value::Float(f) => Ok(Value::Int(f.trunc() as i64)),
        Value::Str(s) => match parse_python_int(s) {
            Some(i) => Ok(Value::Int(i)),
            None => Err(ExprError::Eval(format!(
                "invalid literal for int() with base 10: '{s}'"
            ))),
        },
        other => Err(ExprError::Eval(format!(
            "int() argument must be a string or a number, not '{}'",
            type_name(other)
        ))),
    }
}

fn py_float(v: &Value) -> ExprResult<Value> {
    match v {
        Value::Int(i) => Ok(Value::Float(*i as f64)),
        Value::Bool(b) => Ok(Value::Float(if *b { 1.0 } else { 0.0 })),
        Value::Float(f) => Ok(Value::Float(*f)),
        Value::Str(s) => match crate::value::parse_python_float(s) {
            Some(f) => Ok(Value::Float(f)),
            None => Err(ExprError::Eval(format!(
                "could not convert string to float: '{s}'"
            ))),
        },
        other => Err(ExprError::Eval(format!(
            "float() argument must be a string or a number, not '{}'",
            type_name(other)
        ))),
    }
}

fn py_abs(v: &Value) -> ExprResult<Value> {
    match v {
        Value::Int(i) => Ok(Value::Int(i.abs())),
        Value::Float(f) => Ok(Value::Float(f.abs())),
        Value::Bool(b) => Ok(Value::Int(*b as i64)),
        other => Err(ExprError::Eval(format!(
            "bad operand type for abs(): '{}'",
            type_name(other)
        ))),
    }
}

fn py_round(v: &Value, ndigits: Option<&Value>) -> ExprResult<Value> {
    let num = match v {
        Value::Int(i) => {
            // round(int) is int; round(int, n) is int.
            if ndigits.is_none() {
                return Ok(Value::Int(*i));
            }
            *i as f64
        }
        Value::Float(f) => *f,
        Value::Bool(b) => {
            if ndigits.is_none() {
                return Ok(Value::Int(*b as i64));
            }
            if *b {
                1.0
            } else {
                0.0
            }
        }
        other => {
            return Err(ExprError::Eval(format!(
                "type {} doesn't define __round__ method",
                type_name(other)
            )))
        }
    };
    match ndigits {
        None => Ok(Value::Int(banker_round(num, 0) as i64)),
        Some(Value::Int(n)) => Ok(Value::Float(banker_round(num, *n as i32))),
        Some(Value::Bool(b)) => Ok(Value::Float(banker_round(num, *b as i32))),
        Some(other) => Err(ExprError::Eval(format!(
            "'{}' object cannot be interpreted as an integer",
            type_name(other)
        ))),
    }
}

/// Python's round() uses banker's rounding (round-half-to-even).
fn banker_round(x: f64, ndigits: i32) -> f64 {
    let factor = 10f64.powi(ndigits);
    let scaled = x * factor;
    let rounded = round_half_even(scaled);
    rounded / factor
}

fn round_half_even(x: f64) -> f64 {
    let floor = x.floor();
    let diff = x - floor;
    if diff < 0.5 {
        floor
    } else if diff > 0.5 {
        floor + 1.0
    } else {
        // exactly .5 → round to even
        if (floor as i64) % 2 == 0 {
            floor
        } else {
            floor + 1.0
        }
    }
}
