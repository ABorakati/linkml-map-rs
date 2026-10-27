//! `expr:` and `value:` results are coerced to the target slot like
//! `populated_from` (upstream 11b425b; port of
//! `tests/test_transformer/test_derived_value_coercion.py`).
//!
//! Cardinality and datatype follow the target slot, not how the value was derived.

use linkml_map_core::{
    datamodel::{TransformationSpecification, normalise_spec_json},
    engine::ObjectTransformer,
    schema::SchemaProvider,
    value::Value,
};
use linkml_map_schemaview::SchemaViewProvider;

const SOURCE_SCHEMA: &str = r"
id: https://example.org/derived-source
name: derived_source
prefixes: {linkml: 'https://w3id.org/linkml/'}
default_prefix: derived_source
default_range: string
imports: [linkml:types]
classes:
  Row:
    attributes:
      tag: {}
      amount: {}
";

const TARGET_SCHEMA: &str = r"
id: https://example.org/derived-target
name: derived_target
prefixes: {linkml: 'https://w3id.org/linkml/'}
default_prefix: derived_target
default_range: string
imports: [linkml:types]
classes:
  Out:
    attributes:
      tags_pf: {multivalued: true}
      tags_expr: {multivalued: true}
      tags_value: {multivalued: true}
      amt_pf: {range: decimal}
      amt_expr: {range: decimal}
      amt_value: {range: decimal}
      count_value: {range: integer}
      label_expr: {}
      amt_text: {range: decimal}
";

const SPEC: &str = r#"
id: derived-value-coercion
class_derivations:
  Out:
    populated_from: Row
    slot_derivations:
      tags_pf: {populated_from: tag}
      tags_expr: {expr: "{tag}"}
      tags_value: {value: a}
      amt_pf: {populated_from: amount}
      amt_expr: {expr: "{amount}"}
      amt_value: {value: "5.0"}
      count_value: {value: 7}
      label_expr: {expr: "1 + 1"}
      amt_text: {expr: "{amount}", range: string}
"#;

fn s(v: &str) -> Value {
    Value::Str(v.into())
}

fn map(with_target: bool) -> Value {
    let mut obj: serde_json::Value = serde_yaml::from_str(SPEC).unwrap();
    normalise_spec_json(&mut obj);
    let spec: TransformationSpecification = serde_json::from_value(obj).unwrap();
    let source = SchemaViewProvider::from_yaml_str(SOURCE_SCHEMA).unwrap();
    let target = SchemaViewProvider::from_yaml_str(TARGET_SCHEMA).unwrap();
    let target_ref: Option<&dyn SchemaProvider> = with_target.then_some(&target);
    let row = Value::Map(
        [("tag".to_owned(), s("y")), ("amount".to_owned(), s("5.0"))]
            .into_iter()
            .collect(),
    );
    ObjectTransformer::new(spec, Some(&source), target_ref)
        .map_object(&row, Some("Row"))
        .unwrap()
}

fn get(v: &Value, key: &str) -> Value {
    match v {
        Value::Map(m) => m.get(key).cloned().unwrap_or(Value::Null),
        other => panic!("expected a map, got {other:?}"),
    }
}

#[test]
fn scalar_into_multivalued_becomes_a_list() {
    let out = map(true);
    for slot in ["tags_pf", "tags_expr"] {
        assert_eq!(get(&out, slot), Value::List(vec![s("y")]), "{slot}");
    }
}

#[test]
fn constant_into_multivalued_becomes_a_list() {
    assert_eq!(get(&map(true), "tags_value"), Value::List(vec![s("a")]));
}

#[test]
fn constant_takes_the_target_type() {
    // A `value:` constant isn't forced to a string; the target slot types it.
    assert_eq!(get(&map(true), "count_value"), Value::Int(7));
}

#[test]
fn expr_result_into_string_slot_is_stringified() {
    assert_eq!(get(&map(true), "label_expr"), s("2"));
}

#[test]
fn spec_range_overrides_target_schema() {
    assert_eq!(get(&map(true), "amt_text"), s("5.0"));
}

#[test]
fn no_target_schema_leaves_derived_values_alone() {
    // Without a target schema, only spec ranges coerce.
    let out = map(false);
    assert_eq!(get(&out, "tags_expr"), s("y"));
    assert_eq!(get(&out, "count_value"), Value::Int(7));
    assert_eq!(get(&out, "label_expr"), Value::Int(2));
    assert_eq!(get(&out, "amt_text"), s("5.0"));
}

// Upstream: test_object_transformer.py::test_normalize_leaves_range_unset_for_literal_value.
// The Rust normaliser never defaulted `range: string` for a literal `value:`;
// this guards that it stays that way.
#[test]
fn normalise_leaves_range_unset_for_literal_value() {
    let mut obj: serde_json::Value = serde_yaml::from_str(
        r"
class_derivations:
  MyClass:
    populated_from: Src
    slot_derivations:
      my_slot:
        value: constant
",
    )
    .unwrap();
    normalise_spec_json(&mut obj);
    let spec: TransformationSpecification = serde_json::from_value(obj).unwrap();
    let cd = &spec.class_derivations.as_ref().unwrap()[0];
    let sd = cd
        .slot_derivations
        .as_ref()
        .unwrap()
        .get("my_slot")
        .unwrap();
    assert!(sd.range.is_none());
}
