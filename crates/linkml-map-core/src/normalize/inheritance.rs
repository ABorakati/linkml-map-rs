//! `is_a` / `mixins` inheritance between class derivations.
//!
//! Native port of `Transformer._class_derivation_ancestors`,
//! `Transformer._find_class_derivation_by_name` and
//! `Transformer._with_inherited_fields` (`transformer/transformer.py`).
//!
//! Upstream resolves ancestors by *name* against the original (un-normalised)
//! specification and merges **every** field of each ancestor into a copy of the
//! derivation: collections are merged (the child wins on a key clash), lists are
//! extended and unset scalars are copied. The merge is used by
//! `Transformer._get_class_derivation` (per object) and, since upstream
//! `cd395ff`, once per block by `transform_spec` (streaming tabular input).

use indexmap::IndexMap;

use crate::datamodel::ClassDerivation;
use crate::error::{Error, Result};

/// Find a class derivation by name; the first match wins.
///
/// Mirrors `Transformer._find_class_derivation_by_name`, which raises
/// `KeyError("No class derivation named '<name>'")` for an unknown name.
///
/// # Errors
/// [`Error::NoClassDerivation`] when no derivation has that name.
pub fn find_class_derivation_by_name<'a>(
    all: &'a [ClassDerivation],
    name: &str,
) -> Result<&'a ClassDerivation> {
    all.iter()
        .find(|cd| cd.name == name)
        .ok_or_else(|| Error::NoClassDerivation {
            source_type: name.to_string(),
            count: 0,
        })
}

/// All `is_a` / `mixins` ancestors of `cd`, keyed by name in upstream's
/// insertion order: each parent (mixins first, then `is_a`) followed by that
/// parent's own ancestors.
///
/// Mirrors `Transformer._class_derivation_ancestors`. Ancestors are the raw
/// entries of `all`, not merged copies.
///
/// # Errors
/// [`Error::NoClassDerivation`] for an unknown parent name;
/// [`Error::CyclicInheritance`] if the chain loops (upstream would hit a
/// `RecursionError`).
pub fn class_derivation_ancestors<'a>(
    cd: &ClassDerivation,
    all: &'a [ClassDerivation],
) -> Result<IndexMap<String, &'a ClassDerivation>> {
    fn walk<'a>(
        cd: &ClassDerivation,
        all: &'a [ClassDerivation],
        stack: &mut Vec<String>,
        out: &mut IndexMap<String, &'a ClassDerivation>,
    ) -> Result<()> {
        if stack.contains(&cd.name) {
            return Err(Error::CyclicInheritance(cd.name.clone()));
        }
        stack.push(cd.name.clone());
        let parents = cd
            .mixins
            .iter()
            .flatten()
            .chain(cd.is_a.iter())
            .cloned()
            .collect::<Vec<_>>();
        for parent in parents {
            let parent_cd = find_class_derivation_by_name(all, &parent)?;
            out.insert(parent, parent_cd);
            walk(parent_cd, all, stack, out)?;
        }
        stack.pop();
        Ok(())
    }

    let mut out = IndexMap::new();
    walk(cd, all, &mut Vec::new(), &mut out)?;
    Ok(out)
}

/// Return `cd` with the fields it leaves unset filled in from its
/// `is_a` / `mixins` ancestors.
///
/// Mirrors `Transformer._with_inherited_fields`: for each ancestor (in
/// [`class_derivation_ancestors`] order) and each field the ancestor sets,
/// a list field is extended, a mapping field is merged with the descendant's
/// entries winning (and earlier ancestors winning over later ones), and a scalar
/// the descendant leaves unset is copied. Returns a plain clone when `cd` has
/// no ancestors.
///
/// Not idempotent for list fields (`mixins`, `implements`, `comments`), exactly
/// like upstream, so apply it once per derivation, to the raw entry of `all`.
///
/// # Errors
/// As [`class_derivation_ancestors`].
pub fn with_inherited_fields(
    cd: &ClassDerivation,
    all: &[ClassDerivation],
) -> Result<ClassDerivation> {
    let mut merged = cd.clone();
    induce_populated_from(&mut merged);
    for ancestor in class_derivation_ancestors(cd, all)?.values() {
        // PARITY: transformer.py::_with_inherited_fields iterates
        // `ancestor.__dict__`, so every datamodel field is merged, scalars
        // included (`is_a`, `mixins`-supplied `is_a`, `description`, ...).
        merge_map(&mut merged.joins, &ancestor.joins);
        merge_map(&mut merged.slot_derivations, &ancestor.slot_derivations);
        merge_map(&mut merged.copy_directives, &ancestor.copy_directives);
        merge_map(&mut merged.value_mappings, &ancestor.value_mappings);
        merge_map(
            &mut merged.expression_to_value_mappings,
            &ancestor.expression_to_value_mappings,
        );
        merge_map(
            &mut merged.expression_to_expression_mappings,
            &ancestor.expression_to_expression_mappings,
        );
        extend_list(&mut merged.mixins, &ancestor.mixins);
        extend_list(&mut merged.implements, &ancestor.implements);
        extend_list(&mut merged.comments, &ancestor.comments);
        merge_json(&mut merged.overrides, &ancestor.overrides);
        fill_unset(&mut merged.populated_from, &ancestor.populated_from);
        fill_unset(&mut merged.target_definition, &ancestor.target_definition);
        fill_unset(&mut merged.pivot_operation, &ancestor.pivot_operation);
        fill_unset(&mut merged.is_a, &ancestor.is_a);
        fill_unset(&mut merged.mirror_source, &ancestor.mirror_source);
        fill_unset(&mut merged.description, &ancestor.description);
        fill_unset(&mut merged.mint_ids, &ancestor.mint_ids);
    }
    Ok(merged)
}

/// The part of upstream `induce_missing_values` that rewrites slot derivations: a slot
/// derivation of `cd` itself with none of `populated_from`, `expr`, `value` or (non-empty)
/// `class_derivations` gets `populated_from = <its name>` (the "null mapping copies the
/// same-named source slot" default).
///
/// Everything that reads `populated_from` then sees what it sees upstream: `value_mappings`,
/// `missing_values`, `offset` and unit conversion apply to the copied value, and `sources`
/// is dead (`_derive_slot` tries `populated_from` first). Only `cd`'s own slot derivations are
/// induced: those inherited through `is_a` / `mixins` come from the raw specification, and
/// nested `class_derivations` are never visited, so both stay bare and keep their `sources`.
/// Induction also infers `range` from the source slot's class range; that step is not ported.
///
/// A slot named `_source_class` is left bare: `populated_from: _source_class` is the engine's own
/// virtual slot (#193), which upstream does not have, so upstream reads a source slot of that name.
///
/// "Unset" is `None`, not falsy, as in upstream: `populated_from: ""` or `expr: ""` is set,
/// so it is not induced, and (being falsy) it matches no arm at run time.
fn induce_populated_from(cd: &mut ClassDerivation) {
    for sd in cd.slot_derivations.iter_mut().flat_map(|m| m.values_mut()) {
        if sd.populated_from.is_none()
            && sd.name != "_source_class"
            && sd.expr.is_none()
            && sd.value.is_none()
            && sd.class_derivations.as_ref().is_none_or(Vec::is_empty)
        {
            sd.populated_from = Some(sd.name.clone());
        }
    }
}

/// Upstream `curr_v.update({**v, **curr_v})`: the descendant's entries win and
/// keep their position; entries only the ancestor has are appended.
fn merge_map<V: Clone>(dst: &mut Option<IndexMap<String, V>>, src: &Option<IndexMap<String, V>>) {
    let Some(src) = src.as_ref().filter(|m| !m.is_empty()) else {
        return;
    };
    let dst = dst.get_or_insert_with(IndexMap::new);
    for (k, v) in src {
        dst.entry(k.clone()).or_insert_with(|| v.clone());
    }
}

/// Upstream `curr_v.extend(v)`: duplicates are kept.
fn extend_list(dst: &mut Option<Vec<String>>, src: &Option<Vec<String>>) {
    let Some(src) = src.as_ref().filter(|v| !v.is_empty()) else {
        return;
    };
    dst.get_or_insert_with(Vec::new).extend(src.iter().cloned());
}

/// Upstream `elif curr_v is None: setattr(cd, k, v)`.
fn fill_unset<T: Clone>(dst: &mut Option<T>, src: &Option<T>) {
    if dst.is_none() {
        dst.clone_from(src);
    }
}

/// `overrides` is an untyped `Any` upstream, so the list/dict/unset branches of
/// the merge all apply to its YAML value.
fn merge_json(dst: &mut Option<serde_json::Value>, src: &Option<serde_json::Value>) {
    use serde_json::Value as J;
    let Some(src) = src else { return };
    match (dst.as_mut(), src) {
        (None, _) => *dst = Some(src.clone()),
        (Some(J::Array(d)), J::Array(s)) => d.extend(s.iter().cloned()),
        (Some(J::Object(d)), J::Object(s)) => {
            for (k, v) in s {
                d.entry(k.clone()).or_insert_with(|| v.clone());
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datamodel::{AliasedClass, TransformationSpecification};
    use serde_json::json;

    fn spec(class_derivations: serde_json::Value) -> Vec<ClassDerivation> {
        let mut obj = json!({ "class_derivations": class_derivations });
        crate::datamodel::normalise_spec_json(&mut obj);
        serde_json::from_value::<TransformationSpecification>(obj)
            .unwrap()
            .class_derivations
            .unwrap()
    }

    fn slot_names(cd: &ClassDerivation) -> Vec<&str> {
        cd.slot_derivations
            .iter()
            .flatten()
            .map(|(k, _)| k.as_str())
            .collect()
    }

    #[test]
    fn ancestors_are_parent_then_its_ancestors_mixins_before_is_a() {
        let all = spec(json!({
            "G": {},
            "P1": {"is_a": "G"},
            "M": {},
            "C": {"is_a": "P1", "mixins": ["M"]},
        }));
        let names: Vec<_> = class_derivation_ancestors(&all[3], &all)
            .unwrap()
            .keys()
            .cloned()
            .collect();
        assert_eq!(names, ["M", "P1", "G"]);
    }

    #[test]
    fn child_wins_and_earlier_ancestor_wins_over_later() {
        let all = spec(json!({
            "Base": {"slot_derivations": {
                "a": {"value": "base"}, "b": {"value": "base"}, "c": {"value": "base"}}},
            "Mid": {"is_a": "Base", "slot_derivations": {
                "a": {"value": "mid"}, "b": {"value": "mid"}}},
            "Leaf": {"is_a": "Mid", "slot_derivations": {"a": {"value": "leaf"}}},
        }));
        let leaf = with_inherited_fields(&all[2], &all).unwrap();
        let val = |n: &str| leaf.slot_derivations.as_ref().unwrap()[n].value.clone();
        assert_eq!(val("a"), Some(json!("leaf")));
        assert_eq!(val("b"), Some(json!("mid")));
        assert_eq!(val("c"), Some(json!("base")));
        // The descendant's own slots keep their position; inherited ones follow.
        assert_eq!(slot_names(&leaf), ["a", "b", "c"]);
    }

    #[test]
    fn joins_and_unset_scalars_are_inherited() {
        let all = spec(json!({
            "Base": {
                "populated_from": "tbl",
                "description": "from base",
                "joins": {"sites": {"source_key": "site", "lookup_key": "code"}},
            },
            "Child": {"is_a": "Base", "description": "own"},
        }));
        let child = with_inherited_fields(&all[1], &all).unwrap();
        assert_eq!(child.populated_from.as_deref(), Some("tbl"));
        assert_eq!(child.description.as_deref(), Some("own"));
        assert_eq!(
            child.joins.unwrap()["sites"],
            AliasedClass {
                alias: "sites".into(),
                source_key: Some("site".into()),
                lookup_key: Some("code".into()),
                ..Default::default()
            }
        );
    }

    #[test]
    fn no_ancestors_is_a_plain_copy() {
        let all = spec(json!({"Solo": {"slot_derivations": {"x": {"value": 1}}}}));
        assert_eq!(with_inherited_fields(&all[0], &all).unwrap(), all[0]);
    }

    #[test]
    fn unknown_parent_and_cycles_are_errors() {
        let all = spec(json!({"C": {"is_a": "Nope"}}));
        assert!(matches!(
            with_inherited_fields(&all[0], &all),
            Err(Error::NoClassDerivation { ref source_type, .. }) if source_type == "Nope"
        ));
        let all = spec(json!({"A": {"is_a": "B"}, "B": {"is_a": "A"}}));
        assert!(matches!(
            with_inherited_fields(&all[0], &all),
            Err(Error::CyclicInheritance(_))
        ));
    }
}
