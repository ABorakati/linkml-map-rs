//! Spec normalisation: synthesize implicit cross-table joins.
//!
//! Native port of the join-synthesis pass on Python's `Transformer` base class
//! (`transformer/transformer.py`, method `_synthesize_implicit_joins` and its
//! helpers) plus the two utility modules it depends on
//! ([`join_utils`], [`expression_locations`]).
//!
//! # Why this exists
//!
//! Upstream resolves cross-table references at *normalisation* time: the lazily
//! computed `derived_specification` deep-copies the spec, induces ranges, then
//! rewrites every implicit `{Table.col}` reference into an explicit
//! `AliasedClass` join on the enclosing `ClassDerivation` (the only place a
//! `joins:` block can live). The Rust engine resolves joins at *runtime* from
//! `ClassDerivation.joins`, so this pass performs the same rewrite up front:
//! call [`synthesize_implicit_joins`] on a spec (with the source schema) before
//! handing it to the engine, and every implicit join becomes explicit.
//!
//! Two kinds of implicit reference are normalised into explicit joins:
//!
//! - a nested `class_derivation`, or a `slot_derivation` whose `populated_from`
//!   is a dotted `Table.col`, that names a different source table;
//! - a `{Table.col}` reference in *any* expression on a class derivation or its
//!   slot derivations. This was previously invisible to synthesis, which is why
//!   an expr-only implicit join silently resolved to null.
//!
//! Three cases fail loud (a [`crate::error::Error::JoinSynthesis`]) rather than
//! silently resolving to null at runtime:
//!
//! - an expression `{Table.col}` reference to a table that cannot be keyed;
//! - a qualified `{Name.col}` whose root is neither a table, an in-scope source,
//!   a slot on the current source, a declared join alias, nor a function;
//! - a cross-table reference (expression or structural `populated_from`) in a
//!   derivation with no enclosing `class_derivation` to host the join (a
//!   top-level enum / permissible-value / slot derivation).

pub mod expression_locations;
pub mod join_utils;

use std::collections::BTreeSet;

use indexmap::IndexMap;

use crate::datamodel::{
    AliasedClass, ClassDerivation, EnumDerivation, PermissibleValueDerivation, SlotDerivation,
    TransformationSpecification,
};
use crate::error::{Error, Result};
use crate::expr::{ExprError, FUNCTION_NAMES, INJECTED_EVAL_NAMES};
use crate::schema::SchemaProvider;

use expression_locations::{
    extract_braced_reference_roots, extract_table_references, ExpressionBearing,
};
use join_utils::infer_join_key;

/// Add explicit join specs for every implicit cross-table reference in `spec`.
///
/// Mutates `spec` in place, mirroring Python
/// `Transformer._synthesize_implicit_joins`. Idempotent modulo already-declared
/// joins (an existing `joins:` entry for a table is never overwritten).
///
/// # Errors
/// Returns [`Error::JoinSynthesis`] on an un-keyable required reference, an
/// unresolvable qualified root, or a cross-table reference with nowhere to host
/// its join. Callers should not cache a partially-synthesised spec on error.
pub fn synthesize_implicit_joins(
    spec: &mut TransformationSpecification,
    sv: &dyn SchemaProvider,
) -> Result<()> {
    let table_names: BTreeSet<String> = sv.all_class_names().into_iter().collect();
    if let Some(cds) = spec.class_derivations.as_mut() {
        for cd in cds.iter_mut() {
            let parent_source = cd.populated_from.clone().unwrap_or_else(|| cd.name.clone());
            let available: BTreeSet<String> = std::iter::once(parent_source.clone()).collect();
            walk_and_synthesize_joins(cd, &parent_source, sv, &table_names, &available)?;
        }
    }
    // Cross-table refs in derivations with no enclosing class_derivation
    // (top-level enum/permissible-value/slot derivations) have nowhere to host
    // a join — fail fast rather than silently resolve to null.
    reject_unhostable_cross_table_refs(spec, &table_names, sv)?;
    Ok(())
}

/// One deferred mutation of `class_deriv.joins`, collected during an immutable
/// walk and applied afterwards (Rust cannot mutate `joins` while borrowing the
/// slot derivations that imply the mutation, unlike Python's interleaved walk).
/// Ordering within the vec reproduces upstream's per-expression walk order.
enum JoinAction {
    /// Synthesize a join for `table`; `required` un-keyable references fail loud.
    Synthesize { table: String, required: bool },
    /// Fail loud if any of `roots` cannot be resolved against the known set.
    RejectRoots { roots: BTreeSet<String> },
}

/// Recursively synthesize joins for cross-table references under `class_deriv`.
///
/// `parent_source` is this derivation's `populated_from`; `available` is the set
/// of tables already in scope (the ancestor source chain plus this source) — a
/// reference to one of these is the parent/own row, not a new join. Mirrors
/// Python `Transformer._walk_and_synthesize_joins` +
/// `_synthesize_joins_for_expressions`.
fn walk_and_synthesize_joins(
    class_deriv: &mut ClassDerivation,
    parent_source: &str,
    sv: &dyn SchemaProvider,
    table_names: &BTreeSet<String>,
    available: &BTreeSet<String>,
) -> Result<()> {
    // ── Phase 1: collect the join/reject actions during an immutable walk. ────
    let known = known_roots(class_deriv, parent_source, sv, table_names, available);
    let mut actions: Vec<JoinAction> = Vec::new();

    // Expressions on the class derivation itself are hosted on this derivation.
    collect_expression_actions(class_deriv, table_names, available, &mut actions)?;

    if let Some(sds) = class_deriv.slot_derivations.as_ref() {
        for sd in sds.values() {
            collect_expression_actions(sd, table_names, available, &mut actions)?;

            // A dotted `populated_from: Table.col` on a slot derivation names a
            // different table — synthesize its (structural, non-required) join.
            if let Some(pf) = &sd.populated_from
                && let Some((table, _)) = pf.split_once('.')
                && !available.contains(table)
                && table_names.contains(table)
            {
                actions.push(JoinAction::Synthesize {
                    table: table.to_string(),
                    required: false,
                });
            }

            // A nested class_derivation that references a different table.
            for nested_cd in sd.class_derivations.iter().flatten().map(|(_, v)| v) {
                let nested_source = nested_cd
                    .populated_from
                    .clone()
                    .unwrap_or_else(|| parent_source.to_string());
                if nested_source != parent_source {
                    actions.push(JoinAction::Synthesize {
                        table: nested_source,
                        required: false,
                    });
                }
            }
        }
    }

    // ── Phase 2: apply the collected actions (mutating `class_deriv.joins`). ──
    for action in actions {
        match action {
            JoinAction::Synthesize { table, required } => {
                synthesize_join(class_deriv, parent_source, &table, sv, required)?;
            }
            JoinAction::RejectRoots { roots } => {
                reject_unknown_qualified_roots(class_deriv, parent_source, &roots, &known)?;
            }
        }
    }

    // ── Phase 3: recurse into nested class_derivations. ──────────────────────
    if let Some(sds) = class_deriv.slot_derivations.as_mut() {
        for sd in sds.values_mut() {
            for nested_cd in sd.class_derivations.iter_mut().flatten().map(|(_, v)| v) {
                let nested_source = nested_cd
                    .populated_from
                    .clone()
                    .unwrap_or_else(|| parent_source.to_string());
                if nested_cd.slot_derivations.is_some() {
                    let mut child_available = available.clone();
                    child_available.insert(nested_source.clone());
                    walk_and_synthesize_joins(
                        nested_cd,
                        &nested_source,
                        sv,
                        table_names,
                        &child_available,
                    )?;
                }
            }
        }
    }

    Ok(())
}

/// Collect the synthesize-then-reject actions for every expression on one
/// derivation, mirroring Python `_synthesize_joins_for_expressions`: a table
/// reference not already in scope becomes a *required* join; the braced roots
/// are then checked against the known set.
fn collect_expression_actions<D: ExpressionBearing>(
    derivation: &D,
    table_names: &BTreeSet<String>,
    available: &BTreeSet<String>,
    actions: &mut Vec<JoinAction>,
) -> Result<()> {
    for expression in derivation.expressions() {
        for table in extract_table_references(&expression, table_names).map_err(to_err)? {
            if !available.contains(&table) {
                actions.push(JoinAction::Synthesize {
                    table,
                    required: true,
                });
            }
        }
        let roots = extract_braced_reference_roots(&expression).map_err(to_err)?;
        actions.push(JoinAction::RejectRoots { roots });
    }
    Ok(())
}

/// The set of resolvable qualified-reference roots for a class derivation: all
/// source tables, in-scope sources, declared join aliases, slots on the current
/// source, expression functions, and injected eval names. Mirrors the `known`
/// set built in Python `_reject_unknown_qualified_roots`.
fn known_roots(
    class_deriv: &ClassDerivation,
    parent_source: &str,
    sv: &dyn SchemaProvider,
    table_names: &BTreeSet<String>,
    available: &BTreeSet<String>,
) -> BTreeSet<String> {
    let mut known: BTreeSet<String> = table_names.clone();
    known.extend(available.iter().cloned());
    known.extend(class_deriv.joins.iter().flatten().map(|(k, _)| k.clone()));
    known.extend(source_slot_names(sv, parent_source));
    known.extend(FUNCTION_NAMES.iter().map(|s| (*s).to_string()));
    known.extend(INJECTED_EVAL_NAMES.iter().map(|s| (*s).to_string()));
    known
}

/// The induced slot names of `source`, or empty when it is not a source class.
/// Mirrors Python `Transformer._source_slot_names`.
fn source_slot_names(sv: &dyn SchemaProvider, source: &str) -> BTreeSet<String> {
    if !sv.all_class_names().iter().any(|c| c == source) {
        return BTreeSet::new();
    }
    sv.induced_slots(source)
        .map(|slots| slots.into_iter().map(|s| s.name).collect())
        .unwrap_or_default()
}

/// Fail loud on a qualified `{Name.col}` whose root resolves to nothing.
/// Mirrors Python `Transformer._reject_unknown_qualified_roots`.
fn reject_unknown_qualified_roots(
    class_deriv: &ClassDerivation,
    parent_source: &str,
    roots: &BTreeSet<String>,
    known: &BTreeSet<String>,
) -> Result<()> {
    let unknown: BTreeSet<String> = roots.difference(known).cloned().collect();
    if unknown.is_empty() {
        return Ok(());
    }
    Err(Error::JoinSynthesis(format!(
        "Expression reference(s) {} on class_derivation '{}' cannot be resolved: \
         each root must be a source table, a slot on '{}', or a function. \
         Fix the reference or correct the source schema.",
        py_list(&unknown),
        class_deriv.name,
        parent_source,
    )))
}

/// Add an explicit `AliasedClass` join for `table` on `class_deriv`.
///
/// No-op when the join is already declared/synthesized. When no join key can be
/// inferred: a `required` reference (an expression `{table.col}`, which has no
/// runtime null-safety net) fails loud; a non-required structural
/// `populated_from` reference returns quietly (the engine reports an un-keyable
/// structural join at runtime). Mirrors Python `Transformer._synthesize_join`.
fn synthesize_join(
    class_deriv: &mut ClassDerivation,
    parent_source: &str,
    table: &str,
    sv: &dyn SchemaProvider,
    required: bool,
) -> Result<()> {
    if class_deriv
        .joins
        .as_ref()
        .is_some_and(|j| j.contains_key(table))
    {
        return Ok(());
    }
    let Some(join_key) = infer_join_key(sv, parent_source, table) else {
        if required {
            return Err(Error::JoinSynthesis(format!(
                "Cross-table reference to '{table}' from '{parent_source}' on class_derivation \
                 '{}' cannot be joined: no shared join key could be inferred. Declare an \
                 explicit 'joins:' entry with 'join_on' (or 'source_key'/'lookup_key').",
                class_deriv.name,
            )));
        }
        return Ok(());
    };
    class_deriv.joins.get_or_insert_with(IndexMap::new).insert(
        table.to_string(),
        AliasedClass {
            alias: table.to_string(),
            join_on: Some(join_key),
            ..Default::default()
        },
    );
    Ok(())
}

/// Fail fast on a cross-table reference with no class_derivation to host its
/// join (top-level enum / permissible-value / slot derivations). Covers both an
/// expression `{Table.col}` and a structural `populated_from: Table.col`.
/// Mirrors Python `Transformer._reject_unhostable_cross_table_refs`.
fn reject_unhostable_cross_table_refs(
    spec: &TransformationSpecification,
    table_names: &BTreeSet<String>,
    sv: &dyn SchemaProvider,
) -> Result<()> {
    for ed in spec.enum_derivations.iter().flatten().map(|(_, v)| v) {
        check_unhostable("enum_derivation", &ed.name, ed, enum_pf(ed), table_names)?;
        for pv in ed
            .permissible_value_derivations
            .iter()
            .flatten()
            .map(|(_, v)| v)
        {
            check_unhostable(
                "permissible_value_derivation",
                &pv.name,
                pv,
                pv_pf(pv),
                table_names,
            )?;
        }
    }
    for sd in spec.slot_derivations.iter().flatten().map(|(_, v)| v) {
        check_unhostable(
            "top-level slot_derivation",
            &sd.name,
            sd,
            slot_pf(sd),
            table_names,
        )?;
    }
    let _ = sv; // symmetry with upstream; slot-name resolution not needed here.
    Ok(())
}

/// Scalar `populated_from` (enum / slot derivation) as a single-element slice.
fn enum_pf(ed: &EnumDerivation) -> Vec<String> {
    ed.populated_from.clone().into_iter().collect()
}
fn slot_pf(sd: &SlotDerivation) -> Vec<String> {
    sd.populated_from.clone().into_iter().collect()
}
/// List-form `populated_from` on a permissible-value derivation.
fn pv_pf(pv: &PermissibleValueDerivation) -> Vec<String> {
    pv.populated_from.clone().unwrap_or_default()
}

/// Collect the cross-table refs (expressions + dotted `populated_from`) on a
/// derivation with no host and raise if any exist. Mirrors the inner `check`
/// closure of Python `_reject_unhostable_cross_table_refs`.
fn check_unhostable<D: ExpressionBearing>(
    kind: &str,
    name: &str,
    derivation: &D,
    pf_values: Vec<String>,
    table_names: &BTreeSet<String>,
) -> Result<()> {
    let mut refs: BTreeSet<String> = BTreeSet::new();
    for expression in derivation.expressions() {
        refs.extend(extract_table_references(&expression, table_names).map_err(to_err)?);
    }
    for pf in pf_values {
        if let Some((table, _)) = pf.split_once('.')
            && table_names.contains(table)
        {
            refs.insert(table.to_string());
        }
    }
    if refs.is_empty() {
        return Ok(());
    }
    Err(Error::JoinSynthesis(format!(
        "Cross-table reference(s) {} in {kind} '{name}' cannot be joined: \
         only class_derivations can host joins. Move the derivation under a \
         class_derivation, or reference only same-row columns.",
        py_list(&refs),
    )))
}

/// Format a sorted set as a Python list repr (`['a', 'b']`) so diagnostics match
/// upstream's `sorted(...)` rendering.
fn py_list(items: &BTreeSet<String>) -> String {
    let inner = items
        .iter()
        .map(|s| format!("'{s}'"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{inner}]")
}

/// Map an expression parse error encountered during scanning to a synthesis
/// error (a malformed expression would also fail at evaluation).
fn to_err(e: ExprError) -> Error {
    Error::JoinSynthesis(format!(
        "could not parse expression during join synthesis: {e}"
    ))
}

#[cfg(test)]
mod tests;
