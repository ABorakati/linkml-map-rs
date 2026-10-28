//! Rejection of one join name declared two ways.
//!
//! Native port of the two places upstream refuses a spec whose joins contradict
//! each other, both run by `transform_spec` (`transformer/engine.py`) when it
//! reaches a block, not when the spec is loaded or normalised:
//!
//! - `join_engine._collect_joins`, run by `can_use_join_engine` for a block whose
//!   primary table is a CSV, TSV or JSON file of a source class; it compares the
//!   *raw* `(source_key or join_on, lookup_key or join_on)` pairs, which may hold
//!   `None`;
//! - `engine._collect_all_joins`, run by the per-row fallback; it resolves each
//!   pair through `join_utils.join_keys` first, which raises when a side is
//!   unresolved.
//!
//! Both key the joins by the `joins:` mapping key (the *join name*), walk the
//! derivation's own joins and then those of the class derivations nested under
//! its slot derivations, depth first, and raise a `ValueError` when a name is
//! met again with a different `(source_key, lookup_key)` pair (only the engine
//! route adds "Use distinct join names to join a table more than one way."). Anything else
//! about the spec is ignored (`class_named` and `alias` are not compared), and a
//! repeated, identical spec is accepted. The joins seen are those of the block
//! as `transform_spec` holds it: `is_a`/`mixins` ancestors merged in
//! ([`super::inheritance::with_inherited_fields`], where the child's own entry of
//! a name wins, so an inherited join of the same name never conflicts) and the
//! implicit joins [`super::synthesize_implicit_joins`] added.

use indexmap::IndexMap;

use crate::datamodel::{AliasedClass, ClassDerivation};
use crate::error::{Error, Result};
use crate::pyfmt::py_repr_str;

/// Python's `a or b` over optional strings: `a` unless it is unset or empty.
fn or_join_on<'a>(key: Option<&'a str>, join_on: Option<&'a str>) -> Option<&'a str> {
    key.filter(|k| !k.is_empty()).or(join_on)
}

/// `repr(...)` of an optional string key: `None` or a quoted string.
fn py_repr_key(key: Option<&str>) -> String {
    key.map_or_else(|| "None".to_owned(), py_repr_str)
}

/// `repr((source_key, lookup_key))`.
fn py_repr_pair(source_key: Option<&str>, lookup_key: Option<&str>) -> String {
    format!("({}, {})", py_repr_key(source_key), py_repr_key(lookup_key))
}

/// `utils.join_utils.join_keys`: a join spec's `(source_key, lookup_key)`, each
/// side falling back to `join_on` independently.
///
/// # Errors
/// [`Error::JoinSpec`] when either side is unresolved. Upstream names the spec's
/// `alias` here, not the join name.
pub fn join_keys(spec: &AliasedClass) -> Result<(&str, &str)> {
    let source_key = or_join_on(spec.source_key.as_deref(), spec.join_on.as_deref());
    let lookup_key = or_join_on(spec.lookup_key.as_deref(), spec.join_on.as_deref());
    match (source_key, lookup_key) {
        (Some(source), Some(lookup)) if !source.is_empty() && !lookup.is_empty() => {
            Ok((source, lookup))
        }
        _ => Err(Error::JoinSpec(format!(
            "Join {} must specify 'join_on' or both 'source_key' and 'lookup_key'",
            py_repr_str(&spec.alias)
        ))),
    }
}

/// The error both raise sites share, with the keys already rendered. Only
/// `join_engine._collect_joins` adds the advice to use distinct join names.
fn conflict(join_name: &str, existing: &str, new: &str, advice: bool) -> Error {
    let advice = if advice {
        " Use distinct join names to join a table more than one way."
    } else {
        ""
    };
    Error::JoinSpec(format!(
        "Conflicting join specs for {}: {existing} vs {new}.{advice}",
        py_repr_str(join_name)
    ))
}

/// `join_engine._collect_joins`: every join of `class_deriv` and of the class
/// derivations nested under its slot derivations, by join name. A name met again
/// keeps its first spec.
///
/// # Errors
/// [`Error::JoinSpec`] when a name is met again with different raw key pairs.
pub fn collect_joins<'a>(
    class_deriv: &'a ClassDerivation,
    acc: &mut IndexMap<&'a str, &'a AliasedClass>,
) -> Result<()> {
    for (join_name, join) in class_deriv.joins.iter().flatten() {
        if let Some(existing) = acc.get(join_name.as_str()) {
            let existing_keys = raw_keys(existing);
            let new_keys = raw_keys(join);
            if existing_keys != new_keys {
                // PARITY: join_engine.py::_collect_joins formats the raw pairs, so an
                // unresolved side shows as `None` (the per-row path raises in
                // `join_keys` instead).
                return Err(conflict(
                    join_name,
                    &py_repr_pair(existing_keys.0, existing_keys.1),
                    &py_repr_pair(new_keys.0, new_keys.1),
                    true,
                ));
            }
        }
        acc.entry(join_name.as_str()).or_insert(join);
    }
    for slot_deriv in class_deriv
        .slot_derivations
        .iter()
        .flat_map(IndexMap::values)
    {
        for nested in slot_deriv.class_derivations.iter().flatten() {
            collect_joins(nested, acc)?;
        }
    }
    Ok(())
}

/// `(existing.source_key or existing.join_on, existing.lookup_key or existing.join_on)`.
fn raw_keys(join: &AliasedClass) -> (Option<&str>, Option<&str>) {
    (
        or_join_on(join.source_key.as_deref(), join.join_on.as_deref()),
        or_join_on(join.lookup_key.as_deref(), join.join_on.as_deref()),
    )
}

/// `engine._collect_all_joins`: `{join name: (source_key, lookup_key)}` for every
/// join of `class_deriv` and of the class derivations nested under its slot
/// derivations, each pair resolved by [`join_keys`].
///
/// # Errors
/// [`Error::JoinSpec`] on a join with an unresolved key, or a name met again with
/// a different pair.
pub fn collect_all_joins(class_deriv: &ClassDerivation) -> Result<IndexMap<&str, (&str, &str)>> {
    fn collect_from<'a>(
        cd: &'a ClassDerivation,
        result: &mut IndexMap<&'a str, (&'a str, &'a str)>,
    ) -> Result<()> {
        for (join_name, join_spec) in cd.joins.iter().flatten() {
            let keys = join_keys(join_spec)?;
            if let Some(&existing) = result.get(join_name.as_str())
                && existing != keys
            {
                return Err(conflict(
                    join_name,
                    &py_repr_pair(Some(existing.0), Some(existing.1)),
                    &py_repr_pair(Some(keys.0), Some(keys.1)),
                    false,
                ));
            }
            result.insert(join_name, keys);
        }
        for sd in cd.slot_derivations.iter().flat_map(IndexMap::values) {
            for nested in sd.class_derivations.iter().flatten() {
                collect_from(nested, result)?;
            }
        }
        Ok(())
    }

    let mut result = IndexMap::new();
    collect_from(class_deriv, &mut result)?;
    Ok(result)
}

/// What `transform_spec` checks about a block's joins before it runs the block.
///
/// `engine_candidate` is the part of `can_use_join_engine` that precedes its call
/// of `_collect_joins`: the block's primary table is a DuckDB-readable file and a
/// class of the source schema. Then, whether or not the block goes on to use the
/// set-based engine, the per-row path's `_collect_all_joins` runs (for an engine
/// block it cannot raise anything `_collect_joins` did not).
///
/// # Errors
/// [`Error::JoinSpec`], as [`collect_joins`] and [`collect_all_joins`].
pub fn check_block_joins(class_deriv: &ClassDerivation, engine_candidate: bool) -> Result<()> {
    if engine_candidate {
        collect_joins(class_deriv, &mut IndexMap::new())?;
    }
    collect_all_joins(class_deriv).map(drop)
}
