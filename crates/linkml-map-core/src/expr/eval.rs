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

use std::borrow::Cow;

use indexmap::IndexMap;
use log::warn;

use super::builtins;
use super::compound;
use super::error::{ExprError, ExprResult};
use super::gate::{self, EvalMode, eval_parsed_gated};
use super::numeric;
use super::parser::{Ast, BinOp, BoolOp, CmpOp, UnOp};
use super::pymethods;
use super::stmt::{Program, SingleOrProgram, parse_single_or_program};
use super::syntax;
use crate::pynum::{self, BigInt, IntRef};
use crate::value::Value;
use num_traits::Signed;

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
    /// evaluation via [`with_strict_mode`].
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
fn resolve_name(name: &str, vars: &Bindings) -> ExprResult<Value> {
    match vars.get(name) {
        Some(v) => Ok(v.clone()),
        // PARITY: simpleeval's `_eval_name` falls back to `self.functions[name]`, so
        // a bare function name (`upper`, `max`) evaluates to the function itself,
        // which has no `Value`.
        None if is_function_name(name) => Err(ExprError::unrepresentable("function")),
        None => {
            if strict_mode_enabled() {
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
    pub(super) inner: ParsedInner,
}

/// Internal representation: a single cached expression (the fast path), the
/// `"None"` literal short-circuit, or a multi-statement program (the
/// `asteval`-style block path).
#[derive(Debug, Clone, PartialEq)]
pub(super) enum ParsedInner {
    /// `"None"` literal short-circuit — always evaluates to [`Value::Null`].
    NoneLiteral,
    /// A single parsed expression tree (the common case + fast path).
    Single(Ast),
    /// A multi-statement program (`target = ...` blocks, `if`, comprehensions).
    Program(Program),
}

impl ParsedExpr {
    /// Evaluate this pre-parsed expression against `vars` with the restricted
    /// (default) evaluator.
    ///
    /// Equivalent to [`eval_parsed`]; provided as a method for ergonomics.
    pub fn eval(&self, vars: &Bindings) -> ExprResult<Value> {
        eval_parsed(self, vars)
    }

    /// Evaluate this pre-parsed expression against `vars` with
    /// `unrestricted_eval` enabled.
    ///
    /// Equivalent to [`eval_parsed_gated`] with [`EvalMode::UNRESTRICTED`]: a
    /// statement block runs in the asteval-style interpreter, reading `vars` as
    /// the source object `src`.
    pub fn eval_unrestricted(&self, vars: &Bindings) -> ExprResult<Value> {
        eval_parsed_gated(self, vars, EvalMode::UNRESTRICTED)
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

/// Evaluate a pre-parsed expression against the given `bindings` with the
/// restricted evaluator.
///
/// This performs no parsing — it walks the cached [`Ast`]/program directly.
/// Unbound names resolve to [`Value::Null`]. Mirrors upstream
/// `eval_utils.eval_expr_with_mapping` (the `simpleeval` evaluator): a
/// statement block runs only its first statement, and no `src` is bound. Use
/// [`eval_parsed_gated`] for the `unrestricted_eval` split.
pub fn eval_parsed(parsed: &ParsedExpr, vars: &Bindings) -> ExprResult<Value> {
    gate::eval_restricted(parsed, vars)
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
        SingleOrProgram::Program(program) => {
            if let Some(err) = program.syntax_error() {
                return Err(err.clone());
            }
            program.exprs().into_iter().cloned().collect()
        }
    })
}

/// The object whose attribute `value.attr` reads: the part of
/// `LinkMLEvaluator._eval_attribute` before `_distributed_getattr`. A bound root
/// name is borrowed from `vars` (resolving it would clone a whole record to read
/// one attribute).
pub(super) fn attribute_receiver<'a>(
    value: &Ast,
    attr: &str,
    vars: &'a Bindings,
) -> ExprResult<Cow<'a, Value>> {
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
    if let Ast::Name(root) = value
        && !INJECTED_EVAL_NAMES.contains(&root.as_str())
        && !is_function_name(root)
    {
        // PARITY: the root is resolved from the bindings the expression
        // was given, never from an enclosing comprehension's scope
        // (`super()._eval_name` bypasses the swapped-in comprehension
        // name handler), so `x.attr` on a loop variable `x` is refused
        // in restricted mode; asteval, behind `unrestricted_eval`,
        // evaluates it. See [`gate::comprehension_root`].
        match gate::comprehension_root(root) {
            gate::ComprehensionRoot::NotATarget => {}
            gate::ComprehensionRoot::Outer(outer) => {
                return Ok(Cow::Owned(outer));
            }
            gate::ComprehensionRoot::Unbound => {
                let message = format!(
                    "Attribute access on comprehension-bound '{root}' in '{root}.{attr}' \
                     is not supported. Building records in an expression bypasses the \
                     range, cardinality, and datatype coercion applied to derived objects; \
                     declare a class_derivation keyed on the source range class and reduce \
                     the slot to populated_from instead."
                );
                return Err(ExprError::Undefined {
                    name: root.clone(),
                    message,
                });
            }
        }
    }
    if let Ast::Name(root) = value
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
    // A bound root name (`x.attr`, or `{x}.attr` outside a statement
    // block, where `{x}` is just `x`) is read in place: `resolve_name`
    // would clone the whole bound value (a nested record, say) to read
    // one attribute.
    let root_ast = match value {
        Ast::Brace(inner) => inner.as_ref(),
        other => other,
    };
    if let Ast::Name(root) = root_ast
        && let Some(bound) = vars.get(root.as_str())
    {
        return Ok(Cow::Borrowed(bound));
    }

    Ok(Cow::Owned(eval_ast(value, vars)?))
}

pub(super) fn eval_ast(node: &Ast, vars: &Bindings) -> ExprResult<Value> {
    match node {
        Ast::Int(i) => Ok(Value::Int(*i)),
        Ast::BigInt(n) => Ok(Value::BigInt(n.clone())),
        Ast::Float(f) => Ok(Value::Float(*f)),
        Ast::Str(s) => Ok(Value::Str(s.clone())),
        Ast::Bool(b) => Ok(Value::Bool(*b)),
        Ast::None => Ok(Value::Null),

        Ast::Name(name) => resolve_name(name, vars),

        // {x} resolves identically to bare x (`LinkMLEvaluator._eval_set`).
        Ast::Brace(inner) => eval_ast(inner, vars),
        Ast::Set(elts) => compound::eval_set_display(elts),
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
            let obj = attribute_receiver(value, attr, vars)?;
            // PARITY: a dict built by the expression itself is a real `dict`, not a
            // `DynObj`: `_distributed_getattr` distributes over its values.
            if is_dict_construction(value)
                && let Value::Map(m) = obj.as_ref()
                && !attr.starts_with('_')
            {
                let items = m.values().map(|v| distributed_getattr(v, attr));
                return items.collect::<ExprResult<Vec<_>>>().map(Value::List);
            }
            distributed_getattr(&obj, attr)
        }

        Ast::List(elts) => syntax::eval_elements(elts, vars, true).map(Value::List),

        Ast::Subscript { value, index } => {
            let obj = eval_ast(value, vars)?;
            if let Ast::Slice { lower, upper, step } = &**index {
                return syntax::subscript_slice(&obj, lower, upper, step, vars);
            }
            let idx = eval_ast(index, vars)?;
            eval_subscript(&obj, &idx)
        }

        Ast::Tuple(elts) => {
            // Tuples are only meaningful as case() pair arguments; represent as
            // a list so the evaluator can index into them.
            syntax::eval_elements(elts, vars, false).map(Value::List)
        }

        Ast::Unary { op, operand } => {
            let v = eval_ast(operand, vars)?;
            eval_unary(*op, v)
        }

        Ast::Binary { op, left, right } => {
            if *op == BinOp::MatMul {
                return syntax::matmul_refused();
            }
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

        Ast::Imag(_)
        | Ast::Bytes(_)
        | Ast::Ellipsis
        | Ast::FString(_)
        | Ast::Slice { .. }
        | Ast::Starred(_)
        | Ast::DictUnpack
        | Ast::Lambda(_)
        | Ast::NamedExpr { .. }
        | Ast::CallValue { .. } => syntax::eval_syntax(node, vars),

        Ast::Call {
            func,
            args,
            keywords,
        } => {
            // PARITY: upstream rejects an unknown function before it evaluates
            // any argument (`LinkMLEvaluator._eval_call`), so an error in an
            // argument of an unknown call never surfaces.
            if func != "slot" {
                builtins::ensure_known_function(func)?;
            }
            // case() arguments are (cond, val) pairs that must be evaluated;
            // they go through the generic arg evaluation below.
            let evaled = syntax::eval_call_args(args, vars)?;
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
            let kwargs = syntax::eval_call_keywords(keywords, vars)?;
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
            if strict_mode_enabled() {
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

/// `IndexError` of an index that is no `ssize_t` (an integer beyond `i64`).
fn index_overflow() -> ExprError {
    ExprError::Uncaught("IndexError: cannot fit 'int' into an index-sized integer".into())
}

pub(super) fn eval_subscript(obj: &Value, idx: &Value) -> ExprResult<Value> {
    match obj {
        Value::List(items) => {
            let i = match idx {
                Value::Int(i) => *i,
                Value::Bool(b) => *b as i64,
                Value::BigInt(_) => return Err(index_overflow()),
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
                return Err(ExprError::Uncaught("list index out of range".into()));
            }
            Ok(items[resolved as usize].clone())
        }
        Value::Str(s) => {
            let chars: Vec<char> = s.chars().collect();
            let i = match idx {
                Value::Int(i) => *i,
                Value::Bool(b) => *b as i64,
                Value::BigInt(_) => return Err(index_overflow()),
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
                return Err(ExprError::Uncaught("string index out of range".into()));
            }
            Ok(Value::Str(chars[resolved as usize].to_string()))
        }
        Value::Map(m) => {
            // An unhashable key is a `TypeError`, a rejection; a missing hashable
            // one a `KeyError`. (PARITY: `m[(1, 2)]`, a tuple key, is hashable
            // upstream, but a tuple is a list here.)
            if matches!(idx, Value::List(_) | Value::Map(_)) {
                return Err(ExprError::Eval(format!(
                    "unhashable type: '{}'",
                    type_name(idx)
                )));
            }
            let key = match idx {
                Value::Str(k) => k.clone(),
                other => py_str(other),
            };
            match m.get(&key) {
                Some(v) => Ok(v.clone()),
                None => Err(ExprError::Uncaught(format!("KeyError: {key:?}"))),
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

/// Whether `node` builds a plain Python `dict` (a display, a comprehension or
/// `dict(...)`), as opposed to reading a bound value.
fn is_dict_construction(node: &Ast) -> bool {
    match node {
        Ast::Dict(_) => true,
        Ast::Comp(c) => c.kind == super::parser::CompKind::Dict,
        Ast::Call { func, .. } => func == "dict",
        _ => false,
    }
}

pub(super) fn distributed_getattr(obj: &Value, attr: &str) -> ExprResult<Value> {
    if attr.starts_with('_') {
        // PARITY: a `NameError` upstream (`_distributed_getattr`), which
        // `_eval_expr` never catches.
        return Err(ExprError::Uncaught(format!(
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
        // PARITY: a bound map is a `DynObj` (`dynamic_object` builds one for every
        // class-ranged value and `DynObj(**row)` for a joined row), and
        // `DynObj.__getattr__` is `vars(self).get(p, None)`: a missing key is
        // None, never a distribution over the values. Upstream's plain-`dict`
        // flavour (a value whose slot range is not a class, e.g. `range: Any`)
        // would distribute over its values and so raise `AttributeError` on a
        // scalar value; `Value` cannot tell the two apart and models the far
        // commoner `DynObj`.
        Value::Map(m) => Ok(m.get(attr).cloned().unwrap_or(Value::Null)),
        Value::Null => Ok(Value::Null),
        // A scalar's attributes are those of the Python object (`s.upper`,
        // `x.real`); a missing one is an `AttributeError` upstream, which
        // `_eval_expr` does not catch (asteval's `safe_getattr` gives None).
        other => pymethods::scalar_getattr(other, attr),
    }
}

pub(super) fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "None",
        Value::Bool(_) => "bool",
        Value::Int(_) | Value::BigInt(_) => "int",
        Value::Float(_) => "float",
        Value::Complex(_) => "complex",
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
            // `-(-2**63)` is 2**63 in Python, which `i64` cannot hold.
            Value::Int(i) => Ok(i
                .checked_neg()
                .map_or_else(|| Value::from_bigint(-BigInt::from(i)), Value::Int)),
            Value::BigInt(n) => Ok(Value::from_bigint(-*n)),
            Value::Float(f) => Ok(Value::Float(-f)),
            Value::Complex(z) => Ok(Value::from_complex(-*z)),
            Value::Bool(b) => Ok(Value::Int(-(b as i64))),
            other => Err(ExprError::Eval(format!(
                "bad operand type for unary -: '{}'",
                type_name(&other)
            ))),
        },
        UnOp::Pos => match v {
            Value::Int(_) | Value::BigInt(_) | Value::Float(_) | Value::Complex(_) => Ok(v),
            Value::Bool(b) => Ok(Value::Int(b as i64)),
            other => Err(ExprError::Eval(format!(
                "bad operand type for unary +: '{}'",
                type_name(&other)
            ))),
        },
        UnOp::Invert => match v {
            Value::Int(i) => Ok(Value::Int(!i)),
            Value::BigInt(n) => Ok(Value::from_bigint(pynum::invert(IntRef::Big(&n)))),
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
    let mut cur_node = left;
    let mut result = Value::Bool(true);
    for (op, rhs_node) in ops.iter().zip(comparators) {
        let rhs = eval_ast(rhs_node, vars)?;
        let pair = if matches!(op, CmpOp::Is | CmpOp::IsNot) {
            syntax::identity_compare(*op, cur_node, &cur, rhs_node, &rhs)
        } else {
            eval_single_compare(*op, &cur, &rhs)?
        };
        // Short-circuit on a false/null result, like `and`.
        match &pair {
            Value::Bool(true) => {}
            other => return Ok(other.clone()),
        }
        result = pair;
        cur = rhs;
        cur_node = rhs_node;
    }
    Ok(result)
}

/// `_try_numeric`: the number an operand retries as. Unlike
/// [`Value::try_numeric`] a `Decimal` is not numeric here (`_try_numeric` only
/// accepts `int`, `float` and `str`).
fn retry_numeric(v: &Value) -> Option<f64> {
    match v {
        Value::Decimal(_) => None,
        other => other.try_numeric(),
    }
}

fn eval_single_compare(op: CmpOp, left: &Value, right: &Value) -> ExprResult<Value> {
    // Two plain integers: no coercion, no null, and the order is `i64`'s.
    if let (Value::Int(a), Value::Int(b)) = (left, right) {
        match op {
            CmpOp::Eq => return Ok(Value::Bool(a == b)),
            CmpOp::NotEq => return Ok(Value::Bool(a != b)),
            CmpOp::Lt => return Ok(Value::Bool(a < b)),
            CmpOp::LtE => return Ok(Value::Bool(a <= b)),
            CmpOp::Gt => return Ok(Value::Bool(a > b)),
            CmpOp::GtE => return Ok(Value::Bool(a >= b)),
            _ => {}
        }
    }
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
        // Identity needs the operand expressions: see `syntax::identity_compare`.
        CmpOp::Is | CmpOp::IsNot => Ok(Value::Bool(false)),
        CmpOp::In | CmpOp::NotIn => {
            // _null_propagating (no coercion wrapper in Python for In/NotIn).
            if left.is_null() || right.is_null() {
                return Ok(Value::Null);
            }
            match membership(left, right) {
                Ok(contained) => Ok(Value::Bool(if op == CmpOp::In {
                    contained
                } else {
                    !contained
                })),
                // `_null_propagating` catches the `TypeError` of `"a" in 5` and
                // retries on `_try_numeric` operands: a non-numeric one gives
                // None (with a warning); when both are numeric the operator
                // raises its `TypeError` again from inside the handler.
                Err(e @ ExprError::Eval(_)) => {
                    if retry_numeric(left).is_some() && retry_numeric(right).is_some() {
                        Err(e)
                    } else {
                        warn_non_numeric(cmp_symbol(op), left, right);
                        Ok(Value::Null)
                    }
                }
                Err(e) => Err(e),
            }
        }
    }
}

fn ordering_compare(op: CmpOp, l: &Value, r: &Value) -> ExprResult<Value> {
    use std::cmp::Ordering;
    let ord: Option<Ordering> = match (l, r) {
        // numeric comparisons (bool counts as numeric for ordering in Python),
        // exact between an int and a float as in CPython
        _ if numeric::numeric_for_ordering(l).is_some()
            && numeric::numeric_for_ordering(r).is_some() =>
        {
            match (
                numeric::numeric_for_ordering(l),
                numeric::numeric_for_ordering(r),
            ) {
                (Some(a), Some(b)) => numeric::cmp_ordered(&a, &b),
                _ => unreachable!("checked above"),
            }
        }
        (Value::Str(a), Value::Str(b)) => Some(a.cmp(b)),
        // Lists order lexicographically (the first position whose items differ
        // decides, else the length), as `list_richcompare` does.
        (Value::List(_), Value::List(_)) => builtins::py_cmp(l, r, cmp_symbol(op))?,
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
        CmpOp::Is => "is",
        CmpOp::IsNot => "is not",
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
            // Hashing the needle comes first: a list or dict is a `TypeError`.
            Value::List(_) | Value::Map(_) => Err(ExprError::Eval(format!(
                "unhashable type: '{}'",
                type_name(needle)
            ))),
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
    matches!(v, Value::Int(_) | Value::BigInt(_) | Value::Float(_))
}

fn same_py_type(a: &Value, b: &Value) -> bool {
    matches!(
        (a, b),
        (Value::Null, Value::Null)
            | (Value::Bool(_), Value::Bool(_))
            | (
                Value::Int(_) | Value::BigInt(_),
                Value::Int(_) | Value::BigInt(_)
            )
            | (Value::Float(_), Value::Float(_))
            | (Value::Complex(_), Value::Complex(_))
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
        Value::Int(_) | Value::BigInt(_) => parse_python_int(s),
        Value::Float(_) => crate::value::parse_python_float(s).map(Value::Float),
        _ => None,
    }
}
/// decimal point). `None` also for more than 4300 digits (`ValueError`).
fn parse_python_int(s: &str) -> Option<Value> {
    super::pystr::int_value_from_str(s)
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
    // Two plain integers (by far the commonest operands): nothing to coerce, no
    // strings to refuse, and the operators with limits fall through below.
    match (&left, &right) {
        (Value::Int(a), Value::Int(b)) => {
            if let Some(v) = numeric::int_fast_path(op, *a, *b) {
                return Ok(v);
            }
        }
        (Value::Float(a), Value::Float(b)) => {
            if let Some(v) = numeric::float_fast_path(op, *a, *b) {
                return Ok(v);
            }
        }
        _ => {}
    }
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
    match try_native_binary(op, &left, &right)? {
        Some(v) => Ok(v),
        None => {
            // TypeError path: retry by coercing both to float.
            let ln = left.try_numeric();
            let rn = right.try_numeric();
            match (ln, rn) {
                // Python re-applies the operator to the numeric operands and a
                // bitwise operator on a float is a TypeError again, which
                // propagates (`_null_propagating_arithmetic`).
                (Some(_), Some(_)) if is_bitwise(op) && has_float_operand(&left, &right) => {
                    Err(ExprError::Eval(format!(
                        "unsupported operand type(s) for {}: '{}' and '{}'",
                        binop_symbol(op),
                        retried_type_name(&left),
                        retried_type_name(&right)
                    )))
                }
                (Some(a), Some(b)) => numeric::numeric_binary_f64(op, a, b),
                _ => {
                    warn_non_numeric(binop_name(op), &left, &right);
                    Ok(Value::Null)
                }
            }
        }
    }
}

/// Bitwise and shift operators, which Python defines for `int` only.
fn is_bitwise(op: BinOp) -> bool {
    matches!(
        op,
        BinOp::LShift | BinOp::RShift | BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor
    )
}

/// Whether either operand is, or retries as, a float (a numeric string becomes
/// one under `_try_numeric`).
fn has_float_operand(left: &Value, right: &Value) -> bool {
    [left, right]
        .iter()
        .any(|v| matches!(v, Value::Float(_) | Value::Str(_)))
}

/// The Python type name of an operand after the `_try_numeric` retry: a string
/// is a float by then.
fn retried_type_name(v: &Value) -> &'static str {
    match v {
        Value::Str(_) => "float",
        other => type_name(other),
    }
}

/// The source symbol of a binary operator, for `TypeError` messages.
pub(super) fn binop_symbol(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::FloorDiv => "//",
        BinOp::Mod => "%",
        BinOp::Pow => "** or pow()",
        BinOp::LShift => "<<",
        BinOp::RShift => ">>",
        BinOp::BitAnd => "&",
        BinOp::BitOr => "|",
        BinOp::BitXor => "^",
        BinOp::MatMul => "@",
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
        MatMul => "matmul",
    }
}

/// Attempt the operation with native (non-coerced) semantics.
/// Returns `Ok(Some(v))` on success, `Ok(None)` to signal the Python
/// `TypeError`/`ValueError` retry path. An `Err` is an exception that leaves the
/// operator wrapper altogether (including `ZeroDivisionError`, which only
/// `TypeError` and `ValueError` handlers would catch).
fn try_native_binary(op: BinOp, left: &Value, right: &Value) -> ExprResult<Option<Value>> {
    use Value::*;

    // String concatenation / repetition.
    if let (Str(a), Str(b)) = (left, right) {
        return match op {
            BinOp::Add => {
                check_concat_length(a.len(), b.len(), || a.chars().count() + b.chars().count())?;
                Ok(Some(Str(format!("{a}{b}"))))
            }
            // Other ops on two strings are TypeErrors → trigger numeric retry.
            _ => Ok(None),
        };
    }
    // List concatenation: list + list → joined list (Python `+` on lists).
    if let (List(a), List(b)) = (left, right) {
        return match op {
            BinOp::Add => {
                check_concat_length(a.len(), b.len(), || a.len() + b.len())?;
                let mut out = a.clone();
                out.extend(b.iter().cloned());
                Ok(Some(List(out)))
            }
            _ => Ok(None),
        };
    }

    // list * int  / int * list  → repetition (genuine Python list semantics)
    if op == BinOp::Mul {
        match (left, right) {
            (List(items), n) | (n, List(items)) => {
                if let Some(v) = repeat_list(items, n)? {
                    return Ok(Some(v));
                }
            }
            _ => {}
        }
    }

    // Two numbers: the numeric tower (and simpleeval's `safe_power`/`safe_*shift`).
    numeric::native_binary(op, left, right)
}

// simpleeval's `MAX_STRING_LENGTH`, enforced by its `safe_add` and `safe_mult`
// operators, which `LinkMLEvaluator` wraps. Exceeding it raises `IterableTooLong`,
// an `InvalidExpression`: an [`ExprError::Eval`]. The numeric limits are in
// [`super::numeric`].
const MAX_STRING_LENGTH: usize = 100_000;

/// `simpleeval.safe_add`: `len(a) + len(b) > MAX_STRING_LENGTH` raises
/// `IterableTooLong`. `len_a + len_b` is a cheap upper bound on the true sum
/// (byte lengths of strings); `exact_len` is only computed when it exceeds the
/// limit.
fn check_concat_length(
    len_a: usize,
    len_b: usize,
    exact_len: impl FnOnce() -> usize,
) -> ExprResult<()> {
    if len_a.saturating_add(len_b) > MAX_STRING_LENGTH && exact_len() > MAX_STRING_LENGTH {
        return Err(ExprError::Eval(
            "Sorry, adding those two together would make something too long.".into(),
        ));
    }
    Ok(())
}

/// `simpleeval.safe_mult` for a list and a number, then the repetition itself.
///
/// `None` when `n` is not an int/bool (a TypeError upstream: the caller falls
/// through to the numeric retry). A float count still goes through the length
/// check first, as `b * len(a) > MAX_STRING_LENGTH` runs before `a * b`.
fn repeat_list(items: &[Value], n: &Value) -> ExprResult<Option<Value>> {
    let too_long = || ExprError::Eval("Sorry, I will not evaluate something that long.".into());
    let count = match n {
        Value::Int(n) => *n,
        Value::Bool(b) => i64::from(*b),
        Value::BigInt(n) => {
            // `b * len(a)` is checked first; what passes (a negative or empty
            // product) still cannot be repeated, as the count is no `ssize_t`.
            if !items.is_empty() && !n.is_negative() {
                return Err(too_long());
            }
            return Err(ExprError::Uncaught(
                "cannot fit 'int' into an index-sized integer".into(),
            ));
        }
        Value::Float(f) => {
            if f * items.len() as f64 > MAX_STRING_LENGTH as f64 {
                return Err(too_long());
            }
            return Ok(None);
        }
        _ => return Ok(None),
    };
    let count = usize::try_from(count).unwrap_or(0);
    // `usize` cannot overflow `i128`; compare there to stay exact.
    if (items.len() as i128) * (count as i128) > MAX_STRING_LENGTH as i128 {
        return Err(too_long());
    }
    // Nothing to repeat (and no point looping `count` times over nothing).
    if items.is_empty() {
        return Ok(Some(Value::List(Vec::new())));
    }
    let mut out = Vec::with_capacity(items.len() * count);
    for _ in 0..count {
        out.extend(items.iter().cloned());
    }
    Ok(Some(Value::List(out)))
}

// --- scalar function implementations mirroring Python builtins ---

/// `str(v)`; an integer with too many digits is written in full where
/// [`try_py_str`] would raise (for messages and diagnostics).
pub(crate) fn py_str(v: &Value) -> String {
    match v {
        Value::Int(i) => i.to_string(),
        Value::Str(s) => s.clone(),
        _ => render(v, false, false).unwrap_or_default(),
    }
}

/// `repr(v)`; see [`py_str`].
pub(crate) fn py_repr(v: &Value) -> String {
    render(v, true, false).unwrap_or_default()
}

/// `str(v)` as Python computes it: the `ValueError` of an integer with more than
/// 4300 digits propagates.
pub(crate) fn try_py_str(v: &Value) -> ExprResult<String> {
    match v {
        Value::Int(i) => Ok(i.to_string()),
        Value::Str(s) => Ok(s.clone()),
        _ => Ok(render(v, false, true)?),
    }
}

/// `repr(v)`; see [`try_py_str`].
pub(crate) fn try_py_repr(v: &Value) -> ExprResult<String> {
    Ok(render(v, true, true)?)
}

/// `str` or `repr` of `v`. `checked` applies CPython's digit limit to integers.
fn render(v: &Value, repr: bool, checked: bool) -> Result<String, pynum::NumError> {
    Ok(match v {
        Value::Null => "None".to_string(),
        Value::Bool(b) => if *b { "True" } else { "False" }.to_string(),
        Value::Int(i) => i.to_string(),
        Value::BigInt(n) if checked => pynum::big_to_string(n)?,
        Value::BigInt(n) => n.to_string(),
        Value::Float(f) => format_py_float(*f),
        Value::Complex(z) => pynum::complex::repr(**z),
        Value::Decimal(d) if repr => format!("Decimal('{}')", d.to_sci_string()),
        Value::Decimal(d) => d.to_sci_string(),
        Value::Temporal(t) if repr => t.repr(),
        Value::Temporal(t) => t.to_string(),
        Value::Str(s) if repr => crate::pyfmt::py_repr_str(s),
        Value::Str(s) => s.clone(),
        Value::List(items) => {
            let parts = items
                .iter()
                .map(|item| render(item, true, checked))
                .collect::<Result<Vec<_>, _>>()?;
            format!("[{}]", parts.join(", "))
        }
        Value::Map(m) => {
            let parts = m
                .iter()
                .map(|(k, v)| {
                    Ok(format!(
                        "{}: {}",
                        crate::pyfmt::py_repr_str(k),
                        render(v, true, checked)?
                    ))
                })
                .collect::<Result<Vec<_>, pynum::NumError>>()?;
            format!("{{{}}}", parts.join(", "))
        }
    })
}

fn format_py_float(f: f64) -> String {
    // Delegates to the exact Python `repr(float)` (the previous body diverged
    // for |f| >= 1e16 and < 1e-4).
    crate::pyfmt::py_float_repr(f)
}

pub(crate) fn py_int(v: &Value) -> ExprResult<Value> {
    match v {
        Value::Int(_) | Value::BigInt(_) => Ok(v.clone()),
        Value::Bool(b) => Ok(Value::Int(*b as i64)),
        Value::Float(f) => builtins::float_to_int(f.trunc()),
        Value::Str(s) => builtins::parse_int(s, 10),
        other => Err(ExprError::Eval(format!(
            "int() argument must be a string or a number, not '{}'",
            type_name(other)
        ))),
    }
}

pub(crate) fn py_float(v: &Value) -> ExprResult<Value> {
    match v {
        Value::Int(i) => Ok(Value::Float(*i as f64)),
        Value::BigInt(n) => Ok(Value::Float(pynum::to_f64(IntRef::Big(n))?)),
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

    // Expected values from upstream v0.6.0-rc1 (`eval_expr_with_mapping`, CPython
    // 3.13): an integer outside `i64` is exact, never wrapped or an error.
    #[test]
    fn integer_arithmetic_never_wraps() {
        for (src, want) in [
            ("9223372036854775807 + 1", "9223372036854775808"),
            ("-9223372036854775807 - 2", "-9223372036854775809"),
            ("2**63", "9223372036854775808"),
            ("3**40", "12157665459056928801"),
            ("1 << 63", "9223372036854775808"),
            ("1 << 100", "1267650600228229401496703205376"),
            ("-(-9223372036854775807 - 1)", "9223372036854775808"),
            ("(-9223372036854775807 - 1) * -1", "9223372036854775808"),
            ("(-9223372036854775807 - 1) // -1", "9223372036854775808"),
            ("abs(-9223372036854775807 - 1)", "9223372036854775808"),
            ("sum([9223372036854775807, 1])", "9223372036854775808"),
            ("int(1e19)", "10000000000000000000"),
            ("int('9223372036854775808')", "9223372036854775808"),
            ("round(1e30)", "1000000000000000019884624838656"),
            ("floor(1e30)", "1000000000000000019884624838656"),
            ("ceil(1e30)", "1000000000000000019884624838656"),
            ("9223372036854775807 * 2", "18446744073709551614"),
            ("10 ** 400", &format!("1{}", "0".repeat(400))),
        ] {
            let got = eval(src);
            assert!(matches!(got, Value::BigInt(_)), "{src}: {got:?}");
            assert_eq!(py_str(&got), want, "{src}");
        }
        // A result that fits `i64` again is an `Int`.
        assert_eq!(eval("2**63 - 1"), Value::Int(i64::MAX));
        assert_eq!(eval("(2**64) // 2**62"), Value::Int(4));
        // Results that stay in range are unchanged.
        assert_eq!(eval("(-9223372036854775807 - 1) % -1"), Value::Int(0));
        assert_eq!(eval("1 << 62"), Value::Int(1 << 62));
        assert_eq!(eval("(-2)**63"), Value::Int(i64::MIN));
        assert_eq!(eval("-1 >> 100"), Value::Int(-1));
        assert_eq!(eval("1 >> 100"), Value::Int(0));
    }

    #[test]
    fn shifts_and_powers_follow_simpleeval_limits() {
        for (src, message) in [
            ("1 << -1", "negative shift count"),
            ("1 >> -1", "negative shift count"),
            ("1 << 10001", "Sorry! I don't want to evaluate 1 << 10001"),
            ("1 >> -10001", "Sorry! I don't want to evaluate 1 >> -10001"),
            (
                "3.0 << 9007199254740992",
                "Sorry! I don't want to evaluate 3.0 << 9007199254740992",
            ),
            (
                "2 ** 4000001",
                "Sorry! I don't want to evaluate 2 ** 4000001",
            ),
            (
                "4000001 ** 1",
                "Sorry! I don't want to evaluate 4000001 ** 1",
            ),
            ("1e30 ** 2", "Sorry! I don't want to evaluate 1e+30 ** 2"),
            (
                "[1, 2] * 9223372036854775807",
                "Sorry, I will not evaluate something that long.",
            ),
            (
                "[1] * 1000000000000",
                "Sorry, I will not evaluate something that long.",
            ),
            (
                "[1] * 1e30",
                "Sorry, I will not evaluate something that long.",
            ),
        ] {
            let err = eval_expr(src).expect_err(src);
            assert!(matches!(err, ExprError::Eval(_)), "{src}: {err:?}");
            assert!(err.to_string().contains(message), "{src}: {err}");
        }
        assert_eq!(eval("[1, 2] * 3"), eval("3 * [1, 2]"));
        assert_eq!(
            eval("[1, 2] * True"),
            Value::List(vec![Value::Int(1), Value::Int(2)])
        );
        assert_eq!(eval("[1] * -1"), Value::List(vec![]));
        assert!(matches!(eval("[1] * 100000"), Value::List(items) if items.len() == 100_000));
        // `+` on two long strings or lists is `IterableTooLong`.
        let half = "x".repeat(60_000);
        let err = eval_expr(&format!("'{half}' + '{half}'")).expect_err("too long");
        assert!(err.to_string().contains("too long"), "{err}");
    }

    #[test]
    fn float_errors_that_upstream_does_not_catch() {
        for (src, message) in [
            ("0 ** -1", "0.0 cannot be raised to a negative power"),
            ("10.0 ** 400", "(34, 'Numerical result out of range')"),
            ("2.0 ** 1024", "(34, 'Numerical result out of range')"),
        ] {
            let err = eval_expr(src).expect_err(src);
            assert!(matches!(err, ExprError::Uncaught(_)), "{src}: {err:?}");
            assert!(err.to_string().contains(message), "{src}: {err}");
        }
        // A bitwise operator on a float is a TypeError again on the numeric retry.
        let err = eval_expr("1.5 & 3").expect_err("TypeError");
        assert!(matches!(err, ExprError::Eval(_)), "{err:?}");
        // An odd-looking but correct result: Python floors via fmod.
        assert_eq!(eval("1 // 0.1"), Value::Float(9.0));
        assert_eq!(eval("-4.0 % 2.0"), Value::Float(0.0));
        assert_eq!(eval("0.5 ** -2"), Value::Float(4.0));
    }

    #[test]
    fn int_and_float_compare_exactly() {
        assert_eq!(
            eval("9007199254740993 > 9007199254740992"),
            Value::Bool(true)
        );
        assert_eq!(
            eval("9007199254740993 > 9007199254740992.0"),
            Value::Bool(true)
        );
        assert_eq!(
            eval("9007199254740993 == 9007199254740992.0"),
            Value::Bool(false)
        );
        assert_eq!(
            eval("9223372036854775807 < 9223372036854775808.0"),
            Value::Bool(true)
        );
    }

    #[test]
    fn int_division_is_correctly_rounded() {
        // Python rounds the exact quotient once; dividing the two f64 images rounds thrice.
        assert_eq!(
            eval("9007199254740992 / 10"),
            Value::Float(900_719_925_474_099.2)
        );
        assert_eq!(
            eval("9223372036854775807 / 7"),
            Value::Float(1.3176245766935393e18)
        );
        assert_eq!(
            eval("1 / 9223372036854775807"),
            Value::Float(1.0842021724855044e-19)
        );
        assert_eq!(
            eval("-9223372036854775807 / 3"),
            Value::Float(-3.0744573456182584e18)
        );
        assert_eq!(eval("0 / -5"), Value::Float(-0.0));
    }
}
