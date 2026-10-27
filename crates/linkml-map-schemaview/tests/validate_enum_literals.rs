//! `validate-spec` checks `value` / `value_mappings` literals against the
//! target enum (upstream 7665ee3; the "Enum literals against the target enum
//! (#360)" block of `tests/test_validator.py`).

use linkml_map_core::{
    datamodel::{TransformationSpecification, normalise_spec_json},
    schema::SchemaProvider,
    validate::{Severity, ValidationMessage, validate_spec_semantics},
};
use linkml_map_schemaview::SchemaViewProvider;
use serde_json::json;

const ENUM_TARGET_SCHEMA: &str = r#"
id: https://example.org/enum-target
name: enum-target
prefixes:
  linkml: 'https://w3id.org/linkml/'
  OMOP: 'https://athena.ohdsi.org/search-terms/terms/'
  obo: 'http://purl.obolibrary.org/obo/'
imports: [linkml:types]
default_range: string
enums:
  SexEnum:
    permissible_values:
      "OMOP:8532": {title: FEMALE, meaning: "OMOP:8532"}
      "OMOP:8507": {title: MALE, meaning: "OMOP:8507"}
  Open:
    reachable_from:
      source_ontology: obo:mondo
      source_nodes: ["MONDO:0000001"]
  Tied:
    permissible_values:
      F1: {title: FEMALE}
      F2: {title: FEMALE}
classes:
  Participant:
    attributes:
      sex: {range: SexEnum}
      disease: {range: Open}
      note: {}
      tied: {range: Tied}
"#;

fn spec(slot_derivations: serde_json::Value) -> TransformationSpecification {
    let mut obj = json!({
        "class_derivations": {"Participant": {"slot_derivations": slot_derivations}}
    });
    normalise_spec_json(&mut obj);
    serde_json::from_value(obj).unwrap()
}

/// Messages about permissible values only; the range-override warning
/// (upstream b332fc7) is a separate finding and is filtered out, as upstream's
/// tests do once it lands.
fn enum_literal_msgs(slot_derivations: serde_json::Value, strict: bool) -> Vec<ValidationMessage> {
    let target = SchemaViewProvider::from_yaml_str(ENUM_TARGET_SCHEMA).unwrap();
    validate_spec_semantics(
        &spec(slot_derivations),
        None,
        Some(&target as &dyn SchemaProvider),
        strict,
    )
    .into_iter()
    .filter(|m| m.category.as_deref() != Some("range-override"))
    .filter(|m| m.message.contains("permissible value"))
    .collect()
}

fn summary(msgs: &[ValidationMessage]) -> Vec<(Severity, &str)> {
    msgs.iter()
        .map(|m| (m.severity, m.message.as_str()))
        .collect()
}

#[test]
fn enum_literals_that_resolve() {
    let cases = [
        ("title", json!({"sex": {"value": "FEMALE"}})),
        ("key", json!({"sex": {"value": "OMOP:8507"}})),
        (
            "meaning-uri",
            json!({"sex": {"value": "https://athena.ohdsi.org/search-terms/terms/8507"}}),
        ),
        (
            "value-mappings",
            json!({"sex": {"populated_from": "gender",
                "value_mappings": {"f": "FEMALE", "m": "OMOP:8507", "u": null}}}),
        ),
        ("open-enum", json!({"disease": {"value": "anything"}})),
        ("non-enum", json!({"note": {"value": "anything"}})),
        (
            "spec-range-wins",
            json!({"sex": {"value": "female", "range": "string"}}),
        ),
    ];
    for (id, slot_derivation) in cases {
        let msgs = enum_literal_msgs(slot_derivation, false);
        assert!(msgs.is_empty(), "{id}: {msgs:?}");
    }
}

#[test]
fn unresolved_enum_literals_warn() {
    let msgs = enum_literal_msgs(
        json!({"sex": {"populated_from": "gender",
            "value_mappings": {"f": "FEMALE", "x": "OTHER"}}}),
        false,
    );
    assert_eq!(
        summary(&msgs),
        [(
            Severity::Warning,
            "value_mappings[x]: 'OTHER' is not a permissible value of enum SexEnum"
        )]
    );
    assert_eq!(
        msgs[0].path,
        "class_derivations[Participant].slot_derivations[sex]"
    );
    assert!(msgs[0].category.is_none());
}

#[test]
fn unresolved_enum_literal_strict_error() {
    let msgs = enum_literal_msgs(json!({"sex": {"value": "female"}}), true);
    assert_eq!(
        summary(&msgs),
        [(
            Severity::Error,
            "value: 'female' is not a permissible value of enum SexEnum"
        )]
    );
}

// Additions from the triage plan: ambiguity, `value: false`, ordering.
#[test]
fn ambiguous_enum_literal_is_reported() {
    let msgs = enum_literal_msgs(json!({"tied": {"value": "FEMALE"}}), false);
    assert_eq!(
        summary(&msgs),
        [(
            Severity::Warning,
            "value: 'FEMALE' matches more than one permissible value of enum Tied: ['F1', 'F2']"
        )]
    );
}

#[test]
fn false_value_is_checked_as_python_str() {
    let msgs = enum_literal_msgs(json!({"sex": {"value": false}}), false);
    assert_eq!(
        summary(&msgs),
        [(
            Severity::Warning,
            "value: 'False' is not a permissible value of enum SexEnum"
        )]
    );
}

#[test]
fn value_comes_before_value_mappings_in_declaration_order() {
    let msgs = enum_literal_msgs(
        json!({"sex": {"value": "bad1", "value_mappings": {"a": "bad2", "b": "bad3"}}}),
        false,
    );
    let fields: Vec<&str> = msgs
        .iter()
        .map(|m| m.message.split(':').next().unwrap())
        .collect();
    assert_eq!(fields, ["value", "value_mappings[a]", "value_mappings[b]"]);
}

#[test]
fn no_target_schema_means_no_check() {
    let msgs =
        validate_spec_semantics(&spec(json!({"sex": {"value": "female"}})), None, None, true);
    assert!(
        msgs.iter()
            .all(|m| !m.message.contains("permissible value"))
    );
}
