//! Dict, set and comprehension displays, and the `dict()`/`set()` constructors.
//!
//! Upstream gets these from simpleeval's `EvalWithCompoundTypes`
//! (`_eval_dict`, `_eval_set`, `_eval_comprehension`) plus the
//! `_COMPOUND_TYPE_FUNCTIONS` constructors (`list`, `tuple`, `dict`, `set`), and
//! from `asteval` in unrestricted mode.
//!
//! # Representation
//!
//! A Python `set` is a [`Value::Set`], iterating in CPython's order (see
//! [`crate::pyset`]; sets of strings keep insertion order, which Python itself
//! cannot make reproducible). A dict built here is a [`Value::Dict`]: any
//! hashable key (`1`, `1.0` and `True` are one key, a tuple is a key, a list is
//! `TypeError: unhashable type`), in insertion order, falsy when empty. It
//! becomes a string-keyed [`Value::Map`] when it leaves the evaluator with only
//! string keys (see [`super::gate`]).
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

use super::builtins::py_iter;
use super::error::{ExprError, ExprResult};
use super::eval::{Bindings, eval_ast, expr_truthy};
use super::gate::enter_comprehension;
use super::parser::{Ast, CompKind, Comprehension, Target};
use crate::pyset::PySet;
use crate::value::{PyDict, Value, dict_value, set_value};

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
    let mut dict = PyDict::new();
    for (k, v) in pairs {
        if matches!(k, Ast::DictUnpack) {
            // `{**mapping}`: `result.update(self._eval(value))`.
            dict_update(&mut dict, &eval_ast(v, vars)?)?;
            continue;
        }
        // `result[self._eval(key)] = self._eval(value)`: the value first.
        let value = eval_ast(v, vars)?;
        dict.insert(eval_ast(k, vars)?, value)
            .map_err(ExprError::Eval)?;
    }
    Ok(dict_value(dict))
}

/// `dict.update(other)`: a mapping contributes its items, anything else must
/// iterate over two-element iterables.
fn dict_update(dict: &mut PyDict, other: &Value) -> ExprResult<()> {
    match other {
        Value::Map(m) => {
            for (k, v) in m {
                dict.insert(Value::Str(k.clone()), v.clone())
                    .map_err(ExprError::Eval)?;
            }
            Ok(())
        }
        Value::Dict(d) => {
            for (k, v) in d.iter() {
                dict.insert(k.clone(), v.clone()).map_err(ExprError::Eval)?;
            }
            Ok(())
        }
        iterable => dict_update_from_pairs(dict, iterable),
    }
}

/// `PyDict_MergeFromSeq2`: `dict(iterable_of_pairs)`.
fn dict_update_from_pairs(dict: &mut PyDict, iterable: &Value) -> ExprResult<()> {
    for (i, item) in py_iter(iterable)?.iter().enumerate() {
        let pair: Cow<'_, [Value]> = match item {
            Value::List(_)
            | Value::Tuple(_)
            | Value::Str(_)
            | Value::Map(_)
            | Value::Set(_)
            | Value::Dict(_) => py_iter(item)?,
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
        dict.insert(k.clone(), v.clone()).map_err(ExprError::Eval)?;
    }
    Ok(())
}

/// `set(items)`: the elements added one after another, as a set display and a
/// set comprehension do.
///
/// # Errors
/// An unhashable element (a list or dict) is a `TypeError`.
pub(super) fn set_from_values(items: Vec<Value>) -> ExprResult<Value> {
    PySet::from_items(items)
        .map(set_value)
        .map_err(ExprError::Eval)
}

/// `set(iterable=())`.
pub(super) fn set(args: &[Value]) -> ExprResult<Value> {
    match args {
        [] => Ok(set_value(PySet::new())),
        // A set is copied table to table (`set_merge`).
        [Value::Set(other)] => Ok(set_value(other.copy())),
        // A dict makes room for all its keys up front (`set_update_internal`).
        [Value::Dict(d)] => {
            let mut out = PySet::new();
            out.update_dict_keys(d.keys().cloned().collect())
                .map_err(ExprError::Eval)?;
            Ok(set_value(out))
        }
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
    let mut out = PyDict::new();
    match args {
        [] => {}
        [Value::Map(m)] => out = PyDict::from_map(m),
        [Value::Dict(d)] => out = (**d).clone(),
        [iterable] => dict_update_from_pairs(&mut out, iterable)?,
        _ => {
            return Err(ExprError::Eval(format!(
                "dict expected at most 1 argument, got {}",
                args.len()
            )));
        }
    }
    for (k, v) in keywords {
        out.insert(Value::Str(k), v).map_err(ExprError::Eval)?;
    }
    Ok(dict_value(out))
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
    Pairs(PyDict),
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
        CompKind::Dict => Sink::Pairs(PyDict::new()),
        _ => Sink::Items(Vec::new()),
    };
    run_generators(c, 0, &mut scope, &mut sink)?;
    match sink {
        Sink::Pairs(dict) => Ok(dict_value(dict)),
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
            if !expr_truthy(&eval_ast(cond, scope)?) {
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
                map.insert(eval_ast(&c.elt, scope)?, value)
                    .map_err(ExprError::Eval)?;
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
