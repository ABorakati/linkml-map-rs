//! Evaluation of the syntax the restricted evaluator accepts beyond the basic
//! operators: slices, f-strings, `is`, calls on attributes (`s.upper()`),
//! lambdas, starred elements, keyword unpacking and the nodes it refuses.
//!
//! The parser accepts everything CPython's does, so a construct `simpleeval`
//! cannot evaluate fails when (and only when) it is evaluated, as upstream does
//! (`FeatureNotAvailable`/`OperatorNotDefined`, both `InvalidExpression`: a
//! rejection the `unrestricted_eval` fallback retries). Statement blocks run in
//! [`super::asteval`], not here.

use super::error::{ExprError, ExprResult};
use super::eval::{
    Bindings, attribute_receiver, distributed_getattr, eval_ast, try_py_str, type_name,
};
use super::parser::{Ast, CmpOp, FPart};
use super::pyformat::format_value;
use super::pymethods::{self, Lookup};
use crate::pynum::complex::Complex64;
use crate::value::Value;

/// `simpleeval.MAX_STRING_LENGTH`.
const MAX_STRING_LENGTH: usize = 100_000;

/// Evaluate one of the nodes the basic evaluator delegates here.
pub(super) fn eval_syntax(node: &Ast, vars: &Bindings) -> ExprResult<Value> {
    let unavailable =
        |what: &str| ExprError::Eval(format!("Sorry, {what} is not available in this evaluator"));
    match node {
        Ast::Imag(im) => Ok(Value::from_complex(Complex64::new(0.0, *im))),
        Ast::Bytes(_) => Err(ExprError::unrepresentable("bytes")),
        Ast::Ellipsis => Err(ExprError::unrepresentable("ellipsis")),
        Ast::FString(parts) => join_fstring(parts, vars).map(Value::Str),
        Ast::Slice { .. } => Err(ExprError::unrepresentable("slice")),
        Ast::Starred(_) => Err(unavailable("Starred")),
        Ast::DictUnpack => Err(unavailable("DictUnpack")),
        Ast::NamedExpr { .. } => Err(unavailable("NamedExpr")),
        Ast::Lambda(_) => Err(unavailable("Lambda")),
        Ast::CallValue {
            func,
            args,
            keywords,
        } => call_value(func, args, keywords, vars),
        _ => unreachable!("eval_ast delegates only the nodes above"),
    }
}

/// `operator = self.operators[type(node.op)]` for `@`: simpleeval has no
/// matrix multiplication and raises before evaluating either operand.
pub(super) fn matmul_refused() -> ExprResult<Value> {
    Err(ExprError::Eval(
        "Operator '<ast.MatMult object>' does not exist in expression".into(),
    ))
}

// === display elements and call arguments ==================================

/// The elements of a list or tuple display. A `*iterable` element extends the
/// result in a list under simpleeval (`_eval_list`); a tuple evaluates the
/// `Starred` node, which simpleeval refuses, and asteval refuses it everywhere
/// (it has no `on_starred`).
pub(super) fn eval_elements(
    elts: &[Ast],
    vars: &Bindings,
    always_expand: bool,
) -> ExprResult<Vec<Value>> {
    let mut out = Vec::with_capacity(elts.len());
    for e in elts {
        match e {
            Ast::Starred(inner) if always_expand => {
                let iterable = eval_ast(inner, vars)?;
                out.extend(super::builtins::py_iter(&iterable)?.iter().cloned());
            }
            other => out.push(eval_ast(other, vars)?),
        }
    }
    Ok(out)
}

/// The positional arguments of a call; a `Starred` argument is refused
/// (`FeatureNotAvailable`; asteval has no starred support either).
pub(super) fn eval_call_args(args: &[Ast], vars: &Bindings) -> ExprResult<Vec<Value>> {
    eval_elements(args, vars, false)
}

/// The keyword arguments of a call; an empty name is `**mapping`.
///
/// simpleeval builds `dict(self._eval(k) for k in node.keywords)` and calls
/// `func(**that)`; for `**mapping` the pair is `(None, mapping)`, so the call
/// raises `TypeError: keywords must be strings`.
pub(super) fn eval_call_keywords(
    keywords: &[(String, Ast)],
    vars: &Bindings,
) -> ExprResult<Vec<(String, Value)>> {
    let mut out = Vec::with_capacity(keywords.len());
    for (name, value) in keywords {
        let evaluated = eval_ast(value, vars)?;
        if !name.is_empty() {
            out.push((name.clone(), evaluated));
            continue;
        }
        return Err(ExprError::Eval("keywords must be strings".into()));
    }
    Ok(out)
}

// === calls ================================================================

fn call_value(
    func: &Ast,
    args: &[Ast],
    keywords: &[(String, Ast)],
    vars: &Bindings,
) -> ExprResult<Value> {
    match func {
        Ast::Attribute { value, attr } => {
            // `func = self._eval(node.func)` comes first, then the arguments.
            let receiver = attribute_receiver(value, attr, vars)?;
            let callable = is_callable(&receiver, attr)?;
            let argv = eval_call_args(args, vars)?;
            let kwargs = eval_call_keywords(keywords, vars)?;
            // PARITY: `Value` has no tuple, so a tuple of affixes is a list; one
            // that was not written as a tuple is the `TypeError` Python raises.
            if matches!(attr.as_str(), "startswith" | "endswith")
                && matches!(argv.first(), Some(Value::List(_)))
                && !args.first().is_some_and(is_tuple_node)
            {
                return Err(ExprError::Eval(format!(
                    "{attr} first arg must be str or a tuple of str, not list"
                )));
            }
            match callable {
                Ok(()) => pymethods::call_method(&receiver, attr, argv, kwargs),
                Err(type_name) => Err(ExprError::Eval(format!(
                    "'{type_name}' object is not callable"
                ))),
            }
        }
        // `func = self.functions[node.func.id]` raises AttributeError for any
        // callee that is not a name, which becomes `FeatureNotAvailable`.
        _ => Err(ExprError::Eval("Lambda Functions not implemented".into())),
    }
}

/// Whether `receiver.attr` is a method: `Ok(Ok(()))`; or the Python type name
/// of the (not callable) value it is.
fn is_callable(receiver: &Value, attr: &str) -> ExprResult<Result<(), &'static str>> {
    match receiver {
        Value::Str(_)
        | Value::Int(_)
        | Value::BigInt(_)
        | Value::Complex(_)
        | Value::Float(_)
        | Value::Bool(_) => {
            if attr.starts_with('_') {
                // The private-name check of `_distributed_getattr`.
                distributed_getattr(receiver, attr)?;
            }
            match pymethods::lookup(receiver, attr) {
                Lookup::Method => Ok(Ok(())),
                Lookup::Data(v) => Ok(Err(python_type(&v))),
                Lookup::Missing => Err(pymethods::scalar_getattr(receiver, attr).unwrap_err()),
            }
        }
        // Lists and dicts distribute the attribute over their items, so the
        // result is a list (or whatever the item holds) and never callable.
        other => {
            let found = distributed_getattr(other, attr)?;
            Ok(Err(python_type(&found)))
        }
    }
}

/// Whether `node` builds a tuple: a tuple display or a `tuple(..)` call.
fn is_tuple_node(node: &Ast) -> bool {
    match node {
        Ast::Tuple(_) => true,
        Ast::Call { func, .. } => func == "tuple",
        _ => false,
    }
}

fn python_type(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        other => type_name(other),
    }
}

// === slices ===============================================================

/// `value[lower:upper:step]`, the subscript's `slice` object built from the
/// bounds (evaluated in order, after the container).
pub(super) fn subscript_slice(
    obj: &Value,
    lower: &Option<Box<Ast>>,
    upper: &Option<Box<Ast>>,
    step: &Option<Box<Ast>>,
    vars: &Bindings,
) -> ExprResult<Value> {
    let bound = |b: &Option<Box<Ast>>| -> ExprResult<Option<Value>> {
        b.as_deref().map(|a| eval_ast(a, vars)).transpose()
    };
    let (lower, upper, step) = (bound(lower)?, bound(upper)?, bound(step)?);
    match obj {
        Value::List(_) | Value::Str(_) => {}
        // PARITY: a dict keyed by a slice is a `KeyError` (slices are hashable
        // since Python 3.12); no map here has such a key.
        // An unhashable bound makes the slice itself unhashable: a `TypeError`.
        Value::Map(_) => {
            let unhashable = [&lower, &upper, &step]
                .into_iter()
                .any(|b| matches!(b, Some(Value::List(_) | Value::Map(_))));
            return Err(if unhashable {
                ExprError::Eval("unhashable type in slice".into())
            } else {
                ExprError::Uncaught("KeyError: slice".into())
            });
        }
        other => {
            return Err(ExprError::Eval(format!(
                "'{}' object is not subscriptable",
                python_type(other)
            )));
        }
    }
    let index = |v: Option<Value>| -> ExprResult<Option<i128>> {
        match v {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Int(i)) => Ok(Some(i128::from(i))),
            Some(Value::Bool(b)) => Ok(Some(i128::from(b))),
            // A bound beyond `i64` clamps like any other out-of-range one; this
            // stands in for it without changing which positions are picked.
            Some(Value::BigInt(n)) => Ok(Some(if n.sign() == num_bigint::Sign::Minus {
                -(1 << 100)
            } else {
                1 << 100
            })),
            Some(_) => Err(ExprError::Eval(
                "slice indices must be integers or None or have an __index__ method".into(),
            )),
        }
    };
    let (lower, upper, step) = (index(lower)?, index(upper)?, index(step)?);
    if step == Some(0) {
        return Err(ExprError::Eval("slice step cannot be zero".into()));
    }
    match obj {
        Value::List(items) => {
            let picked = slice_positions(items.len(), lower, upper, step);
            Ok(Value::List(picked.map(|i| items[i].clone()).collect()))
        }
        Value::Str(s) if s.is_ascii() => {
            let bytes = s.as_bytes();
            let picked: String = slice_positions(bytes.len(), lower, upper, step)
                .map(|i| char::from(bytes[i]))
                .collect();
            Ok(Value::Str(picked))
        }
        Value::Str(s) => {
            let chars: Vec<char> = s.chars().collect();
            let picked: String = slice_positions(chars.len(), lower, upper, step)
                .map(|i| chars[i])
                .collect();
            Ok(Value::Str(picked))
        }
        _ => unreachable!("checked above"),
    }
}

/// The positions `PySlice_AdjustIndices` selects from a sequence of `len`.
fn slice_positions(
    len: usize,
    lower: Option<i128>,
    upper: Option<i128>,
    step: Option<i128>,
) -> impl Iterator<Item = usize> {
    let len = len as i128;
    let step = step.unwrap_or(1);
    let (low, high) = if step < 0 { (-1, len - 1) } else { (0, len) };
    let clamp = |bound: Option<i128>, default: i128| match bound {
        None => default,
        Some(b) if b < 0 => (b + len).max(low),
        Some(b) => b.min(high),
    };
    let (start, stop) = if step < 0 {
        (clamp(lower, high), clamp(upper, low))
    } else {
        (clamp(lower, low), clamp(upper, high))
    };
    let count = if step < 0 {
        if stop < start {
            (start - stop - 1) / (-step) + 1
        } else {
            0
        }
    } else if start < stop {
        (stop - start - 1) / step + 1
    } else {
        0
    };
    (0..count).map(move |k| (start + k * step) as usize)
}

// === is / is not ==========================================================

/// `left is right`, evaluated on values that were cloned out of their
/// bindings. Identity is modelled on what CPython does: `None`, booleans, small
/// integers (-5 to 256), the empty string, one-character Latin-1 strings and
/// the empty tuple are singletons; any other value is the same object as
/// another only when both operands read the same reference (`a is a`,
/// `m.x is m.x`, `l[0] is l[0]`).
pub(super) fn identity_compare(
    op: CmpOp,
    left_node: &Ast,
    left: &Value,
    right_node: &Ast,
    right: &Value,
) -> Value {
    let same = is_identical(left_node, left, right_node, right);
    Value::Bool(if op == CmpOp::Is { same } else { !same })
}

fn is_identical(left_node: &Ast, left: &Value, right_node: &Ast, right: &Value) -> bool {
    let same_reference = || same_reference(left_node, right_node);
    match (left, right) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::Int(a), Value::Int(b)) => a == b && ((-5..=256).contains(a) || same_reference()),
        (Value::Str(a), Value::Str(b)) => a == b && (is_cached_str(a) || same_reference()),
        (Value::Float(a), Value::Float(b)) => a.to_bits() == b.to_bits() && same_reference(),
        (Value::List(a), Value::List(b)) => {
            let empty_tuples = matches!(
                (left_node, right_node),
                (Ast::Tuple(x), Ast::Tuple(y)) if x.is_empty() && y.is_empty()
            );
            (a.is_empty() && b.is_empty() && empty_tuples) || (left == right && same_reference())
        }
        (Value::Map(_), Value::Map(_))
        | (Value::BigInt(_), Value::BigInt(_))
        | (Value::Complex(_), Value::Complex(_))
        | (Value::Decimal(_), Value::Decimal(_))
        | (Value::Temporal(_), Value::Temporal(_)) => left == right && same_reference(),
        _ => false,
    }
}

/// The strings CPython keeps as singletons: `''` and one Latin-1 character.
fn is_cached_str(s: &str) -> bool {
    let mut chars = s.chars();
    match (chars.next(), chars.next()) {
        (None, _) => true,
        (Some(c), None) => (c as u32) < 256,
        _ => false,
    }
}

/// Whether two expressions read the very same object: the same name, or the
/// same attribute/index path over the same name (`{x}` is `x`).
fn same_reference(a: &Ast, b: &Ast) -> bool {
    match (a, b) {
        (Ast::Brace(x), y) | (y, Ast::Brace(x)) => same_reference(x, y),
        (Ast::Name(x), Ast::Name(y)) => x == y,
        (
            Ast::Attribute {
                value: v1,
                attr: a1,
            },
            Ast::Attribute {
                value: v2,
                attr: a2,
            },
        ) => a1 == a2 && same_reference(v1, v2),
        (
            Ast::Subscript {
                value: v1,
                index: i1,
            },
            Ast::Subscript {
                value: v2,
                index: i2,
            },
        ) => {
            same_reference(v1, v2)
                && match (&**i1, &**i2) {
                    (Ast::Int(x), Ast::Int(y)) => x == y,
                    (Ast::Str(x), Ast::Str(y)) => x == y,
                    _ => false,
                }
        }
        _ => false,
    }
}

// === f-strings ============================================================

/// An f-string: `simpleeval._eval_joinedstr`.
///
/// PARITY: `_eval_formattedvalue` ignores the conversion (`!r`,
/// `!s`, `!a`, and the implicit `!r` of `{x=}`): the value is `str()`d, so
/// `f"{s!r}"` is `s`, not `repr(s)`. With a format spec the value goes through
/// `("{:" + spec + "}").format(value)`. `_eval_joinedstr` never accumulates its
/// `length` (it starts at 0 and is only added to the one part checked), so only
/// a single part over `MAX_STRING_LENGTH` is refused, not their sum.
fn join_fstring(parts: &[FPart], vars: &Bindings) -> ExprResult<String> {
    let too_long = || ExprError::Eval("Sorry, I will not evaluate something this long.".into());
    let mut out = String::new();
    for part in parts {
        match part {
            FPart::Lit(text) => {
                if text.chars().count() > MAX_STRING_LENGTH {
                    return Err(too_long());
                }
                out.push_str(text);
            }
            FPart::Field(field) => {
                let value = eval_ast(&field.value, vars)?;
                let spec = match &field.spec {
                    Some(spec) => Some(join_fstring(spec, vars)?),
                    None => None,
                };
                let text = match spec {
                    None => try_py_str(&value)?,
                    Some(spec) => format_with_spec(&value, &spec)?,
                };
                if text.chars().count() > MAX_STRING_LENGTH {
                    return Err(too_long());
                }
                out.push_str(&text);
            }
        }
    }
    Ok(out)
}

/// `("{:" + spec + "}").format(value)`: a brace in the spec is a nested field
/// or a stray brace of the template, and fails here.
fn format_with_spec(value: &Value, spec: &str) -> ExprResult<String> {
    if spec.contains(['{', '}']) {
        return Err(ExprError::Eval(
            "Single '}' or unresolvable replacement field in format spec".into(),
        ));
    }
    format_value(value, spec)
}
