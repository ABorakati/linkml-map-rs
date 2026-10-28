//! Dict, set and comprehension displays, and the `dict()`/`set()` constructors.
//!
//! Upstream gets these from simpleeval's `EvalWithCompoundTypes`
//! (`_eval_dict`, `_eval_set`, `_eval_comprehension`) plus the
//! `_COMPOUND_TYPE_FUNCTIONS` constructors (`list`, `tuple`, `dict`, `set`), and
//! from `asteval` in unrestricted mode.
//!
//! # Representation
//!
//! [`Value`] has no set variant and its maps are keyed by string, so:
//!
//! PARITY: a Python `set` is a [`Value::List`] of its distinct elements in
//! first-insertion order. Python iterates a set in hash order, which for
//! strings changes with `PYTHONHASHSEED` (so upstream's own output is not
//! reproducible there) and for small ints is roughly ascending; insertion order
//! is the deterministic choice. `len`, `in`, `sorted`, `list()` and iteration
//! agree with Python; what a list cannot reproduce is set equality (`{1, 2} ==
//! {2, 1}`), `str()`/`repr()` (`{1, 2}`) and `is_list(set())` (False). An
//! unhashable element (a list or dict) raises `TypeError` as in Python.
//!
//! PARITY: a Python dict may be keyed by any hashable value; here the key is
//! stringified like a subscript is (`{1: 'a'}` has the key `"1"`), which is also
//! what JSON output shows. `1 in {1: 'a'}` is therefore false (a non-string
//! needle never matches a map key) and `{1: 'a', '1': 'b'}` collapses to one
//! entry.
//!
//! # Restricted syntax
//!
//! Upstream's restricted evaluator rejects a set display other than `{x}` and a
//! set comprehension (it then falls back to `asteval` only when
//! `unrestricted_eval` is on, and for a single expression that fallback returns
//! the unassigned `target`, i.e. `None`; see [`super::gate`]). Everything
//! outside a statement block is the restricted evaluator, so those two raise
//! there; inside a statement block (`asteval`) they are always accepted.

use std::borrow::Cow;
use std::collections::HashSet;

use indexmap::IndexMap;

use super::builtins::py_iter;
use super::error::{ExprError, ExprResult};
use super::eval::{Bindings, eval_ast, type_name};
use super::gate::enter_comprehension;
use super::parser::{Ast, CompKind, Comprehension, Target};
use crate::value::Value;

/// A set display `{a, b, ...}`.
pub(super) fn eval_set_display(elts: &[Ast]) -> ExprResult<Value> {
    Err(ExprError::Eval(
        if elts.len() == 1 {
            "The {} must enclose a variable"
        } else {
            "The {} must enclose a single variable"
        }
        .into(),
    ))
}

/// A dict display `{k: v, ...}` (`_eval_dict`).
pub(super) fn eval_dict_display(pairs: &[(Ast, Ast)], vars: &Bindings) -> ExprResult<Value> {
    let mut map = IndexMap::with_capacity(pairs.len());
    for (k, v) in pairs {
        if matches!(k, Ast::DictUnpack) {
            // `{**mapping}`: `result.update(self._eval(value))`.
            match eval_ast(v, vars)? {
                Value::Map(other) => map.extend(other),
                other => {
                    return Err(ExprError::Eval(format!(
                        "'{}' object is not a mapping",
                        type_name(&other)
                    )));
                }
            }
            continue;
        }
        // `result[self._eval(key)] = self._eval(value)`: the value first.
        let value = eval_ast(v, vars)?;
        let key = dict_key(&eval_ast(k, vars)?)?;
        map.insert(key, value);
    }
    Ok(Value::Map(map))
}

/// The distinct elements of `items`, first occurrence kept (`set(items)`).
///
/// # Errors
/// An unhashable element (a list or dict) is a `TypeError`.
pub(super) fn set_from_values(items: Vec<Value>) -> ExprResult<Value> {
    // A set display is almost always a handful of elements: compare keys
    // pairwise and allocate nothing. Hash only when it could get quadratic.
    const LINEAR_LIMIT: usize = 16;
    if items.len() <= LINEAR_LIMIT {
        let mut kept: Vec<Value> = Vec::with_capacity(items.len());
        'next: for item in items {
            let key = set_key(&item)?;
            for existing in &kept {
                if set_key(existing)? == key {
                    continue 'next;
                }
            }
            kept.push(item);
        }
        return Ok(Value::List(kept));
    }
    let mut keep = Vec::with_capacity(items.len());
    {
        let mut seen: HashSet<SetKey<'_>> = HashSet::with_capacity(items.len());
        for item in &items {
            keep.push(seen.insert(set_key(item)?));
        }
    }
    let mut flags = keep.into_iter();
    Ok(Value::List(
        items
            .into_iter()
            .filter(|_| flags.next().unwrap_or(false))
            .collect(),
    ))
}

/// What Python's `hash`/`==` see of a hashable [`Value`]: `1`, `1.0` and `True`
/// are one element.
#[derive(Debug, PartialEq, Eq, Hash)]
enum SetKey<'a> {
    Null,
    Int(i64),
    Float(u64),
    /// An integer beyond `i64`, or an integral float that large.
    Big(crate::pynum::BigInt),
    Complex(u64, u64),
    Str(&'a str),
}

fn set_key(v: &Value) -> ExprResult<SetKey<'_>> {
    fn float(f: f64) -> SetKey<'static> {
        // Integral floats equal the int; `-0.0 == 0.0`.
        if f.fract() == 0.0 && f.abs() < 9.0e18 {
            SetKey::Int(f as i64)
        } else if f.is_finite() && f.fract() == 0.0 {
            SetKey::Big(crate::pynum::float_to_bigint(f))
        } else {
            SetKey::Float(f.to_bits())
        }
    }
    match v {
        Value::Null => Ok(SetKey::Null),
        Value::Bool(b) => Ok(SetKey::Int(i64::from(*b))),
        Value::Int(i) => Ok(SetKey::Int(*i)),
        Value::BigInt(n) => Ok(SetKey::Big((**n).clone())),
        Value::Float(f) => Ok(float(*f)),
        // A complex with no imaginary part is its real part.
        Value::Complex(z) if z.im == 0.0 => Ok(float(z.re)),
        Value::Complex(z) => Ok(SetKey::Complex(z.re.to_bits(), z.im.to_bits())),
        Value::Decimal(d) => Ok(float(d.to_f64())),
        Value::Temporal(_) => Err(ExprError::Eval(format!(
            "unsupported set element of type '{}'",
            type_name(v)
        ))),
        Value::Str(s) => Ok(SetKey::Str(s)),
        Value::List(_) | Value::Map(_) => Err(ExprError::Eval(format!(
            "unhashable type: '{}'",
            type_name(v)
        ))),
    }
}

/// The map key for a dict key `v` (see the module docs).
///
/// # Errors
/// An unhashable key (a list or dict) is a `TypeError`.
pub(super) fn dict_key(v: &Value) -> ExprResult<String> {
    match v {
        Value::Str(s) => Ok(s.clone()),
        Value::List(_) | Value::Map(_) => Err(ExprError::Eval(format!(
            "unhashable type: '{}'",
            type_name(v)
        ))),
        other => Ok(super::eval::py_str(other)),
    }
}

/// `set(iterable=())`.
pub(super) fn set(args: &[Value]) -> ExprResult<Value> {
    match args {
        [] => Ok(Value::List(Vec::new())),
        [iterable] => set_from_values(py_iter(iterable)?.into_owned()),
        _ => Err(ExprError::Eval(format!(
            "set expected at most 1 argument, got {}",
            args.len()
        ))),
    }
}

/// `dict(mapping_or_pairs=(), **kwargs)`: a mapping is copied, an iterable must
/// yield two-element iterables, and keyword arguments are added last.
pub(super) fn dict(args: &[Value], keywords: Vec<(String, Value)>) -> ExprResult<Value> {
    let mut map = IndexMap::new();
    match args {
        [] => {}
        [Value::Map(m)] => map.clone_from(m),
        [iterable] => {
            for (i, item) in py_iter(iterable)?.iter().enumerate() {
                let pair: Cow<'_, [Value]> = match item {
                    Value::List(_) | Value::Str(_) | Value::Map(_) => py_iter(item)?,
                    _ => {
                        return Err(ExprError::Eval(format!(
                            "cannot convert dictionary update sequence element #{i} to a sequence"
                        )));
                    }
                };
                let [k, v] = &*pair else {
                    return Err(ExprError::Eval(format!(
                        "dictionary update sequence element #{i} has length {}; 2 is required",
                        pair.len()
                    )));
                };
                map.insert(dict_key(k)?, v.clone());
            }
        }
        _ => {
            return Err(ExprError::Eval(format!(
                "dict expected at most 1 argument, got {}",
                args.len()
            )));
        }
    }
    for (k, v) in keywords {
        map.insert(k, v);
    }
    Ok(Value::Map(map))
}

// --- comprehensions (simpleeval `_eval_comprehension`) ---

/// Items a comprehension ranges over: lists by element, dicts by key, strings
/// by character; `None` yields nothing.
///
/// PARITY: iterating `None` is a `TypeError`.
fn iteration_items(v: Value) -> ExprResult<Vec<Value>> {
    match v {
        Value::List(items) => Ok(items),
        other => py_iter(&other).map(Cow::into_owned),
    }
}

/// Where a comprehension's elements go.
enum Sink {
    Items(Vec<Value>),
    Pairs(IndexMap<String, Value>),
}

/// A list, set, dict or generator comprehension.
pub(super) fn eval_comprehension(c: &Comprehension, vars: &Bindings) -> ExprResult<Value> {
    if c.kind == CompKind::Set {
        return Err(ExprError::Eval(
            "Sorry, SetComp is not available in this evaluator".into(),
        ));
    }
    // Record the loop variables for the restricted-mode attribute check (see
    // `Ast::Attribute`); the guards unwind when the comprehension ends.
    let _scopes: Vec<_> = c
        .generators
        .iter()
        .flat_map(|g| g.target.names())
        .map(|name| enter_comprehension(name, vars))
        .collect();
    // The loop targets shadow the enclosing bindings in a child scope. The
    // targets bind single elements, so attribute access on one is per-element
    // (a scalar field read on a single Map), not the list auto-distribution
    // used in plain expressions.
    let mut scope = vars.clone();
    let mut sink = match c.kind {
        CompKind::Dict => Sink::Pairs(IndexMap::new()),
        _ => Sink::Items(Vec::new()),
    };
    run_generators(c, 0, &mut scope, &mut sink)?;
    match sink {
        Sink::Pairs(map) => Ok(Value::Map(map)),
        Sink::Items(items) if c.kind == CompKind::Set => set_from_values(items),
        Sink::Items(items) => Ok(Value::List(items)),
    }
}

fn run_generators(
    c: &Comprehension,
    depth: usize,
    scope: &mut Bindings,
    sink: &mut Sink,
) -> ExprResult<()> {
    let generator = &c.generators[depth];
    let iterable = eval_ast(&generator.iter, scope)?;
    'items: for item in iteration_items(iterable)? {
        bind_target(&generator.target, item, scope)?;
        for cond in &generator.ifs {
            if !eval_ast(cond, scope)?.is_truthy() {
                continue 'items;
            }
        }
        if depth + 1 < c.generators.len() {
            run_generators(c, depth + 1, scope, sink)?;
            continue;
        }
        match (&mut *sink, &c.value) {
            (Sink::Pairs(map), Some(value)) => {
                // `to_return[self._eval(node.key)] = self._eval(node.value)`: the
                // value is evaluated first, then the key, then the key is hashed.
                let value = eval_ast(value, scope)?;
                let key = dict_key(&eval_ast(&c.elt, scope)?)?;
                map.insert(key, value);
            }
            (Sink::Items(items), _) => items.push(eval_ast(&c.elt, scope)?),
            (Sink::Pairs(_), None) => unreachable!("a dict comprehension has a value"),
        }
    }
    Ok(())
}

/// Bind the loop variable(s) for one item. A tuple target pairs its names with
/// the item's elements.
///
/// PARITY: simpleeval zips the names with the item, so surplus or missing
/// elements are silently ignored instead of raising Python's "too many/not
/// enough values to unpack" (upstream's `test_comprehension_bound_root_...`
/// relies on `for k, v in codes` binding `k`/`v` to the first two characters of
/// a key).
fn bind_target(target: &Target, item: Value, scope: &mut Bindings) -> ExprResult<()> {
    match target {
        Target::Name(name) => {
            match scope.get_mut(name.as_str()) {
                Some(slot) => *slot = item,
                None => {
                    scope.insert(name.clone(), item);
                }
            }
            Ok(())
        }
        Target::Tuple(names) => {
            for (name, element) in names.iter().zip(py_iter(&item)?.into_owned()) {
                scope.insert(name.clone(), element);
            }
            Ok(())
        }
    }
}
