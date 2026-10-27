//! Ports of upstream `tests/test_schema_mapper/test_schema_mapper.py`, plus the
//! `derive-schema` behaviours the byte-for-byte replay corpus pins down.

use std::path::PathBuf;

use super::{SchemaMapper, SchemaView};
use crate::{
    datamodel::{TransformationSpecification, normalise_spec_json},
    pyyaml::{DumpOptions, dump_yaml},
    value::Value,
};

fn repo(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(path)
}

fn spec(yaml: &str) -> TransformationSpecification {
    let mut json: serde_json::Value = serde_yaml_ng::from_str(yaml).unwrap();
    normalise_spec_json(&mut json);
    serde_json::from_value(json).unwrap()
}

fn personinfo() -> SchemaMapper {
    SchemaMapper::from_path(&repo(
        "tests/golden/personinfo_basic/source/personinfo.yaml",
    ))
    .unwrap()
}

/// A mapper over an inline schema (what upstream builds with `SchemaBuilder`).
fn mapper(schema: &str) -> SchemaMapper {
    SchemaMapper::new(SchemaView::from_yaml_str(schema, None).unwrap())
}

fn key<'a>(v: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut cur = v;
    for p in path {
        let Value::Map(m) = cur else { return None };
        cur = m.get(*p)?;
    }
    Some(cur)
}

fn keys(v: &Value) -> Vec<String> {
    match v {
        Value::Map(m) => m.keys().cloned().collect(),
        _ => vec![],
    }
}

const THING_SCHEMA: &str = "
id: https://example.org/s1
name: s1
classes:
  Person:
    attributes:
      id: {identifier: true}
      name: {}
";

#[test]
fn test_derive_schema() {
    // upstream: test_derive_schema (SCHEMA1 + SPECIFICATION, i.e. personinfo-to-agent)
    let spec_path =
        repo("tests/golden/personinfo_basic/transform/personinfo-to-agent.transform.yaml");
    let spec = spec(&std::fs::read_to_string(spec_path).unwrap());
    let derived = personinfo().derive_schema(&spec).unwrap();
    for (class, slots) in [
        (
            "Agent",
            vec![
                "id",
                "age",
                "label",
                "has_familial_relationships",
                "primary_email",
                "gender",
                "current_address",
            ],
        ),
        ("FamilialRelationship", vec!["related_to", "type"]),
    ] {
        let attributes =
            key(&derived, &["classes", class, "attributes"]).unwrap_or_else(|| panic!("{class}"));
        for s in slots {
            assert!(keys(attributes).contains(&s.to_string()), "{class}.{s}");
        }
    }
    assert_eq!(
        key(&derived, &["classes", "Agent", "is_a"]),
        Some(&Value::Str("Entity".into()))
    );

    // ...and the committed example output (what `derive-schema` prints) is byte-identical.
    let yaml = dump_yaml(
        &derived,
        DumpOptions {
            sort_keys: false,
            ..Default::default()
        },
    );
    let expected = std::fs::read_to_string(repo(
        "tests/golden/personinfo_basic/output/personinfo_derived.yaml",
    ))
    .unwrap();
    assert_eq!(yaml, expected);
}

#[test]
fn test_null_specification() {
    // upstream: test_null_specification / test_null_specification_and_source
    let derived = personinfo().derive_schema(&spec("id: test")).unwrap();
    assert!(
        key(&derived, &["classes"]).is_none(),
        "an empty spec derives no classes"
    );
}

#[test]
fn test_definition_in_derivation() {
    // upstream: test_definition_in_derivation (schema entirely specified by the spec)
    let spec = spec(
        r#"
id: test
class_derivations:
  Thing:
    slot_derivations:
      id:
        target_definition: {identifier: "true", range: uriorcurie}
  Agent:
    slot_derivations:
      age:
        name: role
        target_definition: {range: integer}
    target_definition:
      description: A person or organization.
      is_a: Thing
"#,
    );
    let derived = personinfo().derive_schema(&spec).unwrap();
    assert_eq!(
        keys(key(&derived, &["classes"]).unwrap()),
        ["Thing", "Agent"]
    );
    let thing_attrs = key(&derived, &["classes", "Thing", "attributes"]).unwrap();
    assert_eq!(keys(thing_attrs), ["id"]);
    assert_eq!(
        key(thing_attrs, &["id", "range"]),
        Some(&Value::Str("uriorcurie".into()))
    );
    // `"true"` is coerced to a boolean by `Bool(...)`
    assert_eq!(
        key(thing_attrs, &["id", "identifier"]),
        Some(&Value::Bool(true))
    );
    assert_eq!(
        key(&derived, &["classes", "Agent", "is_a"]),
        Some(&Value::Str("Thing".into()))
    );
}

#[test]
fn test_hidden_slots_excluded_from_target_schema() {
    // upstream: test_hidden_slots_excluded_from_target_schema
    let spec = spec(
        r#"
id: test
class_derivations:
  Agent:
    slot_derivations:
      _intermediate:
        hide: true
        target_definition: {range: string}
      label:
        target_definition: {range: string}
"#,
    );
    let derived = personinfo().derive_schema(&spec).unwrap();
    let attrs = key(&derived, &["classes", "Agent", "attributes"]).unwrap();
    assert_eq!(keys(attrs), ["label"]);
}

#[test]
fn test_derive_partial() {
    // upstream: test_derive_partial
    let derived = personinfo()
        .derive_schema(&spec(
            "id: test\nclass_derivations:\n  Agent:\n    populated_from: Person\n",
        ))
        .unwrap();
    assert_eq!(keys(key(&derived, &["classes"]).unwrap()), ["Agent"]);
}

#[test]
fn test_rewire() {
    // upstream: test_rewire (SchemaBuilder schema, empty spec)
    let schema = "
id: https://example.org/rewire
name: rewire
slots:
  id: {range: string, identifier: true}
  name: {range: string}
  pets: {range: Pet, multivalued: true}
  salary: {range: integer}
classes:
  Thing: {slots: [id, name]}
  Person: {is_a: Thing, slots: [pets]}
  Employee: {is_a: Person, slots: [salary]}
  Pet: {is_a: Thing, slots: [pets]}
";
    let derived = mapper(schema).derive_schema(&spec("id: test")).unwrap();
    assert!(key(&derived, &["classes"]).is_none());
}

#[test]
fn rewire_follows_is_a_to_the_nearest_derived_ancestor() {
    let schema = "
id: https://example.org/rewire
name: rewire
classes:
  Thing: {}
  Person: {is_a: Thing}
  Employee: {is_a: Person}
";
    let derived = mapper(schema)
        .derive_schema(&spec("id: t\nclass_derivations:\n  Agent:\n    populated_from: Thing\n  Worker:\n    populated_from: Employee\n"))
        .unwrap();
    // Employee -> Person (not derived) -> Thing -> Agent
    assert_eq!(
        key(&derived, &["classes", "Worker", "is_a"]),
        Some(&Value::Str("Agent".into()))
    );
}

#[test]
fn rewire_to_two_targets_is_an_error() {
    let derived = mapper(THING_SCHEMA).derive_schema(&spec(
        "id: t\nclass_derivations:\n  A:\n    populated_from: Person\n  B:\n    populated_from: Person\n  Person:\n    populated_from: Person\n    is_a: Person\n",
    ));
    assert!(
        derived
            .unwrap_err()
            .0
            .contains("Cannot rewire to non-isomorphic mappings Person")
    );
}

#[test]
fn target_schema_id_name_and_default_prefix() {
    let derived = mapper(THING_SCHEMA).derive_schema(&spec("id: t")).unwrap();
    assert_eq!(
        key(&derived, &["id"]),
        Some(&Value::Str("https://example.org/s1-derived".into()))
    );
    assert_eq!(
        key(&derived, &["name"]),
        Some(&Value::Str("s1-derived".into()))
    );
    assert_eq!(
        key(&derived, &["default_prefix"]),
        Some(&Value::Str("https://example.org/s1-derived/".into()))
    );
    assert_eq!(keys(&derived)[..2], ["name", "id"]);
}

#[test]
fn stringification_sets_multivalued_both_ways() {
    let spec = spec(
        "
id: t
class_derivations:
  P:
    populated_from: Person
    slot_derivations:
      a: {populated_from: name, stringification: {reversed: true}}
      b: {populated_from: name, stringification: {delimiter: ','}}
",
    );
    let derived = mapper(THING_SCHEMA).derive_schema(&spec).unwrap();
    assert_eq!(
        key(
            &derived,
            &["classes", "P", "attributes", "a", "multivalued"]
        ),
        Some(&Value::Bool(true))
    );
    // an explicit `multivalued: false` is kept: only None / empty containers are dropped
    assert_eq!(
        key(
            &derived,
            &["classes", "P", "attributes", "b", "multivalued"]
        ),
        Some(&Value::Bool(false))
    );
}

#[test]
fn dictionary_key_marks_the_range_classes_slot_as_identifier() {
    let schema = "
id: https://example.org/s
name: s
classes:
  Holder: {attributes: {things: {range: Thing, multivalued: true}}}
  Thing: {attributes: {label: {range: string}}}
";
    let spec = spec(
        "
id: t
class_derivations:
  Holder:
    slot_derivations:
      things: {dictionary_key: label}
  Thing:
    slot_derivations:
      label: {}
",
    );
    // no `range` is set on the derived slot unless it comes from the source slot
    let derived = mapper(schema).derive_schema(&spec).unwrap();
    assert_eq!(
        key(
            &derived,
            &["classes", "Thing", "attributes", "label", "identifier"]
        ),
        Some(&Value::Bool(true))
    );
    assert_eq!(
        key(
            &derived,
            &[
                "classes",
                "Holder",
                "attributes",
                "things",
                "inlined_as_list"
            ]
        ),
        Some(&Value::Bool(false))
    );
}

#[test]
fn dictionary_key_on_an_underived_range_is_a_key_error() {
    let spec = spec(
        "
id: t
class_derivations:
  P:
    populated_from: Person
    slot_derivations:
      a: {populated_from: name, range: Missing, dictionary_key: x}
",
    );
    assert!(
        mapper(THING_SCHEMA)
            .derive_schema(&spec)
            .unwrap_err()
            .0
            .contains("KeyError")
    );
}

#[test]
fn copy_directives_copy_classes_by_reference_and_exclude_requires_a_prior_copy() {
    let copy_all = spec("id: t\ncopy_directives:\n  d: {element_name: d, copy_all: true}\n");
    let derived = mapper(THING_SCHEMA).derive_schema(&copy_all).unwrap();
    assert_eq!(keys(key(&derived, &["classes"]).unwrap()), ["Person"]);

    let exclude_only = spec("id: t\ncopy_directives:\n  d: {element_name: d, exclude: Person}\n");
    // PARITY: `del tgt_elements[element]` on something never copied raises KeyError.
    assert!(mapper(THING_SCHEMA).derive_schema(&exclude_only).is_err());

    // upstream's shape normalisation keeps only the first element of a list-valued `exclude`
    let listed = spec(
        "id: t\ncopy_directives:\n  d: {element_name: d, copy_all: true, exclude: [Nope, Person]}\n",
    );
    let derived = mapper(THING_SCHEMA).derive_schema(&listed).unwrap();
    assert_eq!(keys(key(&derived, &["classes"]).unwrap()), ["Person"]);
}

#[test]
fn duplicate_class_derivations_merge_attributes() {
    let spec = spec(
        "
id: t
class_derivations:
  - name: Both
    populated_from: Person
    slot_derivations: {id: {}}
  - name: Both
    populated_from: Person
    slot_derivations: {name: {}}
",
    );
    let derived = mapper(THING_SCHEMA).derive_schema(&spec).unwrap();
    assert_eq!(
        keys(key(&derived, &["classes", "Both", "attributes"]).unwrap()),
        ["id", "name"]
    );
}

#[test]
fn unknown_override_field_is_an_error() {
    let spec = spec(
        "
id: t
class_derivations:
  P:
    populated_from: Person
    overrides: {not_a_metamodel_field: 1}
",
    );
    assert!(
        mapper(THING_SCHEMA)
            .derive_schema(&spec)
            .unwrap_err()
            .0
            .contains("Unknown argument")
    );
}
