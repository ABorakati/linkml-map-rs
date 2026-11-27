//! A joined column read with `populated_from` is coerced and enum-resolved like any
//! value (upstream 0617b59, #403, closes #402; port of
//! `tests/test_transformer/test_cross_table_lookup.py::test_populated_from_cross_table_is_typed_to_target`).
//!
//! Upstream reads the tables from TSV through `DataLoader`, which parses `30` to an
//! integer; the rows here carry the same parsed values.

use std::sync::Arc;

use linkml_map_core::{
    datamodel::{TransformationSpecification, normalise_spec_json},
    engine::{LookupIndex, ObjectTransformer},
    value::Value,
};
use linkml_map_schemaview::SchemaViewProvider;

const SOURCE_SCHEMA: &str = r"
id: https://example.org/cross-table-source
name: cross_table_source
prefixes:
  linkml: https://w3id.org/linkml/
imports:
  - linkml:types
default_range: string
classes:
  lab_results:
    attributes:
      sample_id:
        identifier: true
      participant_id: {}
      analyte: {}
      result_value: {}
  demographics:
    attributes:
      participant_id:
        identifier: true
      age_at_exam: {}
      sex: {}
  site_info:
    attributes:
      site_code:
        identifier: true
      site_name: {}
";

/// `TARGET_SCHEMA_YAML` with `participant_sex: {range: SexEnum}`, `site_name`
/// replaced by `age_label: {range: string}`, and the `SexEnum`.
const TARGET_SCHEMA: &str = r"
id: https://example.org/cross-table-target
name: cross_table_target
prefixes:
  linkml: https://w3id.org/linkml/
imports:
  - linkml:types
default_range: string
classes:
  MeasurementObservation:
    attributes:
      sample_id:
        identifier: true
      analyte_value: {range: float}
      age_at_observation: {range: integer}
      participant_sex: {range: SexEnum}
      age_label: {range: string}
enums:
  SexEnum:
    permissible_values:
      female: {aliases: [F]}
      male: {aliases: [M]}
";

const SPEC: &str = r"
class_derivations:
  MeasurementObservation:
    populated_from: lab_results
    joins:
      demographics:
        join_on: participant_id
    slot_derivations:
      sample_id:
        populated_from: sample_id
      participant_sex:
        populated_from: demographics.sex
      age_label:
        populated_from: demographics.age_at_exam
";

fn s(v: &str) -> Value {
    Value::Str(v.into())
}

fn map(pairs: Vec<(&str, Value)>) -> Value {
    Value::Map(pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
}

fn get(v: &Value, key: &str) -> Value {
    match v {
        Value::Map(m) => m.get(key).cloned().unwrap_or(Value::Null),
        other => panic!("expected a map, got {other:?}"),
    }
}

#[test]
fn populated_from_cross_table_is_typed_to_target() {
    let mut obj: serde_json::Value = serde_yaml::from_str(SPEC).unwrap();
    normalise_spec_json(&mut obj);
    let spec: TransformationSpecification = serde_json::from_value(obj).unwrap();
    let source = SchemaViewProvider::from_yaml_str(SOURCE_SCHEMA).unwrap();
    let target = SchemaViewProvider::from_yaml_str(TARGET_SCHEMA).unwrap();

    let mut index = LookupIndex::new();
    index.register_table(
        "demographics",
        &[
            map(vec![
                ("participant_id", s("P001")),
                ("age_at_exam", Value::Int(30)),
                ("sex", s("F")),
            ]),
            map(vec![
                ("participant_id", s("P002")),
                ("age_at_exam", Value::Int(45)),
                ("sex", s("M")),
            ]),
        ],
        "participant_id",
    );
    let tr = ObjectTransformer::new(spec, Some(&source), Some(&target))
        .with_lookup_index(Arc::new(index));

    let lab_results = [
        map(vec![
            ("sample_id", s("S001")),
            ("participant_id", s("P001")),
            ("analyte", s("glucose")),
            ("result_value", Value::Float(5.5)),
        ]),
        map(vec![
            ("sample_id", s("S002")),
            ("participant_id", s("P002")),
            ("analyte", s("cholesterol")),
            ("result_value", Value::Int(200)),
        ]),
    ];
    let results: Vec<(Value, Value)> = lab_results
        .iter()
        .map(|row| {
            let out = tr.map_object(row, Some("lab_results")).unwrap();
            (get(&out, "participant_sex"), get(&out, "age_label"))
        })
        .collect();
    assert_eq!(results, vec![(s("female"), s("30")), (s("male"), s("45"))]);
}
