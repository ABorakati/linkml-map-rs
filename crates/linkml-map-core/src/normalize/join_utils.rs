//! Implicit cross-table join-key inference.
//!
//! Native port of Python `linkml_map.utils.join_utils`. Given two source
//! classes, infer the column to join them on (or explain why it cannot be
//! inferred). Synthesis, validation, and runtime diagnostics all call
//! [`resolve_join`] so they agree on both the chosen key and the failure reason.

use std::collections::BTreeSet;

use crate::schema::SchemaProvider;

/// Consistently-named subject-id columns preferred as the join key when present
/// in both tables, before the structural heuristic. Real dbGaP joins are
/// subject-keyed, and the subject id (`dbGaP_Subject_ID`) is named consistently
/// across prepared tables even when other identifier columns differ. Mirrors
/// Python `join_utils.SUBJECT_KEY_CANDIDATES`.
pub const SUBJECT_KEY_CANDIDATES: &[&str] = &["dbGaP_Subject_ID"];

/// True if `col` is an identifier slot in either `class_a` or `class_b`.
///
/// Uses `induced_slot` because an attribute defined within a class carries its
/// `identifier` flag on the class-level definition. Mirrors Python
/// `join_utils._is_identifier_in_either` (which swallows lookup errors).
fn is_identifier_in_either(
    sv: &dyn SchemaProvider,
    col: &str,
    class_a: &str,
    class_b: &str,
) -> bool {
    for cls in [class_a, class_b] {
        if let Ok(slot) = sv.induced_slot(col, cls)
            && slot.identifier
        {
            return true;
        }
    }
    false
}

/// Find column names shared between two source classes.
///
/// Returns an empty set when either class is unknown. Mirrors Python
/// `join_utils.find_common_columns`. The result is a [`BTreeSet`] so ordering
/// (used to build a deterministic ambiguity reason) matches Python's `sorted`.
pub fn find_common_columns(
    sv: &dyn SchemaProvider,
    class_a: &str,
    class_b: &str,
) -> BTreeSet<String> {
    let classes = sv.all_class_names();
    if !classes.iter().any(|c| c == class_a) || !classes.iter().any(|c| c == class_b) {
        return BTreeSet::new();
    }
    let a: BTreeSet<String> = sv
        .induced_slots(class_a)
        .map(|slots| slots.into_iter().map(|s| s.name).collect())
        .unwrap_or_default();
    let b: BTreeSet<String> = sv
        .induced_slots(class_b)
        .map(|slots| slots.into_iter().map(|s| s.name).collect())
        .unwrap_or_default();
    a.intersection(&b).cloned().collect()
}

/// Determine the implicit join key between two source classes.
///
/// Prefers a single non-identifier common column; falls back to a lone
/// identifier column; returns `None` when multiple non-identifier columns are
/// common (ambiguous). Mirrors Python `join_utils.pick_join_key`.
pub fn pick_join_key(sv: &dyn SchemaProvider, class_a: &str, class_b: &str) -> Option<String> {
    let common = find_common_columns(sv, class_a, class_b);
    if common.is_empty() {
        return None;
    }
    let non_id: BTreeSet<String> = common
        .iter()
        .filter(|col| !is_identifier_in_either(sv, col, class_a, class_b))
        .cloned()
        .collect();
    if non_id.len() == 1 {
        return non_id.into_iter().next();
    }
    if non_id.is_empty() && common.len() == 1 {
        return common.into_iter().next();
    }
    // Multiple non-id common columns — can't pick automatically.
    None
}

/// Outcome of resolving an implicit join between two source classes.
///
/// Exactly one of `key`/`reason` is set: a successful resolution carries the
/// join column in `key`; a failure carries a human-readable `reason`. Mirrors
/// Python `join_utils.JoinResolution`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinResolution {
    pub key: Option<String>,
    pub reason: Option<String>,
}

/// Resolve the implicit join key between two source classes, or explain why not.
///
/// Single source of truth for join-key inference. Prefers a consistently-named
/// subject-id column ([`SUBJECT_KEY_CANDIDATES`]) present in both classes, then
/// falls back to [`pick_join_key`]. When no key can be determined, the returned
/// `reason` explains whether the classes share no columns or share too many to
/// disambiguate. Mirrors Python `join_utils.resolve_join`.
pub fn resolve_join(sv: &dyn SchemaProvider, class_a: &str, class_b: &str) -> JoinResolution {
    resolve_join_with(sv, class_a, class_b, SUBJECT_KEY_CANDIDATES)
}

/// [`resolve_join`] with an explicit subject-key candidate list (the default is
/// [`SUBJECT_KEY_CANDIDATES`]). Mirrors the `subject_keys` parameter upstream.
pub fn resolve_join_with(
    sv: &dyn SchemaProvider,
    class_a: &str,
    class_b: &str,
    subject_keys: &[&str],
) -> JoinResolution {
    let common = find_common_columns(sv, class_a, class_b);
    for candidate in subject_keys {
        if common.contains(*candidate) {
            return JoinResolution {
                key: Some((*candidate).to_string()),
                reason: None,
            };
        }
    }
    if let Some(key) = pick_join_key(sv, class_a, class_b) {
        return JoinResolution {
            key: Some(key),
            reason: None,
        };
    }
    let reason = if common.is_empty() {
        format!("no columns are shared between '{class_a}' and '{class_b}'")
    } else {
        // BTreeSet already iterates in sorted order (matches Python `sorted`).
        let candidates = common.iter().cloned().collect::<Vec<_>>().join(", ");
        format!(
            "multiple candidate join columns are shared between '{class_a}' and \
             '{class_b}' ({candidates}); cannot pick automatically"
        )
    };
    JoinResolution {
        key: None,
        reason: Some(reason),
    }
}

/// Infer the join key between two source classes.
///
/// Thin wrapper over [`resolve_join`] for callers that only need the key.
/// Mirrors Python `join_utils.infer_join_key`.
pub fn infer_join_key(sv: &dyn SchemaProvider, class_a: &str, class_b: &str) -> Option<String> {
    resolve_join(sv, class_a, class_b).key
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{ClassDef, InMemorySchema, InMemorySchemaBuilder, RangeKind, SlotDef};

    fn class(name: &str) -> ClassDef {
        ClassDef {
            name: name.into(),
            tree_root: false,
            is_a: None,
            mixins: vec![],
        }
    }

    fn slot(name: &str, identifier: bool) -> SlotDef {
        SlotDef {
            name: name.into(),
            range: RangeKind::Type("string".into()),
            multivalued: false,
            inlined: false,
            inlined_as_list: false,
            required: false,
            identifier,
            key: false,
            unit: None,
            any_of_enums: vec![],
        }
    }

    /// A ⋈ B share `dbGaP_Subject_ID` and `site` (both non-id); pick_join_key is
    /// ambiguous but the subject-key candidate wins in resolve/infer.
    fn subject_key_schema() -> InMemorySchema {
        InMemorySchemaBuilder::new()
            .add_class(class("A"))
            .add_slot("A", slot("a_id", true))
            .add_slot("A", slot("dbGaP_Subject_ID", false))
            .add_slot("A", slot("site", false))
            .add_class(class("B"))
            .add_slot("B", slot("b_id", true))
            .add_slot("B", slot("dbGaP_Subject_ID", false))
            .add_slot("B", slot("site", false))
            .build()
    }

    #[test]
    fn prefers_subject_key_over_other_common_columns() {
        let sv = subject_key_schema();
        // pick_join_key is ambiguous (two non-id common cols) -> None.
        assert_eq!(pick_join_key(&sv, "A", "B"), None);
        // inference prefers the subject key.
        assert_eq!(
            infer_join_key(&sv, "A", "B"),
            Some("dbGaP_Subject_ID".to_string())
        );
    }

    #[test]
    fn falls_back_to_pick_join_key_without_subject_key() {
        let sv = InMemorySchemaBuilder::new()
            .add_class(class("A"))
            .add_slot("A", slot("a_id", true))
            .add_slot("A", slot("subject_id", false))
            .add_class(class("B"))
            .add_slot("B", slot("b_id", true))
            .add_slot("B", slot("subject_id", false))
            .build();
        assert_eq!(infer_join_key(&sv, "A", "B"), Some("subject_id".to_string()));
    }

    #[test]
    fn returns_none_when_no_common_column() {
        let sv = InMemorySchemaBuilder::new()
            .add_class(class("A"))
            .add_slot("A", slot("a_id", true))
            .add_class(class("B"))
            .add_slot("B", slot("b_id", true))
            .build();
        assert_eq!(infer_join_key(&sv, "A", "B"), None);
        let r = resolve_join(&sv, "A", "B");
        assert!(r.key.is_none());
        assert_eq!(
            r.reason.as_deref(),
            Some("no columns are shared between 'A' and 'B'")
        );
    }

    #[test]
    fn ambiguous_reason_lists_sorted_candidates() {
        let sv = subject_key_schema_no_subject();
        let r = resolve_join(&sv, "A", "B");
        assert!(r.key.is_none());
        // Candidates are listed sorted: "site, zone".
        assert_eq!(
            r.reason.as_deref(),
            Some(
                "multiple candidate join columns are shared between 'A' and 'B' \
                 (site, zone); cannot pick automatically"
            )
        );
    }

    /// A ⋈ B share two non-id columns (`site`, `zone`) and no subject key —
    /// genuinely ambiguous.
    fn subject_key_schema_no_subject() -> InMemorySchema {
        InMemorySchemaBuilder::new()
            .add_class(class("A"))
            .add_slot("A", slot("a_id", true))
            .add_slot("A", slot("zone", false))
            .add_slot("A", slot("site", false))
            .add_class(class("B"))
            .add_slot("B", slot("b_id", true))
            .add_slot("B", slot("zone", false))
            .add_slot("B", slot("site", false))
            .build()
    }
}
