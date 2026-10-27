//! Classification of class derivations built only from constants.
//!
//! Native port of the block classification in upstream `transform_spec`
//! (`transformer/engine.py`, commit `1323cc8`): `_reads_no_input`,
//! `_is_constant_slot`, and the `parents` / `blocks` / `tables` bookkeeping.
//!
//! A *constant block* maps one empty row to one object and needs no table, so
//! the streaming driver emits it exactly once instead of once per input row.

use std::collections::BTreeSet;

use crate::datamodel::{ClassDerivation, SlotDerivation, TransformationSpecification};
use crate::error::Result;

use super::inheritance::with_inherited_fields;

/// One class derivation of the spec as `transform_spec` sees it: `is_a` /
/// `mixins` ancestors merged in, plus its constant-block classification.
#[derive(Debug, Clone)]
pub struct Block {
    /// The derivation with its ancestors merged
    /// ([`with_inherited_fields`], upstream `transformer._with_inherited_fields`).
    pub class_derivation: ClassDerivation,
    /// `true` when the block is emitted once and reads no input.
    pub constant: bool,
    /// Whether another block inherits from this one (a template, never emitted
    /// as a constant record).
    pub is_parent: bool,
}

impl Block {
    /// The table a row block reads: `populated_from`, else its own name.
    pub fn table(&self) -> &str {
        self.class_derivation
            .populated_from
            .as_deref()
            .filter(|t| !t.is_empty())
            .unwrap_or(&self.class_derivation.name)
    }
}

/// Classify every class derivation of `spec`, in spec order.
///
/// Mirrors the top of `transform_spec`:
///
/// ```python
/// merged = [transformer._with_inherited_fields(cd) for cd in spec.class_derivations]
/// parents = {name for cd in spec.class_derivations for name in [*cd.mixins, cd.is_a] if name}
/// blocks = [(cd, cd.name not in parents and _reads_no_input(cd)) for cd in merged]
/// ```
///
/// `source_classes` stands in for upstream's `induce_missing_values`, which
/// gives a block named after a source class that class as its `populated_from`
/// (so it reads input); Rust does not rewrite the spec.
///
/// # Errors
/// As [`with_inherited_fields`]: an unknown `is_a` / `mixins` parent.
pub fn classify_blocks(
    spec: &TransformationSpecification,
    source_classes: &BTreeSet<String>,
) -> Result<Vec<Block>> {
    let all = spec.class_derivations.as_deref().unwrap_or_default();
    let parents: BTreeSet<&str> = all
        .iter()
        .flat_map(|cd| cd.mixins.iter().flatten().chain(cd.is_a.iter()))
        .map(String::as_str)
        .filter(|name| !name.is_empty())
        .collect();
    all.iter()
        .map(|cd| {
            let merged = with_inherited_fields(cd, all)?;
            let is_parent = parents.contains(cd.name.as_str());
            let constant = !is_parent && reads_no_input(&merged, source_classes.contains(&cd.name));
            Ok(Block {
                class_derivation: merged,
                constant,
                is_parent,
            })
        })
        .collect()
}

/// Whether a block is built only from constant `value:`s, so it maps to one
/// object and needs no table.
///
/// Expects `is_a` / `mixins` already merged. A slot counts as constant only when
/// it sets `value:` or holds nested class derivations that are constant
/// themselves. Mirrors upstream `_reads_no_input`.
///
/// `named_after_source_class` is upstream's induced `populated_from`: a
/// top-level block named after a source class reads that table. Nested
/// derivations are never induced, so recursion passes `false`.
pub fn reads_no_input(class_deriv: &ClassDerivation, named_after_source_class: bool) -> bool {
    if non_empty(class_deriv.populated_from.as_deref())
        || named_after_source_class
        || class_deriv.joins.as_ref().is_some_and(|j| !j.is_empty())
        || class_deriv
            .copy_directives
            .as_ref()
            .is_some_and(|c| !c.is_empty())
    {
        return false;
    }
    let slots = class_deriv.slot_derivations.iter().flat_map(|m| m.values());
    let mut any = false;
    for sd in slots {
        any = true;
        if !is_constant_slot(sd) {
            return false;
        }
    }
    any
}

/// Whether a slot derivation sets its value without reading the source row.
/// Mirrors upstream `_is_constant_slot`.
///
/// Rust has no `object_derivations` field (flattened into `class_derivations`
/// at spec load), so upstream's `or slot_deriv.object_derivations` clause has no
/// counterpart. `value: null` deserialises to an unset `value`, like Python's
/// `value is None`.
pub fn is_constant_slot(slot_deriv: &SlotDerivation) -> bool {
    if non_empty(slot_deriv.populated_from.as_deref()) || non_empty(slot_deriv.expr.as_deref()) {
        return false;
    }
    if let Some(nested) = slot_deriv
        .class_derivations
        .as_ref()
        .filter(|n| !n.is_empty())
    {
        return slot_deriv.value.is_none() && nested.values().all(|cd| reads_no_input(cd, false));
    }
    slot_deriv.value.is_some()
}

/// Python truthiness of an optional string: unset and `""` are both falsy.
fn non_empty(s: Option<&str>) -> bool {
    s.is_some_and(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn blocks(class_derivations: serde_json::Value, source_classes: &[&str]) -> Vec<Block> {
        let mut obj = json!({ "class_derivations": class_derivations });
        crate::datamodel::normalise_spec_json(&mut obj);
        let spec: TransformationSpecification = serde_json::from_value(obj).unwrap();
        let classes = source_classes.iter().map(|s| (*s).to_owned()).collect();
        classify_blocks(&spec, &classes).unwrap()
    }

    fn constants(
        class_derivations: serde_json::Value,
        source_classes: &[&str],
    ) -> Vec<(String, bool)> {
        blocks(class_derivations, source_classes)
            .into_iter()
            .map(|b| (b.class_derivation.name, b.constant))
            .collect()
    }

    #[test]
    fn value_only_block_is_constant_and_row_block_is_not() {
        let got = constants(
            json!({
                "Study": {"slot_derivations": {"id": {"value": "x"}}},
                "Participant": {
                    "populated_from": "participants",
                    "slot_derivations": {"pid": null},
                },
            }),
            &["participants"],
        );
        assert_eq!(got, [("Study".into(), true), ("Participant".into(), false)]);
    }

    #[test]
    fn anything_that_reads_a_row_is_not_constant() {
        let value_slot = json!({"id": {"value": "x"}});
        for body in [
            json!({"slot_derivations": {"name": null}}),
            json!({"slot_derivations": {"name": {"expr": "name"}}}),
            json!({"slot_derivations": {"name": {"populated_from": "name"}}}),
            json!({"slot_derivations": {"id": {"value": "x"}, "n": {"populated_from": "n"}}}),
            json!({"slot_derivations": value_slot, "copy_directives": {"all": {"element_name": "all", "copy_all": true}}}),
            json!({"slot_derivations": value_slot, "joins": {"t": {"join_on": "k"}}}),
            json!({"populated_from": "tbl", "slot_derivations": value_slot}),
            // no slots at all
            json!({"description": "nothing to emit"}),
            // nested derivation reading a row
            json!({"slot_derivations": {"p": {"class_derivations": {
                "P": {"slot_derivations": {"c": {"expr": "name"}}}}}}}),
        ] {
            assert_eq!(
                constants(json!({"B": body}), &[]),
                [("B".into(), false)],
                "{body}"
            );
        }
    }

    #[test]
    fn nested_constant_derivations_keep_the_block_constant() {
        let spec = json!({"B": {"slot_derivations": {"project": {"class_derivations": {
            "Project": {"slot_derivations": {"code": {"value": "S"}}}}}}}});
        assert_eq!(constants(spec, &[]), [("B".into(), true)]);
    }

    #[test]
    fn a_block_named_after_a_source_class_reads_that_table() {
        let spec = json!({"Study": {"slot_derivations": {"id": {"value": "x"}}}});
        assert_eq!(
            constants(spec.clone(), &["Study"]),
            [("Study".into(), false)]
        );
        assert_eq!(constants(spec, &[]), [("Study".into(), true)]);
    }

    #[test]
    fn a_parent_block_is_a_template_not_a_constant_record() {
        let got = blocks(
            json!({
                "Base": {"slot_derivations": {"tag": {"value": "x"}}},
                "Child": {
                    "is_a": "Base",
                    "populated_from": "tbl",
                    "slot_derivations": {"pid": null},
                },
            }),
            &[],
        );
        assert!(got[0].is_parent && !got[0].constant);
        // The merged child carries the parent's slots.
        let slots = got[1].class_derivation.slot_derivations.as_ref().unwrap();
        assert_eq!(slots.keys().collect::<Vec<_>>(), ["pid", "tag"]);
    }

    #[test]
    fn inherited_row_slot_makes_the_child_a_row_block() {
        let spec = json!({
            "Base": {"slot_derivations": {"name": null}},
            "Study": {"is_a": "Base", "slot_derivations": {"id": {"value": "x"}}},
        });
        assert_eq!(
            constants(spec, &[]),
            [("Base".into(), false), ("Study".into(), false)]
        );
    }
}
