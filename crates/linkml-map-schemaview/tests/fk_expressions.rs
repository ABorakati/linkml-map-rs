//! A foreign key in an `expr:` and in a `populated_from:` path, as upstream's `ObjectIndex` /
//! `ProxyObject` resolve it (`linkml_runtime.index.object_index`).
//!
//! Every expected value was produced by upstream `linkml-map` v0.6.0-rc1 on the same schema, spec
//! and data (`tests/differential/corpus/*` replays the CLI cases).

use linkml_map_core::{
    datamodel::{TransformationSpecification, normalise_spec_json},
    engine::ObjectTransformer,
    value::Value,
};
use linkml_map_schemaview::SchemaViewProvider;

const SCHEMA: &str = r"
id: https://example.org/fk
name: fk
prefixes: {linkml: 'https://w3id.org/linkml/'}
default_prefix: fk
default_range: string
imports: [linkml:types]
classes:
  Container:
    tree_root: true
    attributes:
      persons: {range: Person, multivalued: true, inlined: true}
      orgs: {range: Org, multivalued: true, inlined: true}
  Org:
    attributes:
      id: {identifier: true}
      label: {}
  Person:
    attributes:
      id: {identifier: true}
      label: {}
      boss: {range: Person}
      friends: {range: Person, multivalued: true}
      org: {range: Org}
  Counter:
    attributes:
      id: {identifier: true, range: integer}
      label: {}
      next: {range: Counter}
  CounterSet:
    tree_root: true
    attributes:
      counters: {range: Counter, multivalued: true, inlined_as_list: true}
";

fn provider() -> SchemaViewProvider {
    SchemaViewProvider::from_yaml_str(SCHEMA).unwrap()
}

fn data(yaml: &str) -> Value {
    serde_yaml::from_str(yaml).unwrap()
}

/// The spec: the container's slot copied as is, and `slots` (the body of a `Person` derivation).
fn spec(source: &str, collection: &str, slots: &str) -> TransformationSpecification {
    let yaml = format!(
        r"
id: fk
class_derivations:
  Out:
    populated_from: {source}
    slot_derivations:
      {collection}: {{populated_from: {collection}}}
  Agent:
    populated_from: {item}
    slot_derivations:
      id: {{populated_from: id}}
{slots}
",
        item = if source == "Container" {
            "Person"
        } else {
            "Counter"
        },
    );
    let mut obj: serde_json::Value = serde_yaml::from_str(&yaml).unwrap();
    normalise_spec_json(&mut obj);
    serde_json::from_value(obj).unwrap()
}

/// `slot_yaml` over `input`, mapped as `tr.index(input, source); tr.map_object(input, source)`;
/// the transformed items' `x`.
fn derive(
    source: &str,
    collection: &str,
    input: &str,
    slot_yaml: &str,
) -> Result<Vec<Value>, String> {
    let sv = provider();
    let spec = spec(source, collection, &format!("      x: {slot_yaml}"));
    let out = ObjectTransformer::new(spec, Some(&sv), None)
        .map_container(&data(input), Some(source))
        .map_err(|e| e.to_string())?;
    let Value::Map(m) = out else {
        panic!("expected a map, got {out:?}")
    };
    let Some(Value::List(items)) = m.get(collection) else {
        panic!("expected {collection}: {m:?}")
    };
    Ok(items
        .iter()
        .map(|p| match p {
            Value::Map(p) => p.get("x").cloned().unwrap_or(Value::Null),
            other => panic!("{other:?}"),
        })
        .collect())
}

fn s(v: &str) -> Value {
    Value::Str(v.into())
}

const PEOPLE: &str = "
persons:
  - {id: A, label: LA, boss: B, org: O1}
  - {id: B, label: LB, boss: C, org: O1}
  - {id: C, label: LC, boss: A}
orgs:
  - {id: O1, label: Acme}
";

#[test]
fn expression_reads_the_referenced_object() {
    assert_eq!(
        derive("Container", "persons", PEOPLE, "{expr: '{boss.label}'}").unwrap(),
        vec![s("LB"), s("LC"), s("LA")]
    );
    assert_eq!(
        derive("Container", "persons", PEOPLE, "{expr: '{org.label}'}").unwrap(),
        vec![s("Acme"), s("Acme"), Value::Null]
    );
}

#[test]
fn expression_follows_a_chain_of_references() {
    // `ProxyObject.__getattr__` dereferences on every access: `boss.boss.label` is the boss's
    // boss's label, and a cycle (A -> B -> C -> A) is followed as far as the expression goes.
    assert_eq!(
        derive(
            "Container",
            "persons",
            PEOPLE,
            "{expr: '{boss.boss.label}'}"
        )
        .unwrap(),
        vec![s("LC"), s("LA"), s("LB")]
    );
    assert_eq!(
        derive(
            "Container",
            "persons",
            PEOPLE,
            "{expr: '{boss.boss.boss.label}'}"
        )
        .unwrap(),
        vec![s("LA"), s("LB"), s("LC")]
    );
    assert_eq!(
        derive("Container", "persons", PEOPLE, "{expr: '{boss.org.label}'}").unwrap(),
        vec![s("Acme"), Value::Null, s("Acme")]
    );
}

#[test]
fn dangling_reference_stays_the_identifier() {
    let input = "
persons:
  - {id: A, label: LA, boss: Z}
";
    let err = derive("Container", "persons", input, "{expr: '{boss.label}'}").unwrap_err();
    assert!(
        err.contains("'str' object has no attribute 'label'"),
        "{err}"
    );
}

#[test]
fn reference_to_another_class_does_not_resolve() {
    // The cache key is `(range class, identifier)`: `O1` is an Org, not a Person.
    let input = "
persons:
  - {id: A, label: LA, boss: O1}
orgs:
  - {id: O1, label: Acme}
";
    let err = derive("Container", "persons", input, "{expr: '{boss.label}'}").unwrap_err();
    assert!(
        err.contains("'str' object has no attribute 'label'"),
        "{err}"
    );
}

#[test]
fn nested_objects_are_indexed() {
    // `_index` walks the whole tree, not only the container's collections: an object inlined
    // below another object (`friends`) is found by a reference elsewhere (`boss: N`).
    let input = "
persons:
  - {id: A, label: LA, boss: N, friends: [{id: N, label: LN}]}
  - {id: B, label: LB, boss: N}
";
    assert_eq!(
        derive("Container", "persons", input, "{expr: '{boss.label}'}").unwrap(),
        vec![s("LN"), s("LN")]
    );
    assert_eq!(
        derive(
            "Container",
            "persons",
            input,
            "{populated_from: boss.label}"
        )
        .unwrap(),
        vec![s("LN"), s("LN")]
    );
}

#[test]
fn the_root_object_is_indexed() {
    // The root is registered under its own identifier: a person that is its own boss.
    let sv = provider();
    let spec = spec("Container", "persons", "      x: {expr: '{boss.label}'}");
    let own_boss = data("{id: A, label: LA, boss: A}");
    let out = ObjectTransformer::new(spec, Some(&sv), None)
        .map_container(&own_boss, Some("Person"))
        .unwrap();
    let Value::Map(m) = out else {
        panic!("expected a map")
    };
    assert_eq!(m.get("x"), Some(&s("LA")));
}

#[test]
fn identifier_lookup_is_by_exact_value() {
    // Python's dict key: the integer 2 is not the string "2".
    let input = "
counters:
  - {id: 1, label: one, next: '2'}
  - {id: 2, label: two, next: 1}
";
    let err = derive("CounterSet", "counters", input, "{expr: '{next.label}'}").unwrap_err();
    assert!(
        err.contains("'str' object has no attribute 'label'"),
        "{err}"
    );
}

#[test]
fn populated_from_path_keys_by_the_string_of_the_reference() {
    // `_perform_fk_resolution` looks up `(class, str(fk_value))`: a string identifier is found...
    assert_eq!(
        derive(
            "Container",
            "persons",
            PEOPLE,
            "{populated_from: boss.label}"
        )
        .unwrap(),
        vec![s("LB"), s("LC"), s("LA")]
    );
    // ...an integer identifier never is (the cache key holds the integer).
    let input = "
counters:
  - {id: 1, label: one, next: 2}
  - {id: 2, label: two, next: 1}
";
    assert_eq!(
        derive(
            "CounterSet",
            "counters",
            input,
            "{populated_from: next.label}"
        )
        .unwrap(),
        vec![Value::Null, Value::Null]
    );
    // ...whereas the expression, which looks the integer up as it is, finds it.
    assert_eq!(
        derive("CounterSet", "counters", input, "{expr: '{next.label}'}").unwrap(),
        vec![s("two"), s("one")]
    );
}

#[test]
fn unindexed_transformer_leaves_the_reference_alone() {
    // `ObjectTransformer.object_index` is None unless `index()` ran: `map_object` alone.
    let sv = provider();
    let spec = spec(
        "Container",
        "persons",
        "      x: {expr: '{boss.label}'}\n      y: {populated_from: boss.label}",
    );
    let person = data("{id: A, label: LA, boss: B}");
    let engine = ObjectTransformer::new(spec, Some(&sv), None);
    let err = engine.map_object(&person, Some("Person")).unwrap_err();
    assert!(
        err.to_string()
            .contains("'str' object has no attribute 'label'"),
        "{err}"
    );
}

#[test]
fn multivalued_reference_is_a_list_of_objects() {
    let input = "
persons:
  - {id: A, label: LA, friends: [B, C]}
  - {id: B, label: LB}
  - {id: C, label: LC}
";
    // A restricted expression distributes an attribute over a list.
    assert_eq!(
        derive("Container", "persons", input, "{expr: '{friends.label}'}").unwrap(),
        vec![
            Value::List(vec![s("LB"), s("LC")]),
            Value::Null,
            Value::Null
        ]
    );
}
