//! Tests for semantic spec validation, ported from a representative subset of
//! upstream `tests/test_validator.py` (commit `5a42c2af67`) — the
//! `validate_spec_semantics` and expression-reference-extraction cases.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::json;

use super::expr_refs::{extract_expr_attribute_references, extract_expr_slot_references};
use super::*;
use crate::datamodel::{TransformationSpecification, normalise_spec_json};
use crate::schema::{ClassDef, EnumDef, InMemorySchema, InMemorySchemaBuilder, RangeKind, SlotDef};

// ── fixtures ────────────────────────────────────────────────────────────────

fn class(name: &str) -> ClassDef {
    ClassDef {
        name: name.into(),
        tree_root: false,
        is_a: None,
        mixins: vec![],
    }
}

fn slot(name: &str, identifier: bool, required: bool) -> SlotDef {
    SlotDef {
        name: name.into(),
        range: RangeKind::Type("string".into()),
        multivalued: false,
        inlined: false,
        inlined_as_list: false,
        required,
        identifier,
        key: false,
        unit: None,
        any_of_enums: vec![],
    }
}

/// `Person { id(id), name, age_in_years, primary_email }`. `name_required`
/// toggles whether `name` is a required slot (for the required-slot test).
fn person_schema(name_required: bool) -> InMemorySchema {
    InMemorySchemaBuilder::new()
        .add_class(class("Person"))
        .add_slot("Person", slot("id", true, false))
        .add_slot("Person", slot("name", false, name_required))
        .add_slot("Person", slot("age_in_years", false, false))
        .add_slot("Person", slot("primary_email", false, false))
        .build()
}

/// Build a spec from a dict-keyed JSON value, running the same normalization
/// (name injection, mapping→list) the real loaders use.
fn spec(mut value: serde_json::Value) -> TransformationSpecification {
    normalise_spec_json(&mut value);
    serde_json::from_value(value).expect("spec should deserialize")
}

fn errors(msgs: &[ValidationMessage]) -> Vec<&ValidationMessage> {
    msgs.iter()
        .filter(|m| m.severity == Severity::Error)
        .collect()
}

// ── validate_spec_semantics: class / slot resolution ─────────────────────────

#[test]
fn valid_spec_produces_no_messages() {
    let sv = person_schema(false);
    let s = spec(json!({
        "class_derivations": {
            "Person": {
                "populated_from": "Person",
                // An underived identifier is an error unless minted (upstream 76db207),
                // so these specs derive `id` explicitly.
                "slot_derivations": {"id": None::<()>, "primary_email": None::<()>},
            }
        }
    }));
    let msgs = validate_spec_semantics(&s, Some(&sv), Some(&sv), false);
    assert!(msgs.is_empty(), "expected no messages, got {msgs:?}");
}

#[test]
fn missing_target_class_is_error() {
    let sv = person_schema(false);
    let s = spec(json!({
        "class_derivations": {"PersonTypo": {"populated_from": "Person"}}
    }));
    let msgs = validate_spec_semantics(&s, None, Some(&sv), false);
    let errs = errors(&msgs);
    assert_eq!(errs.len(), 1);
    assert!(errs[0].message.contains("PersonTypo"));
    assert!(errs[0].message.contains("not found in target schema"));
}

#[test]
fn missing_source_class_is_error() {
    let sv = person_schema(false);
    let s = spec(json!({
        "class_derivations": {"Person": {"populated_from": "NonExistent"}}
    }));
    let msgs = validate_spec_semantics(&s, Some(&sv), None, false);
    let errs = errors(&msgs);
    assert_eq!(errs.len(), 1);
    assert!(errs[0].message.contains("NonExistent"));
    assert!(errs[0].message.contains("not found in source schema"));
}

#[test]
fn missing_target_slot_is_error() {
    let sv = person_schema(false);
    let s = spec(json!({
        "class_derivations": {
            "Person": {"populated_from": "Person", "slot_derivations": {"naem": None::<()>}}
        }
    }));
    let msgs = validate_spec_semantics(&s, None, Some(&sv), false);
    assert!(errors(&msgs).iter().any(|e| e.message.contains("naem")));
}

#[test]
fn missing_source_slot_populated_from_is_error() {
    let sv = person_schema(false);
    let s = spec(json!({
        "class_derivations": {
            "Person": {
                "populated_from": "Person",
                "slot_derivations": {"primary_email": {"populated_from": "nonexistent_slot"}},
            }
        }
    }));
    let msgs = validate_spec_semantics(&s, Some(&sv), Some(&sv), false);
    let errs = errors(&msgs);
    assert!(errs.iter().any(|e| e.message.contains("nonexistent_slot")
        && e.message.contains("not found on source class")));
}

// ── expression slot references ───────────────────────────────────────────────

#[test]
fn unresolved_expr_ref_is_warning_non_strict() {
    let sv = person_schema(false);
    let s = spec(json!({
        "class_derivations": {
            "Person": {
                "populated_from": "Person",
                // An underived identifier is an error unless minted (upstream 76db207),
                // so these specs derive `id` explicitly.
                "slot_derivations": {"id": None::<()>, "name": {"expr": "str({nonexistent}) + '!'"}},
            }
        }
    }));
    let msgs = validate_spec_semantics(&s, Some(&sv), Some(&sv), false);
    assert_eq!(msgs.len(), 1, "got {msgs:?}");
    assert_eq!(msgs[0].severity, Severity::Warning);
    assert!(msgs[0].message.contains("nonexistent"));
    assert!(msgs[0].message.contains("not a slot on the source class"));
}

#[test]
fn unresolved_expr_ref_is_error_strict() {
    let sv = person_schema(false);
    let s = spec(json!({
        "class_derivations": {
            "Person": {
                "populated_from": "Person",
                // An underived identifier is an error unless minted (upstream 76db207),
                // so these specs derive `id` explicitly.
                "slot_derivations": {"id": None::<()>, "name": {"expr": "str({nonexistent}) + '!'"}},
            }
        }
    }));
    let msgs = validate_spec_semantics(&s, Some(&sv), Some(&sv), true);
    assert_eq!(msgs.len(), 1, "got {msgs:?}");
    assert_eq!(msgs[0].severity, Severity::Error);
    assert!(msgs[0].message.contains("nonexistent"));
}

#[test]
fn valid_expr_ref_produces_no_messages() {
    let sv = person_schema(false);
    let s = spec(json!({
        "class_derivations": {
            "Person": {
                "populated_from": "Person",
                // An underived identifier is an error unless minted (upstream 76db207),
                // so these specs derive `id` explicitly.
                "slot_derivations": {"id": None::<()>, "primary_email": {"expr": "str({age_in_years}) + ' years'"}},
            }
        }
    }));
    let msgs = validate_spec_semantics(&s, Some(&sv), Some(&sv), false);
    assert!(msgs.is_empty(), "expected none, got {msgs:?}");
}

#[test]
fn join_alias_in_expr_is_not_flagged() {
    // Person ⋈ Address on the shared non-id `person_id`; {Address.city} is a
    // valid cross-table reference and the bare alias `Address` is excluded.
    let sv = InMemorySchemaBuilder::new()
        .add_class(class("Person"))
        .add_slot("Person", slot("id", true, false))
        .add_slot("Person", slot("person_id", false, false))
        .add_slot("Person", slot("primary_email", false, false))
        .add_class(class("Address"))
        .add_slot("Address", slot("id", true, false))
        .add_slot("Address", slot("person_id", false, false))
        .add_slot("Address", slot("city", false, false))
        .build();
    let s = spec(json!({
        "class_derivations": {
            "Person": {
                "populated_from": "Person",
                "joins": {"Address": {"alias": "Address", "join_on": "person_id"}},
                // An underived identifier is an error unless minted (upstream 76db207),
                // so these specs derive `id` explicitly.
                "slot_derivations": {"id": None::<()>, "primary_email": {"expr": "{Address.city}"}},
            }
        }
    }));
    let msgs = validate_spec_semantics(&s, Some(&sv), Some(&sv), false);
    assert!(msgs.is_empty(), "expected none, got {msgs:?}");
}

// ── required slot warning ────────────────────────────────────────────────────

#[test]
fn required_target_slot_without_derivation_warns() {
    let sv = person_schema(true); // `name` is required
    let s = spec(json!({
        "class_derivations": {
            "Person": {"populated_from": "Person", "slot_derivations": {"primary_email": None::<()>}}
        }
    }));
    let msgs = validate_spec_semantics(&s, None, Some(&sv), false);
    assert!(
        msgs.iter().any(|m| m.severity == Severity::Warning
            && m.message.contains("Required target slot 'name'"))
    );
}

// ── is_a / mixins resolution ─────────────────────────────────────────────────

#[test]
fn is_a_resolves_via_spec_internal_pool() {
    let sv = person_schema(false);
    // `Base` is another top-level class_derivation (also a target class here);
    // Person is_a Base must resolve against the spec pool with no error.
    let s = spec(json!({
        "class_derivations": {
            "Person": {"populated_from": "Person", "is_a": "Base"},
            "Base": {"populated_from": "Person"},
        }
    }));
    let msgs = validate_spec_semantics(&s, None, Some(&sv), false);
    // No inheritance error for the is_a ref (Base is in the pool).
    assert!(!msgs.iter().any(|m| m.message.contains("does not resolve")));
}

#[test]
fn is_a_resolves_via_target_schema() {
    let sv = InMemorySchemaBuilder::new()
        .add_class(class("Person"))
        .add_slot("Person", slot("id", true, false))
        .add_class(class("NamedThing"))
        .add_slot("NamedThing", slot("id", true, false))
        .build();
    let s = spec(json!({
        "class_derivations": {"Person": {"populated_from": "Person", "is_a": "NamedThing"}}
    }));
    let msgs = validate_spec_semantics(&s, None, Some(&sv), false);
    assert!(!msgs.iter().any(|m| m.message.contains("does not resolve")));
}

#[test]
fn is_a_resolving_to_neither_is_error() {
    let sv = person_schema(false);
    let s = spec(json!({
        "class_derivations": {"Person": {"populated_from": "Person", "is_a": "Ghost"}}
    }));
    let msgs = validate_spec_semantics(&s, None, Some(&sv), false);
    assert!(
        errors(&msgs)
            .iter()
            .any(|e| e.message.contains("is_a: Ghost") && e.message.contains("does not resolve"))
    );
}

#[test]
fn is_a_unresolved_skipped_without_target_schema() {
    let sv = person_schema(false);
    let s = spec(json!({
        "class_derivations": {"Person": {"populated_from": "Person", "is_a": "Ghost"}}
    }));
    // Source only: inheritance refs are ambiguous, so no error is emitted.
    let msgs = validate_spec_semantics(&s, Some(&sv), None, false);
    assert!(!msgs.iter().any(|m| m.message.contains("does not resolve")));
}

// ── cross-table joins (nested class derivations) ─────────────────────────────

/// `Measurement{id, subject_id, method}` ⋈ `Reading{id, subject_id, score}` on
/// the shared non-id `subject_id` (pick_join_key resolvable).
fn joinable_schema() -> InMemorySchema {
    InMemorySchemaBuilder::new()
        .add_class(class("Measurement"))
        .add_slot("Measurement", slot("id", true, false))
        .add_slot("Measurement", slot("subject_id", false, false))
        .add_slot("Measurement", slot("method", false, false))
        .add_class(class("Reading"))
        .add_slot("Reading", slot("id", true, false))
        .add_slot("Reading", slot("subject_id", false, false))
        .add_slot("Reading", slot("score", false, false))
        .build()
}

fn nested_cross_table_spec() -> TransformationSpecification {
    spec(json!({
        "class_derivations": {
            "Measurement": {
                "populated_from": "Measurement",
                "slot_derivations": {
                    "reading": {"class_derivations": {"Reading": {"populated_from": "Reading"}}}
                },
            }
        }
    }))
}

#[test]
fn cross_table_implicit_resolvable_emits_info() {
    let sv = joinable_schema();
    let s = nested_cross_table_spec();
    let msgs = validate_spec_semantics(&s, Some(&sv), None, false);
    let info: Vec<_> = msgs
        .iter()
        .filter(|m| m.severity == Severity::Info)
        .collect();
    assert_eq!(info.len(), 1, "got {msgs:?}");
    assert!(
        info[0]
            .message
            .contains("implicit join will be synthesized on column 'subject_id'")
    );
    assert!(
        info[0]
            .path
            .contains("slot_derivations[reading].class_derivations[Reading]")
    );
}

#[test]
fn cross_table_no_shared_column_emits_warning() {
    // A{a_id, foo} and B{b_id, bar} share no columns.
    let sv = InMemorySchemaBuilder::new()
        .add_class(class("A"))
        .add_slot("A", slot("a_id", true, false))
        .add_slot("A", slot("foo", false, false))
        .add_class(class("B"))
        .add_slot("B", slot("b_id", true, false))
        .add_slot("B", slot("bar", false, false))
        .build();
    let s = spec(json!({
        "class_derivations": {
            "A": {
                "populated_from": "A",
                "slot_derivations": {
                    "b": {"class_derivations": {"B": {"populated_from": "B"}}}
                },
            }
        }
    }));
    let msgs = validate_spec_semantics(&s, Some(&sv), None, false);
    let warns: Vec<_> = msgs
        .iter()
        .filter(|m| m.severity == Severity::Warning)
        .collect();
    assert_eq!(warns.len(), 1, "got {msgs:?}");
    assert!(warns[0].message.contains("no columns are shared"));
    assert!(
        warns[0]
            .message
            .contains("no implicit join can be synthesized")
    );
}

#[test]
fn cross_table_ambiguous_emits_warning() {
    // A and B share two non-id columns (site, zone) — ambiguous.
    let sv = InMemorySchemaBuilder::new()
        .add_class(class("A"))
        .add_slot("A", slot("a_id", true, false))
        .add_slot("A", slot("site", false, false))
        .add_slot("A", slot("zone", false, false))
        .add_class(class("B"))
        .add_slot("B", slot("b_id", true, false))
        .add_slot("B", slot("site", false, false))
        .add_slot("B", slot("zone", false, false))
        .build();
    let s = spec(json!({
        "class_derivations": {
            "A": {
                "populated_from": "A",
                "slot_derivations": {
                    "b": {"class_derivations": {"B": {"populated_from": "B"}}}
                },
            }
        }
    }));
    let msgs = validate_spec_semantics(&s, Some(&sv), None, false);
    let warns: Vec<_> = msgs
        .iter()
        .filter(|m| m.severity == Severity::Warning)
        .collect();
    assert_eq!(warns.len(), 1, "got {msgs:?}");
    assert!(warns[0].message.contains("multiple candidate join columns"));
    assert!(warns[0].message.contains("'site', 'zone'"));
}

#[test]
fn cross_table_explicit_empty_join_emits_warning() {
    let sv = joinable_schema();
    let s = spec(json!({
        "class_derivations": {
            "Measurement": {
                "populated_from": "Measurement",
                "joins": {"Reading": {"alias": "Reading"}},
                "slot_derivations": {
                    "reading": {"class_derivations": {"Reading": {"populated_from": "Reading"}}}
                },
            }
        }
    }));
    let msgs = validate_spec_semantics(&s, Some(&sv), None, false);
    assert!(
        msgs.iter()
            .any(|m| m.severity == Severity::Warning && m.message.contains("is missing keys"))
    );
}

#[test]
fn cross_table_join_on_typo_emits_warning_per_side() {
    let sv = joinable_schema();
    let s = spec(json!({
        "class_derivations": {
            "Measurement": {
                "populated_from": "Measurement",
                "joins": {"Reading": {"alias": "Reading", "join_on": "nope"}},
                "slot_derivations": {
                    "reading": {"class_derivations": {"Reading": {"populated_from": "Reading"}}}
                },
            }
        }
    }));
    let msgs = validate_spec_semantics(&s, Some(&sv), None, false);
    let warns: Vec<_> = msgs
        .iter()
        .filter(|m| m.severity == Severity::Warning && m.message.contains("join_on=nope"))
        .collect();
    // One per side: not a slot on Measurement, not a slot on Reading.
    assert_eq!(warns.len(), 2, "got {msgs:?}");
}

// ── dotted references through joined tables (upstream cbf2f97) ────────────────

fn warnings(msgs: &[ValidationMessage]) -> Vec<&ValidationMessage> {
    msgs.iter()
        .filter(|m| m.severity == Severity::Warning)
        .collect()
}

/// Upstream `_dotted_spec`: `Outer` populated from `Measurement` with one slot
/// `value`, plus optional `joins:`.
fn dotted_spec(
    slot_derivation: serde_json::Value,
    joins: Option<serde_json::Value>,
) -> TransformationSpecification {
    let mut cd = json!({
        "populated_from": "Measurement",
        "slot_derivations": {"value": slot_derivation},
    });
    if let Some(joins) = joins {
        cd["joins"] = joins;
    }
    spec(json!({"class_derivations": {"Outer": cd}}))
}

/// Upstream `test_dotted_reference_to_joined_class_or_slot_passes`.
#[test]
fn dotted_reference_to_joined_class_or_slot_passes() {
    let sv = joinable_schema();
    let cases = [
        (
            "explicit-join",
            json!({"populated_from": "Reading.score"}),
            Some(json!({"Reading": {"join_on": "subject_id"}})),
        ),
        (
            "implicit-join",
            json!({"populated_from": "Reading.score"}),
            None,
        ),
        (
            "implicit-join-expr",
            json!({"expr": "{Reading.score}"}),
            None,
        ),
        (
            "path-through-source-slot",
            json!({"populated_from": "method.label"}),
            None,
        ),
    ];
    for (id, slot_derivation, joins) in cases {
        let s = dotted_spec(slot_derivation, joins);
        let msgs = validate_spec_semantics(&s, Some(&sv), None, false);
        let problems: Vec<_> = msgs
            .iter()
            .filter(|m| m.severity != Severity::Info)
            .collect();
        assert!(problems.is_empty(), "{id}: got {problems:?}");
    }
}

/// Upstream `test_dotted_populated_from_checks_joined_slot`.
#[test]
fn dotted_populated_from_checks_joined_slot() {
    let sv = joinable_schema();
    for joins in [Some(json!({"Reading": {"join_on": "subject_id"}})), None] {
        let s = dotted_spec(json!({"populated_from": "Reading.bogus"}), joins);
        let msgs = validate_spec_semantics(&s, Some(&sv), None, false);
        let errs: Vec<_> = errors(&msgs).iter().map(|m| m.message.as_str()).collect();
        assert_eq!(
            errs,
            [
                "Source slot 'Reading.bogus' (populated_from): 'bogus' is not a slot on joined class 'Reading'"
            ]
        );
    }
}

/// Upstream `test_dotted_populated_from_unknown_base_still_errors`.
#[test]
fn dotted_populated_from_unknown_base_still_errors() {
    let sv = joinable_schema();
    let s = dotted_spec(json!({"populated_from": "Nowhere.score"}), None);
    let msgs = validate_spec_semantics(&s, Some(&sv), None, false);
    let errs: Vec<_> = errors(&msgs).iter().map(|m| m.message.as_str()).collect();
    assert_eq!(
        errs,
        ["Source slot 'Nowhere.score' (populated_from) not found on source class"]
    );
}

// ── join keys in a derivation's own `joins:` block (upstream e373d93) ──────────

/// Upstream `test_top_level_join_keys_checked`: `(key, class)` pairs of the
/// "is not a slot on source class" warnings.
#[test]
fn top_level_join_keys_checked() {
    let sv = joinable_schema();
    // (id, join spec, expected (key, class) misses)
    type Case<'a> = (&'a str, serde_json::Value, Vec<(&'a str, &'a str)>);
    let cases: [Case; 4] = [
        (
            "join_on-typo",
            json!({"join_on": "subjet_id"}),
            vec![
                ("join_on=subjet_id", "Measurement"),
                ("join_on=subjet_id", "Reading"),
            ],
        ),
        (
            "source_key-typo",
            json!({"source_key": "subjet_id", "lookup_key": "subject_id"}),
            vec![("source_key=subjet_id", "Measurement")],
        ),
        (
            "lookup_key-typo",
            json!({"source_key": "subject_id", "lookup_key": "subjet_id"}),
            vec![("lookup_key=subjet_id", "Reading")],
        ),
        ("valid", json!({"join_on": "subject_id"}), vec![]),
    ];
    for (id, join, expected) in cases {
        let s = dotted_spec(
            json!({"populated_from": "Reading.score"}),
            Some(json!({"Reading": join})),
        );
        let msgs = validate_spec_semantics(&s, Some(&sv), None, false);
        let found: Vec<(String, String)> = warnings(&msgs)
            .iter()
            .filter(|m| m.message.contains("is not a slot on source class"))
            .map(|m| {
                (
                    m.message.split('\'').nth(3).unwrap().to_owned(),
                    m.message
                        .split("source class '")
                        .nth(1)
                        .unwrap()
                        .split('\'')
                        .next()
                        .unwrap()
                        .to_owned(),
                )
            })
            .collect();
        let expected: Vec<(String, String)> = expected
            .into_iter()
            .map(|(k, c)| (k.to_owned(), c.to_owned()))
            .collect();
        assert_eq!(found, expected, "{id}");
        assert!(
            warnings(&msgs)
                .iter()
                .all(|m| m.path == "class_derivations[Outer].joins[Reading]"),
            "{id}: {msgs:?}"
        );
    }
}

/// Upstream `test_top_level_join_keys_checked_against_class_named`.
#[test]
fn top_level_join_keys_checked_against_class_named() {
    let sv = joinable_schema();
    let s = dotted_spec(
        json!({"expr": "{r.score}"}),
        Some(
            json!({"r": {"class_named": "Reading", "source_key": "subject_id", "lookup_key": "nope"}}),
        ),
    );
    let msgs = validate_spec_semantics(&s, Some(&sv), None, false);
    let warns: Vec<_> = warnings(&msgs).iter().map(|m| m.message.as_str()).collect();
    assert_eq!(
        warns,
        [
            "Join spec for 'r': 'lookup_key=nope' is not a slot on source class 'Reading'. \
             Runtime will silently resolve cross-table values to null."
        ]
    );
}

/// Upstream `test_top_level_join_missing_keys_warns`.
#[test]
fn top_level_join_missing_keys_warns() {
    let sv = joinable_schema();
    let s = dotted_spec(
        json!({"populated_from": "Reading.score"}),
        Some(json!({"Reading": {"source_key": "subject_id"}})),
    );
    let msgs = validate_spec_semantics(&s, Some(&sv), None, false);
    let missing: Vec<_> = warnings(&msgs)
        .into_iter()
        .filter(|m| m.message.contains("missing keys"))
        .collect();
    assert_eq!(missing.len(), 1, "{msgs:?}");
    assert_eq!(missing[0].path, "class_derivations[Outer].joins[Reading]");
    assert_eq!(
        missing[0].message,
        "Join spec for 'Reading' is missing keys: must specify 'join_on' or both \
         'source_key' and 'lookup_key'. Runtime will raise ValueError."
    );
}

/// Edge cases from the triage: an empty-string key counts as missing; a joined
/// class absent from the schema gives no key warnings; an identity (no
/// `populated_from`) derivation checks keys against its own name.
#[test]
fn top_level_join_key_edge_cases() {
    let sv = joinable_schema();

    let s = dotted_spec(
        json!({"expr": "{Reading.score}"}),
        Some(json!({"Reading": {"join_on": ""}})),
    );
    let msgs = validate_spec_semantics(&s, Some(&sv), None, false);
    assert_eq!(
        warnings(&msgs)
            .iter()
            .filter(|m| m.message.contains("missing keys"))
            .count(),
        1
    );

    let s = dotted_spec(
        json!({"expr": "{Ghost.score}"}),
        Some(json!({"Ghost": {"join_on": "nope"}})),
    );
    let msgs = validate_spec_semantics(&s, Some(&sv), None, false);
    assert!(warnings(&msgs).is_empty(), "{msgs:?}");

    let s = spec(json!({
        "class_derivations": {
            "Measurement": {"joins": {"Reading": {"join_on": "nope"}}}
        }
    }));
    let msgs = validate_spec_semantics(&s, Some(&sv), None, false);
    let found: Vec<_> = warnings(&msgs).iter().map(|m| m.message.as_str()).collect();
    assert_eq!(found.len(), 2, "{msgs:?}");
    assert!(found[0].ends_with(
        "source class 'Measurement'. Runtime will silently resolve cross-table values to null."
    ));
}

// ── spec range overriding the target slot's range (upstream b332fc7) ─────────

/// Upstream `_RANGE_TARGET_SCHEMA`: custom `Score typeof decimal`, enum
/// `Relationship`, class `Site`, and `Participant` with a slot of each kind.
fn range_target_schema() -> InMemorySchema {
    let ranged = |name: &str, range: RangeKind| SlotDef {
        range,
        ..slot(name, false, false)
    };
    let ty = |t: &str| RangeKind::Type(t.into());
    let mut b = InMemorySchemaBuilder::new()
        .default_range("string")
        .add_enum(EnumDef {
            name: "Relationship".into(),
            permissible_values: vec![],
        })
        .add_type_with_parent("Score", "decimal")
        .add_class(class("Site"))
        .add_slot("Site", slot("id", true, false))
        .add_class(class("Participant"));
    for t in [
        "string",
        "integer",
        "float",
        "decimal",
        "boolean",
        "date",
        "uriorcurie",
    ] {
        b = b.add_type(t);
    }
    b.add_slot(
        "Participant",
        ranged("relationship", RangeKind::Enum("Relationship".into())),
    )
    .add_slot("Participant", ranged("amount", ty("decimal")))
    .add_slot("Participant", ranged("score", ty("Score")))
    .add_slot("Participant", ranged("ratio", ty("float")))
    .add_slot("Participant", ranged("count", ty("integer")))
    .add_slot("Participant", ranged("flag", ty("boolean")))
    .add_slot("Participant", ranged("note", ty("string")))
    .add_slot(
        "Participant",
        ranged("site", RangeKind::Class("Site".into())),
    )
    // No declared range: falls back to `default_range: string`.
    .add_slot("Participant", ranged("untyped", RangeKind::None))
    .build()
}

/// Upstream `_range_override_msgs`: only the `range-override` findings.
fn range_override_msgs(slot_derivation: serde_json::Value, strict: bool) -> Vec<ValidationMessage> {
    let s = spec(json!({
        "class_derivations": {"Participant": {"slot_derivations": slot_derivation}}
    }));
    let sv = range_target_schema();
    validate_spec_semantics(&s, None, Some(&sv), strict)
        .into_iter()
        .filter(|m| m.category.as_deref() == Some("range-override"))
        .collect()
}

/// Upstream `test_semantics_conflicting_range_warns`.
#[test]
fn conflicting_range_warns() {
    let cases = [
        (
            "enum",
            json!({"relationship": {"value": "ONESELF", "range": "string"}}),
            "range 'string' overrides the target slot's range 'Relationship', so its values \
             aren't resolved to the target enum's permissible values. Remove the range to use \
             the target schema's.",
        ),
        (
            "decimal",
            json!({"amount": {"value": "1.5", "range": "string"}}),
            "range 'string' overrides the target slot's range 'decimal', so its values aren't \
             coerced to decimal. Remove the range to use the target schema's.",
        ),
        (
            "string",
            json!({"note": {"value": "1", "range": "integer"}}),
            "range 'integer' overrides the target slot's range 'string', so its values aren't \
             coerced to string. Remove the range to use the target schema's.",
        ),
        (
            "float-into-integer",
            json!({"count": {"value": "1.5", "range": "float"}}),
            "range 'float' overrides the target slot's range 'integer', so its values aren't \
             coerced to integer. Remove the range to use the target schema's.",
        ),
        (
            "boolean",
            json!({"flag": {"value": "true", "range": "string"}}),
            "range 'string' overrides the target slot's range 'boolean', so its values aren't \
             coerced to boolean. Remove the range to use the target schema's.",
        ),
        // Custom types resolve through `typeof`.
        (
            "custom-type-typeof-decimal",
            json!({"score": {"value": "1", "range": "string"}}),
            "range 'string' overrides the target slot's range 'Score', so its values aren't \
             coerced to decimal. Remove the range to use the target schema's.",
        ),
        // The target range falls back to the schema's `default_range`.
        (
            "default-range-fallback",
            json!({"untyped": {"value": "1", "range": "integer"}}),
            "range 'integer' overrides the target slot's range 'string', so its values aren't \
             coerced to string. Remove the range to use the target schema's.",
        ),
    ];
    for (id, slot_derivation, message) in cases {
        let msgs = range_override_msgs(slot_derivation, false);
        let got: Vec<_> = msgs
            .iter()
            .map(|m| (m.severity, m.message.as_str()))
            .collect();
        assert_eq!(got, [(Severity::Warning, message)], "{id}");
    }
}

/// Upstream `test_semantics_compatible_range_not_reported`.
#[test]
fn compatible_range_not_reported() {
    let cases = [
        ("no-range", json!({"relationship": {"value": "ONESELF"}})),
        (
            "same-range",
            json!({"relationship": {"value": "ONESELF", "range": "Relationship"}}),
        ),
        (
            "same-datatype",
            json!({"score": {"value": "0.1", "range": "decimal"}}),
        ),
        (
            "enum-into-string",
            json!({"note": {"value": "ONESELF", "range": "Relationship"}}),
        ),
        (
            "class-range",
            json!({"site": {"value": "S1", "range": "string"}}),
        ),
        (
            "integer-into-decimal",
            json!({"amount": {"value": "1", "range": "integer"}}),
        ),
        (
            "float-into-decimal",
            json!({"amount": {"value": "1.5", "range": "float"}}),
        ),
        (
            "integer-into-float",
            json!({"ratio": {"value": "1", "range": "integer"}}),
        ),
        (
            "date-into-string",
            json!({"note": {"value": "2020-01-01", "range": "date"}}),
        ),
        (
            "uriorcurie-into-string",
            json!({"note": {"value": "X:1", "range": "uriorcurie"}}),
        ),
    ];
    for (id, slot_derivation) in cases {
        let msgs = range_override_msgs(slot_derivation, false);
        assert!(msgs.is_empty(), "{id}: {msgs:?}");
    }
}

/// Upstream `test_semantics_conflicting_range_strict_error`.
#[test]
fn conflicting_range_strict_error() {
    let msgs = range_override_msgs(json!({"amount": {"value": "1.5", "range": "string"}}), true);
    let severities: Vec<_> = msgs.iter().map(|m| m.severity).collect();
    assert_eq!(severities, [Severity::Error]);
}

/// The finding is attached to the slot derivation's path.
#[test]
fn conflicting_range_reports_slot_derivation_path() {
    let msgs = range_override_msgs(
        json!({"amount": {"value": "1.5", "range": "string"}}),
        false,
    );
    assert_eq!(
        msgs[0].path,
        "class_derivations[Participant].slot_derivations[amount]"
    );
}

/// Upstream `_datatype_fits` doctests.
#[test]
fn datatype_fits_doctests() {
    assert!(datatype_fits(Some("integer"), "decimal"));
    assert!(!datatype_fits(Some("float"), "integer"));
    assert!(datatype_fits(None, "string"));
    assert!(!datatype_fits(Some("integer"), "string"));
    assert!(!datatype_fits(Some("string"), "decimal"));
    assert!(!datatype_fits(None, "boolean"));
}

// ── unknown range (upstream f299f3a, `unknown-range`) ──────────────────────

/// Upstream `test_slot_range_must_name_something`.
#[test]
fn slot_range_must_name_something() {
    let sv = joinable_schema();
    let cases = [
        (
            "typo",
            "integr",
            vec!["range 'integr' is not a type, class or enum in the source or target schema"],
        ),
        ("builtin-type", "integer", vec![]),
        ("source-class", "Reading", vec![]),
        ("spec-derivation", "Outer", vec![]),
    ];
    for (id, range_name, expected) in cases {
        let s = dotted_spec(
            json!({"populated_from": "method", "range": range_name}),
            None,
        );
        let msgs = validate_spec_semantics(&s, Some(&sv), None, false);
        let got: Vec<_> = msgs
            .iter()
            .filter(|m| m.category.as_deref() == Some("unknown-range"))
            .map(|m| m.message.as_str())
            .collect();
        assert_eq!(got, expected, "{id}");
    }
}

/// Upstream `test_slot_range_unknown_is_error_when_strict`.
#[test]
fn slot_range_unknown_is_error_when_strict() {
    let sv = joinable_schema();
    let s = dotted_spec(json!({"populated_from": "method", "range": "integr"}), None);
    let msgs = validate_spec_semantics(&s, Some(&sv), None, true);
    let severities: Vec<_> = msgs
        .iter()
        .filter(|m| m.category.as_deref() == Some("unknown-range"))
        .map(|m| m.severity)
        .collect();
    assert_eq!(severities, [Severity::Error]);
}

/// Enum-derivation names and target-schema elements are known ranges too.
#[test]
fn slot_range_known_via_enum_derivation_and_target_schema() {
    let target = range_target_schema();
    let s = spec(json!({
        "class_derivations": {"Participant": {"slot_derivations": {
            "note": {"value": "x", "range": "MyEnum"},
            "relationship": {"value": "x", "range": "Relationship"},
            "site": {"value": "x", "range": "Site"},
        }}},
        "enum_derivations": {"MyEnum": {}},
    }));
    let msgs = validate_spec_semantics(&s, None, Some(&target), false);
    assert!(
        !msgs
            .iter()
            .any(|m| m.category.as_deref() == Some("unknown-range")),
        "{msgs:?}"
    );
}

// ── constant `value:` needs no source slot (upstream d2297e5) ────────────────

/// `Row { person_id, note }` (source) and `Out { person_id, note, tag }` (target).
fn constant_value_schemas() -> (InMemorySchema, InMemorySchema) {
    let source = InMemorySchemaBuilder::new()
        .add_class(class("Row"))
        .add_slot("Row", slot("person_id", false, false))
        .add_slot("Row", slot("note", false, false))
        .build();
    let target = InMemorySchemaBuilder::new()
        .add_class(class("Out"))
        .add_slot("Out", slot("person_id", false, false))
        .add_slot("Out", slot("note", false, false))
        .add_slot("Out", slot("tag", false, false))
        .build();
    (source, target)
}

/// A constant `value:` has no `populated_from`, so it is never checked against
/// the source class. Regression for upstream #338 / #326 (the induced
/// `populated_from: tag` produced a spurious "not found on source class" error);
/// Rust never synthesises `populated_from`, so this pins the contract.
#[test]
fn constant_value_needs_no_source_slot() {
    let (source, target) = constant_value_schemas();
    let s = spec(json!({
        "class_derivations": {
            "Out": {
                "populated_from": "Row",
                "slot_derivations": {
                    "person_id": {"populated_from": "person_id"},
                    "note": {"populated_from": "note"},
                    "tag": {"value": 7},
                },
            }
        }
    }));
    for (src, tgt) in [
        (
            Some(&source as &dyn SchemaProvider),
            Some(&target as &dyn SchemaProvider),
        ),
        (Some(&source), None),
    ] {
        let msgs = validate_spec_semantics(&s, src, tgt, false);
        assert!(errors(&msgs).is_empty(), "{msgs:?}");
    }
}

// ── enum derivations ─────────────────────────────────────────────────────────

fn enum_schema() -> InMemorySchema {
    InMemorySchemaBuilder::new()
        .add_enum(EnumDef {
            name: "VitalStatus".into(),
            permissible_values: vec![],
        })
        .build()
}

#[test]
fn enum_derivation_valid() {
    let sv = enum_schema();
    let s = spec(json!({
        "enum_derivations": {"VitalStatus": {"populated_from": "VitalStatus"}}
    }));
    let msgs = validate_spec_semantics(&s, Some(&sv), Some(&sv), false);
    assert!(msgs.is_empty(), "got {msgs:?}");
}

#[test]
fn enum_derivation_invalid_target_is_error() {
    let sv = enum_schema();
    let s = spec(json!({
        "enum_derivations": {"Bogus": {"populated_from": "VitalStatus"}}
    }));
    let msgs = validate_spec_semantics(&s, None, Some(&sv), false);
    assert!(
        errors(&msgs)
            .iter()
            .any(|e| e.message.contains("Target enum 'Bogus' not found"))
    );
}

#[test]
fn enum_derivation_invalid_source_is_error() {
    let sv = enum_schema();
    let s = spec(json!({
        "enum_derivations": {"VitalStatus": {"populated_from": "Ghost"}}
    }));
    let msgs = validate_spec_semantics(&s, Some(&sv), None, false);
    assert!(errors(&msgs).iter().any(|e| {
        e.message
            .contains("Source enum 'Ghost' (populated_from) not found")
    }));
}

// ── no schemas ───────────────────────────────────────────────────────────────

#[test]
fn no_schemas_produce_no_messages() {
    let s = spec(json!({
        "class_derivations": {"PersonTypo": {"populated_from": "Ghost"}}
    }));
    assert!(validate_spec_semantics(&s, None, None, false).is_empty());
}

// ── ValidationMessage Display ────────────────────────────────────────────────

#[test]
fn validation_message_display_matches_python() {
    let m = ValidationMessage::new(Severity::Error, "class_derivations[X]", "boom");
    assert_eq!(m.to_string(), "class_derivations[X]: [error] boom");
}

// ── expr_refs: extract_expr_slot_references ──────────────────────────────────

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[test]
fn slot_reference_extraction_cases() {
    let cases: &[(&str, &[&str])] = &[
        ("str({age_in_years}) + ' years'", &["age_in_years"]),
        ("{x} + {y}", &["x", "y"]),
        ("x + y", &["x", "y"]),
        ("subject.id", &["subject"]),
        ("src.has_events", &["has_events"]),
        ("src.slot_a + src.slot_b", &["slot_a", "slot_b"]),
        ("case((x == '1', 'YES'), (True, 'NO'))", &["x"]),
        ("'hello'", &[]),
        ("42", &[]),
        ("True", &[]),
        ("None", &[]),
        ("NULL", &[]),
        ("str(x)", &["x"]),
        ("strlen(name)", &["name"]),
        // `lookup` is not a registered function, so the call target survives.
        ("lookup(predicate)", &["lookup", "predicate"]),
    ];
    for (expr, expected) in cases {
        assert_eq!(
            extract_expr_slot_references(expr),
            set(expected),
            "expr: {expr}"
        );
    }
}

#[test]
fn slot_reference_extraction_multiline_filters_bound_names() {
    let expr = "d_test = [x.important_event_date for x in src.has_important_life_events if str(x.event_name) == \"PASSED_DRIVING_TEST\"]\nif len(d_test):\n    target = d_test[0]";
    let refs = extract_expr_slot_references(expr);
    assert!(refs.contains("has_important_life_events"));
    // `d_test` (assignment target) and `x` (comprehension var) are bound.
    assert!(!refs.contains("d_test"));
    assert!(!refs.contains("x"));
}

#[test]
fn slot_reference_extraction_unparsable_is_empty() {
    assert!(extract_expr_slot_references("{{{{").is_empty());
}

// ── expr_refs: extract_expr_attribute_references ─────────────────────────────

fn attr_map(pairs: &[(&str, &[&str])]) -> BTreeMap<String, BTreeSet<String>> {
    pairs
        .iter()
        .map(|(base, attrs)| (base.to_string(), set(attrs)))
        .collect()
}

#[test]
fn attribute_reference_extraction_cases() {
    assert_eq!(
        extract_expr_attribute_references("{demographics.age}"),
        attr_map(&[("demographics", &["age"])])
    );
    assert_eq!(
        extract_expr_attribute_references("{a.x} + {a.y}"),
        attr_map(&[("a", &["x", "y"])])
    );
    assert_eq!(
        extract_expr_attribute_references("{a.x} + {b.y}"),
        attr_map(&[("a", &["x"]), ("b", &["y"])])
    );
    // src.* is handled by the slot-ref extractor, so excluded here.
    assert!(extract_expr_attribute_references("src.foo").is_empty());
    assert!(extract_expr_attribute_references("plain_var").is_empty());
    assert!(extract_expr_attribute_references("'hello'").is_empty());
}

#[test]
fn attribute_reference_extraction_unparsable_is_empty() {
    assert!(extract_expr_attribute_references("{{{{").is_empty());
}
