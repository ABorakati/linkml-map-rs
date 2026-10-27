//! Target-schema-driven coercion against real LinkML schemas.
//!
//! Ports upstream `tests/test_transformer/test_target_schema_range.py`
//! (0469da7) and `test_derived_value_coercion.py` (11b425b): with a target
//! schema present, a slot value is coerced to the target slot's range when the
//! spec declares none; an explicit spec range wins; without a target schema
//! only spec ranges apply.

use linkml_map_core::{
    datamodel::{TransformationSpecification, normalise_spec_json},
    decimal::Decimal,
    engine::ObjectTransformer,
    schema::SchemaProvider,
    value::Value,
};
use linkml_map_schemaview::SchemaViewProvider;

const SOURCE_SCHEMA: &str = r"
id: https://example.org/range-source
name: range_source
prefixes: {linkml: 'https://w3id.org/linkml/'}
default_prefix: range_source
default_range: string
imports: [linkml:types]
classes:
  Row:
    attributes:
      id: {identifier: true}
      count: {}
      ratio: {}
      amount: {}
      flag: {}
      score: {}
      color: {}
      site: {}
      note: {}
      counts: {multivalued: true}
";

const TARGET_SCHEMA: &str = r"
id: https://example.org/range-target
name: range_target
prefixes: {linkml: 'https://w3id.org/linkml/'}
default_prefix: range_target
default_range: string
imports: [linkml:types]
types:
  Score:
    typeof: decimal
enums:
  Color:
    permissible_values:
      red: {}
classes:
  Site:
    attributes:
      id: {identifier: true}
  Measurement:
    attributes:
      id: {identifier: true}
      count: {range: integer}
      ratio: {range: float}
      ratio_text: {range: float}
      amount: {range: decimal}
      flag: {range: boolean}
      score: {range: Score}
      color: {range: Color}
      site: {range: Site}
      note: {}
      counts: {range: integer, multivalued: true}
";

const SPEC: &str = r"
id: target-schema-range
class_derivations:
  Measurement:
    populated_from: Row
    slot_derivations:
      id:
        populated_from: id
      count:
        populated_from: count
      ratio:
        populated_from: ratio
      ratio_text:
        populated_from: ratio
        range: string
      amount:
        populated_from: amount
      flag:
        populated_from: flag
      score:
        populated_from: score
      color:
        populated_from: color
      site:
        populated_from: site
      note:
        populated_from: note
      counts:
        populated_from: counts
";

pub fn spec_from_yaml(yaml: &str) -> TransformationSpecification {
    let mut obj: serde_json::Value = serde_yaml::from_str(yaml).unwrap();
    normalise_spec_json(&mut obj);
    serde_json::from_value(obj).unwrap()
}

fn s(v: &str) -> Value {
    Value::Str(v.into())
}

fn dec(text: &str) -> Value {
    Value::Decimal(Decimal::parse_python(text).unwrap())
}

fn sci(v: &Value) -> String {
    match v {
        Value::Decimal(d) => d.to_sci_string(),
        other => panic!("expected a decimal, got {other:?}"),
    }
}

fn row() -> Value {
    Value::Map(
        [
            ("id", s("r1")),
            ("count", s("7")),
            ("ratio", s("1.5")),
            ("amount", s("52.30")),
            ("flag", s("false")),
            ("score", s("0.10")),
            ("color", s("red")),
            ("site", s("S1")),
            ("note", Value::Int(42)),
            ("counts", Value::List(vec![s("1"), s("2")])),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect(),
    )
}

fn map_object(with_target: bool, input: &Value) -> Result<Value, String> {
    let source = SchemaViewProvider::from_yaml_str(SOURCE_SCHEMA).unwrap();
    let target = SchemaViewProvider::from_yaml_str(TARGET_SCHEMA).unwrap();
    let target_ref: Option<&dyn SchemaProvider> = with_target.then_some(&target);
    ObjectTransformer::new(spec_from_yaml(SPEC), Some(&source), target_ref)
        .map_object(input, Some("Row"))
        .map_err(|e| e.to_string())
}

fn get(v: &Value, key: &str) -> Value {
    match v {
        Value::Map(m) => m.get(key).cloned().unwrap_or(Value::Null),
        other => panic!("expected a map, got {other:?}"),
    }
}

#[test]
fn values_take_the_target_schema_range() {
    let out = map_object(true, &row()).unwrap();
    assert_eq!(get(&out, "count"), Value::Int(7));
    assert_eq!(get(&out, "ratio"), Value::Float(1.5));
    assert_eq!(get(&out, "amount"), dec("52.30"));
    assert_eq!(sci(&get(&out, "amount")), "52.30");
    // `score: Score` where `Score typeof decimal`.
    assert_eq!(sci(&get(&out, "score")), "0.10");
    assert_eq!(get(&out, "flag"), Value::Bool(false));
    assert_eq!(get(&out, "note"), s("42"));
    assert_eq!(
        get(&out, "counts"),
        Value::List(vec![Value::Int(1), Value::Int(2)])
    );
}

#[test]
fn spec_range_overrides_target_schema() {
    let out = map_object(true, &row()).unwrap();
    assert_eq!(get(&out, "ratio_text"), s("1.5"));
}

#[test]
fn class_and_enum_ranges_pass_through() {
    let out = map_object(true, &row()).unwrap();
    assert_eq!(get(&out, "color"), s("red"));
    assert_eq!(get(&out, "site"), s("S1"));
}

#[test]
fn no_target_schema_leaves_values_alone() {
    let out = map_object(false, &row()).unwrap();
    assert_eq!(get(&out, "count"), s("7"));
    assert_eq!(get(&out, "flag"), s("false"));
    assert_eq!(get(&out, "note"), Value::Int(42));
    assert_eq!(get(&out, "ratio_text"), s("1.5"));
}

#[test]
fn boolean_strings_are_parsed() {
    for (source, expected) in [
        (s("true"), true),
        (s("FALSE"), false),
        (s("1"), true),
        (s("0"), false),
        (Value::Bool(true), true),
        (Value::Int(0), false),
    ] {
        let input = Value::Map(
            [
                ("id".to_owned(), s("r1")),
                ("flag".to_owned(), source.clone()),
            ]
            .into_iter()
            .collect(),
        );
        let out = map_object(true, &input).unwrap();
        assert_eq!(get(&out, "flag"), Value::Bool(expected), "{source:?}");
    }
}

#[test]
fn unparseable_boolean_fails() {
    let input = Value::Map(
        [("id".to_owned(), s("r1")), ("flag".to_owned(), s("maybe"))]
            .into_iter()
            .collect(),
    );
    let err = map_object(true, &input).unwrap_err();
    assert!(err.contains("to boolean"), "{err}");
}
