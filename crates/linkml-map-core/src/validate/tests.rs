//! Tests for semantic spec validation, ported from a representative subset of
//! upstream `tests/test_validator.py` (commit `5a42c2af67`) — the
//! `validate_spec_semantics` and expression-reference-extraction cases.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::json;

use super::expr_refs::{extract_expr_attribute_references, extract_expr_slot_references};
use super::*;
use crate::datamodel::{normalise_spec_json, TransformationSpecification};
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
    msgs.iter().filter(|m| m.severity == Severity::Error).collect()
}

// ── validate_spec_semantics: class / slot resolution ─────────────────────────

#[test]
fn valid_spec_produces_no_messages() {
    let sv = person_schema(false);
    let s = spec(json!({
        "class_derivations": {
            "Person": {
                "populated_from": "Person",
                "slot_derivations": {"primary_email": None::<()>},
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
    assert!(errs
        .iter()
        .any(|e| e.message.contains("nonexistent_slot")
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
                "slot_derivations": {"name": {"expr": "str({nonexistent}) + '!'"}},
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
                "slot_derivations": {"name": {"expr": "str({nonexistent}) + '!'"}},
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
                "slot_derivations": {"primary_email": {"expr": "str({age_in_years}) + ' years'"}},
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
                "slot_derivations": {"primary_email": {"expr": "{Address.city}"}},
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
    assert!(msgs.iter().any(|m| m.severity == Severity::Warning
        && m.message.contains("Required target slot 'name'")));
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
    assert!(errors(&msgs)
        .iter()
        .any(|e| e.message.contains("is_a: Ghost") && e.message.contains("does not resolve")));
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
    let info: Vec<_> = msgs.iter().filter(|m| m.severity == Severity::Info).collect();
    assert_eq!(info.len(), 1, "got {msgs:?}");
    assert!(info[0].message.contains("implicit join will be synthesized on column 'subject_id'"));
    assert!(info[0]
        .path
        .contains("slot_derivations[reading].class_derivations[Reading]"));
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
    let warns: Vec<_> = msgs.iter().filter(|m| m.severity == Severity::Warning).collect();
    assert_eq!(warns.len(), 1, "got {msgs:?}");
    assert!(warns[0].message.contains("no columns are shared"));
    assert!(warns[0].message.contains("no implicit join can be synthesized"));
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
    let warns: Vec<_> = msgs.iter().filter(|m| m.severity == Severity::Warning).collect();
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
    assert!(msgs.iter().any(|m| m.severity == Severity::Warning
        && m.message.contains("is missing keys")));
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
    assert!(errors(&msgs)
        .iter()
        .any(|e| e.message.contains("Target enum 'Bogus' not found")));
}

#[test]
fn enum_derivation_invalid_source_is_error() {
    let sv = enum_schema();
    let s = spec(json!({
        "enum_derivations": {"VitalStatus": {"populated_from": "Ghost"}}
    }));
    let msgs = validate_spec_semantics(&s, Some(&sv), None, false);
    assert!(errors(&msgs)
        .iter()
        .any(|e| e.message.contains("Source enum 'Ghost' (populated_from) not found")));
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
