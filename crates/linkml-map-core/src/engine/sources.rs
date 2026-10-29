//! The `sources:` arm of `ObjectTransformer._derive_slot`: a slot read from the one of
//! several candidate source slots a row has a value for.
//!
//! `_derive_slot` reaches the arm only when `value`, `unit_conversion`,
//! `pivot_operation`, `expr` and `populated_from` are all unset (or falsy) and
//! `sources` is non-empty, which is rarer than the field suggests: a slot derivation
//! a class derivation of the specification owns, with none of `populated_from`,
//! `expr`, `value` or `class_derivations`, gets `populated_from = <its name>` from
//! `induce_missing_values` first, and `sources` is then dead. The arm is live for the
//! slot derivations induction does not touch: those inherited through `is_a` /
//! `mixins` (taken from the raw specification) and those of nested
//! `class_derivations`, and for an own one whose `populated_from` or `expr` is the
//! empty string (set, but falsy). See [`crate::normalize::inheritance`] for the
//! engine's stand-in for the induction.

use indexmap::IndexMap;

use super::{
    MergedRow, ObjectTransformer, merged_row::ambiguous_column_error, populated_from::Populated,
};
use crate::{
    datamodel::SlotDerivation,
    error::{Error, Result},
    expr::eval::py_repr,
    pyfmt::py_repr_str,
    value::Value,
};

impl<'s> ObjectTransformer<'s> {
    /// `_resolve_sources`: the value of the one source slot of `slot_deriv.sources`
    /// the row holds a (non-null) value for, with that slot's definition on the source
    /// type; null with no slot when the row holds none.
    ///
    /// # Errors
    /// `ValueError: Multiple sources for <slot>: {...}` when the row holds a value for
    /// more than one; the `No such slot` `ValueError` of the source type's
    /// `induced_slot` when the one it holds is not a slot of the source type.
    ///
    /// PARITY (deliberate divergence): an ambiguous column of a merged row (a name both
    /// the parent and a joined nested table have) is upstream's `_AMBIGUOUS` sentinel, a
    /// value like any other here, so it counts towards "Multiple sources" (and reads
    /// `<AMBIGUOUS>` in the message). Alone, it is not rejected as it is by
    /// `populated_from`: the sentinel becomes the slot's value, which YAML writes as
    /// `!!python/object/apply:linkml_map.transformer.object_transformer._AmbiguousType []`,
    /// JSON refuses (`TypeError`) and CSV / TSV write as `<AMBIGUOUS>`. That is an upstream
    /// bug (suggested upstream issue) with no [`Value`] to carry it; the ambiguous-column
    /// error of `populated_from` is raised instead.
    pub(super) fn resolve_sources(
        &self,
        slot_deriv: &SlotDerivation,
        sources: &[String],
        source_map: &IndexMap<String, Value>,
        source_type: &str,
        merged: Option<&MergedRow<'_>>,
    ) -> Result<Populated<'s>> {
        // `vmap = {s: source_obj.get(s) for s in sources}` without the nulls: a
        // repeated name is one key, in the order of its first occurrence. `None` is the
        // `_AMBIGUOUS` sentinel.
        let mut present: IndexMap<&str, Option<&Value>> = IndexMap::new();
        for name in sources {
            if merged.is_some_and(|m| m.is_ambiguous(name)) {
                present.insert(name, None);
            } else if let Some(v) = source_map.get(name).filter(|v| !v.is_null()) {
                present.insert(name, Some(v));
            }
        }
        let mut found = present.iter();
        let Some((&name, &v)) = found.next() else {
            return Ok((Value::Null, None));
        };
        if found.next().is_some() {
            let vmap: Vec<String> = present
                .iter()
                .map(|(k, v)| {
                    let repr = v.map_or_else(|| "<AMBIGUOUS>".to_owned(), py_repr);
                    format!("{}: {repr}", py_repr_str(k))
                })
                .collect();
            return Err(Error::Python(format!(
                "ValueError: Multiple sources for {}: {{{}}}",
                slot_deriv.name,
                vmap.join(", ")
            )));
        }
        let slot = self.require_source_slot(name, source_type)?;
        match v {
            Some(v) => Ok((v.clone(), slot)),
            None => Err(ambiguous_column_error(name)),
        }
    }
}
