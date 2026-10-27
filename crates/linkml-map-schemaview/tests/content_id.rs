//! Content-hash identifiers for identifier slots the spec does not derive.
//!
//! Port of upstream `tests/test_transformer/test_content_id.py` (linkml-map
//! `76db207` #342, `e70715e`): the same source/target schemas, spec and row, and
//! the same pinned UUIDs. Validator and CLI cases live in
//! `linkml-map-core/src/validate/` and `linkml-map-cli/tests/`.

use linkml_map_core::datamodel::{TransformationSpecification, normalise_spec_json};
use linkml_map_core::engine::ObjectTransformer;
use linkml_map_core::error::Error;
use linkml_map_core::validate::{Severity, validate_spec_semantics};
use linkml_map_core::value::Value;
use linkml_map_schemaview::SchemaViewProvider;
use serde_json::{Value as Json, json};

pub const SOURCE_SCHEMA: &str = r#"
id: https://example.org/source
name: source
prefixes: {linkml: 'https://w3id.org/linkml/'}
default_prefix: source
default_range: string
imports: [linkml:types]
classes:
  Row:
    attributes:
      participant: {}
      visit: {}
      observation_type: {}
      method: {}
      value: {}
      unit: {}
      tags: {multivalued: true}
      steps: {multivalued: true}
      note: {}
"#;

pub const TARGET_SCHEMA: &str = r#"
id: https://example.org/target
name: target
prefixes: {linkml: 'https://w3id.org/linkml/'}
default_prefix: target
default_range: string
imports: [linkml:types]
classes:
  Measurement:
    attributes:
      id: {identifier: true, required: true}
      associated_participant: {}
      associated_visit: {}
      observation_type: {}
      method_type: {}
      tags: {multivalued: true}
      steps: {multivalued: true, list_elements_ordered: true}
      value_quantity: {range: Quantity, inlined: true}
  Quantity:
    attributes:
      id: {identifier: true, required: true}
      value_decimal: {range: decimal}
      unit: {}
"#;

const INHERITING_TARGET: &str = r#"
id: https://example.org/inheriting
name: inheriting
prefixes: {linkml: 'https://w3id.org/linkml/'}
default_prefix: inheriting
default_range: string
imports: [linkml:types]
classes:
  Record:
    abstract: true
    attributes:
      id: {identifier: true, required: true}
  Measurement:
    is_a: Record
    attributes:
      observation_type: {}
"#;

fn spec_json() -> Json {
    json!({
        "id": "content-id",
        "mint_ids": true,
        "class_derivations": {
            "Measurement": {
                "populated_from": "Row",
                "slot_derivations": {
                    "associated_participant": {"populated_from": "participant"},
                    "associated_visit": {"populated_from": "visit"},
                    "observation_type": {"populated_from": "observation_type"},
                    "method_type": {"populated_from": "method"},
                    "tags": {"populated_from": "tags"},
                    "steps": {"populated_from": "steps"},
                    "_note": {"populated_from": "note", "hide": true},
                    "value_quantity": {
                        "class_derivations": {
                            "Quantity": {
                                "populated_from": "Row",
                                "slot_derivations": {
                                    "value_decimal": {"populated_from": "value", "range": "float"},
                                    "unit": {"populated_from": "unit"}
                                }
                            }
                        }
                    }
                }
            }
        }
    })
}

fn row_json() -> Json {
    json!({
        "participant": "P1",
        "visit": "V1",
        "observation_type": "OBA:VT0000184",
        "method": "lipid assay",
        "value": "52.30",
        "unit": "mg/dL",
        "tags": ["fasting", "exam 1"],
        "steps": ["draw", "spin"],
        "note": "first"
    })
}

fn with(changes: Json) -> Json {
    let mut row = row_json();
    for (k, v) in changes.as_object().unwrap() {
        row[k] = v.clone();
    }
    row
}

fn to_spec(mut spec: Json) -> TransformationSpecification {
    normalise_spec_json(&mut spec);
    serde_json::from_value(spec).expect("spec parses")
}

fn value(j: Json) -> Value {
    serde_json::from_value(j).expect("value")
}

fn provider(yaml: &str) -> SchemaViewProvider {
    SchemaViewProvider::from_yaml_str(yaml).expect("schema loads")
}

fn transform_with(row: Json, spec: &Json, target: Option<&str>) -> Result<Value, Error> {
    let source = provider(SOURCE_SCHEMA);
    let target = target.map(provider);
    ObjectTransformer::new(
        to_spec(spec.clone()),
        Some(&source),
        target
            .as_ref()
            .map(|t| t as &dyn linkml_map_core::schema::SchemaProvider),
    )
    .map_object(&value(row), Some("Row"))
}

fn transform(row: Json) -> Value {
    transform_with(row, &spec_json(), Some(TARGET_SCHEMA)).expect("transforms")
}

fn map(v: &Value) -> &indexmap::IndexMap<String, Value> {
    match v {
        Value::Map(m) => m,
        other => panic!("not a map: {other:?}"),
    }
}

fn id_of(v: &Value) -> String {
    match map(v).get("id") {
        Some(Value::Str(s)) => s.clone(),
        other => panic!("no id: {other:?}"),
    }
}

/// `_spec_with`: the spec without `mint_ids`, optionally without the nested
/// Quantity derivation, with the given spec-level / class-level settings.
fn spec_with(spec_mint_ids: Option<bool>, class_mint_ids: Option<bool>, nested: bool) -> Json {
    let mut spec = spec_json();
    spec.as_object_mut().unwrap().remove("mint_ids");
    let cd = &mut spec["class_derivations"]["Measurement"];
    if !nested {
        cd["slot_derivations"]
            .as_object_mut()
            .unwrap()
            .remove("value_quantity");
    }
    if let Some(b) = spec_mint_ids {
        spec["mint_ids"] = json!(b);
    }
    if let Some(b) = class_mint_ids {
        spec["class_derivations"]["Measurement"]["mint_ids"] = json!(b);
    }
    spec
}

fn error_text(r: Result<Value, Error>) -> String {
    r.expect_err("expected an error").to_string()
}

// test_synthesized_id_is_pinned
#[test]
fn synthesized_id_is_pinned() {
    let out = transform(row_json());
    assert_eq!(id_of(&out), "e2f19c73-a77b-5b80-90b8-3394ef6f96d3");
    let quantity = map(&out).get("value_quantity").unwrap();
    assert_eq!(id_of(quantity), "199f65b6-2954-565e-912a-b814c2568cd9");
    assert_eq!(map(&out).keys().next().unwrap(), "id");
    // The hidden slot is not part of the emitted record.
    assert!(!map(&out).contains_key("_note"));
}

// test_minted_record_round_trips_through_yaml (e70715e): the id key is a plain string.
#[test]
fn minted_record_round_trips_through_yaml() {
    let out = transform(row_json());
    let yaml = serde_yaml::to_string(&out).unwrap();
    let loaded: Json = serde_yaml::from_str(&yaml).unwrap();
    assert_eq!(loaded["id"], "e2f19c73-a77b-5b80-90b8-3394ef6f96d3");
    assert_eq!(
        loaded["value_quantity"]["id"],
        "199f65b6-2954-565e-912a-b814c2568cd9"
    );
}

// test_same_content_same_id_across_transformers
#[test]
fn same_content_same_id_across_transformers() {
    let row = row_json();
    let reversed: serde_json::Map<String, Json> = row
        .as_object()
        .unwrap()
        .iter()
        .rev()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    assert_eq!(
        id_of(&transform(row)),
        id_of(&transform(Json::Object(reversed)))
    );
}

// test_different_content_different_id
#[test]
fn different_content_different_id() {
    let base = id_of(&transform(row_json()));
    for changes in [
        json!({"method": "recalibration"}),
        json!({"visit": "V2"}),
        json!({"value": "52.31"}),
        json!({"steps": ["spin", "draw"]}),
    ] {
        assert_ne!(base, id_of(&transform(with(changes.clone()))), "{changes}");
    }
}

// test_equivalent_content_same_id
#[test]
fn equivalent_content_same_id() {
    let base = id_of(&transform(row_json()));
    for changes in [
        json!({"tags": ["exam 1", "fasting"]}),
        json!({"value": "52.300"}),
        json!({"note": "a hidden slot is not part of the record"}),
    ] {
        assert_eq!(base, id_of(&transform(with(changes.clone()))), "{changes}");
    }
}

// test_null_and_empty_count_as_absent
#[test]
fn null_and_empty_count_as_absent() {
    let mut without = row_json();
    without.as_object_mut().unwrap().remove("tags");
    let expected = id_of(&transform(without));
    for absent in [json!({"tags": null}), json!({"tags": []})] {
        assert_eq!(
            expected,
            id_of(&transform(with(absent.clone()))),
            "{absent}"
        );
    }
}

// test_explicit_id_derivation_wins
#[test]
fn explicit_id_derivation_wins() {
    let mut spec = spec_json();
    spec["class_derivations"]["Measurement"]["slot_derivations"]["id"] =
        json!({"expr": "'M-' + {participant}"});
    let out = transform_with(row_json(), &spec, Some(TARGET_SCHEMA)).unwrap();
    assert_eq!(id_of(&out), "M-P1");
}

// test_no_target_schema_no_id
#[test]
fn no_target_schema_no_id() {
    let mut spec = spec_json();
    spec["class_derivations"]["Measurement"]["slot_derivations"]
        .as_object_mut()
        .unwrap()
        .remove("value_quantity");
    let out = transform_with(row_json(), &spec, None).unwrap();
    assert!(!map(&out).contains_key("id"));
}

// test_mint_ids_setting
#[test]
fn mint_ids_setting() {
    for (spec_mint_ids, class_mint_ids, minted) in [
        (None, None, false),
        (Some(false), None, false),
        (Some(true), None, true),
        (None, Some(true), true),
        (Some(false), Some(true), true),
        (Some(true), Some(false), false),
    ] {
        let spec = spec_with(spec_mint_ids, class_mint_ids, false);
        let r = transform_with(row_json(), &spec, Some(TARGET_SCHEMA));
        if minted {
            assert!(map(&r.unwrap()).contains_key("id"));
        } else {
            let text = error_text(r);
            assert!(
                text.contains("does not derive identifier slot 'id' and mint_ids is not true"),
                "{text}"
            );
        }
    }
}

// test_nested_derivation_follows_the_spec_not_its_parent
#[test]
fn nested_derivation_follows_the_spec_not_its_parent() {
    let mut spec = spec_with(None, Some(true), true);
    let text = error_text(transform_with(row_json(), &spec, Some(TARGET_SCHEMA)));
    assert!(
        text.contains("'Quantity' does not derive identifier slot 'id'"),
        "{text}"
    );
    spec["class_derivations"]["Measurement"]["slot_derivations"]["value_quantity"]["class_derivations"]
        ["Quantity"]["mint_ids"] = json!(true);
    let out = transform_with(row_json(), &spec, Some(TARGET_SCHEMA)).unwrap();
    assert!(map(&out).contains_key("id"));
    assert!(map(map(&out).get("value_quantity").unwrap()).contains_key("id"));
}

// test_missing_identifiers_covers_nested_derivations
#[test]
fn missing_identifiers_covers_nested_derivations() {
    let source = provider(SOURCE_SCHEMA);
    let target = provider(TARGET_SCHEMA);
    let tr = ObjectTransformer::new(
        to_spec(spec_with(None, None, true)),
        Some(&source),
        Some(&target),
    );
    assert_eq!(
        tr.missing_identifiers().unwrap(),
        vec![
            "Class derivation 'Measurement' does not derive identifier slot 'id' and mint_ids is not true; \
             derive it explicitly or set mint_ids: true",
            "Class derivation 'Quantity' does not derive identifier slot 'id' and mint_ids is not true; \
             derive it explicitly or set mint_ids: true",
        ]
    );
    let err = tr.check_identifiers().unwrap_err().to_string();
    assert!(err.contains("'Measurement' does not derive"), "{err}");
}

// test_class_defaults_precedence (the last case is the compact `Class: true` form)
#[test]
fn class_defaults_precedence() {
    for (spec_mint_ids, class_default, class_mint_ids, minted) in [
        (None, json!({"mint_ids": true}), None, true),
        (Some(false), json!({"mint_ids": true}), None, true),
        (Some(true), json!({"mint_ids": false}), None, false),
        (None, json!({"mint_ids": true}), Some(false), false),
        (None, json!({"mint_ids": false}), Some(true), true),
        (None, json!(true), None, true),
    ] {
        let mut spec = spec_with(spec_mint_ids, class_mint_ids, false);
        spec["class_defaults"] = json!({"Measurement": class_default});
        let r = transform_with(row_json(), &spec, Some(TARGET_SCHEMA));
        if minted {
            assert!(map(&r.unwrap()).contains_key("id"));
        } else {
            assert!(error_text(r).contains("mint_ids is not true"));
        }
    }
}

// The explicit list form and `{Class: null}` (unset) of class_defaults.
#[test]
fn class_defaults_list_form_and_null() {
    let mut spec = spec_with(None, None, false);
    spec["class_defaults"] = json!([{"name": "Measurement", "mint_ids": true}]);
    assert!(
        map(&transform_with(row_json(), &spec, Some(TARGET_SCHEMA)).unwrap()).contains_key("id")
    );
    spec["class_defaults"] = json!({"Measurement": null});
    assert!(
        error_text(transform_with(row_json(), &spec, Some(TARGET_SCHEMA)))
            .contains("mint_ids is not true")
    );
}

// `mint_ids` and `class_defaults` survive a serde round trip of the spec.
#[test]
fn mint_settings_round_trip() {
    let mut spec = spec_with(Some(true), Some(false), false);
    spec["class_defaults"] = json!({"Measurement": true, "Quantity": {"mint_ids": false}});
    let parsed = to_spec(spec);
    let again: TransformationSpecification =
        serde_json::from_value(serde_json::to_value(&parsed).unwrap()).unwrap();
    assert_eq!(parsed, again);
    assert_eq!(again.mint_ids, Some(true));
    assert_eq!(
        again.class_defaults.as_ref().unwrap()["Measurement"].mint_ids,
        Some(true)
    );
    assert_eq!(
        again.class_defaults.as_ref().unwrap()["Quantity"].mint_ids,
        Some(false)
    );
}

// test_inherited_id_derivation_and_abstract_classes_need_no_minting
#[test]
fn inherited_id_derivation_and_abstract_classes_need_no_minting() {
    let spec = json!({
        "id": "inheriting",
        "class_derivations": {
            "Record": {"slot_derivations": {"id": {"expr": "'R-' + {participant}"}}},
            "Measurement": {
                "is_a": "Record",
                "populated_from": "Row",
                "slot_derivations": {"observation_type": {"populated_from": "observation_type"}}
            }
        }
    });
    let source = provider(SOURCE_SCHEMA);
    let target = provider(INHERITING_TARGET);
    let tr = ObjectTransformer::new(to_spec(spec.clone()), Some(&source), Some(&target));
    assert!(tr.missing_identifiers().unwrap().is_empty());
    let out = tr.map_object(&value(row_json()), Some("Row")).unwrap();
    let got: Json = serde_json::to_value(&out).unwrap();
    assert_eq!(
        got,
        json!({"id": "R-P1", "observation_type": "OBA:VT0000184"})
    );

    let messages = validate_spec_semantics(&to_spec(spec), Some(&source), Some(&target), false);
    assert!(
        !messages.iter().any(|m| m.message.contains("identifier")),
        "{messages:?}"
    );
}

// `mint_ids` is inherited through is_a like any unset scalar (`_with_inherited_fields`).
#[test]
fn mint_ids_is_inherited_through_is_a() {
    let spec = json!({
        "id": "inherit-mint",
        "class_derivations": {
            "Base": {"mint_ids": true},
            "Measurement": {
                "is_a": "Base",
                "populated_from": "Row",
                "slot_derivations": {"observation_type": {"populated_from": "observation_type"}}
            }
        }
    });
    let source = provider(SOURCE_SCHEMA);
    let target = provider(TARGET_SCHEMA);
    let tr = ObjectTransformer::new(to_spec(spec), Some(&source), Some(&target));
    let out = tr.map_object(&value(row_json()), Some("Row")).unwrap();
    assert!(map(&out).contains_key("id"));
}

// ── Validator (test_validator_*) ─────────────────────────────────────────────

fn about_id(messages: &[linkml_map_core::validate::ValidationMessage]) -> Vec<(Severity, String)> {
    messages
        .iter()
        .filter(|m| m.message.contains("'id'") && m.path.ends_with("[Measurement]"))
        .map(|m| (m.severity, m.message.clone()))
        .collect()
}

fn validate(spec: &Json) -> Vec<linkml_map_core::validate::ValidationMessage> {
    let source = provider(SOURCE_SCHEMA);
    let target = provider(TARGET_SCHEMA);
    validate_spec_semantics(&to_spec(spec.clone()), Some(&source), Some(&target), false)
}

const SYNTHESIZED: &str = "Identifier slot 'id' has no derivation; it will be synthesized from a hash of the record's content";

// test_validator_reports_minting
#[test]
fn validator_reports_minting() {
    assert_eq!(
        about_id(&validate(&spec_json())),
        vec![(Severity::Info, SYNTHESIZED.to_string())]
    );
}

// test_validator_errors_when_not_minting
#[test]
fn validator_errors_when_not_minting() {
    assert_eq!(
        about_id(&validate(&spec_with(None, None, false))),
        vec![(
            Severity::Error,
            "Class derivation 'Measurement' does not derive identifier slot 'id' and mint_ids is not true; \
             derive it explicitly or set mint_ids: true"
                .to_string()
        )]
    );
}

// test_validator_honors_class_defaults_and_rejects_unknown_classes
#[test]
fn validator_honors_class_defaults_and_rejects_unknown_classes() {
    let mut spec = spec_with(None, None, false);
    spec["class_defaults"] =
        json!({"Measurement": {"mint_ids": true}, "NoSuchClass": {"mint_ids": true}});
    let messages = validate(&spec);
    assert_eq!(
        about_id(&messages),
        vec![(Severity::Info, SYNTHESIZED.to_string())]
    );
    let defaults: Vec<_> = messages
        .iter()
        .filter(|m| m.path.starts_with("class_defaults"))
        .map(|m| (m.severity, m.path.clone(), m.message.clone()))
        .collect();
    assert_eq!(
        defaults,
        vec![(
            Severity::Error,
            "class_defaults[NoSuchClass]".to_string(),
            "class_defaults names 'NoSuchClass', which is not a class in the target schema"
                .to_string()
        )]
    );
    // The unknown-class check comes first, as upstream.
    assert!(messages[0].path.starts_with("class_defaults"));
}
