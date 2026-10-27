//! The built-in expression function table.
//!
//! Port of the function tables in upstream `linkml_map.utils.eval_utils`:
//!
//! | upstream table          | wrapper                  | here                         |
//! |-------------------------|--------------------------|------------------------------|
//! | `_LIST_FUNCTIONS`       | `_null_safe`             | [`LIST_FUNCTIONS`]           |
//! | `_SCALAR_FUNCTIONS`     | `_distributing`          | [`SCALAR_FUNCTIONS`]         |
//! | `_TYPE_PREDICATES`      | none                     | [`TYPE_PREDICATES`]          |
//! | `case`, `coalesce`, `is_numeric` | none            | [`PLAIN_FUNCTIONS`]          |
//! | `list`, `tuple` (simpleeval `EvalWithCompoundTypes`) | none | [`COMPOUND_FUNCTIONS`] |
//!
//! Most upstream entries are Python builtins or unbound `str` methods
//! (`str.upper`, `str.split`, `sum`, `sorted`, ...), so an entry here reproduces
//! the Python call: its positional arity, which argument types raise
//! `TypeError`/`ValueError` (mapped to [`ExprError::Eval`]), and its result type.
//!
//! The wrappers are applied by [`call_function`]:
//!
//! * `_null_safe(f)`: any `None` argument gives `None`, before `f` (and so before
//!   any arity check) runs.
//! * `_distributing(f)`: when the *first* argument is a list, `f` is applied to
//!   each element (with the remaining arguments unchanged, and `None` handled per
//!   element as in `_null_safe`); otherwise a `None` argument gives `None`.
//!   There is no recursion: a nested list is handed to `f` as a list.
//!
//! PARITY: [`Value`] has no tuple, set or bytes type and no big integers.
//! A tuple literal is a list (so `is_list((1, 2))` is true here and a list
//! distributes where a tuple would not), `startswith`/`endswith` accept a list of
//! strings where Python wants a tuple, and an integer result outside `i64` is an
//! error where Python would return a big integer.

use std::borrow::Cow;
use std::cmp::Ordering;

use super::error::{ExprError, ExprResult};
use super::eval::{eval_subscript, membership, py_str};
use super::{compound, pystr, slugify};
use crate::decimal::Decimal;
use crate::pyfmt::py_repr_str;
use crate::value::Value;

/// A built-in's body: called with the (already wrapper-processed) positional
/// arguments, it checks its own arity as the Python callable would.
type Builtin = fn(&[Value]) -> ExprResult<Value>;

/// Upstream `_LIST_FUNCTIONS`: each is `_null_safe(...)`, and none distributes.
const LIST_FUNCTIONS: &[(&str, Builtin)] = &[
    ("max", max),
    ("min", min),
    ("len", len),
    ("sum", sum),
    ("sorted", sorted),
    ("any", any),
    ("all", all),
    ("reversed", reversed),
    ("first", first),
    ("last", last),
];

/// Upstream `_SCALAR_FUNCTIONS`: each is `_distributing(...)`.
const SCALAR_FUNCTIONS: &[(&str, Builtin)] = &[
    ("str", str_),
    ("int", int),
    ("float", float),
    ("bool", bool_),
    ("abs", abs),
    ("round", round),
    ("floor", floor),
    ("ceil", ceil),
    ("strlen", len),
    ("uuid5", uuid5),
    ("substr", substr),
    // String case/formatting
    ("upper", upper),
    ("lower", lower),
    ("title", title),
    ("capitalize", capitalize),
    // Whitespace trimming
    ("strip", strip),
    ("lstrip", lstrip),
    ("rstrip", rstrip),
    // String content
    ("replace", replace),
    ("startswith", startswith),
    ("endswith", endswith),
    ("contains", contains),
    // String splitting/joining
    ("split", split),
    ("join", join),
    // Identifier-shaped normalization
    ("slugify", slugify_),
    ("to_snake", to_snake),
    ("to_camel", to_camel),
    ("to_pascal", to_pascal),
];

/// Upstream `_TYPE_PREDICATES`: not distributing, they test the value as it is.
const TYPE_PREDICATES: &[(&str, Builtin)] = &[
    ("is_str", is_str),
    ("is_int", is_int),
    ("is_float", is_float),
    ("is_bool", is_bool),
    ("is_list", is_list),
];

/// The unwrapped entries of upstream `FUNCTIONS`.
const PLAIN_FUNCTIONS: &[(&str, Builtin)] = &[
    ("case", case),
    ("coalesce", coalesce),
    ("is_numeric", is_numeric),
];

/// Constructors simpleeval's `EvalWithCompoundTypes` adds to the namespace
/// (upstream `_COMPOUND_TYPE_FUNCTIONS`). `dict` also takes keyword arguments
/// (see [`call_function_kw`]); `set` builds a de-duplicated list (see
/// [`compound`]).
const COMPOUND_FUNCTIONS: &[(&str, Builtin)] = &[
    ("list", list),
    ("tuple", list),
    ("dict", |args| compound::dict(args, Vec::new())),
    ("set", compound::set),
];

/// How upstream wraps an entry.
#[derive(Clone, Copy)]
enum Wrap {
    Plain,
    NullSafe,
    Distributing,
}

const TABLES: &[(Wrap, &[(&str, Builtin)])] = &[
    (Wrap::NullSafe, LIST_FUNCTIONS),
    (Wrap::Distributing, SCALAR_FUNCTIONS),
    (Wrap::Plain, TYPE_PREDICATES),
    (Wrap::Plain, PLAIN_FUNCTIONS),
    (Wrap::Plain, COMPOUND_FUNCTIONS),
];

/// Every built-in function name (upstream `FUNCTIONS` plus the compound-type
/// constructors), in table order.
pub(super) fn function_names() -> impl Iterator<Item = &'static str> {
    TABLES
        .iter()
        .flat_map(|(_, table)| table.iter().map(|&(name, _)| name))
}

fn lookup(name: &str) -> Option<(Wrap, Builtin)> {
    TABLES.iter().find_map(|&(wrap, table)| {
        table
            .iter()
            .find(|&&(n, _)| n == name)
            .map(|&(_, f)| (wrap, f))
    })
}

/// Call the built-in `name` with already-evaluated positional `args` and
/// keyword arguments (`name(a, k=v)`).
///
/// Upstream wraps its functions in `*args`-only closures, so only the
/// constructors take keywords: `dict(a=1)` builds a dict, while the others raise
/// the `TypeError` Python gives for an unexpected keyword.
pub(super) fn call_function_kw(
    name: &str,
    args: Vec<Value>,
    keywords: Vec<(String, Value)>,
) -> ExprResult<Value> {
    let Some((first, _)) = keywords.first() else {
        return call_function(name, args);
    };
    match name {
        "dict" => compound::dict(&args, keywords),
        "list" | "tuple" | "set" => err(format!("{name}() takes no keyword arguments")),
        _ if lookup(name).is_none() => Err(unknown_function(name)),
        _ => err(format!(
            "{name}() got an unexpected keyword argument {}",
            py_repr_str(first)
        )),
    }
}

/// Call the built-in `name` with already-evaluated `args`.
pub(super) fn call_function(name: &str, mut args: Vec<Value>) -> ExprResult<Value> {
    let Some((wrap, f)) = lookup(name) else {
        return Err(unknown_function(name));
    };
    match wrap {
        Wrap::Plain => f(&args),
        // _null_safe: any None argument short-circuits to None.
        Wrap::NullSafe => {
            if args.iter().any(Value::is_null) {
                Ok(Value::Null)
            } else {
                f(&args)
            }
        }
        // _distributing: a list first argument is mapped over, element-wise
        // null-safe; anything else is null-safe as a whole.
        Wrap::Distributing => {
            if matches!(args.first(), Some(Value::List(_))) {
                let Value::List(items) = std::mem::take(&mut args[0]) else {
                    unreachable!("checked to be a list above");
                };
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    args[0] = item;
                    out.push(if args.iter().any(Value::is_null) {
                        Value::Null
                    } else {
                        f(&args)?
                    });
                }
                Ok(Value::List(out))
            } else if args.iter().any(Value::is_null) {
                Ok(Value::Null)
            } else {
                f(&args)
            }
        }
    }
}

// --- errors ---

fn err<T>(message: impl Into<String>) -> ExprResult<T> {
    Err(ExprError::Eval(message.into()))
}

/// Python's name for the type of `v`.
fn tn(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
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

fn check_arity(name: &str, args: &[Value], min: usize, max: usize) -> ExprResult<()> {
    let given = args.len();
    if (min..=max).contains(&given) {
        return Ok(());
    }
    let expected = if min == max {
        format!("exactly {min} argument{}", if min == 1 { "" } else { "s" })
    } else if given < min {
        format!("at least {min} arguments")
    } else {
        format!("at most {max} arguments")
    };
    err(format!("{name}() takes {expected} ({given} given)"))
}

fn overflow<T>() -> ExprResult<T> {
    err("integer result out of range (big integers are not supported)")
}

/// The `str` a `str.<method>` descriptor is applied to.
fn self_str<'a>(method: &str, v: &'a Value) -> ExprResult<&'a str> {
    match v {
        Value::Str(s) => Ok(s),
        other => err(format!(
            "descriptor '{method}' for 'str' objects doesn't apply to a '{}' object",
            tn(other)
        )),
    }
}

/// A `str` argument of a `str` method.
fn str_arg<'a>(method: &str, position: usize, v: &'a Value) -> ExprResult<&'a str> {
    match v {
        Value::Str(s) => Ok(s),
        other => err(format!(
            "{method}() argument {position} must be str, not {}",
            tn(other)
        )),
    }
}

/// An `int`-like argument (`__index__`): ints and bools.
fn int_arg(v: &Value) -> ExprResult<i64> {
    match v {
        Value::Int(i) => Ok(*i),
        Value::Bool(b) => Ok(i64::from(*b)),
        other => err(format!(
            "'{}' object cannot be interpreted as an integer",
            tn(other)
        )),
    }
}

/// Python iteration over a value: lists by element, strings by character and
/// dicts by key.
pub(super) fn py_iter(v: &Value) -> ExprResult<Cow<'_, [Value]>> {
    match v {
        Value::List(items) => Ok(Cow::Borrowed(items)),
        Value::Str(s) => Ok(Cow::Owned(
            s.chars().map(|c| Value::Str(c.to_string())).collect(),
        )),
        Value::Map(m) => Ok(Cow::Owned(m.keys().cloned().map(Value::Str).collect())),
        other => err(format!("'{}' object is not iterable", tn(other))),
    }
}

// --- unknown functions (upstream `LinkMLEvaluator._eval_call`) ---

/// The error for a call to a name that is not in the function table: upstream's
/// `UnknownFunctionError` message, with did-you-mean suggestions.
fn unknown_function(name: &str) -> ExprError {
    ExprError::Eval(if name == "__import__" {
        "Import expressions are not supported.".to_string()
    } else if name.starts_with('_') {
        format!("Unknown function {}.", py_repr_str(name))
    } else {
        suggest_for_unknown_name(name)
    })
}

/// Upstream `suggest_for_unknown_name`.
fn suggest_for_unknown_name(name: &str) -> String {
    let mut pool: Vec<&str> = function_names().collect();
    pool.sort_unstable();
    pool.dedup();
    let suggestions = get_close_matches(name, &pool, 3, 0.6);
    let hint = if suggestions.is_empty() {
        String::new()
    } else {
        let quoted: Vec<String> = suggestions.iter().map(|s| py_repr_str(s)).collect();
        format!(" Did you mean: {}?", quoted.join(", "))
    };
    format!(
        "Unknown function {}.{hint} (If this is a custom function, pass it via --functions <path>.)",
        py_repr_str(name)
    )
}

/// `difflib.get_close_matches(word, possibilities, n, cutoff)`.
fn get_close_matches<'a>(
    word: &str,
    possibilities: &[&'a str],
    n: usize,
    cutoff: f64,
) -> Vec<&'a str> {
    let b: Vec<char> = word.chars().collect();
    let mut scored: Vec<(f64, &str)> = possibilities
        .iter()
        .filter_map(|&x| {
            let a: Vec<char> = x.chars().collect();
            let ratio = sequence_ratio(&a, &b);
            (ratio >= cutoff).then_some((ratio, x))
        })
        .collect();
    // heapq.nlargest over (score, string) tuples.
    scored.sort_by(|l, r| r.0.total_cmp(&l.0).then_with(|| r.1.cmp(l.1)));
    scored.into_iter().take(n).map(|(_, x)| x).collect()
}

/// `difflib.SequenceMatcher(None, a, b).ratio()` (no junk heuristics).
fn sequence_ratio(a: &[char], b: &[char]) -> f64 {
    let total = a.len() + b.len();
    if total == 0 {
        return 1.0;
    }
    2.0 * matching_total(a, b, 0, a.len(), 0, b.len()) as f64 / total as f64
}

fn matching_total(a: &[char], b: &[char], alo: usize, ahi: usize, blo: usize, bhi: usize) -> usize {
    let (i, j, k) = longest_match(a, b, alo, ahi, blo, bhi);
    if k == 0 {
        return 0;
    }
    let left = if alo < i && blo < j {
        matching_total(a, b, alo, i, blo, j)
    } else {
        0
    };
    let right = if i + k < ahi && j + k < bhi {
        matching_total(a, b, i + k, ahi, j + k, bhi)
    } else {
        0
    };
    k + left + right
}

/// `SequenceMatcher.find_longest_match`: the longest block common to
/// `a[alo..ahi]` and `b[blo..bhi]`, earliest in `a`, then earliest in `b`.
fn longest_match(
    a: &[char],
    b: &[char],
    alo: usize,
    ahi: usize,
    blo: usize,
    bhi: usize,
) -> (usize, usize, usize) {
    let (mut best_i, mut best_j, mut best_size) = (alo, blo, 0);
    // j2len[j + 1] is the length of the match ending at a[i - 1], b[j].
    let mut j2len = vec![0_usize; b.len() + 1];
    for (i, &ac) in a.iter().enumerate().take(ahi).skip(alo) {
        let mut new_j2len = vec![0_usize; b.len() + 1];
        for (j, &bc) in b.iter().enumerate().take(bhi).skip(blo) {
            if ac == bc {
                let k = j2len[j] + 1;
                new_j2len[j + 1] = k;
                if k > best_size {
                    (best_i, best_j, best_size) = (i + 1 - k, j + 1 - k, k);
                }
            }
        }
        j2len = new_j2len;
    }
    (best_i, best_j, best_size)
}

// --- ordering (Python `<` / `>` on raw values; no numeric-string coercion) ---

/// A number as Python compares it: `bool` and `int` exactly, `float`, `Decimal`.
enum Num<'a> {
    Int(i64),
    Float(f64),
    Dec(&'a Decimal),
}

fn as_num(v: &Value) -> Option<Num<'_>> {
    match v {
        Value::Bool(b) => Some(Num::Int(i64::from(*b))),
        Value::Int(i) => Some(Num::Int(*i)),
        Value::Float(f) => Some(Num::Float(*f)),
        Value::Decimal(d) => Some(Num::Dec(d)),
        _ => None,
    }
}

/// Exact comparison of an integer with a float; `None` for NaN.
fn cmp_int_float(i: i64, f: f64) -> Option<Ordering> {
    if f.is_nan() {
        return None;
    }
    // 2^63 is exactly representable; every i64 is below it.
    if f >= 9_223_372_036_854_775_808.0 {
        return Some(Ordering::Less);
    }
    if f < -9_223_372_036_854_775_808.0 {
        return Some(Ordering::Greater);
    }
    let floor = f.floor();
    // `floor` is within i64 here.
    let fi = floor as i64;
    Some(i.cmp(&fi).then(if f > floor {
        Ordering::Less
    } else {
        Ordering::Equal
    }))
}

fn cmp_num(a: &Num<'_>, b: &Num<'_>) -> Option<Ordering> {
    match (a, b) {
        (Num::Int(x), Num::Int(y)) => Some(x.cmp(y)),
        (Num::Float(x), Num::Float(y)) => x.partial_cmp(y),
        (Num::Int(x), Num::Float(y)) => cmp_int_float(*x, *y),
        (Num::Float(x), Num::Int(y)) => cmp_int_float(*y, *x).map(Ordering::reverse),
        (Num::Dec(x), Num::Dec(y)) => Some(x.cmp(y)),
        (Num::Dec(x), Num::Int(y)) => Some((*x).cmp(&Decimal::from_i64(*y))),
        (Num::Int(x), Num::Dec(y)) => Some(Decimal::from_i64(*x).cmp(y)),
        (Num::Dec(x), Num::Float(y)) => x.cmp_f64(*y),
        (Num::Float(x), Num::Dec(y)) => y.cmp_f64(*x).map(Ordering::reverse),
    }
}

/// Python's ordering of two values for `symbol` (`<` or `>`): `None` when
/// unordered (NaN), an error when the types do not order (`TypeError`).
fn py_cmp(a: &Value, b: &Value, symbol: &str) -> ExprResult<Option<Ordering>> {
    if let (Some(x), Some(y)) = (as_num(a), as_num(b)) {
        return Ok(cmp_num(&x, &y));
    }
    match (a, b) {
        (Value::Str(x), Value::Str(y)) => Ok(Some(x.cmp(y))),
        (Value::List(x), Value::List(y)) => {
            // The first position whose items differ decides; otherwise length.
            for (p, q) in x.iter().zip(y) {
                if !p.py_eq(q) {
                    return py_cmp(p, q, symbol);
                }
            }
            Ok(Some(x.len().cmp(&y.len())))
        }
        _ => err(format!(
            "'{symbol}' not supported between instances of '{}' and '{}'",
            tn(a),
            tn(b)
        )),
    }
}

fn py_lt(a: &Value, b: &Value) -> ExprResult<bool> {
    Ok(py_cmp(a, b, "<")? == Some(Ordering::Less))
}

fn py_gt(a: &Value, b: &Value) -> ExprResult<bool> {
    Ok(py_cmp(a, b, ">")? == Some(Ordering::Greater))
}

// --- list functions ---

/// CPython `min_max`: one iterable argument, or two or more positional values.
fn min_max(name: &str, args: &[Value], want_max: bool) -> ExprResult<Value> {
    let items: Cow<'_, [Value]> = match args {
        [] => return err(format!("{name} expected at least 1 argument, got 0")),
        [only] => py_iter(only)?,
        many => Cow::Borrowed(many),
    };
    let mut iter = items.iter();
    let Some(mut best) = iter.next() else {
        return err(format!("{name}() iterable argument is empty"));
    };
    for item in iter {
        // max keeps the first maximal item (`item > best`), min the first minimal.
        let better = if want_max {
            py_gt(item, best)?
        } else {
            py_lt(item, best)?
        };
        if better {
            best = item;
        }
    }
    Ok(best.clone())
}

fn max(args: &[Value]) -> ExprResult<Value> {
    min_max("max", args, true)
}

fn min(args: &[Value]) -> ExprResult<Value> {
    min_max("min", args, false)
}

/// `len`, and `strlen` (`_distributing(len)`).
fn len(args: &[Value]) -> ExprResult<Value> {
    check_arity("len", args, 1, 1)?;
    match &args[0] {
        Value::Str(s) => Ok(Value::Int(s.chars().count() as i64)),
        Value::List(l) => Ok(Value::Int(l.len() as i64)),
        Value::Map(m) => Ok(Value::Int(m.len() as i64)),
        other => err(format!("object of type '{}' has no len()", tn(other))),
    }
}

/// Python `a + b` on raw values (no numeric-string coercion).
fn py_add(a: &Value, b: &Value) -> ExprResult<Value> {
    let unsupported = || {
        err(format!(
            "unsupported operand type(s) for +: '{}' and '{}'",
            tn(a),
            tn(b)
        ))
    };
    match (a, b) {
        (Value::Str(x), Value::Str(y)) => Ok(Value::Str(format!("{x}{y}"))),
        (Value::List(x), Value::List(y)) => Ok(Value::List(x.iter().chain(y).cloned().collect())),
        (Value::Decimal(_), _) | (_, Value::Decimal(_)) => err("Decimal addition is not supported"),
        _ => match (as_num(a), as_num(b)) {
            (Some(Num::Int(x)), Some(Num::Int(y))) => match x.checked_add(y) {
                Some(sum) => Ok(Value::Int(sum)),
                None => overflow(),
            },
            (Some(x), Some(y)) => {
                let float = |n: Num<'_>| match n {
                    Num::Int(i) => i as f64,
                    Num::Float(f) => f,
                    Num::Dec(d) => d.to_f64(),
                };
                Ok(Value::Float(float(x) + float(y)))
            }
            _ => unsupported(),
        },
    }
}

/// `sum(iterable, start=0)`, following CPython's `builtin_sum_impl`: an exact
/// `int` accumulator, then (once the running total is a `float`) Neumaier
/// compensated summation of the floats, then plain `+`.
///
/// PARITY: the compensated float path is CPython >= 3.12; earlier versions add
/// the floats naively.
fn sum(args: &[Value]) -> ExprResult<Value> {
    check_arity("sum", args, 1, 2)?;
    let items = py_iter(&args[0])?;
    let mut acc = match args.get(1) {
        Some(Value::Str(_)) => return err("sum() can't sum strings [use ''.join(seq) instead]"),
        Some(start) => start.clone(),
        None => Value::Int(0),
    };
    let mut rest: &[Value] = &items;
    if let Value::Int(mut total) = acc {
        while let Some((Value::Int(i), tail)) = rest.split_first() {
            total = match total.checked_add(*i) {
                Some(t) => t,
                None => return overflow(),
            };
            rest = tail;
        }
        acc = Value::Int(total);
        // The first non-int item is added generically; the result may be a float.
        if let Some((item, tail)) = rest.split_first() {
            acc = py_add(&acc, item)?;
            rest = tail;
        }
    }
    if let Value::Float(mut total) = acc {
        let mut compensation = 0.0_f64;
        while let Some((item, tail)) = rest.split_first() {
            match item {
                Value::Float(x) => {
                    // Neumaier's improved Kahan-Babuska summation.
                    let t = total + x;
                    if total.abs() >= x.abs() {
                        compensation += (total - t) + x;
                    } else {
                        compensation += (x - t) + total;
                    }
                    total = t;
                }
                Value::Int(i) => total += *i as f64,
                Value::Bool(b) => total += f64::from(u8::from(*b)),
                _ => break,
            }
            rest = tail;
        }
        // Don't let adding the compensation turn an infinite sum into NaN.
        if compensation != 0.0 && compensation.is_finite() {
            total += compensation;
        }
        acc = Value::Float(total);
    }
    for item in rest {
        acc = py_add(&acc, item)?;
    }
    Ok(acc)
}

/// `sorted(iterable)`: a stable sort by Python `<`.
fn sorted(args: &[Value]) -> ExprResult<Value> {
    check_arity("sorted", args, 1, 1)?;
    Ok(Value::List(merge_sort(py_iter(&args[0])?.into_owned())?))
}

fn merge_sort(mut items: Vec<Value>) -> ExprResult<Vec<Value>> {
    if items.len() <= 1 {
        return Ok(items);
    }
    let right = items.split_off(items.len() / 2);
    let (left, right) = (merge_sort(items)?, merge_sort(right)?);
    let mut out = Vec::with_capacity(left.len() + right.len());
    let (mut l, mut r) = (left.into_iter().peekable(), right.into_iter().peekable());
    while let (Some(lv), Some(rv)) = (l.peek(), r.peek()) {
        // Take from the right only when strictly smaller, which keeps it stable.
        let take_right = py_lt(rv, lv)?;
        out.extend(if take_right { r.next() } else { l.next() });
    }
    out.extend(l);
    out.extend(r);
    Ok(out)
}

fn any(args: &[Value]) -> ExprResult<Value> {
    check_arity("any", args, 1, 1)?;
    Ok(Value::Bool(py_iter(&args[0])?.iter().any(Value::is_truthy)))
}

fn all(args: &[Value]) -> ExprResult<Value> {
    check_arity("all", args, 1, 1)?;
    Ok(Value::Bool(py_iter(&args[0])?.iter().all(Value::is_truthy)))
}

/// `_list_reversed`: `list(reversed(x))`.
fn reversed(args: &[Value]) -> ExprResult<Value> {
    check_arity("_list_reversed", args, 1, 1)?;
    match &args[0] {
        Value::List(_) | Value::Str(_) | Value::Map(_) => {
            let mut items = py_iter(&args[0])?.into_owned();
            items.reverse();
            Ok(Value::List(items))
        }
        other => err(format!("'{}' object is not reversible", tn(other))),
    }
}

/// `x[index] if x else None` (upstream `_first` / `_last`).
fn first_or_last(name: &str, args: &[Value], index: i64) -> ExprResult<Value> {
    check_arity(name, args, 1, 1)?;
    let x = &args[0];
    if !x.is_truthy() {
        return Ok(Value::Null);
    }
    if let Value::Map(_) = x {
        // A non-empty dict has no integer key here.
        return err(format!("KeyError: {index}"));
    }
    eval_subscript(x, &Value::Int(index))
}

fn first(args: &[Value]) -> ExprResult<Value> {
    first_or_last("_first", args, 0)
}

fn last(args: &[Value]) -> ExprResult<Value> {
    first_or_last("_last", args, -1)
}

// --- numeric scalar functions ---

fn str_(args: &[Value]) -> ExprResult<Value> {
    check_arity("str", args, 0, 1)?;
    Ok(Value::Str(args.first().map(py_str).unwrap_or_default()))
}

fn bool_(args: &[Value]) -> ExprResult<Value> {
    if args.len() > 1 {
        return err(format!(
            "bool expected at most 1 argument, got {}",
            args.len()
        ));
    }
    Ok(Value::Bool(args.first().is_some_and(Value::is_truthy)))
}

/// A float as an `int`: `ValueError`/`OverflowError` for NaN and infinities
/// (`int(float("nan"))`), and for anything outside `i64`.
fn float_to_int(f: f64) -> ExprResult<Value> {
    if f.is_nan() {
        return err("cannot convert float NaN to integer");
    }
    if f.is_infinite() {
        return err("cannot convert float infinity to integer");
    }
    if (-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&f) {
        Ok(Value::Int(f as i64))
    } else {
        overflow()
    }
}

/// How a decimal becomes an integer.
#[derive(Clone, Copy)]
enum ToInt {
    Floor,
    Ceil,
    Trunc,
    HalfEven,
}

/// `math.floor` / `math.ceil` / `int()` / `round()` of a `Decimal`.
fn decimal_to_int(d: &Decimal, mode: ToInt) -> ExprResult<Value> {
    let text = d.to_fixed_string();
    let (negative, body) = match text.strip_prefix('-') {
        Some(b) => (true, b),
        None => (false, text.as_str()),
    };
    let (int_part, frac) = body.split_once('.').unwrap_or((body, ""));
    let Ok(magnitude) = int_part.parse::<i64>() else {
        return overflow();
    };
    let frac_nonzero = frac.bytes().any(|b| b != b'0');
    let first = frac.bytes().next().unwrap_or(b'0');
    let rest_nonzero = frac.bytes().skip(1).any(|b| b != b'0');
    let bump = match mode {
        ToInt::Floor => negative && frac_nonzero,
        ToInt::Ceil => !negative && frac_nonzero,
        ToInt::Trunc => false,
        ToInt::HalfEven => first > b'5' || (first == b'5' && (rest_nonzero || magnitude % 2 == 1)),
    };
    let Some(magnitude) = (if bump {
        magnitude.checked_add(1)
    } else {
        Some(magnitude)
    }) else {
        return overflow();
    };
    Ok(Value::Int(if negative { -magnitude } else { magnitude }))
}

/// `int(text, base)`: CPython's `PyLong_FromString` (surrounding whitespace, a
/// sign, `0x`/`0o`/`0b` prefixes, single underscores between digits).
///
fn parse_int(text: &str, base: i64) -> ExprResult<Value> {
    if base != 0 && !(2..=36).contains(&base) {
        return err("int() base must be >= 2 and <= 36, or 0");
    }
    let invalid = || {
        err(format!(
            "invalid literal for int() with base {base}: {}",
            py_repr_str(text)
        ))
    };
    let normalised = pystr::ascii_digits(text);
    let trimmed = normalised.trim_matches(pystr::is_py_space);
    let (negative, unsigned) = match trimmed.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, trimmed.strip_prefix('+').unwrap_or(trimmed)),
    };
    let bytes = unsigned.as_bytes();
    let prefix_base = match bytes {
        [b'0', p, ..] => match p | 0x20 {
            b'x' => 16,
            b'o' => 8,
            b'b' => 2,
            _ => 0,
        },
        _ => 0,
    };
    let (radix, mut digits) = if base == 0 && prefix_base != 0 {
        (prefix_base, &unsigned[2..])
    } else if base == 0 {
        (10, unsigned)
    } else if base == prefix_base {
        (base, &unsigned[2..])
    } else {
        (base, unsigned)
    };
    if digits.len() != unsigned.len() {
        digits = digits.strip_prefix('_').unwrap_or(digits);
    }
    // A base-0 decimal may not have leading zeros ("012" is invalid, "00" is not).
    if base == 0
        && radix == 10
        && digits.len() > 1
        && digits.starts_with('0')
        && digits.bytes().any(|b| b != b'0' && b != b'_')
    {
        return invalid();
    }
    if digits.is_empty() {
        return invalid();
    }
    let mut value: i128 = 0;
    let mut previous_underscore = true; // a leading underscore is invalid
    for c in digits.chars() {
        if c == '_' {
            if previous_underscore {
                return invalid();
            }
            previous_underscore = true;
            continue;
        }
        let Some(d) = c.to_digit(36).filter(|&d| i64::from(d) < radix) else {
            return invalid();
        };
        previous_underscore = false;
        value = match value
            .checked_mul(i128::from(radix))
            .and_then(|v| v.checked_add(i128::from(d)))
        {
            Some(v) => v,
            None => return overflow(),
        };
    }
    if previous_underscore {
        return invalid();
    }
    match i64::try_from(if negative { -value } else { value }) {
        Ok(i) => Ok(Value::Int(i)),
        Err(_) => overflow(),
    }
}

/// `int(x)` / `int(x, base)`.
fn int(args: &[Value]) -> ExprResult<Value> {
    check_arity("int", args, 0, 2)?;
    let Some(x) = args.first() else {
        return Ok(Value::Int(0));
    };
    if let Some(base) = args.get(1) {
        let base = int_arg(base)?;
        let Value::Str(text) = x else {
            return err("int() can't convert non-string with explicit base");
        };
        return parse_int(text, base);
    }
    match x {
        Value::Int(i) => Ok(Value::Int(*i)),
        Value::Bool(b) => Ok(Value::Int(i64::from(*b))),
        Value::Float(f) => float_to_int(f.trunc()),
        Value::Decimal(d) => decimal_to_int(d, ToInt::Trunc),
        Value::Str(text) => parse_int(text, 10),
        other => err(format!(
            "int() argument must be a string, a bytes-like object or a real number, not '{}'",
            tn(other)
        )),
    }
}

/// Python's `float(str)`: surrounding whitespace, `inf`/`nan` spellings, and
/// single underscores between digits.
fn parse_py_float(text: &str) -> Option<f64> {
    let normalised = pystr::ascii_digits(text);
    let trimmed = normalised.trim_matches(pystr::is_py_space);
    if trimmed.contains('_') {
        let bytes = trimmed.as_bytes();
        let valid = bytes.iter().enumerate().all(|(i, &b)| {
            b != b'_'
                || (i > 0
                    && bytes[i - 1].is_ascii_digit()
                    && bytes.get(i + 1).is_some_and(u8::is_ascii_digit))
        });
        if !valid {
            return None;
        }
        return crate::value::parse_python_float(&trimmed.replace('_', ""));
    }
    crate::value::parse_python_float(trimmed)
}

fn float(args: &[Value]) -> ExprResult<Value> {
    check_arity("float", args, 0, 1)?;
    match args.first() {
        None => Ok(Value::Float(0.0)),
        Some(Value::Int(i)) => Ok(Value::Float(*i as f64)),
        Some(Value::Bool(b)) => Ok(Value::Float(f64::from(u8::from(*b)))),
        Some(Value::Float(f)) => Ok(Value::Float(*f)),
        Some(Value::Decimal(d)) => Ok(Value::Float(d.to_f64())),
        Some(Value::Str(s)) => match parse_py_float(s) {
            Some(f) => Ok(Value::Float(f)),
            None => err(format!(
                "could not convert string to float: {}",
                py_repr_str(s)
            )),
        },
        Some(other) => err(format!(
            "float() argument must be a string or a real number, not '{}'",
            tn(other)
        )),
    }
}

fn abs(args: &[Value]) -> ExprResult<Value> {
    check_arity("abs", args, 1, 1)?;
    match &args[0] {
        Value::Int(i) => match i.checked_abs() {
            Some(a) => Ok(Value::Int(a)),
            None => overflow(),
        },
        Value::Bool(b) => Ok(Value::Int(i64::from(*b))),
        Value::Float(f) => Ok(Value::Float(f.abs())),
        Value::Decimal(d) => {
            let text = d.to_sci_string();
            match Decimal::parse_python(text.trim_start_matches('-')) {
                Ok(positive) => Ok(Value::Decimal(positive)),
                Err(_) => err("abs() of this Decimal is not supported"),
            }
        }
        other => err(format!("bad operand type for abs(): '{}'", tn(other))),
    }
}

/// `math.floor`.
fn floor(args: &[Value]) -> ExprResult<Value> {
    floor_or_ceil("floor", args, f64::floor, ToInt::Floor)
}

/// `math.ceil`.
fn ceil(args: &[Value]) -> ExprResult<Value> {
    floor_or_ceil("ceil", args, f64::ceil, ToInt::Ceil)
}

fn floor_or_ceil(
    name: &str,
    args: &[Value],
    on_float: fn(f64) -> f64,
    mode: ToInt,
) -> ExprResult<Value> {
    check_arity(name, args, 1, 1)?;
    match &args[0] {
        Value::Int(i) => Ok(Value::Int(*i)),
        Value::Bool(b) => Ok(Value::Int(i64::from(*b))),
        Value::Float(f) => float_to_int(on_float(*f)),
        Value::Decimal(d) => decimal_to_int(d, mode),
        other => err(format!("must be real number, not {}", tn(other))),
    }
}

/// `round(number, ndigits=None)`.
///
/// Without `ndigits` an int stays an int and a float becomes an int, rounding
/// halves to even. With `ndigits` the result keeps the argument's type; a float
/// is rounded from its exact decimal value (`round(2.675, 2)` is `2.67`).
fn round(args: &[Value]) -> ExprResult<Value> {
    check_arity("round", args, 1, 2)?;
    let ndigits = args.get(1).map(int_arg).transpose()?;
    match (&args[0], ndigits) {
        (Value::Int(i), None) => Ok(Value::Int(*i)),
        (Value::Bool(b), None) => Ok(Value::Int(i64::from(*b))),
        (Value::Int(i), Some(n)) => round_int(*i, n),
        (Value::Bool(b), Some(n)) => round_int(i64::from(*b), n),
        (Value::Float(f), None) => float_to_int(f.round_ties_even()),
        (Value::Float(f), Some(n)) => round_float(*f, n).map(Value::Float),
        (Value::Decimal(d), None) => decimal_to_int(d, ToInt::HalfEven),
        (Value::Decimal(_), Some(_)) => err("round() of a Decimal with ndigits is not supported"),
        (other, _) => err(format!(
            "type {} doesn't define __round__ method",
            tn(other)
        )),
    }
}

/// `int.__round__(ndigits)`: negative `ndigits` rounds to a multiple of a power
/// of ten, halves to even.
fn round_int(i: i64, ndigits: i64) -> ExprResult<Value> {
    if ndigits >= 0 {
        return Ok(Value::Int(i));
    }
    let Some(scale) = u32::try_from(-ndigits)
        .ok()
        .filter(|&k| k <= 38)
        .map(|k| 10_i128.pow(k))
    else {
        return Ok(Value::Int(0));
    };
    let i = i128::from(i);
    let remainder = i.rem_euclid(scale);
    let base = i - remainder;
    let up = 2 * remainder > scale || (2 * remainder == scale && (base / scale) % 2 != 0);
    match i64::try_from(if up { base + scale } else { base }) {
        Ok(rounded) => Ok(Value::Int(rounded)),
        Err(_) => overflow(),
    }
}

/// `float.__round__(ndigits)` (CPython's correctly rounded `double_round`).
fn round_float(x: f64, ndigits: i64) -> ExprResult<f64> {
    // CPython's NDIGITS_MAX / NDIGITS_MIN.
    if !x.is_finite() || ndigits > 323 {
        return Ok(x);
    }
    if ndigits < -308 {
        return Ok(0.0 * x);
    }
    if ndigits >= 0 {
        // Rust's fixed formatting rounds the exact binary value half to even,
        // as dtoa mode 3 does; parsing the digits back is correctly rounded.
        let text = format!("{x:.*}", ndigits as usize);
        return Ok(text.parse().unwrap_or(x));
    }
    // Negative ndigits: round the exact decimal expansion to a multiple of 10^k.
    let k = (-ndigits) as usize;
    let exact = format!("{:.1100}", x.abs());
    let (int_part, frac_part) = exact.split_once('.').unwrap_or((&exact, ""));
    let padded = format!("{int_part:0>k$}");
    let (head, tail) = padded.split_at(padded.len() - k);
    let tail_digits = format!("{tail}{frac_part}");
    let first = tail_digits.as_bytes().first().copied().unwrap_or(b'0');
    let rest_nonzero = tail_digits.bytes().skip(1).any(|b| b != b'0');
    let head_odd = head.bytes().last().is_some_and(|b| (b - b'0') % 2 == 1);
    let up = first > b'5' || (first == b'5' && (rest_nonzero || head_odd));
    let mut digits: Vec<u8> = head.bytes().collect();
    if up {
        let mut carry = true;
        for d in digits.iter_mut().rev() {
            if *d == b'9' {
                *d = b'0';
            } else {
                *d += 1;
                carry = false;
                break;
            }
        }
        if carry {
            digits.insert(0, b'1');
        }
    }
    let magnitude = format!("{}{}", String::from_utf8_lossy(&digits), "0".repeat(k));
    let rounded: f64 = magnitude.parse().unwrap_or(0.0);
    if rounded.is_infinite() {
        return err("rounded value too large to represent");
    }
    Ok(rounded.copysign(x))
}

fn uuid5(args: &[Value]) -> ExprResult<Value> {
    check_arity("_uuid5", args, 2, 2)?;
    // PARITY: `uuid.uuid5` needs `str` for both: a non-string namespace fails in
    // `uuid5(NAMESPACE_URL, namespace)` and a non-string name in `bytes + name`.
    let (Value::Str(namespace), Value::Str(name)) = (&args[0], &args[1]) else {
        return err("uuid5() namespace and name must be str");
    };
    let derived = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, namespace.as_bytes());
    Ok(Value::Str(
        uuid::Uuid::new_v5(&derived, name.as_bytes()).to_string(),
    ))
}

// --- string functions (the unbound `str` methods) ---

fn upper(args: &[Value]) -> ExprResult<Value> {
    check_arity("upper", args, 1, 1)?;
    Ok(Value::Str(pystr::upper(self_str("upper", &args[0])?)))
}

fn lower(args: &[Value]) -> ExprResult<Value> {
    check_arity("lower", args, 1, 1)?;
    Ok(Value::Str(pystr::lower(self_str("lower", &args[0])?)))
}

fn title(args: &[Value]) -> ExprResult<Value> {
    check_arity("title", args, 1, 1)?;
    Ok(Value::Str(pystr::title(self_str("title", &args[0])?)))
}

fn capitalize(args: &[Value]) -> ExprResult<Value> {
    check_arity("capitalize", args, 1, 1)?;
    Ok(Value::Str(pystr::capitalize(self_str(
        "capitalize",
        &args[0],
    )?)))
}

fn strip_with(name: &str, args: &[Value], left: bool, right: bool) -> ExprResult<Value> {
    check_arity(name, args, 1, 2)?;
    let s = self_str(name, &args[0])?;
    let chars = match args.get(1) {
        None => None,
        Some(Value::Str(set)) => Some(set.as_str()),
        Some(_) => return err(format!("{name} arg must be None or str")),
    };
    Ok(Value::Str(pystr::strip(s, chars, left, right).to_string()))
}

fn strip(args: &[Value]) -> ExprResult<Value> {
    strip_with("strip", args, true, true)
}

fn lstrip(args: &[Value]) -> ExprResult<Value> {
    strip_with("lstrip", args, true, false)
}

fn rstrip(args: &[Value]) -> ExprResult<Value> {
    strip_with("rstrip", args, false, true)
}

/// `str.replace(old, new, count=-1)`.
fn replace(args: &[Value]) -> ExprResult<Value> {
    check_arity("replace", args, 3, 4)?;
    let s = self_str("replace", &args[0])?;
    let old = str_arg("replace", 1, &args[1])?;
    let new = str_arg("replace", 2, &args[2])?;
    let count = args.get(3).map(int_arg).transpose()?.unwrap_or(-1);
    Ok(Value::Str(match usize::try_from(count) {
        Ok(n) => s.replacen(old, new, n),
        Err(_) => s.replace(old, new),
    }))
}

/// `str.startswith` / `str.endswith(affix, start, end)`.
fn tail_match(name: &str, args: &[Value], at_start: bool) -> ExprResult<Value> {
    check_arity(name, args, 2, 4)?;
    let s = self_str(name, &args[0])?;
    let affixes: Vec<&str> = match &args[1] {
        Value::Str(a) => vec![a.as_str()],
        // PARITY: Python takes a tuple here; a list is the nearest `Value`.
        Value::List(items) => items
            .iter()
            .map(|item| match item {
                Value::Str(a) => Ok(a.as_str()),
                other => err(format!(
                    "tuple for {name} must only contain str, not {}",
                    tn(other)
                )),
            })
            .collect::<ExprResult<_>>()?,
        other => {
            return err(format!(
                "{name} first arg must be str or a tuple of str, not {}",
                tn(other)
            ));
        }
    };
    let bound = |position: usize| args.get(position).map(int_arg).transpose();
    let (start, end) = (bound(2)?, bound(3)?);
    if start.is_none() && end.is_none() {
        return Ok(Value::Bool(affixes.iter().any(|a| {
            if at_start {
                s.starts_with(a)
            } else {
                s.ends_with(a)
            }
        })));
    }
    // ADJUST_INDICES: slice-style clamping, except that `start` is not clamped
    // to the length (so a start past the end never matches).
    let chars: Vec<char> = s.chars().collect();
    let length = chars.len() as i64;
    let clamp_end = |e: i64| {
        if e > length {
            length
        } else if e < 0 {
            (e + length).max(0)
        } else {
            e
        }
    };
    let end = end.map_or(length, clamp_end);
    let start = start.map_or(0, |b| if b < 0 { (b + length).max(0) } else { b });
    let matched = affixes.iter().any(|affix| {
        let affix: Vec<char> = affix.chars().collect();
        let n = affix.len() as i64;
        if end - n < start {
            return false;
        }
        let from = if at_start { start } else { end - n } as usize;
        chars[from..from + affix.len()] == affix[..]
    });
    Ok(Value::Bool(matched))
}

fn startswith(args: &[Value]) -> ExprResult<Value> {
    tail_match("startswith", args, true)
}

fn endswith(args: &[Value]) -> ExprResult<Value> {
    tail_match("endswith", args, false)
}

/// `_contains(s, sub)`: `sub in s`.
fn contains(args: &[Value]) -> ExprResult<Value> {
    check_arity("_contains", args, 2, 2)?;
    membership(&args[1], &args[0]).map(Value::Bool)
}

/// `str.split(sep=None, maxsplit=-1)`.
fn split(args: &[Value]) -> ExprResult<Value> {
    check_arity("split", args, 1, 3)?;
    let s = self_str("split", &args[0])?;
    let maxsplit = args.get(2).map(int_arg).transpose()?.unwrap_or(-1);
    let parts = match args.get(1) {
        None => pystr::split_whitespace(s, maxsplit),
        Some(Value::Str(sep)) => match pystr::split_sep(s, sep, maxsplit) {
            Some(parts) => parts,
            None => return err("empty separator"),
        },
        Some(other) => {
            return err(format!("must be str or None, not {}", tn(other)));
        }
    };
    Ok(Value::List(parts.into_iter().map(Value::Str).collect()))
}

/// `sep.join(iterable)`.
fn join(args: &[Value]) -> ExprResult<Value> {
    check_arity("join", args, 2, 2)?;
    let sep = self_str("join", &args[0])?;
    let mut out = String::new();
    for (i, item) in py_iter(&args[1])?.iter().enumerate() {
        let Value::Str(s) = item else {
            return err(format!(
                "sequence item {i}: expected str instance, {} found",
                tn(item)
            ));
        };
        if i > 0 {
            out.push_str(sep);
        }
        out.push_str(s);
    }
    Ok(Value::Str(out))
}

/// `_substr(s, start, end=None)`: `s[start:end]`.
fn substr(args: &[Value]) -> ExprResult<Value> {
    check_arity("_substr", args, 2, 3)?;
    let bound = |v: &Value| match v {
        Value::Int(i) => Ok(*i),
        Value::Bool(b) => Ok(i64::from(*b)),
        _ => err("slice indices must be integers or None or have an __index__ method"),
    };
    let start = bound(&args[1])?;
    let end = args.get(2).map(bound).transpose()?;
    // Slice normalisation (step 1): negative counts from the end, then clamp.
    let range = |length: usize| {
        let length = length as i64;
        let norm = |i: i64| (if i < 0 { i + length } else { i }).clamp(0, length) as usize;
        (norm(start), end.map_or(length as usize, norm))
    };
    match &args[0] {
        Value::Str(s) => {
            let (from, to) = range(s.chars().count());
            let byte_at = |n: usize| s.char_indices().nth(n).map_or(s.len(), |(i, _)| i);
            Ok(Value::Str(if from < to {
                s[byte_at(from)..byte_at(to)].to_string()
            } else {
                String::new()
            }))
        }
        Value::List(items) => {
            let (from, to) = range(items.len());
            Ok(Value::List(if from < to {
                items[from..to].to_vec()
            } else {
                Vec::new()
            }))
        }
        Value::Map(_) => err("unhashable type: 'slice'"),
        other => err(format!("'{}' object is not subscriptable", tn(other))),
    }
}

/// `_slugify(s, separator="_")`.
fn slugify_(args: &[Value]) -> ExprResult<Value> {
    check_arity("_slugify", args, 1, 2)?;
    // PARITY: python-slugify only accepts `str` (it decodes anything else as
    // bytes, which raises `TypeError` for ints, floats, lists, ...).
    let Value::Str(s) = &args[0] else {
        return err(format!(
            "decoding to str: need a bytes-like object, {} found",
            tn(&args[0])
        ));
    };
    let separator = match args.get(1) {
        None => "_",
        Some(Value::Str(sep)) => sep.as_str(),
        Some(other) => {
            return err(format!(
                "replace() argument 2 must be str, not {}",
                tn(other)
            ));
        }
    };
    Ok(slugify::slugify(s, separator).map_or(Value::Null, Value::Str))
}

/// The `str` argument of an `inflection` function (`re.sub` rejects others).
fn inflection_arg(arg: &Value) -> ExprResult<&str> {
    match arg {
        Value::Str(s) => Ok(s),
        other => err(format!(
            "expected string or bytes-like object, got '{}'",
            tn(other)
        )),
    }
}

fn to_snake(args: &[Value]) -> ExprResult<Value> {
    check_arity("_to_snake", args, 1, 1)?;
    Ok(Value::Str(pystr::underscore(inflection_arg(&args[0])?)))
}

fn to_pascal(args: &[Value]) -> ExprResult<Value> {
    check_arity("_to_pascal", args, 1, 1)?;
    Ok(Value::Str(pystr::camelize(inflection_arg(&args[0])?)))
}

fn to_camel(args: &[Value]) -> ExprResult<Value> {
    check_arity("_to_camel", args, 1, 1)?;
    // PARITY: `inflection.camelize("", uppercase_first_letter=False)` indexes
    // `string[0]` and raises IndexError for the empty string.
    match pystr::camelize_lower(inflection_arg(&args[0])?) {
        Some(s) => Ok(Value::Str(s)),
        None => err("string index out of range"),
    }
}

// --- type predicates, coalesce, case, is_numeric ---

fn is_str(args: &[Value]) -> ExprResult<Value> {
    check_arity("_is_str", args, 1, 1)?;
    Ok(Value::Bool(matches!(args[0], Value::Str(_))))
}

/// `isinstance(value, int) and not isinstance(value, bool)`.
fn is_int(args: &[Value]) -> ExprResult<Value> {
    check_arity("_is_int", args, 1, 1)?;
    Ok(Value::Bool(matches!(args[0], Value::Int(_))))
}

fn is_float(args: &[Value]) -> ExprResult<Value> {
    check_arity("_is_float", args, 1, 1)?;
    Ok(Value::Bool(matches!(args[0], Value::Float(_))))
}

fn is_bool(args: &[Value]) -> ExprResult<Value> {
    check_arity("_is_bool", args, 1, 1)?;
    Ok(Value::Bool(matches!(args[0], Value::Bool(_))))
}

fn is_list(args: &[Value]) -> ExprResult<Value> {
    check_arity("_is_list", args, 1, 1)?;
    Ok(Value::Bool(matches!(args[0], Value::List(_))))
}

/// `_coalesce(*args)`: the first argument that is not `None`.
fn coalesce(args: &[Value]) -> ExprResult<Value> {
    Ok(args
        .iter()
        .find(|a| !a.is_null())
        .cloned()
        .unwrap_or(Value::Null))
}

/// `_is_numeric(value)`: `_try_numeric(value) is not None`.
///
/// PARITY: a `Decimal` is neither `int | float` nor `str`, so it is not numeric
/// to `_try_numeric` (unlike [`Value::is_numeric`], which the operators use).
fn is_numeric(args: &[Value]) -> ExprResult<Value> {
    check_arity("_is_numeric", args, 1, 1)?;
    Ok(Value::Bool(match &args[0] {
        Value::Int(_) | Value::Float(_) => true,
        Value::Str(s) => parse_py_float(s).is_some(),
        _ => false,
    }))
}

/// `eval_conditional(*conds)`: unpack each `(is_true, val)` pair in turn and
/// return the first `val` whose condition is true, else `None`.
fn case(args: &[Value]) -> ExprResult<Value> {
    for cond in args {
        let pair: Cow<'_, [Value]> = match cond {
            Value::List(_) | Value::Str(_) | Value::Map(_) => py_iter(cond)?,
            other => {
                return err(format!("cannot unpack non-iterable {} object", tn(other)));
            }
        };
        match &*pair {
            [is_true, val] => {
                if is_true.is_truthy() {
                    return Ok(val.clone());
                }
            }
            short if short.len() < 2 => {
                return err(format!(
                    "not enough values to unpack (expected 2, got {})",
                    short.len()
                ));
            }
            _ => return err("too many values to unpack (expected 2)"),
        }
    }
    Ok(Value::Null)
}

/// `list(iterable)` and `tuple(iterable)`.
///
/// PARITY: a tuple is a list here.
fn list(args: &[Value]) -> ExprResult<Value> {
    check_arity("list", args, 0, 1)?;
    match args.first() {
        None => Ok(Value::List(Vec::new())),
        Some(iterable) => Ok(Value::List(py_iter(iterable)?.into_owned())),
    }
}
