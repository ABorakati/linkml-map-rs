//! A dotted `populated_from` (`Class.slot`, `slot.sub`, `fk_slot.slot`) is coerced
//! to the target slot like any other `populated_from`: upstream
//! `ObjectTransformer._derive_slot` coerces whenever the resolution hands back a
//! source slot, and every dotted path but a join does.
//!
//! The expected values were produced by upstream `linkml-map` v0.6.0-rc1 on the same
//! schemas, specs and rows (`tests/differential/corpus/dotted_*` replays the CLI cases).

use linkml_map_core::{
    datamodel::{TransformationSpecification, normalise_spec_json},
    engine::ObjectTransformer,
    schema::SchemaProvider,
    value::Value,
};
use linkml_map_schemaview::SchemaViewProvider;

const SOURCE_SCHEMA: &str = r"
id: https://example.org/dotted-source
name: dotted_source
prefixes: {linkml: 'https://w3id.org/linkml/'}
default_prefix: dotted_source
default_range: string
imports: [linkml:types]
enums:
  Kind:
    permissible_values: {a: {}, b: {}}
classes:
  Container:
    tree_root: true
    attributes:
      persons: {range: Person, multivalued: true, inlined_as_list: true}
      orgs: {range: Org, multivalued: true, inlined_as_list: true}
  Address:
    attributes:
      zip: {}
      zips: {multivalued: true}
      kind: {range: Kind}
  Org:
    attributes:
      id: {identifier: true}
      name: {}
      size: {}
  Person:
    attributes:
      id: {identifier: true}
      code: {}
      codes: {multivalued: true}
      kind: {range: Kind}
      addr: {range: Address, inlined: true}
      org_id: {range: Org}
";

const TARGET_SCHEMA: &str = r"
id: https://example.org/dotted-target
name: dotted_target
prefixes: {linkml: 'https://w3id.org/linkml/'}
default_prefix: dotted_target
default_range: string
imports: [linkml:types]
enums:
  TKind:
    permissible_values: {x: {}, y: {}}
classes:
  Out:
    attributes:
      id: {identifier: true}
      num: {range: integer}
      nums: {range: integer, multivalued: true}
      real: {range: float}
      text: {}
      kind: {range: TKind}
";

fn s(v: &str) -> Value {
    Value::Str(v.into())
}

fn map(pairs: Vec<(&str, Value)>) -> Value {
    Value::Map(pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
}

fn person() -> Value {
    map(vec![
        ("id", s("p1")),
        ("code", s("12")),
        ("codes", Value::List(vec![s("1"), s("2")])),
        ("kind", s("a")),
        ("org_id", s("o1")),
        (
            "addr",
            map(vec![
                ("zip", s("34")),
                ("zips", Value::List(vec![s("5"), s("6")])),
                ("kind", s("b")),
            ]),
        ),
    ])
}

fn spec(slot: &str, populated_from: &str, range: Option<&str>) -> TransformationSpecification {
    let range = range.map(|r| format!(", range: {r}")).unwrap_or_default();
    let yaml = format!(
        r"
id: dotted
enum_derivations:
  TKind:
    populated_from: Kind
    permissible_value_derivations:
      x: {{populated_from: a}}
      y: {{populated_from: b}}
class_derivations:
  Out:
    populated_from: Person
    slot_derivations:
      id: {{populated_from: id}}
      {slot}: {{populated_from: {populated_from}{range}}}
"
    );
    let mut obj: serde_json::Value = serde_yaml::from_str(&yaml).unwrap();
    normalise_spec_json(&mut obj);
    serde_json::from_value(obj).unwrap()
}

/// Derive `slot` from `populated_from` on `person()`, with or without the target schema.
fn derive(
    slot: &str,
    populated_from: &str,
    range: Option<&str>,
    with_target: bool,
) -> Result<Value, String> {
    let source = SchemaViewProvider::from_yaml_str(SOURCE_SCHEMA).unwrap();
    let target = SchemaViewProvider::from_yaml_str(TARGET_SCHEMA).unwrap();
    let target_ref: Option<&dyn SchemaProvider> = with_target.then_some(&target);
    let out = ObjectTransformer::new(spec(slot, populated_from, range), Some(&source), target_ref)
        .map_object(&person(), Some("Person"))
        .map_err(|e| e.to_string())?;
    let Value::Map(m) = out else {
        panic!("expected a map")
    };
    Ok(m.get(slot).cloned().unwrap_or(Value::Null))
}

#[test]
fn source_class_path_is_coerced_to_the_spec_range() {
    // upstream: `populated_from: Person.code`, `range: integer` -> 12 (with or without a target).
    for with_target in [false, true] {
        assert_eq!(
            derive("num", "Person.code", Some("integer"), with_target).unwrap(),
            Value::Int(12)
        );
    }
}

#[test]
fn source_class_path_is_coerced_to_the_target_slot_datatype() {
    // No spec `range`: the target schema's `Out.num: integer` decides.
    assert_eq!(
        derive("num", "Person.code", None, true).unwrap(),
        Value::Int(12)
    );
    // Without a target schema nothing coerces.
    assert_eq!(derive("num", "Person.code", None, false).unwrap(), s("12"));
}

#[test]
fn source_class_path_follows_the_source_slots_cardinality() {
    // upstream: a multivalued source slot into the multivalued `nums` -> [1, 2] ...
    assert_eq!(
        derive("nums", "Person.codes", Some("integer"), true).unwrap(),
        Value::List(vec![Value::Int(1), Value::Int(2)])
    );
    // ... and into the single-valued `num` it cannot be coerced (the target's cardinality).
    let err = derive("num", "Person.codes", Some("integer"), true).unwrap_err();
    assert!(
        err.contains("Cannot coerce multiple values"),
        "unexpected error: {err}"
    );
}

#[test]
fn source_class_path_maps_enum_values() {
    // upstream: `Person.kind` (a) through the Kind -> TKind derivation -> x.
    assert_eq!(
        derive("kind", "Person.kind", Some("TKind"), true).unwrap(),
        s("x")
    );
}

#[test]
fn source_class_path_to_an_unknown_slot_is_an_error() {
    // `induced_slot(field_path, source_type)` raises `ValueError`.
    let err = derive("num", "Person.nosuch", Some("integer"), true).unwrap_err();
    assert!(
        err.contains("No such slot nosuch"),
        "unexpected error: {err}"
    );
}

#[test]
fn inline_path_is_coerced_to_the_target_slot() {
    assert_eq!(
        derive("num", "addr.zip", Some("integer"), false).unwrap(),
        Value::Int(34)
    );
    assert_eq!(
        derive("num", "addr.zip", None, true).unwrap(),
        Value::Int(34)
    );
    assert_eq!(
        derive("real", "addr.zip", Some("float"), true).unwrap(),
        Value::Float(34.0)
    );
    // The final slot is a string: the value stays one.
    assert_eq!(
        derive("text", "addr.zip", Some("string"), true).unwrap(),
        s("34")
    );
}

#[test]
fn inline_path_maps_enum_values() {
    // upstream: `addr.kind` (b) through the enum derivation -> y.
    assert_eq!(
        derive("kind", "addr.kind", Some("TKind"), true).unwrap(),
        s("y")
    );
}

#[test]
fn inline_path_to_a_missing_leaf_is_null() {
    assert_eq!(
        derive("num", "addr.nosuch", Some("integer"), true).unwrap(),
        Value::Null
    );
}

#[test]
fn dotted_path_that_is_no_join_inline_or_fk_is_an_error() {
    // `_resolve_fk_or_literal(.., require_fk=True)`.
    let err = derive("num", "nosuch.code", Some("integer"), true).unwrap_err();
    assert!(
        err.contains(
            "Dot-notation 'nosuch.code' in populated_from requires a matching join spec or FK path"
        ),
        "unexpected error: {err}"
    );
}

// ── foreign-key paths ───────────────────────────────────────────────────────

fn container() -> Value {
    let person =
        |id: &str, org: &str| map(vec![("id", s(id)), ("org_id", s(org)), ("code", s("7"))]);
    map(vec![
        (
            "persons",
            Value::List(vec![person("p1", "o1"), person("p2", "o9")]),
        ),
        (
            "orgs",
            Value::List(vec![map(vec![
                ("id", s("o1")),
                ("name", s("Acme")),
                ("size", s("5")),
            ])]),
        ),
    ])
}

/// Map `container()`'s persons through `slot: {populated_from, range}`; the output's `slot` per person.
fn derive_fk(slot: &str, populated_from: &str, range: &str) -> Vec<Value> {
    let yaml = format!(
        r"
id: dotted-fk
class_derivations:
  Container:
    populated_from: Container
    slot_derivations:
      persons: {{populated_from: persons}}
  Out:
    populated_from: Person
    slot_derivations:
      id: {{populated_from: id}}
      {slot}: {{populated_from: {populated_from}, range: {range}}}
"
    );
    let mut obj: serde_json::Value = serde_yaml::from_str(&yaml).unwrap();
    normalise_spec_json(&mut obj);
    let spec: TransformationSpecification = serde_json::from_value(obj).unwrap();
    let source = SchemaViewProvider::from_yaml_str(SOURCE_SCHEMA).unwrap();
    let out = ObjectTransformer::new(spec, Some(&source), None)
        .map_container(&container(), Some("Container"))
        .unwrap();
    let Value::Map(m) = out else {
        panic!("expected a map")
    };
    let Some(Value::List(persons)) = m.get("persons") else {
        panic!("expected persons")
    };
    persons
        .iter()
        .map(|p| match p {
            Value::Map(p) => p.get(slot).cloned().unwrap_or(Value::Null),
            other => panic!("{other:?}"),
        })
        .collect()
}

#[test]
fn fk_path_reads_the_referenced_object() {
    // upstream: `populated_from: org_id.name` -> the referenced org's name; a
    // dangling reference (o9) is null.
    assert_eq!(
        derive_fk("name", "org_id.name", "string"),
        vec![s("Acme"), Value::Null]
    );
}

#[test]
fn fk_path_is_coerced_to_the_final_slots_target() {
    // upstream: `org_id.size` ('5' in the org) with `range: integer` -> 5.
    assert_eq!(
        derive_fk("num", "org_id.size", "integer"),
        vec![Value::Int(5), Value::Null]
    );
}

#[test]
fn fk_path_without_an_index_is_null() {
    // upstream warns ("Cross-class lookup requires object_index") and yields None.
    let source = SchemaViewProvider::from_yaml_str(SOURCE_SCHEMA).unwrap();
    let out = ObjectTransformer::new(
        spec("num", "org_id.size", Some("integer")),
        Some(&source),
        None,
    )
    .map_object(&person(), Some("Person"))
    .unwrap();
    let Value::Map(m) = out else {
        panic!("expected a map")
    };
    assert_eq!(m.get("num"), Some(&Value::Null));
}

// ── join paths ──────────────────────────────────────────────────────────────

/// Derive `slot` from the joined `Org` row of `person()` with a `populated_from` of
/// `Org.<field>`.
fn derive_join(slot: &str, field: &str, range: &str) -> Value {
    use linkml_map_core::engine::LookupIndex;
    use std::sync::Arc;

    let yaml = format!(
        r"
id: dotted-join
enum_derivations:
  TKind:
    populated_from: Kind
    permissible_value_derivations:
      x: {{populated_from: a}}
      y: {{populated_from: b}}
class_derivations:
  Out:
    populated_from: Person
    joins:
      Org: {{class_named: Org, source_key: org_id, lookup_key: id}}
    slot_derivations:
      id: {{populated_from: id}}
      {slot}: {{populated_from: Org.{field}, range: {range}}}
"
    );
    let mut obj: serde_json::Value = serde_yaml::from_str(&yaml).unwrap();
    normalise_spec_json(&mut obj);
    let spec: TransformationSpecification = serde_json::from_value(obj).unwrap();
    let source = SchemaViewProvider::from_yaml_str(SOURCE_SCHEMA).unwrap();
    let target = SchemaViewProvider::from_yaml_str(TARGET_SCHEMA).unwrap();
    let mut index = LookupIndex::new();
    index.register_table(
        "Org",
        &[map(vec![
            ("id", s("o1")),
            ("size", s("5")),
            ("kind", s("b")),
        ])],
        "id",
    );
    let out = ObjectTransformer::new(spec, Some(&source), Some(&target))
        .with_lookup_index(Arc::new(index))
        .map_object(&person(), Some("Person"))
        .unwrap();
    let Value::Map(m) = out else {
        panic!("expected a map")
    };
    m.get(slot).cloned().unwrap_or(Value::Null)
}

#[test]
fn join_path_is_not_coerced() {
    // PARITY: upstream's `_perform_join_resolution` finds no source slot (its
    // `field_path in class_induced_slots(..)` compares a string with slot objects),
    // so a joined value is never coerced. The same joins, run through the upstream
    // CLI (`corpus/*` `joinenum` case), leave the enum value `b` unmapped.
    assert_eq!(derive_join("num", "size", "integer"), s("5"));
    assert_eq!(derive_join("kind", "kind", "TKind"), s("b"));
}
