//! Attributes and methods of Python scalars: `s.upper()`, `x.real`, `n.bit_length()`.
//!
//! Upstream's restricted evaluator resolves `obj.attr` with `getattr` (after
//! distributing over lists and dicts, see `_distributed_getattr`), with no
//! allow-list of attributes (`allowed_attrs` is never passed) and with only the
//! leading-underscore check of `LinkMLEvaluator._eval_attribute`'s helper. So a
//! call such as `name.upper()` or `n.to_bytes(2, 'big')` reaches the real
//! Python method. Only scalars get here: a list or a dict distributes the
//! attribute over its items, and a scalar item is `str`, `int`, `float` or
//! `bool`, whose public attributes are the tables below.
//!
//! PARITY: a bound method that is *not* called (`s.upper`), and a result that
//! is `bytes` (`s.encode()`, `n.to_bytes(..)`) or a mapping with integer keys
//! (`str.maketrans`), have no [`Value`]: they raise an "unrepresentable" error
//! (see [`ExprError::unrepresentable`]). Mutating methods of lists and dicts
//! are never reachable (attribute access distributes over them).
//!
//! `str.casefold()` reads CPython's full case folding from the generated tables.

use super::error::{ExprError, ExprResult};
use super::eval::type_name;
use super::pyformat;
use super::pystr;
use super::pytables::{self, CASE_IGNORABLE, LOWERCASE, TITLECASE_LETTERS, UPPERCASE};
use crate::value::Value;

/// What `getattr(scalar, name)` finds.
pub(super) enum Lookup {
    /// A method (a bound builtin when not called).
    Method,
    /// A data attribute (`real`, `imag`, `numerator`, `denominator`).
    Data(Value),
    /// No such attribute.
    Missing,
}

const STR_METHODS: &[&str] = &[
    "capitalize",
    "casefold",
    "center",
    "count",
    "encode",
    "endswith",
    "expandtabs",
    "find",
    "format",
    "format_map",
    "index",
    "isalnum",
    "isalpha",
    "isascii",
    "isdecimal",
    "isdigit",
    "isidentifier",
    "islower",
    "isnumeric",
    "isprintable",
    "isspace",
    "istitle",
    "isupper",
    "join",
    "ljust",
    "lower",
    "lstrip",
    "maketrans",
    "partition",
    "removeprefix",
    "removesuffix",
    "replace",
    "rfind",
    "rindex",
    "rjust",
    "rpartition",
    "rsplit",
    "rstrip",
    "split",
    "splitlines",
    "startswith",
    "strip",
    "swapcase",
    "title",
    "translate",
    "upper",
    "zfill",
];
const INT_METHODS: &[&str] = &[
    "as_integer_ratio",
    "bit_count",
    "bit_length",
    "conjugate",
    "from_bytes",
    "is_integer",
    "to_bytes",
];
const FLOAT_METHODS: &[&str] = &[
    "as_integer_ratio",
    "conjugate",
    "fromhex",
    "hex",
    "is_integer",
];

/// `getattr(recv, name)` for a scalar `recv`.
pub(super) fn lookup(recv: &Value, name: &str) -> Lookup {
    let methods = match recv {
        Value::Str(_) => STR_METHODS,
        Value::Int(_) | Value::Bool(_) => INT_METHODS,
        Value::Float(_) => FLOAT_METHODS,
        _ => return Lookup::Missing,
    };
    if methods.contains(&name) {
        return Lookup::Method;
    }
    match (recv, name) {
        (Value::Int(i), "real" | "numerator") => Lookup::Data(Value::Int(*i)),
        (Value::Bool(b), "real" | "numerator") => Lookup::Data(Value::Int(i64::from(*b))),
        (Value::Int(_) | Value::Bool(_), "imag") => Lookup::Data(Value::Int(0)),
        (Value::Int(_) | Value::Bool(_), "denominator") => Lookup::Data(Value::Int(1)),
        (Value::Float(f), "real") => Lookup::Data(Value::Float(*f)),
        (Value::Float(_), "imag") => Lookup::Data(Value::Float(0.0)),
        _ => Lookup::Missing,
    }
}

/// `getattr(scalar, attr)` as a value: the attribute access `scalar.attr`
/// outside a call.
///
/// A missing attribute is an `AttributeError`.
pub(super) fn scalar_getattr(recv: &Value, attr: &str) -> ExprResult<Value> {
    match lookup(recv, attr) {
        Lookup::Data(v) => Ok(v),
        Lookup::Method => Err(ExprError::unrepresentable("builtin_function_or_method")),
        Lookup::Missing => Err(ExprError::Uncaught(format!(
            "AttributeError: '{}' object has no attribute '{attr}'",
            python_type(recv)
        ))),
    }
}

/// `.attr` inside a `str.format` replacement field: data attributes only.
pub(super) fn format_getattr(recv: &Value, attr: &str) -> ExprResult<Value> {
    scalar_getattr(recv, attr)
}

fn python_type(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::List(_) => "list",
        Value::Map(_) => "dict",
        other => type_name(other),
    }
}

fn type_error(message: impl Into<String>) -> ExprError {
    ExprError::Eval(message.into())
}

// === argument binding ======================================================

/// A method's positional and keyword arguments, bound to its parameters.
struct Bound(Vec<Option<Value>>);

impl Bound {
    /// Bind `args` and `kwargs` to `params` (`(name, keyword_allowed)`),
    /// requiring the first `required`.
    fn new(
        method: &str,
        params: &[(&str, bool)],
        required: usize,
        args: Vec<Value>,
        kwargs: Vec<(String, Value)>,
    ) -> ExprResult<Self> {
        if args.len() > params.len() {
            return Err(type_error(format!(
                "{method}() takes at most {} arguments ({} given)",
                params.len(),
                args.len()
            )));
        }
        let mut slots: Vec<Option<Value>> = vec![None; params.len()];
        for (slot, value) in slots.iter_mut().zip(args) {
            *slot = Some(value);
        }
        for (key, value) in kwargs {
            match params.iter().position(|(name, kw)| *kw && *name == key) {
                Some(i) if slots[i].is_none() => slots[i] = Some(value),
                Some(_) => {
                    return Err(type_error(format!(
                        "argument for {method}() given by name ('{key}') and position"
                    )));
                }
                None => {
                    return Err(type_error(format!(
                        "{method}() got an unexpected keyword argument '{key}'"
                    )));
                }
            }
        }
        if slots.iter().take(required).any(Option::is_none) {
            return Err(type_error(format!(
                "{method}() takes at least {required} argument{} ({} given)",
                if required == 1 { "" } else { "s" },
                slots.iter().flatten().count()
            )));
        }
        Ok(Self(slots))
    }

    fn get(&self, i: usize) -> Option<&Value> {
        self.0[i].as_ref()
    }

    /// A required or optional `str` argument.
    fn str(&self, method: &str, i: usize) -> ExprResult<Option<&str>> {
        match self.get(i) {
            None => Ok(None),
            Some(Value::Str(s)) => Ok(Some(s)),
            Some(other) => Err(type_error(format!(
                "{method}() argument must be str, not {}",
                python_type(other)
            ))),
        }
    }

    /// An optional integer argument (`None` counts as absent when `none_ok`).
    fn int(&self, method: &str, i: usize, none_ok: bool) -> ExprResult<Option<i64>> {
        match self.get(i) {
            None => Ok(None),
            Some(Value::Null) if none_ok => Ok(None),
            Some(Value::Int(n)) => Ok(Some(*n)),
            Some(Value::Bool(b)) => Ok(Some(i64::from(*b))),
            Some(other) => Err(type_error(format!(
                "{method}() argument must be an integer, not '{}'",
                python_type(other)
            ))),
        }
    }
}

fn no_args(method: &str, args: &[Value], kwargs: &[(String, Value)]) -> ExprResult<()> {
    if args.is_empty() && kwargs.is_empty() {
        Ok(())
    } else {
        Err(type_error(format!(
            "{method}() takes no arguments ({} given)",
            args.len() + kwargs.len()
        )))
    }
}

// === dispatch ==============================================================

/// Call method `name` of the scalar `recv`.
///
/// # Errors
/// What the Python method raises (`TypeError`, `ValueError`, ...), or an
/// "unrepresentable" error for a result that has no [`Value`].
pub(super) fn call_method(
    recv: &Value,
    name: &str,
    args: Vec<Value>,
    kwargs: Vec<(String, Value)>,
) -> ExprResult<Value> {
    match recv {
        Value::Str(s) => str_method(s, name, args, kwargs),
        Value::Int(_) | Value::Bool(_) => {
            let n = match recv {
                Value::Int(i) => i128::from(*i),
                Value::Bool(b) => i128::from(*b),
                _ => unreachable!(),
            };
            int_method(n, matches!(recv, Value::Bool(_)), name, args, kwargs)
        }
        Value::Float(f) => float_method(*f, name, args, kwargs),
        Value::List(items) => list_method(items, name, args, kwargs),
        other => Err(type_error(format!(
            "'{}' object has no method '{name}'",
            python_type(other)
        ))),
    }
}

// === list ==================================================================

/// A method of a list reached inside a statement block.
///
/// PARITY: the Python list is mutated in place and keeps the change; `Value`s
/// are copied out of their bindings, so a mutating method (`append`, `sort`,
/// `pop`, ...) returns what Python returns but changes nothing the expression
/// can read back.
fn list_method(
    items: &[Value],
    name: &str,
    args: Vec<Value>,
    kwargs: Vec<(String, Value)>,
) -> ExprResult<Value> {
    let index_error = |what: &str| ExprError::Uncaught(format!("IndexError: {what}"));
    match name {
        "copy" => {
            no_args(name, &args, &kwargs)?;
            Ok(Value::List(items.to_vec()))
        }
        "clear" | "reverse" => {
            no_args(name, &args, &kwargs)?;
            Ok(Value::Null)
        }
        "append" => {
            Bound::new(name, &[("object", false)], 1, args, kwargs)?;
            Ok(Value::Null)
        }
        "extend" => {
            let bound = Bound::new(name, &[("iterable", false)], 1, args, kwargs)?;
            super::builtins::py_iter(bound.get(0).expect("required"))?;
            Ok(Value::Null)
        }
        "insert" => {
            let bound = Bound::new(
                name,
                &[("index", false), ("object", false)],
                2,
                args,
                kwargs,
            )?;
            bound.int(name, 0, false)?;
            Ok(Value::Null)
        }
        "count" => {
            let bound = Bound::new(name, &[("value", false)], 1, args, kwargs)?;
            let needle = bound.get(0).expect("required");
            Ok(Value::Int(
                items.iter().filter(|v| v.py_eq(needle)).count() as i64
            ))
        }
        "index" => {
            let bound = Bound::new(
                name,
                &[("value", false), ("start", false), ("stop", false)],
                1,
                args,
                kwargs,
            )?;
            let needle = bound.get(0).expect("required");
            let (start, end) = adjust_indices(
                items.len(),
                bound.int(name, 1, false)?,
                bound.int(name, 2, false)?,
            );
            let found = (start..end.max(start)).find(|&i| items[i].py_eq(needle));
            found
                .map(|i| Value::Int(i as i64))
                .ok_or_else(|| type_error("value is not in list"))
        }
        "pop" => {
            let bound = Bound::new(name, &[("index", false)], 0, args, kwargs)?;
            if items.is_empty() {
                return Err(index_error("pop from empty list"));
            }
            let i = bound.int(name, 0, false)?.unwrap_or(-1);
            let len = items.len() as i64;
            let resolved = if i < 0 { i + len } else { i };
            usize::try_from(resolved)
                .ok()
                .and_then(|i| items.get(i))
                .cloned()
                .ok_or_else(|| index_error("pop index out of range"))
        }
        "remove" => {
            let bound = Bound::new(name, &[("value", false)], 1, args, kwargs)?;
            let needle = bound.get(0).expect("required");
            if items.iter().any(|v| v.py_eq(needle)) {
                Ok(Value::Null)
            } else {
                Err(type_error("list.remove(x): x not in list"))
            }
        }
        "sort" => {
            let bound = Bound::new(name, &[("key", true), ("reverse", true)], 0, args, kwargs)?;
            if !matches!(bound.get(0), None | Some(Value::Null)) {
                return Err(ExprError::unrepresentable("function"));
            }
            // Raises the `TypeError` of an unorderable pair, like `list.sort`.
            super::builtins::call_function("sorted", vec![Value::List(items.to_vec())])?;
            Ok(Value::Null)
        }
        _ => Err(type_error(format!("'list' object has no method '{name}'"))),
    }
}

// === int and float =========================================================

fn int_method(
    n: i128,
    is_bool: bool,
    name: &str,
    args: Vec<Value>,
    kwargs: Vec<(String, Value)>,
) -> ExprResult<Value> {
    let int = |v: i128| {
        i64::try_from(v)
            .map(Value::Int)
            .map_err(|_| ExprError::big_int())
    };
    match name {
        "conjugate" => {
            no_args(name, &args, &kwargs)?;
            int(n)
        }
        "bit_length" => {
            no_args(name, &args, &kwargs)?;
            int(i128::from(128 - n.unsigned_abs().leading_zeros()))
        }
        "bit_count" => {
            no_args(name, &args, &kwargs)?;
            int(i128::from(n.unsigned_abs().count_ones()))
        }
        "is_integer" => {
            no_args(name, &args, &kwargs)?;
            Ok(Value::Bool(true))
        }
        "as_integer_ratio" => {
            no_args(name, &args, &kwargs)?;
            Ok(Value::List(vec![int(n)?, Value::Int(1)]))
        }
        // `to_bytes` yields bytes; `from_bytes` needs bytes, which no expression can build.
        "to_bytes" | "from_bytes" => {
            let _ = is_bool;
            Err(ExprError::unrepresentable("bytes"))
        }
        _ => Err(type_error(format!("'int' object has no method '{name}'"))),
    }
}

fn float_method(
    f: f64,
    name: &str,
    args: Vec<Value>,
    kwargs: Vec<(String, Value)>,
) -> ExprResult<Value> {
    match name {
        "conjugate" => {
            no_args(name, &args, &kwargs)?;
            Ok(Value::Float(f))
        }
        "is_integer" => {
            no_args(name, &args, &kwargs)?;
            Ok(Value::Bool(f.is_finite() && f == f.trunc()))
        }
        "hex" => {
            no_args(name, &args, &kwargs)?;
            Ok(Value::Str(float_hex(f)))
        }
        "as_integer_ratio" => {
            no_args(name, &args, &kwargs)?;
            float_ratio(f)
        }
        "fromhex" => {
            let bound = Bound::new(name, &[("string", false)], 1, args, kwargs)?;
            let text = bound.str(name, 0)?.expect("required");
            parse_float_hex(text).map(Value::Float)
        }
        _ => Err(type_error(format!("'float' object has no method '{name}'"))),
    }
}

/// `float.hex()`.
fn float_hex(f: f64) -> String {
    if f.is_nan() {
        return "nan".into();
    }
    if f.is_infinite() {
        return if f < 0.0 { "-inf" } else { "inf" }.into();
    }
    let sign = if f.is_sign_negative() { "-" } else { "" };
    if f == 0.0 {
        return format!("{sign}0x0.0p+0");
    }
    let bits = f.to_bits();
    let raw_exp = ((bits >> 52) & 0x7ff) as i64;
    let fraction = bits & ((1u64 << 52) - 1);
    let (lead, exp) = if raw_exp == 0 {
        (0, -1022)
    } else {
        (1, raw_exp - 1023)
    };
    let sign_exp = if exp < 0 { '-' } else { '+' };
    format!("{sign}0x{lead}.{fraction:013x}p{sign_exp}{}", exp.abs())
}

/// `float.as_integer_ratio()`.
fn float_ratio(f: f64) -> ExprResult<Value> {
    if f.is_nan() {
        return Err(type_error("cannot convert NaN to integer ratio"));
    }
    if f.is_infinite() {
        return Err(ExprError::Uncaught(
            "OverflowError: cannot convert Infinity to integer ratio".into(),
        ));
    }
    if f == 0.0 {
        return Ok(Value::List(vec![Value::Int(0), Value::Int(1)]));
    }
    let bits = f.to_bits();
    let raw_exp = ((bits >> 52) & 0x7ff) as i32;
    let fraction = bits & ((1u64 << 52) - 1);
    let (mut mantissa, mut exp) = if raw_exp == 0 {
        (fraction, -1074)
    } else {
        (fraction | (1u64 << 52), raw_exp - 1075)
    };
    let trailing = mantissa.trailing_zeros() as i32;
    mantissa >>= trailing;
    exp += trailing;
    let signed = |m: u64| i64::try_from(m).map(|m| if f < 0.0 { -m } else { m });
    let numerator = signed(mantissa).map_err(|_| ExprError::big_int())?;
    if exp >= 0 {
        let scaled = 1i128
            .checked_shl(exp as u32)
            .and_then(|s| i128::from(numerator).checked_mul(s))
            .and_then(|v| i64::try_from(v).ok())
            .ok_or_else(ExprError::big_int)?;
        Ok(Value::List(vec![Value::Int(scaled), Value::Int(1)]))
    } else {
        let denominator = 1i64
            .checked_shl((-exp) as u32)
            .filter(|d| *d > 0 && (-exp) < 63)
            .ok_or_else(ExprError::big_int)?;
        Ok(Value::List(vec![
            Value::Int(numerator),
            Value::Int(denominator),
        ]))
    }
}

/// `float.fromhex(text)`.
fn parse_float_hex(text: &str) -> ExprResult<f64> {
    let bad = || type_error("invalid hexadecimal floating-point string");
    let t = text.trim_matches(pystr::is_py_space);
    let (negative, t) = match t.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let lower = t.to_ascii_lowercase();
    let value = match lower.as_str() {
        "inf" | "infinity" => f64::INFINITY,
        "nan" => f64::NAN,
        _ => {
            let body = lower.strip_prefix("0x").unwrap_or(&lower);
            let (mantissa, exp) = match body.split_once('p') {
                Some((m, e)) => (m, e.parse::<i32>().map_err(|_| bad())?),
                None => (body, 0),
            };
            let (int_part, frac_part) = mantissa.split_once('.').unwrap_or((mantissa, ""));
            if int_part.is_empty() && frac_part.is_empty() {
                return Err(bad());
            }
            let mut value = 0.0_f64;
            for c in int_part.chars() {
                value = value * 16.0 + f64::from(c.to_digit(16).ok_or_else(bad)?);
            }
            let mut scale = 1.0 / 16.0;
            for c in frac_part.chars() {
                value += f64::from(c.to_digit(16).ok_or_else(bad)?) * scale;
                scale /= 16.0;
            }
            let scaled = value * 2f64.powi(exp);
            if scaled.is_infinite() {
                return Err(ExprError::Uncaught(
                    "OverflowError: hexadecimal value too large to represent as a float".into(),
                ));
            }
            scaled
        }
    };
    Ok(if negative { -value } else { value })
}

// === str ===================================================================

fn str_method(
    s: &str,
    name: &str,
    args: Vec<Value>,
    kwargs: Vec<(String, Value)>,
) -> ExprResult<Value> {
    let text = |v: String| Ok(Value::Str(v));
    let flag = |b: bool| Ok(Value::Bool(b));
    let count = |n: usize| Ok(Value::Int(n as i64));
    match name {
        "upper" | "lower" | "title" | "capitalize" | "swapcase" | "casefold" => {
            no_args(name, &args, &kwargs)?;
            text(match name {
                "upper" => pystr::upper(s),
                "lower" => pystr::lower(s),
                "title" => pystr::title(s),
                "capitalize" => pystr::capitalize(s),
                "swapcase" => swapcase(s),
                _ => casefold(s),
            })
        }
        "isalnum" | "isalpha" | "isascii" | "isdecimal" | "isdigit" | "isidentifier"
        | "islower" | "isnumeric" | "isprintable" | "isspace" | "istitle" | "isupper" => {
            no_args(name, &args, &kwargs)?;
            flag(str_predicate(s, name))
        }
        "strip" | "lstrip" | "rstrip" => {
            let bound = Bound::new(name, &[("chars", false)], 0, args, kwargs)?;
            let chars = match bound.get(0) {
                None | Some(Value::Null) => None,
                Some(Value::Str(c)) => Some(c.as_str()),
                Some(other) => {
                    return Err(type_error(format!(
                        "{name} arg must be None or str, not {}",
                        python_type(other)
                    )));
                }
            };
            text(pystr::strip(s, chars, name != "rstrip", name != "lstrip").to_owned())
        }
        "split" | "rsplit" => {
            let bound = Bound::new(name, &[("sep", true), ("maxsplit", true)], 0, args, kwargs)?;
            let maxsplit = bound.int(name, 1, false)?.unwrap_or(-1);
            let sep = match bound.get(0) {
                None | Some(Value::Null) => None,
                Some(Value::Str(sep)) => Some(sep.as_str()),
                Some(other) => {
                    return Err(type_error(format!(
                        "must be str or None, not {}",
                        python_type(other)
                    )));
                }
            };
            let parts = if name == "split" {
                match sep {
                    None => pystr::split_whitespace(s, maxsplit),
                    Some(sep) => pystr::split_sep(s, sep, maxsplit)
                        .ok_or_else(|| type_error("empty separator"))?,
                }
            } else {
                rsplit(s, sep, maxsplit)?
            };
            Ok(Value::List(parts.into_iter().map(Value::Str).collect()))
        }
        "splitlines" => {
            let bound = Bound::new(name, &[("keepends", true)], 0, args, kwargs)?;
            let keepends = bound.get(0).is_some_and(Value::is_truthy);
            Ok(Value::List(
                splitlines(s, keepends)
                    .into_iter()
                    .map(Value::Str)
                    .collect(),
            ))
        }
        "join" => {
            let bound = Bound::new(name, &[("iterable", false)], 1, args, kwargs)?;
            let items = super::builtins::py_iter(bound.get(0).expect("required"))?;
            let mut parts = Vec::with_capacity(items.len());
            for (i, item) in items.iter().enumerate() {
                match item {
                    Value::Str(p) => parts.push(p.as_str()),
                    other => {
                        return Err(type_error(format!(
                            "sequence item {i}: expected str instance, {} found",
                            python_type(other)
                        )));
                    }
                }
            }
            text(parts.join(s))
        }
        "replace" => {
            let bound = Bound::new(
                name,
                &[("old", false), ("new", false), ("count", true)],
                2,
                args,
                kwargs,
            )?;
            let old = bound.str(name, 0)?.expect("required");
            let new = bound.str(name, 1)?.expect("required");
            let limit = bound.int(name, 2, false)?.unwrap_or(-1);
            text(replace(s, old, new, limit))
        }
        "startswith" | "endswith" => {
            let bound = Bound::new(
                name,
                &[("prefix", false), ("start", false), ("end", false)],
                1,
                args,
                kwargs,
            )?;
            let start = bound.int(name, 1, true)?;
            let end = bound.int(name, 2, true)?;
            let chars: Vec<char> = s.chars().collect();
            let (start, end) = adjust_indices(chars.len(), start, end);
            let window: String = if start <= end {
                chars[start..end].iter().collect()
            } else {
                String::new()
            };
            let matches = |affix: &str| {
                start <= chars.len()
                    && end >= start
                    && if name == "startswith" {
                        window.starts_with(affix)
                    } else {
                        window.ends_with(affix)
                    }
            };
            match bound.get(0).expect("required") {
                Value::Str(affix) => flag(matches(affix)),
                // PARITY: a tuple of affixes is a list here (`Value` has no tuple).
                Value::List(affixes) => {
                    let mut found = false;
                    for affix in affixes {
                        match affix {
                            Value::Str(a) => found = found || matches(a),
                            other => {
                                return Err(type_error(format!(
                                    "tuple for {name} must only contain str, not {}",
                                    python_type(other)
                                )));
                            }
                        }
                    }
                    flag(found)
                }
                other => Err(type_error(format!(
                    "{name} first arg must be str or a tuple of str, not {}",
                    python_type(other)
                ))),
            }
        }
        "find" | "rfind" | "index" | "rindex" | "count" => {
            let bound = Bound::new(
                name,
                &[("sub", false), ("start", false), ("end", false)],
                1,
                args,
                kwargs,
            )?;
            let sub: Vec<char> = bound.str(name, 0)?.expect("required").chars().collect();
            let hay: Vec<char> = s.chars().collect();
            let (start, end) = adjust_indices(
                hay.len(),
                bound.int(name, 1, true)?,
                bound.int(name, 2, true)?,
            );
            if name == "count" {
                return count(count_sub(&hay, &sub, start, end));
            }
            let found = if name.starts_with('r') {
                find_sub(&hay, &sub, start, end, true)
            } else {
                find_sub(&hay, &sub, start, end, false)
            };
            match found {
                Some(i) => Ok(Value::Int(i as i64)),
                None if name.ends_with("index") => Err(type_error("substring not found")),
                None => Ok(Value::Int(-1)),
            }
        }
        "center" | "ljust" | "rjust" => {
            let bound = Bound::new(
                name,
                &[("width", false), ("fillchar", false)],
                1,
                args,
                kwargs,
            )?;
            let width = bound.int(name, 0, false)?.expect("required");
            let fill = match bound.str(name, 1)? {
                None => ' ',
                Some(f) => {
                    let mut it = f.chars();
                    match (it.next(), it.next()) {
                        (Some(c), None) => c,
                        _ => {
                            return Err(type_error(
                                "The fill character must be exactly one character long",
                            ));
                        }
                    }
                }
            };
            text(justify(s, name, width, fill)?)
        }
        "zfill" => {
            let bound = Bound::new(name, &[("width", false)], 1, args, kwargs)?;
            let width = bound.int(name, 0, false)?.expect("required");
            text(zfill(s, width)?)
        }
        "partition" | "rpartition" => {
            let bound = Bound::new(name, &[("sep", false)], 1, args, kwargs)?;
            let sep = bound.str(name, 0)?.expect("required");
            if sep.is_empty() {
                return Err(type_error("empty separator"));
            }
            let found = if name == "partition" {
                s.find(sep)
            } else {
                s.rfind(sep)
            };
            let parts = match found {
                Some(i) => [
                    s[..i].to_owned(),
                    sep.to_owned(),
                    s[i + sep.len()..].to_owned(),
                ],
                None if name == "partition" => [s.to_owned(), String::new(), String::new()],
                None => [String::new(), String::new(), s.to_owned()],
            };
            Ok(Value::List(parts.into_iter().map(Value::Str).collect()))
        }
        "removeprefix" | "removesuffix" => {
            let bound = Bound::new(name, &[("affix", false)], 1, args, kwargs)?;
            let affix = bound.str(name, 0)?.expect("required");
            text(
                if name == "removeprefix" {
                    s.strip_prefix(affix)
                } else {
                    s.strip_suffix(affix)
                }
                .unwrap_or(s)
                .to_owned(),
            )
        }
        "expandtabs" => {
            let bound = Bound::new(name, &[("tabsize", true)], 0, args, kwargs)?;
            text(expandtabs(s, bound.int(name, 0, false)?.unwrap_or(8))?)
        }
        "translate" => {
            let bound = Bound::new(name, &[("table", false)], 1, args, kwargs)?;
            text(translate(s, bound.get(0).expect("required"))?)
        }
        "format" => text(pyformat::str_format(s, &args, &kwargs)?),
        "format_map" => {
            let bound = Bound::new(name, &[("mapping", false)], 1, args, kwargs)?;
            // A non-mapping is only an error once a field looks something up.
            let kw: Vec<(String, Value)> = match bound.get(0).expect("required") {
                Value::Map(m) => m.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
                _ => Vec::new(),
            };
            text(pyformat::str_format(s, &[], &kw)?)
        }
        // bytes, and a dict keyed by integers.
        "encode" => {
            Bound::new(
                name,
                &[("encoding", true), ("errors", true)],
                0,
                args,
                kwargs,
            )?;
            Err(ExprError::unrepresentable("bytes"))
        }
        "maketrans" => Err(ExprError::unrepresentable("dict")),
        _ => Err(type_error(format!("'str' object has no method '{name}'"))),
    }
}

/// CPython's `ADJUST_INDICES`: `start` and `end` of a `find`-style call, clamped
/// to `0..=len` after counting negatives from the end (`start` may exceed `len`).
fn adjust_indices(len: usize, start: Option<i64>, end: Option<i64>) -> (usize, usize) {
    let len_i = len as i64;
    let mut end = end.unwrap_or(len_i);
    if end > len_i {
        end = len_i;
    } else if end < 0 {
        end = (end + len_i).max(0);
    }
    let mut start = start.unwrap_or(0);
    if start < 0 {
        start = (start + len_i).max(0);
    }
    (start as usize, end as usize)
}

fn count_sub(hay: &[char], sub: &[char], start: usize, end: usize) -> usize {
    if end < start || end - start < sub.len() {
        return 0;
    }
    if sub.is_empty() {
        return end - start + 1;
    }
    let mut n = 0;
    let mut i = start;
    while i + sub.len() <= end {
        if hay[i..i + sub.len()] == *sub {
            n += 1;
            i += sub.len();
        } else {
            i += 1;
        }
    }
    n
}

fn find_sub(
    hay: &[char],
    sub: &[char],
    start: usize,
    end: usize,
    from_right: bool,
) -> Option<usize> {
    if end < start || end - start < sub.len() {
        return None;
    }
    let last = end - sub.len();
    let matches = |i: &usize| hay[*i..*i + sub.len()] == *sub;
    if from_right {
        (start..=last).rev().find(matches)
    } else {
        (start..=last).find(matches)
    }
}

fn replace(s: &str, old: &str, new: &str, limit: i64) -> String {
    if limit < 0 {
        return s.replace(old, new);
    }
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    if old.is_empty() {
        // Insert `new` before each of the first `limit` characters and, if
        // there is room left, at the end.
        let mut out = String::new();
        let mut done = 0;
        for c in s.chars() {
            if done < limit {
                out.push_str(new);
                done += 1;
            }
            out.push(c);
        }
        if done < limit {
            out.push_str(new);
        }
        return out;
    }
    s.replacen(old, new, limit)
}

fn rsplit(s: &str, sep: Option<&str>, maxsplit: i64) -> ExprResult<Vec<String>> {
    let limit = if maxsplit < 0 {
        usize::MAX
    } else {
        maxsplit as usize
    };
    let mut parts: Vec<String> = Vec::new();
    match sep {
        Some("") => return Err(type_error("empty separator")),
        Some(sep) => {
            let mut rest = s;
            while parts.len() < limit {
                match rest.rfind(sep) {
                    Some(i) => {
                        parts.push(rest[i + sep.len()..].to_owned());
                        rest = &rest[..i];
                    }
                    None => break,
                }
            }
            parts.push(rest.to_owned());
        }
        None => {
            let mut rest = s;
            while parts.len() < limit {
                rest = rest.trim_end_matches(pystr::is_py_space);
                if rest.is_empty() {
                    break;
                }
                match rest.rfind(pystr::is_py_space) {
                    Some(i) => {
                        let word_start = i + rest[i..].chars().next().map_or(1, char::len_utf8);
                        parts.push(rest[word_start..].to_owned());
                        rest = &rest[..i];
                    }
                    None => {
                        parts.push(rest.to_owned());
                        rest = "";
                        break;
                    }
                }
            }
            let rest = rest.trim_end_matches(pystr::is_py_space);
            if !rest.is_empty() {
                parts.push(rest.to_owned());
            }
        }
    }
    parts.reverse();
    Ok(parts)
}

fn splitlines(s: &str, keepends: bool) -> Vec<String> {
    let mut out = Vec::new();
    let mut line = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        let is_break = matches!(
            c,
            '\n' | '\r'
                | '\x0b'
                | '\x0c'
                | '\x1c'
                | '\x1d'
                | '\x1e'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        );
        if !is_break {
            line.push(c);
            continue;
        }
        if keepends {
            line.push(c);
            if c == '\r' && chars.peek() == Some(&'\n') {
                line.push('\n');
                chars.next();
            }
        } else if c == '\r' && chars.peek() == Some(&'\n') {
            chars.next();
        }
        out.push(std::mem::take(&mut line));
    }
    if !line.is_empty() {
        out.push(line);
    }
    out
}

fn justify(s: &str, method: &str, width: i64, fill: char) -> ExprResult<String> {
    let len = s.chars().count();
    let width = usize::try_from(width).unwrap_or(0);
    if width > 10_000_000 {
        return Err(ExprError::unrepresentable("a string of that width"));
    }
    if width <= len {
        return Ok(s.to_owned());
    }
    let pad = width - len;
    let (left, right) = match method {
        "ljust" => (0, pad),
        "rjust" => (pad, 0),
        // `left = pad / 2 + (pad & width & 1)`, as `str.center`.
        _ => {
            let left = pad / 2 + (pad & width & 1);
            (left, pad - left)
        }
    };
    let fill = |n: usize| fill.to_string().repeat(n);
    Ok(format!("{}{s}{}", fill(left), fill(right)))
}

fn zfill(s: &str, width: i64) -> ExprResult<String> {
    let len = s.chars().count();
    let width = usize::try_from(width).unwrap_or(0);
    if width > 10_000_000 {
        return Err(ExprError::unrepresentable("a string of that width"));
    }
    if width <= len {
        return Ok(s.to_owned());
    }
    let zeros = "0".repeat(width - len);
    Ok(match s.chars().next() {
        Some(sign @ ('+' | '-')) => format!("{sign}{zeros}{}", &s[1..]),
        _ => format!("{zeros}{s}"),
    })
}

fn expandtabs(s: &str, tabsize: i64) -> ExprResult<String> {
    let tabsize = usize::try_from(tabsize).unwrap_or(0);
    let mut out = String::with_capacity(s.len());
    let mut column = 0usize;
    for c in s.chars() {
        match c {
            '\t' => {
                if tabsize > 0 {
                    let spaces = tabsize - column % tabsize;
                    if spaces > 10_000_000 {
                        return Err(ExprError::unrepresentable("a string of that width"));
                    }
                    out.extend(std::iter::repeat_n(' ', spaces));
                    column += spaces;
                }
            }
            '\n' | '\r' => {
                out.push(c);
                column = 0;
            }
            _ => {
                out.push(c);
                column += 1;
            }
        }
    }
    Ok(out)
}

fn translate(s: &str, table: &Value) -> ExprResult<String> {
    // `table[ord(c)]`: a lookup error leaves the character alone.
    let lookup = |c: char| -> Option<Value> {
        match table {
            Value::Map(m) => m.get(&(c as u32).to_string()).cloned(),
            Value::List(items) => items.get(c as usize).cloned(),
            Value::Str(chars) => chars
                .chars()
                .nth(c as usize)
                .map(|x| Value::Str(x.to_string())),
            _ => None,
        }
    };
    if !matches!(table, Value::Map(_) | Value::List(_) | Value::Str(_)) {
        return Err(type_error(format!(
            "'{}' object is not subscriptable",
            python_type(table)
        )));
    }
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match lookup(c) {
            None => out.push(c),
            Some(Value::Null) => {}
            Some(Value::Str(r)) => out.push_str(&r),
            Some(Value::Int(n)) => out.push(
                u32::try_from(n)
                    .ok()
                    .and_then(char::from_u32)
                    .ok_or_else(|| type_error("character mapping must be in range(0x110000)"))?,
            ),
            Some(_) => {
                return Err(type_error(
                    "character mapping must return integer, None or str",
                ));
            }
        }
    }
    Ok(out)
}

// === predicates and case ===================================================

fn in_ranges(ranges: &[(u32, u32)], c: char) -> bool {
    let cp = c as u32;
    ranges
        .binary_search_by(|&(lo, hi)| {
            if cp < lo {
                std::cmp::Ordering::Greater
            } else if cp > hi {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

/// `c.isalpha()`.
pub(super) fn is_alpha(c: char) -> bool {
    if c.is_ascii() {
        c.is_ascii_alphabetic()
    } else {
        in_ranges(pytables::ALPHA, c)
    }
}

/// An identifier's first character (`XID_Start` or `_`).
pub(super) fn is_identifier_start(c: char) -> bool {
    if c.is_ascii() {
        c.is_ascii_alphabetic() || c == '_'
    } else {
        in_ranges(pytables::XID_START, c)
    }
}

/// An identifier's later characters (`XID_Continue`).
pub(super) fn is_identifier_continue(c: char) -> bool {
    if c.is_ascii() {
        c.is_ascii_alphanumeric() || c == '_'
    } else {
        in_ranges(pytables::XID_CONTINUE, c)
    }
}

fn is_titlecase(c: char) -> bool {
    TITLECASE_LETTERS.binary_search(&(c as u32)).is_ok()
}

fn str_predicate(s: &str, name: &str) -> bool {
    let all = |f: fn(char) -> bool| s.chars().all(f);
    match name {
        "isascii" => s.is_ascii(),
        "isalpha" => !s.is_empty() && all(is_alpha),
        "isdecimal" => {
            !s.is_empty()
                && all(|c| c.is_ascii_digit() || (!c.is_ascii() && pystr::is_decimal_digit(c)))
        }
        "isdigit" => {
            !s.is_empty()
                && all(|c| c.is_ascii_digit() || (!c.is_ascii() && in_ranges(pytables::DIGIT, c)))
        }
        "isnumeric" => {
            !s.is_empty()
                && all(|c| c.is_ascii_digit() || (!c.is_ascii() && in_ranges(pytables::NUMERIC, c)))
        }
        "isalnum" => {
            !s.is_empty()
                && all(|c| {
                    is_alpha(c)
                        || c.is_ascii_digit()
                        || (!c.is_ascii() && in_ranges(pytables::NUMERIC, c))
                })
        }
        "isspace" => !s.is_empty() && all(pystr::is_py_space),
        "isprintable" => all(|c| {
            if c.is_ascii() {
                (' '..='~').contains(&c)
            } else {
                !in_ranges(pytables::NON_PRINTABLE, c)
            }
        }),
        "isidentifier" => {
            let mut chars = s.chars();
            chars.next().is_some_and(is_identifier_start) && chars.all(is_identifier_continue)
        }
        "islower" => {
            let mut cased = false;
            for c in s.chars() {
                if is_upper(c) || is_titlecase(c) {
                    return false;
                }
                if is_lower(c) {
                    cased = true;
                }
            }
            cased
        }
        "isupper" => {
            let mut cased = false;
            for c in s.chars() {
                if is_lower(c) || is_titlecase(c) {
                    return false;
                }
                if is_upper(c) {
                    cased = true;
                }
            }
            cased
        }
        _ => {
            // istitle
            let (mut cased, mut previous_cased) = (false, false);
            for c in s.chars() {
                if is_upper(c) || is_titlecase(c) {
                    if previous_cased {
                        return false;
                    }
                    previous_cased = true;
                    cased = true;
                } else if is_lower(c) {
                    if !previous_cased {
                        return false;
                    }
                    previous_cased = true;
                    cased = true;
                } else {
                    previous_cased = false;
                }
            }
            cased
        }
    }
}

fn is_cased(c: char) -> bool {
    pystr::is_cased(c)
}

fn is_upper(c: char) -> bool {
    in_ranges(UPPERCASE, c)
}

fn is_lower(c: char) -> bool {
    in_ranges(LOWERCASE, c)
}

/// `str.swapcase()`: lowercase letters become their full uppercase mapping,
/// uppercase ones their lowercase (capital sigma becoming final sigma where the
/// Final_Sigma context holds).
fn swapcase(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let ignorable = |c: char| in_ranges(CASE_IGNORABLE, c);
    let mut out = String::with_capacity(s.len());
    for (i, &c) in chars.iter().enumerate() {
        if c == '\u{3a3}' {
            let before = chars[..i].iter().rev().find(|&&p| !ignorable(p));
            let after = chars[i + 1..].iter().find(|&&n| !ignorable(n));
            let final_sigma =
                before.is_some_and(|&p| is_cased(p)) && !after.is_some_and(|&n| is_cased(n));
            out.push(if final_sigma { '\u{3c2}' } else { '\u{3c3}' });
        } else if is_upper(c) {
            pystr::push_lower_char(&mut out, c);
        } else if is_lower(c) {
            pystr::push_upper(&mut out, c);
        } else {
            out.push(c);
        }
    }
    out
}

fn casefold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        pystr::push_casefold(&mut out, c);
    }
    out
}
