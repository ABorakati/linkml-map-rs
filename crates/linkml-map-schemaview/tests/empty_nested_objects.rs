//! A nested object whose slots all come out None is omitted from its parent
//! (upstream 09667a6, refs #264; port of
//! `tests/test_transformer/test_empty_nested_objects.py` and the
//! `test_missing_values_applies_within_nested_class_derivation` change in
//! `test_slot_derivation_features.py`).
//!
//! With no object left, a single-valued slot is null and a multivalued slot `[]`.
//! The expected values are upstream v0.6.0's.

use std::sync::Arc;

use linkml_map_core::{
    datamodel::{TransformationSpecification, normalise_spec_json},
    engine::{LookupIndex, ObjectTransformer},
    value::Value,
};
use linkml_map_schemaview::SchemaViewProvider;

const SOURCE: &str = r"
id: https://example.org/s
name: s
prefixes:
  linkml: https://w3id.org/linkml/
imports:
  - linkml:types
default_range: string
classes:
  people:
    attributes:
      pid:
        identifier: true
      code1:
      code2:
  deaths:
    attributes:
      death_pid:
        identifier: true
      code1:
      code2:
";

const TARGET: &str = r"
id: https://example.org/t
name: t
prefixes:
  linkml: https://w3id.org/linkml/
imports:
  - linkml:types
default_range: string
classes:
  Person:
    attributes:
      id:
        identifier: true
      cause_of_death:
        range: CauseOfDeath
        multivalued: true
        inlined_as_list: true
      primary_cause:
        range: CauseOfDeath
        inlined: true
      arm:
        range: Arm
        inlined: true
  CauseOfDeath:
    attributes:
      cause:
      order:
        range: integer
  Arm:
    attributes:
      name:
";

fn s(v: &str) -> Value {
    Value::Str(v.into())
}

fn map(pairs: Vec<(&str, Value)>) -> Value {
    Value::Map(pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
}

fn spec(yaml: &str) -> TransformationSpecification {
    let mut obj: serde_json::Value = serde_yaml::from_str(yaml).unwrap();
    normalise_spec_json(&mut obj);
    serde_json::from_value(obj).unwrap()
}

/// `_cause`: a CauseOfDeath whose slots are all None when `column` is empty.
fn cause(column: &str, order: u8, joined: bool) -> String {
    let joins = if joined {
        "\n          populated_from: people\n          joins:\n            deaths: {source_key: pid, lookup_key: death_pid}"
    } else {
        ""
    };
    format!(
        r#"
      - CauseOfDeath:{joins}
          slot_derivations:
            cause: {{expr: 'case(({{{column}}} is not None, "ICD9CM:" + str({{{column}}})))'}}
            order: {{expr: 'case(({{{column}}} is not None, {order}))'}}"#
    )
}

/// `_person`: Person slots reading the code columns from the row, or from the
/// `deaths` table through a join.
fn person_spec(joined: bool) -> TransformationSpecification {
    let prefix = if joined { "deaths." } else { "" };
    spec(&format!(
        r"
id: empty-nested
class_derivations:
  Person:
    populated_from: people
    slot_derivations:
      id: {{populated_from: pid}}
      cause_of_death:
        class_derivations:{c1}{c2}
      primary_cause:
        class_derivations:{p1}
      arm:
        class_derivations:
          - Arm:
              slot_derivations:
                name: {{value: control}}
",
        c1 = indent(&cause(&format!("{prefix}code1"), 1, joined)),
        c2 = indent(&cause(&format!("{prefix}code2"), 2, joined)),
        p1 = indent(&cause(&format!("{prefix}code1"), 1, joined)),
    ))
}

fn indent(block: &str) -> String {
    block.replace('\n', "\n    ")
}

fn rows() -> Vec<Value> {
    vec![
        map(vec![
            ("pid", s("P1")),
            ("code1", s("410")),
            ("code2", s("428")),
        ]),
        map(vec![
            ("pid", s("P2")),
            ("code1", s("410")),
            ("code2", Value::Null),
        ]),
        map(vec![
            ("pid", s("P3")),
            ("code1", Value::Null),
            ("code2", Value::Null),
        ]),
    ]
}

fn cause_of(code: &str, order: i64) -> Value {
    map(vec![
        ("cause", s(&format!("ICD9CM:{code}"))),
        ("order", Value::Int(order)),
    ])
}

fn expected() -> Vec<Value> {
    let arm = || map(vec![("name", s("control"))]);
    vec![
        map(vec![
            ("id", s("P1")),
            (
                "cause_of_death",
                Value::List(vec![cause_of("410", 1), cause_of("428", 2)]),
            ),
            ("primary_cause", cause_of("410", 1)),
            ("arm", arm()),
        ]),
        map(vec![
            ("id", s("P2")),
            ("cause_of_death", Value::List(vec![cause_of("410", 1)])),
            ("primary_cause", cause_of("410", 1)),
            ("arm", arm()),
        ]),
        map(vec![
            ("id", s("P3")),
            ("cause_of_death", Value::List(vec![])),
            ("primary_cause", Value::Null),
            ("arm", arm()),
        ]),
    ]
}

#[test]
fn single_object_path() {
    // `map_object` on one row drops the empty nested objects.
    let source = SchemaViewProvider::from_yaml_str(SOURCE).unwrap();
    let target = SchemaViewProvider::from_yaml_str(TARGET).unwrap();
    let tr = ObjectTransformer::new(person_spec(false), Some(&source), Some(&target));
    let out: Vec<Value> = rows()
        .iter()
        .map(|row| tr.map_object(row, Some("people")).unwrap())
        .collect();
    assert_eq!(out, expected());
}

#[test]
fn joined_path() {
    // Reading the codes from a joined `deaths` table (rows only for the people with a
    // code1) drops the same empty nested objects; P3 has no `deaths` row at all.
    let source = SchemaViewProvider::from_yaml_str(SOURCE).unwrap();
    let target = SchemaViewProvider::from_yaml_str(TARGET).unwrap();
    let deaths: Vec<Value> = rows()
        .iter()
        .filter_map(|row| {
            let Value::Map(r) = row else { unreachable!() };
            (!r["code1"].is_null()).then(|| {
                map(vec![
                    ("death_pid", r["pid"].clone()),
                    ("code1", r["code1"].clone()),
                    ("code2", r["code2"].clone()),
                ])
            })
        })
        .collect();
    let mut index = LookupIndex::new();
    index.register_table("deaths", &deaths, "death_pid");
    let tr = ObjectTransformer::new(person_spec(true), Some(&source), Some(&target))
        .with_lookup_index(Arc::new(index));
    let out: Vec<Value> = rows()
        .iter()
        .map(|row| tr.map_object(row, Some("people")).unwrap())
        .collect();
    assert_eq!(out, expected());
}

#[test]
fn minted_id_does_not_keep_an_empty_object() {
    // A content-hash id is minted from the slots, so an object with no other content
    // is still dropped.
    let minted_target = TARGET
        .replace(
            "  Arm:\n",
            "  Death:\n    attributes:\n      id:\n        identifier: true\n      cause:\n  Arm:\n",
        )
        .replace(
            "      arm:\n",
            "      death:\n        range: Death\n        inlined: true\n      arm:\n",
        );
    let source = SchemaViewProvider::from_yaml_str(SOURCE).unwrap();
    let target = SchemaViewProvider::from_yaml_str(&minted_target).unwrap();
    let sp = spec(
        r"
id: minted
class_derivations:
  Person:
    populated_from: people
    slot_derivations:
      id: {populated_from: pid}
      death:
        class_derivations:
          - Death:
              mint_ids: true
              slot_derivations:
                cause: {expr: 'case(({code1} is not None, {code1}))'}
",
    );
    let tr = ObjectTransformer::new(sp, Some(&source), Some(&target));
    let rows = rows();
    let with_cause = tr.map_object(&rows[0], Some("people")).unwrap();
    let without = tr.map_object(&rows[2], Some("people")).unwrap();
    let Value::Map(with_cause) = with_cause else {
        panic!("expected a map")
    };
    let Some(Value::Map(death)) = with_cause.get("death") else {
        panic!("expected a death object: {with_cause:?}")
    };
    assert_eq!(death.get("cause"), Some(&s("410")));
    assert!(matches!(death.get("id"), Some(Value::Str(id)) if !id.is_empty()));
    let Value::Map(without) = without else {
        panic!("expected a map")
    };
    assert_eq!(without.get("death"), Some(&Value::Null));
}

// ── what counts as empty (probed against upstream v0.6.0) ────────────────────

const BOX_SOURCE: &str = r"
id: https://example.org/s
name: s
prefixes: {linkml: https://w3id.org/linkml/}
imports: [linkml:types]
default_range: string
classes:
  Person:
    attributes:
      id: {identifier: true}
      code: {}
";

const BOX_TARGET: &str = r"
id: https://example.org/t
name: t
prefixes: {linkml: https://w3id.org/linkml/}
imports: [linkml:types]
default_range: string
classes:
  Out:
    attributes:
      id: {identifier: true}
      box: {range: Box, inlined: true}
      boxes: {range: Box, multivalued: true, inlined_as_list: true}
  Box:
    attributes:
      a: {}
      tags: {multivalued: true}
";

/// `(box, boxes)` of `Out` for a nested `Box` derivation with `box_yaml` as its body.
fn derive_box(box_yaml: &str) -> (Value, Value) {
    let sp = spec(&format!(
        r"
id: boxes
class_derivations:
  Out:
    populated_from: Person
    slot_derivations:
      id: {{populated_from: id}}
      box: {{class_derivations: [{{Box: {box_yaml}}}]}}
      boxes: {{class_derivations: [{{Box: {box_yaml}}}]}}
"
    ));
    let source = SchemaViewProvider::from_yaml_str(BOX_SOURCE).unwrap();
    let target = SchemaViewProvider::from_yaml_str(BOX_TARGET).unwrap();
    let out = ObjectTransformer::new(sp, Some(&source), Some(&target))
        .map_object(
            &map(vec![("id", s("p1")), ("code", s("c"))]),
            Some("Person"),
        )
        .unwrap();
    let Value::Map(m) = out else {
        panic!("expected a map")
    };
    (m["box"].clone(), m["boxes"].clone())
}

#[test]
fn an_object_with_no_slots_or_only_hidden_ones_is_empty() {
    // upstream's `all(...)` over an empty dict is true; a hidden slot is not in the
    // object `map_object` returns.
    for body in [
        "{}",
        "{slot_derivations: {a: {populated_from: code, hide: true}}}",
    ] {
        assert_eq!(
            derive_box(body),
            (Value::Null, Value::List(vec![])),
            "{body}"
        );
    }
}

#[test]
fn only_none_counts_as_no_value() {
    // An empty list, an empty string and a false are values (`v is None` only).
    let tags = map(vec![("tags", Value::List(vec![]))]);
    assert_eq!(
        derive_box("{slot_derivations: {tags: {expr: '[]'}}}"),
        (tags.clone(), Value::List(vec![tags]))
    );
    let empty = map(vec![("a", s(""))]);
    assert_eq!(
        derive_box("{slot_derivations: {a: {value: ''}}}"),
        (empty.clone(), Value::List(vec![empty]))
    );
    let false_ = map(vec![("a", s("False"))]);
    assert_eq!(
        derive_box("{slot_derivations: {a: {expr: 'False'}}}"),
        (false_.clone(), Value::List(vec![false_]))
    );
    assert_eq!(
        derive_box("{slot_derivations: {a: {expr: 'None'}}}"),
        (Value::Null, Value::List(vec![]))
    );
}

// ── test_slot_derivation_features.py::test_missing_values_applies_within_nested_class_derivation

const LOCATION_SOURCE: &str = r"
id: https://example.org/loc-source
name: loc_source
prefixes: {linkml: https://w3id.org/linkml/}
imports: [linkml:types]
default_range: string
classes:
  Person:
    attributes:
      id: {identifier: true}
    slots: [zip]
slots:
  zip: {range: integer}
";

const LOCATION_TARGET: &str = r"
id: https://example.org/loc-target
name: loc_target
prefixes: {linkml: https://w3id.org/linkml/}
imports: [linkml:types]
default_range: string
classes:
  Agent:
    attributes:
      id: {identifier: true}
    slots: [location]
  Location:
    slots: [zip, country]
slots:
  location: {range: Location, inlined: true}
  zip: {range: integer}
  country: {range: string}
";

fn derive_location(location_slots: &str) -> Value {
    let sp = spec(&format!(
        r"
id: loc
class_derivations:
  Agent:
    populated_from: Person
    slot_derivations:
      id: {{populated_from: id}}
      location:
        class_derivations:
          - Location:
              slot_derivations:
                zip: {{populated_from: zip, missing_values: [99999]}}{location_slots}
"
    ));
    let source = SchemaViewProvider::from_yaml_str(LOCATION_SOURCE).unwrap();
    let target = SchemaViewProvider::from_yaml_str(LOCATION_TARGET).unwrap();
    let out = ObjectTransformer::new(sp, Some(&source), Some(&target))
        .map_object(
            &map(vec![("id", s("p1")), ("zip", Value::Int(99999))]),
            Some("Person"),
        )
        .unwrap();
    let Value::Map(m) = out else {
        panic!("expected a map")
    };
    m["location"].clone()
}

#[test]
fn missing_values_applies_within_nested_class_derivation() {
    // The `country` constant keeps the object; its `zip` is nulled by `missing_values`.
    let location = derive_location("\n                country: {value: US}");
    assert_eq!(
        location,
        map(vec![("zip", Value::Null), ("country", s("US"))])
    );
}

#[test]
fn a_nested_object_nulled_entirely_by_missing_values_is_dropped() {
    // Without `country` (upstream's test before 09667a6) nothing is left of the object.
    assert_eq!(derive_location(""), Value::Null);
}
