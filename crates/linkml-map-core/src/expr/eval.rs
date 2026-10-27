//! Native AST evaluator for the restricted LinkML expression language.
//!
//! Semantics mirror `linkml_map.utils.eval_utils` exactly:
//!
//! - SQL-style NULL propagation through arithmetic, ordering comparisons,
//!   membership, unary ops, and function calls — but NOT through `==`/`!=`,
//!   which use native Python equality.
//! - Numeric-string coercion for comparisons; arithmetic operators coerce
//!   numeric-looking strings to match the other operand's type (so `"71" * 365`
//!   is `25915`, not string repetition — mirroring upstream #285 fix).
//! - Two strings keep native semantics (`"a" + "b"` concatenates, `"71" + "365"
//!   == "71365"`); a list times a number stays genuine repetition.
//! - `^` is bitwise XOR, `**` is power (per the Python doctests).
//! - Attribute access distributes over lists/dicts.
//! - A fixed function set with list-funcs (`max`/`min`/`len`) and distributing
//!   scalar funcs.

use indexmap::IndexMap;
use log::warn;

use super::builtins;
use super::compound;
use super::error::{ExprError, ExprResult};
use super::parser::{Ast, BinOp, BoolOp, CmpOp, UnOp};
use super::stmt::{Program, SingleOrProgram, parse_single_or_program};
use crate::value::Value;

/// Variable bindings: name → value mapping.
pub type Bindings = IndexMap<String, Value>;

/// Names injected into the unrestricted-eval namespace by the transformer
/// (`src`, `target`, `uuid5`) that are neither source slots nor tables. The
/// restricted evaluator does not bind them, so a qualified `{src.x}` must not be
/// flagged as an unknown reference. Mirrors Python
/// `eval_utils.INJECTED_EVAL_NAMES`; shared with normalization-time reference
/// checking (see `crate::normalize`).
pub const INJECTED_EVAL_NAMES: &[&str] = &["src", "target", "uuid5"];

/// Names of the built-in expression functions. A qualified `{func.x}` root
/// resolves to a function (not an unknown name), so it must not fail loud;
/// normalization-time reference checking treats these as known too. Mirrors the
/// keys of Python `eval_utils.FUNCTIONS` (plus the `list`/`tuple`/`dict`/`set`
/// constructors simpleeval adds, and the injected `slot`); `builtins::function_names` is the
/// table the evaluator dispatches on and a unit test keeps the two in step.
pub const FUNCTION_NAMES: &[&str] = &[
    "max",
    "min",
    "len",
    "sum",
    "sorted",
    "any",
    "all",
    "reversed",
    "first",
    "last",
    "str",
    "int",
    "float",
    "bool",
    "abs",
    "round",
    "floor",
    "ceil",
    "strlen",
    "uuid5",
    "substr",
    "upper",
    "lower",
    "title",
    "capitalize",
    "strip",
    "lstrip",
    "rstrip",
    "replace",
    "startswith",
    "endswith",
    "contains",
    "split",
    "join",
    "slugify",
    "to_snake",
    "to_camel",
    "to_pascal",
    "is_str",
    "is_int",
    "is_float",
    "is_bool",
    "is_list",
    "case",
    "coalesce",
    "is_numeric",
    "list",
    "tuple",
    "dict",
    "set",
    "slot",
];

/// True when `name` is a built-in expression function.
fn is_function_name(name: &str) -> bool {
    FUNCTION_NAMES.contains(&name)
}

thread_local! {
    /// When set, an unbound identifier (or an unresolved `slot()` lookup)
    /// raises [`ExprError::Eval`] instead of resolving to [`Value::Null`].
    ///
    /// Default (lax) behaviour is off, so all existing public entry points are
    /// unchanged. The `*_strict` entry points ([`eval_parsed_strict`],
    /// [`eval_expr_with_mapping_strict`]) flip this on for the duration of the
    /// evaluation via [`with_strict_mode`]. Modelled on [`OBJECT_ATTR_MODE`].
    static STRICT_MODE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run `f` with strict unbound-name semantics set to `strict`, restoring the
/// prior mode afterwards (nesting-safe).
pub(crate) fn with_strict_mode<T>(strict: bool, f: impl FnOnce() -> T) -> T {
    STRICT_MODE.with(|m| {
        let prev = m.get();
        m.set(strict);
        let out = f();
        m.set(prev);
        out
    })
}

/// True when strict unbound-name mode is currently active.
fn strict_mode_enabled() -> bool {
    STRICT_MODE.with(|m| m.get())
}

/// Resolve `name` from `vars`, honouring the current strict mode: in lax mode an
/// unbound name yields [`Value::Null`]; in strict mode it is an error.
///
/// PARITY: inside a statement block (see [`with_object_attr_mode`]) an unbound
/// name is always an error, strict or not. asteval has no lax mode: reading a
/// name missing from its symbol table is a `NameError`
/// (`asteval.Interpreter._getsym`), which upstream `_unrestricted_eval`
/// re-raises as `ValueError`.
fn resolve_name(name: &str, vars: &Bindings) -> ExprResult<Value> {
    match vars.get(name) {
        Some(v) => Ok(v.clone()),
        None => {
            if strict_mode_enabled() || object_attr_mode_enabled() {
                let message = format!("name '{name}' is not defined");
                Err(ExprError::Undefined {
                    name: name.to_string(),
                    message,
                })
            } else {
                Ok(Value::Null)
            }
        }
    }
}

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

    /// Evaluate this pre-parsed expression against `vars` in **strict** mode,
    /// where unbound names error instead of resolving to [`Value::Null`].
    ///
    /// Equivalent to [`eval_parsed_strict`]; provided as a method for ergonomics.
    pub fn eval_strict(&self, vars: &Bindings) -> ExprResult<Value> {
        eval_parsed_strict(self, vars)
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
    // Statement blocks route to the statement parser; single expressions keep
    // the cached single-AST fast path untouched. The split mirrors upstream
    // `_is_statement_block`: whatever parses as one expression is one.
    Ok(ParsedExpr {
        inner: match parse_single_or_program(expr)? {
            SingleOrProgram::Single(ast) => ParsedInner::Single(ast),
            SingleOrProgram::Program(program) => ParsedInner::Program(program),
        },
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
        ParsedInner::Program(program) => program.eval(vars),
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

/// Evaluate a pre-parsed expression in **strict** mode.
///
/// Identical to [`eval_parsed`] except that an unbound identifier — or an
/// unresolved `slot()` lookup — returns `Err(ExprError::Eval(..))` instead of
/// resolving to [`Value::Null`]. The default ([`eval_parsed`]) stays lax, so
/// existing callers are unchanged; strict mode is fully opt-in.
pub fn eval_parsed_strict(parsed: &ParsedExpr, vars: &Bindings) -> ExprResult<Value> {
    with_strict_mode(true, || eval_parsed(parsed, vars))
}

/// String-in convenience for strict evaluation; equivalent to
/// `eval_parsed_strict(&parse_expr(expr)?, vars)`. See [`eval_parsed_strict`].
pub fn eval_expr_with_mapping_strict(expr: &str, vars: &Bindings) -> ExprResult<Value> {
    let parsed = parse_expr(expr)?;
    eval_parsed_strict(&parsed, vars)
}

/// Convenience: evaluate against an empty binding set.
pub fn eval_expr(expr: &str) -> ExprResult<Value> {
    let vars = Bindings::new();
    eval_expr_with_mapping(expr, &vars)
}

/// Parse `expr` and return the top-level expression [`Ast`] node(s) it carries.
///
/// A single expression yields one node; a multi-statement (`asteval`-style)
/// block yields one node per assignment RHS, `if` condition, and expression
/// statement (recursing into nested `if` bodies). The `"None"` literal yields a
/// single [`Ast::None`]. This is the Rust counterpart to Python's
/// `ast.parse(expr, mode="exec")` + `ast.walk` used by
/// `expression_locations.extract_*`; callers walk each returned node to find
/// braced references and table references.
///
/// # Errors
/// Propagates a parse error for a malformed expression (mirroring upstream
/// surfacing a `SyntaxError` rather than masking it).
pub fn expression_asts(expr: &str) -> ExprResult<Vec<Ast>> {
    if expr == "None" {
        return Ok(vec![Ast::None]);
    }
    Ok(match parse_single_or_program(expr)? {
        SingleOrProgram::Single(ast) => vec![ast],
        SingleOrProgram::Program(program) => program.exprs().into_iter().cloned().collect(),
    })
}

pub(super) fn eval_ast(node: &Ast, vars: &Bindings) -> ExprResult<Value> {
    match node {
        Ast::Int(i) => Ok(Value::Int(*i)),
        Ast::Float(f) => Ok(Value::Float(*f)),
        Ast::Str(s) => Ok(Value::Str(s.clone())),
        Ast::Bool(b) => Ok(Value::Bool(*b)),
        Ast::None => Ok(Value::Null),

        Ast::Name(name) => resolve_name(name, vars),

        // {x} resolves identically to bare x (`LinkMLEvaluator._eval_set`) ...
        // PARITY: ... except in a statement block, which runs under `asteval`,
        // where `{x}` is an ordinary one-element set display.
        Ast::Brace(inner) => {
            if object_attr_mode_enabled() {
                compound::eval_brace_as_set(inner, vars)
            } else {
                eval_ast(inner, vars)
            }
        }
        Ast::Set(elts) => compound::eval_set_display(elts, vars),
        Ast::Dict(pairs) => compound::eval_dict_display(pairs, vars),
        Ast::Comp(c) => compound::eval_comprehension(c, vars),

        // Lazy: only the branch taken is evaluated; truthiness is Python's.
        Ast::IfExp { test, body, orelse } => {
            if eval_ast(test, vars)?.is_truthy() {
                eval_ast(body, vars)
            } else {
                eval_ast(orelse, vars)
            }
        }

        Ast::Attribute { value, attr } => {
            // A qualified `{Name.attr}` whose root Name is genuinely unbound —
            // not an injected eval name, not a function, and absent from the
            // bindings — is a broken reference (a typo or a missing/renamed
            // table) and fails loud even in lax mode. Mirrors upstream
            // `RestrictedEvaluator._eval_attribute` (utils/eval_utils.py at
            // 66cbcc31b5), where the root is resolved via the base name lookup
            // and a `NameNotDefined` there becomes a `NameError`. A *bare*
            // `{col}` (a Name, not an Attribute) and a root that resolves to
            // None (a sparse-join miss, or an absent schema slot bound to Null)
            // both stay null — only a root with no possible binding errors.
            if let Ast::Name(root) = value.as_ref()
                && !INJECTED_EVAL_NAMES.contains(&root.as_str())
                && !is_function_name(root)
                && !vars.contains_key(root)
            {
                let message = format!(
                    "Expression references unknown '{root}' in '{root}.{attr}': \
                     no such source class, join, or slot. Typo or stale reference?"
                );
                return Err(ExprError::Undefined {
                    name: root.clone(),
                    message,
                });
            }
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

        Ast::Call {
            func,
            args,
            keywords,
        } => {
            // case() arguments are (cond, val) pairs that must be evaluated;
            // they go through the generic arg evaluation below.
            let mut evaled = Vec::with_capacity(args.len());
            for a in args {
                evaled.push(eval_ast(a, vars)?);
            }
            if func == "slot" {
                if let Some((name, _)) = keywords.first() {
                    return Err(ExprError::Eval(format!(
                        "slot() got an unexpected keyword argument '{name}'"
                    )));
                }
                return call_slot_function(evaled, vars);
            }
            if keywords.is_empty() {
                return builtins::call_function(func, evaled);
            }
            let mut kwargs = Vec::with_capacity(keywords.len());
            for (name, value) in keywords {
                kwargs.push((name.clone(), eval_ast(value, vars)?));
            }
            builtins::call_function_kw(func, evaled, kwargs)
        }
    }
}

fn call_slot_function(args: Vec<Value>, vars: &Bindings) -> ExprResult<Value> {
    if args.len() != 1 {
        return Err(ExprError::Eval(format!(
            "slot() takes 1 argument ({} given)",
            args.len()
        )));
    }
    let slot_name = match &args[0] {
        Value::Str(s) => s,
        other => {
            return Err(ExprError::Eval(format!(
                "slot() argument must be str, got {}",
                type_name(other)
            )));
        }
    };
    let resolved = match vars.get("__slot_values") {
        Some(Value::Map(slots)) => slots.get(slot_name).cloned(),
        _ => None,
    };
    match resolved {
        Some(v) => Ok(v),
        None => {
            // PARITY: in a statement block `slot` is the plain lambda
            // `tgt_attrs.get(name)` (ObjectTransformer._derive_class), so an
            // unresolved slot is None there, whatever the strict setting.
            if strict_mode_enabled() && !object_attr_mode_enabled() {
                Err(ExprError::Eval(format!(
                    "slot '{slot_name}' is not defined"
                )))
            } else {
                Ok(Value::Null)
            }
        }
    }
}

// --- subscript (mirrors Python __getitem__ for list/str/dict) ---

pub(super) fn eval_subscript(obj: &Value, idx: &Value) -> ExprResult<Value> {
    match obj {
        Value::List(items) => {
            let i = match idx {
                Value::Int(i) => *i,
                Value::Bool(b) => *b as i64,
                other => {
                    return Err(ExprError::Eval(format!(
                        "list indices must be integers, not {}",
                        type_name(other)
                    )));
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
                    )));
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

/// True while a statement block is being evaluated (see [`with_object_attr_mode`]).
pub(super) fn object_attr_mode_enabled() -> bool {
    OBJECT_ATTR_MODE.with(|c| c.get())
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
            if object_attr_mode_enabled() {
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

pub(super) fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "None",
        Value::Bool(_) => "bool",
        Value::Int(_) => "int",
        Value::Float(_) => "float",
        Value::Decimal(_) => "Decimal",
        Value::Temporal(t) => t.type_name(),
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

pub(super) fn membership(needle: &Value, haystack: &Value) -> ExprResult<bool> {
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
    if is_real_number(left)
        && let Value::Str(s) = right
        && let Some(coerced) = coerce_str_to_type_of(s, left)
    {
        return (left.clone(), coerced);
    }
    // right numeric (non-bool), left str → coerce left to type(right)
    if is_real_number(right)
        && let Value::Str(s) = left
        && let Some(coerced) = coerce_str_to_type_of(s, right)
    {
        return (coerced, right.clone());
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
/// decimal point).
fn parse_python_int(s: &str) -> Option<i64> {
    s.trim().parse::<i64>().ok()
}

// --- arithmetic numeric-string coercion (mirrors upstream #285) ---
// PARITY: _coerce_like, _maybe_coerce_arithmetic, _warn_non_numeric added in
// upstream linkml/linkml-map PR #288 (commits 8966fe7eda, 328dc30f52)
// to fix #285 — numeric-looking string-typed columns silently corrupting
// arithmetic expressions via Python's string-repetition semantics.

/// Convert numeric string `s` to `num`'s type, falling back to float.
/// Returns `None` when `s` is not numeric-looking.
/// Mirrors Python `_coerce_like` at `linkml_map.utils.eval_utils`.
fn coerce_like(s: &str, num: &Value) -> Option<Value> {
    match coerce_str_to_type_of(s, num) {
        Some(v) => Some(v),
        None => {
            // Fallback: try numeric → float (mirrors _try_numeric(s)).
            // `int("3.14")` fails but Python's _try_numeric returns 3.14.
            crate::value::parse_python_float(s).map(Value::Float)
        }
    }
}

/// Coerce a numeric-string operand so an arithmetic operator evaluates
/// numerically. Mirrors Python `_maybe_coerce_arithmetic`.
///
/// When exactly one operand is a real number (int/float, not bool) and the
/// other is a numeric-looking string, the string is converted to a number
/// matching the number's type when it parses cleanly, else float.
///
/// Two strings are returned unchanged (so `"a" + "b"` still concatenates).
/// A number paired with a non-numeric string is returned unchanged.
fn maybe_coerce_arithmetic(left: &Value, right: &Value) -> (Value, Value) {
    // left str, right real-number (non-bool), coerce left to numeric
    if let Value::Str(s) = left
        && is_real_number(right)
        && let Some(coerced) = coerce_like(s, right)
    {
        return (coerced, right.clone());
    }
    // right str, left real-number (non-bool), coerce right to numeric
    if let Value::Str(s) = right
        && is_real_number(left)
        && let Some(coerced) = coerce_like(s, left)
    {
        return (left.clone(), coerced);
    }
    (left.clone(), right.clone())
}

/// Log that an operator got a non-numeric operand and is returning None.
/// Mirrors Python `_warn_non_numeric`.
fn warn_non_numeric(op_name: &str, left: &Value, right: &Value) {
    warn!("Non-numeric operand in {op_name}: {left:?}, {right:?}; returning None");
}

// --- binary arithmetic / bitwise (null-propagating with numeric retry) ---

fn eval_binary(op: BinOp, left: Value, right: Value) -> ExprResult<Value> {
    // Null propagation.
    if left.is_null() || right.is_null() {
        return Ok(Value::Null);
    }
    // PARITY: mirrors Python _null_propagating_arithmetic — coerce numeric-looking
    // strings to numbers BEFORE trying the native op, so "71" * 365 == 25915 not
    // string repetition (linkml/linkml-map#285).
    let (left, right) = maybe_coerce_arithmetic(&left, &right);
    // After coercion, a lone string opposite a number is non-numeric: treat it
    // as a failed numeric op rather than letting * repeat the string.
    if matches!(&left, Value::Str(_)) != matches!(&right, Value::Str(_)) {
        warn_non_numeric(binop_name(op), &left, &right);
        return Ok(Value::Null);
    }
    // Try native op first — two strings still concatenate, list*int stays
    // repetition, int op int stays int.
    match try_native_binary(op, &left, &right) {
        Ok(Some(v)) => Ok(v),
        Ok(None) | Err(_) => {
            // TypeError/ValueError path: retry by coercing both to float.
            let ln = left.try_numeric();
            let rn = right.try_numeric();
            match (ln, rn) {
                (Some(a), Some(b)) => numeric_binary_f64(op, a, b),
                _ => {
                    warn_non_numeric(binop_name(op), &left, &right);
                    Ok(Value::Null)
                }
            }
        }
    }
}

/// Human-readable name for a binary operator, used in warning messages.
fn binop_name(op: BinOp) -> &'static str {
    use BinOp::*;
    match op {
        Add => "add",
        Sub => "sub",
        Mul => "mul",
        Div => "div",
        FloorDiv => "floordiv",
        Mod => "mod",
        Pow => "pow",
        LShift => "lshift",
        RShift => "rshift",
        BitAnd => "bitand",
        BitOr => "bitor",
        BitXor => "bitxor",
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

    // list * int  / int * list  → repetition (genuine Python list semantics)
    if op == BinOp::Mul {
        if let (List(items), Int(n)) = (left, right) {
            let n = (*n).max(0) as usize;
            let mut out = Vec::with_capacity(items.len() * n);
            for _ in 0..n {
                out.extend(items.iter().cloned());
            }
            return Ok(Some(List(out)));
        }
        if let (Int(n), List(items)) = (left, right) {
            let n = (*n).max(0) as usize;
            let mut out = Vec::with_capacity(items.len() * n);
            for _ in 0..n {
                out.extend(items.iter().cloned());
            }
            return Ok(Some(List(out)));
        }
    }

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

// --- scalar function implementations mirroring Python builtins ---

pub(crate) fn py_str(v: &Value) -> String {
    match v {
        Value::Null => "None".to_string(),
        Value::Bool(b) => if *b { "True" } else { "False" }.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => format_py_float(*f),
        Value::Decimal(d) => d.to_sci_string(),
        Value::Temporal(t) => t.to_string(),
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

pub(crate) fn py_repr(v: &Value) -> String {
    match v {
        Value::Str(s) => crate::pyfmt::py_repr_str(s),
        Value::Decimal(d) => format!("Decimal('{}')", d.to_sci_string()),
        Value::Temporal(t) => t.repr(),
        other => py_str(other),
    }
}

fn format_py_float(f: f64) -> String {
    // Delegates to the exact Python `repr(float)` (the previous body diverged
    // for |f| >= 1e16 and < 1e-4).
    crate::pyfmt::py_float_repr(f)
}

pub(crate) fn py_int(v: &Value) -> ExprResult<Value> {
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

pub(crate) fn py_float(v: &Value) -> ExprResult<Value> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(expr: &str) -> Value {
        eval_expr(expr).unwrap()
    }

    // --- Task 1: string functions ---

    #[test]
    fn test_lower() {
        assert_eq!(eval("lower('HeLLo')"), Value::Str("hello".into()));
    }

    #[test]
    fn test_upper() {
        assert_eq!(eval("upper('HeLLo')"), Value::Str("HELLO".into()));
    }

    #[test]
    fn test_title() {
        assert_eq!(
            eval("title('hello world')"),
            Value::Str("Hello World".into())
        );
        // Letters after a non-letter are uppercased; rest lowercased.
        assert_eq!(
            eval("title('they\\'re BILL\\'s friends')"),
            Value::Str("They'Re Bill'S Friends".into())
        );
        assert_eq!(eval("title('a1b')"), Value::Str("A1B".into()));
    }

    #[test]
    fn test_capitalize() {
        assert_eq!(
            eval("capitalize('hELLO wORLD')"),
            Value::Str("Hello world".into())
        );
        assert_eq!(eval("capitalize('')"), Value::Str("".into()));
    }

    #[test]
    fn test_slugify() {
        // PARITY: upstream `_slugify` defaults to separator "_" and returns None
        // when nothing identifier-shaped remains (see expr/slugify.rs).
        assert_eq!(
            eval("slugify('  Hello, World!  ')"),
            Value::Str("hello_world".into())
        );
        assert_eq!(
            eval("slugify('Foo___Bar  Baz')"),
            Value::Str("foo_bar_baz".into())
        );
        assert_eq!(eval("slugify('---abc---')"), Value::Str("abc".into()));
        assert_eq!(eval("slugify('!!!')"), Value::Null);
        assert_eq!(eval("slugify('')"), Value::Null);
        assert_eq!(
            eval("slugify('caf\u{e9}-au-lait', '-')"),
            Value::Str("cafe-au-lait".into())
        );
        assert_eq!(eval("slugify('123 hits')"), Value::Str("_123_hits".into()));
        // `or`-chain fallback, the documented use.
        assert_eq!(
            eval("slugify('!!!') or 'anonymous'"),
            Value::Str("anonymous".into())
        );
        // Distributes over a list; None stays None.
        assert_eq!(
            eval("slugify(['A b', None, '!'])"),
            Value::List(vec![Value::Str("a_b".into()), Value::Null, Value::Null])
        );
    }

    #[test]
    fn test_string_funcs_non_string_input() {
        // PARITY: upstream binds the unbound `str` methods (`str.upper`, ...), so
        // a non-string argument is a TypeError rather than being stringified.
        assert!(eval_expr("upper(123)").is_err());
        assert!(eval_expr("lower(True)").is_err());
        assert!(eval_expr("slugify(123)").is_err());
    }

    #[test]
    fn test_string_funcs_distribute_over_list() {
        let mut vars = Bindings::new();
        vars.insert(
            "xs".into(),
            Value::List(vec![Value::Str("aB".into()), Value::Str("cD".into())]),
        );
        let got = eval_expr_with_mapping("upper(xs)", &vars).unwrap();
        assert_eq!(
            got,
            Value::List(vec![Value::Str("AB".into()), Value::Str("CD".into())])
        );
    }

    #[test]
    fn test_string_funcs_null_passthrough() {
        // None scalar arg → None.
        assert_eq!(eval("lower(None)"), Value::Null);
        assert_eq!(eval("slugify(None)"), Value::Null);
        // None inside a list distributes to None.
        let mut vars = Bindings::new();
        vars.insert(
            "xs".into(),
            Value::List(vec![Value::Str("Ab".into()), Value::Null]),
        );
        let got = eval_expr_with_mapping("title(xs)", &vars).unwrap();
        assert_eq!(got, Value::List(vec![Value::Str("Ab".into()), Value::Null]));
    }

    // --- Task 2: strict mode on unbound names ---

    #[test]
    fn test_unbound_name_lax_is_null() {
        let vars = Bindings::new();
        assert_eq!(
            eval_expr_with_mapping("missing", &vars).unwrap(),
            Value::Null
        );
        // Braced form resolves identically.
        assert_eq!(
            eval_expr_with_mapping("{missing}", &vars).unwrap(),
            Value::Null
        );
    }

    #[test]
    fn test_unbound_name_strict_is_err() {
        let vars = Bindings::new();
        assert!(eval_expr_with_mapping_strict("missing", &vars).is_err());
        assert!(eval_expr_with_mapping_strict("{missing}", &vars).is_err());
    }

    #[test]
    fn test_bound_name_strict_ok() {
        let mut vars = Bindings::new();
        vars.insert("x".into(), Value::Int(7));
        assert_eq!(
            eval_expr_with_mapping_strict("x", &vars).unwrap(),
            Value::Int(7)
        );
    }

    #[test]
    fn test_strict_does_not_leak_to_lax() {
        // After a strict eval, a subsequent lax eval must still return Null.
        let vars = Bindings::new();
        assert!(eval_expr_with_mapping_strict("missing", &vars).is_err());
        assert_eq!(
            eval_expr_with_mapping("missing", &vars).unwrap(),
            Value::Null
        );
    }

    #[test]
    fn test_qualified_unknown_root_fails_loud() {
        // A qualified `{Unknown.col}` whose root is unbound raises, even in lax
        // mode — distinct from a bare `{missing}` and a known-but-None root.
        let mut vars = Bindings::new();
        vars.insert("a".into(), Value::Int(1));
        let err = eval_expr_with_mapping("{Nope.col}", &vars).unwrap_err();
        assert!(err.to_string().contains("unknown 'Nope'"), "got: {err}");
    }

    #[test]
    fn test_bare_absent_and_known_none_stay_null() {
        // Bare absent names and known-but-None roots null out rather than raising.
        let mut vars = Bindings::new();
        vars.insert("a".into(), Value::Int(1));
        assert_eq!(
            eval_expr_with_mapping("{missing}", &vars).unwrap(),
            Value::Null
        );
        // Root `a` is bound (to None) → getattr(None, b) → Null, no raise.
        let mut vars2 = Bindings::new();
        vars2.insert("a".into(), Value::Null);
        assert_eq!(
            eval_expr_with_mapping("{a.b}", &vars2).unwrap(),
            Value::Null
        );
    }

    #[test]
    fn test_slot_strict_unresolved_is_err() {
        // No __slot_values bound at all → lax Null, strict error.
        let vars = Bindings::new();
        assert_eq!(
            eval_expr_with_mapping("slot('s')", &vars).unwrap(),
            Value::Null
        );
        assert!(eval_expr_with_mapping_strict("slot('s')", &vars).is_err());

        // __slot_values present but missing the key → same lax/strict split.
        let mut vars2 = Bindings::new();
        let mut slots = IndexMap::new();
        slots.insert("present".to_string(), Value::Int(1));
        vars2.insert("__slot_values".into(), Value::Map(slots));
        assert_eq!(
            eval_expr_with_mapping("slot('absent')", &vars2).unwrap(),
            Value::Null
        );
        assert!(eval_expr_with_mapping_strict("slot('absent')", &vars2).is_err());
        // A present slot resolves in strict mode.
        assert_eq!(
            eval_expr_with_mapping_strict("slot('present')", &vars2).unwrap(),
            Value::Int(1)
        );
    }

    // --- Task 3: arithmetic coercion (#285) ---

    #[test]
    fn test_arithmetic_multiply_numeric_string_is_numeric() {
        // "71" * 365 == 25915 (int), not string repetition
        let mut vars = Bindings::new();
        vars.insert("col".into(), Value::Str("71".into()));
        assert_eq!(
            eval_expr_with_mapping("{col} * 365", &vars).unwrap(),
            Value::Int(25915)
        );
        // Same result as with int column
        vars.insert("col".into(), Value::Int(71));
        assert_eq!(
            eval_expr_with_mapping("{col} * 365", &vars).unwrap(),
            Value::Int(25915)
        );
        // 365 * "71" (RHS path)
        vars.insert("col".into(), Value::Str("71".into()));
        assert_eq!(
            eval_expr_with_mapping("365 * {col}", &vars).unwrap(),
            Value::Int(25915)
        );
        // "3.14" * 365 ~= 1146.1
        vars.insert("col".into(), Value::Str("3.14".into()));
        let got = eval_expr_with_mapping("{col} * 365", &vars).unwrap();
        match got {
            Value::Float(f) => assert!((f - 1146.1).abs() < 0.5, "got {f}"),
            _ => panic!("expected Float, got {got:?}"),
        }
        // ("71" * 365) + 1825 == 27740
        vars.insert("col".into(), Value::Str("71".into()));
        assert_eq!(
            eval_expr_with_mapping("({col} * 365) + 1825", &vars).unwrap(),
            Value::Int(27740)
        );
    }

    #[test]
    fn test_arithmetic_multiply_string_and_number_preserves_concatenation_and_lists() {
        let mut vars = Bindings::new();
        // Two strings still concatenate with +
        vars.insert("a".into(), Value::Str("foo".into()));
        vars.insert("b".into(), Value::Str("bar".into()));
        assert_eq!(
            eval_expr_with_mapping("{a} + {b}", &vars).unwrap(),
            Value::Str("foobar".into())
        );
        // "71" + "365" == "71365" (concatenation, not addition)
        vars.insert("a".into(), Value::Str("71".into()));
        vars.insert("b".into(), Value::Str("365".into()));
        assert_eq!(
            eval_expr_with_mapping("{a} + {b}", &vars).unwrap(),
            Value::Str("71365".into())
        );
        // A list times a number is genuine repetition/distribution
        vars.insert("x".into(), Value::List(vec![Value::Int(1), Value::Int(2)]));
        assert_eq!(
            eval_expr_with_mapping("{x} * 3", &vars).unwrap(),
            Value::List(vec![
                Value::Int(1),
                Value::Int(2),
                Value::Int(1),
                Value::Int(2),
                Value::Int(1),
                Value::Int(2),
            ])
        );
    }

    #[test]
    fn test_arithmetic_non_numeric_string_returns_none() {
        let mut vars = Bindings::new();
        // Non-numeric string in arithmetic returns None
        vars.insert("x".into(), Value::Str("100".into()));
        vars.insert("y".into(), Value::Str("abc".into()));
        assert_eq!(eval_expr_with_mapping("x / y", &vars).unwrap(), Value::Null);
        // "abc" * 10 returns None, not "abcabc..."
        vars.insert("x".into(), Value::Str("abc".into()));
        vars.insert("y".into(), Value::Int(10));
        assert_eq!(eval_expr_with_mapping("x * y", &vars).unwrap(), Value::Null);
        // 10 * "abc" returns None
        vars.insert("x".into(), Value::Int(10));
        vars.insert("y".into(), Value::Str("abc".into()));
        assert_eq!(eval_expr_with_mapping("x * y", &vars).unwrap(), Value::Null);
        // "abc" + 10 returns None
        vars.insert("x".into(), Value::Str("abc".into()));
        vars.insert("y".into(), Value::Int(10));
        assert_eq!(eval_expr_with_mapping("x + y", &vars).unwrap(), Value::Null);
    }

    #[test]
    fn test_arithmetic_bool_not_treated_as_number() {
        let mut vars = Bindings::new();
        // bool is NOT a real number for coercion purposes
        vars.insert("x".into(), Value::Bool(true));
        vars.insert("y".into(), Value::Str("1".into()));
        // Bool + "string" — bool is not treated as a number for _maybe_coerce_arithmetic.
        // But in Python, bool IS treated as int for the native operator.
        // The key test: True * "1" stays True * "1" (not coerced to 1 * 1).
        // Since True is not `is_real_number`, maybe_coerce_arithmetic returns unchanged.
        // Then the native op runs: True * "1" — but in Rust, try_native_binary
        // with (Bool, Str) and op=Mul falls through to None → numeric retry.
        // As it should: Python's `True * "1"` == "1" (repetition), but arithmetic
        // wrapper prevents this.
        let got = eval_expr_with_mapping("x * y", &vars).unwrap();
        assert_eq!(got, Value::Null, "True * '1' should be None, not '1'");
    }
}
