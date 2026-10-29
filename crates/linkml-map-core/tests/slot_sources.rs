//! Slot-level `sources:` — upstream `ObjectTransformer._resolve_sources`.
//!
//! The arm is live only for a slot derivation `induce_missing_values` leaves alone: one
//! inherited through `is_a` / `mixins`, one of a nested `class_derivations`, or an own one
//! whose `populated_from` / `expr` is the empty string. Elsewhere it is dead, because the
//! induced `populated_from = <slot name>` wins. The differential corpus pins the same
//! behaviour byte for byte (`tests/differential/corpus`, notes "slot-level sources").

use linkml_map_core::datamodel::{TransformationSpecification, normalise_spec_json};
use linkml_map_core::engine::ObjectTransformer;
use linkml_map_core::schema::{
    ClassDef, InMemorySchema, InMemorySchemaBuilder, RangeKind, SlotDef,
};
use linkml_map_core::value::Value;
use serde_json::json;

fn slot(name: &str, range: &str) -> SlotDef {
    SlotDef {
        name: name.into(),
        range: RangeKind::Type(range.into()),
        multivalued: false,
        inlined: false,
        inlined_as_list: false,
        required: false,
        identifier: false,
        key: false,
        unit: None,
        any_of_enums: vec![],
    }
}

/// Source class `Person{id, name, alias, age: integer}`.
fn person() -> InMemorySchema {
    InMemorySchemaBuilder::new()
        .add_type("string")
        .add_type("integer")
        .add_class(ClassDef {
            name: "Person".into(),
            tree_root: true,
            is_a: None,
            mixins: vec![],
        })
        .add_slot("Person", slot("id", "string"))
        .add_slot("Person", slot("name", "string"))
        .add_slot("Person", slot("alias", "string"))
        .add_slot("Person", slot("age", "integer"))
        .build()
}

fn spec(class_derivations: serde_json::Value) -> TransformationSpecification {
    let mut obj = json!({ "class_derivations": class_derivations });
    normalise_spec_json(&mut obj);
    serde_json::from_value(obj).unwrap()
}

/// `Base` carries `label`, `Agent` inherits it (`is_a`), so `label` is a raw slot
/// derivation: not induced, `sources` read.
fn inherited(label: serde_json::Value) -> TransformationSpecification {
    spec(json!({
        "Base": {"slot_derivations": {"label": label}},
        "Agent": {"is_a": "Base", "populated_from": "Person",
                  "slot_derivations": {"id": {}}},
    }))
}

fn map(spec: TransformationSpecification, row: serde_json::Value) -> Result<Value, String> {
    let schema = person();
    ObjectTransformer::new(spec, Some(&schema), None)
        .map_object(&Value::from(&row), Some("Person"))
        .map_err(|e| e.to_string())
}

fn label(spec: TransformationSpecification, row: serde_json::Value) -> Option<Value> {
    match map(spec, row).unwrap() {
        Value::Map(m) => m.get("label").cloned(),
        other => panic!("expected a mapping, got {other:?}"),
    }
}

#[test]
fn the_one_source_with_a_value_wins() {
    let sd = || json!({"sources": ["name", "alias"]});
    assert_eq!(
        label(inherited(sd()), json!({"id": "p1", "name": "Al"})),
        Some(Value::Str("Al".into()))
    );
    assert_eq!(
        label(inherited(sd()), json!({"id": "p2", "alias": "Bo"})),
        Some(Value::Str("Bo".into()))
    );
}

#[test]
fn no_source_with_a_value_is_null() {
    let sd = json!({"sources": ["name", "alias"]});
    assert_eq!(
        label(inherited(sd.clone()), json!({"id": "p3"})),
        Some(Value::Null)
    );
    // an explicit null is no value either
    assert_eq!(
        label(inherited(sd), json!({"id": "p3", "name": null})),
        Some(Value::Null)
    );
}

#[test]
fn falsy_values_still_count() {
    // `v is not None`: an empty string and a zero are values, and clash.
    let err = map(
        inherited(json!({"sources": ["name", "alias"]})),
        json!({"id": "p", "name": "", "alias": "Z"}),
    )
    .unwrap_err();
    assert!(
        err.contains("Multiple sources for label: {'name': '', 'alias': 'Z'}"),
        "{err}"
    );
}

#[test]
fn several_sources_with_values_are_an_error_naming_them_in_order() {
    let err = map(
        inherited(json!({"sources": ["alias", "name"]})),
        json!({"id": "p4", "name": "X", "alias": "Y"}),
    )
    .unwrap_err();
    assert!(
        err.contains("ValueError: Multiple sources for label: {'alias': 'Y', 'name': 'X'}"),
        "{err}"
    );
}

#[test]
fn a_repeated_source_is_one_source() {
    assert_eq!(
        label(
            inherited(json!({"sources": ["name", "name"]})),
            json!({"id": "p", "name": "Al"})
        ),
        Some(Value::Str("Al".into()))
    );
}

#[test]
fn a_source_the_source_class_lacks_is_an_error_only_when_it_has_a_value() {
    let sd = || json!({"sources": ["zzz", "name"]});
    assert_eq!(
        label(inherited(sd()), json!({"id": "p", "name": "n"})),
        Some(Value::Str("n".into()))
    );
    let err = map(inherited(sd()), json!({"id": "p", "zzz": "v"})).unwrap_err();
    assert!(
        err.contains("No such slot zzz as an attribute of Person ancestors"),
        "{err}"
    );
}

#[test]
fn the_source_slots_range_coerces_like_populated_from() {
    // `age` is an integer slot, the spec range string.
    let sd = json!({"sources": ["age", "name"], "range": "string"});
    assert_eq!(
        label(inherited(sd), json!({"id": "p", "age": 3})),
        Some(Value::Str("3".into()))
    );
}

#[test]
fn missing_values_mappings_and_offset_do_not_apply() {
    let sd = json!({
        "sources": ["age"],
        "missing_values": [3],
        "value_mappings": {"3": {"value": "three"}},
    });
    assert_eq!(
        label(inherited(sd), json!({"id": "p", "age": 3})),
        Some(Value::Int(3))
    );
}

#[test]
fn a_hidden_slot_is_not_emitted() {
    let sd = json!({"sources": ["name"], "hide": true});
    assert_eq!(label(inherited(sd), json!({"id": "p", "name": "n"})), None);
}

#[test]
fn an_own_slot_with_no_other_source_is_induced_and_its_sources_are_dead() {
    // `name` is a source slot: induction makes it `populated_from: name`.
    let own = spec(json!({"Agent": {
        "populated_from": "Person",
        "slot_derivations": {"name": {"sources": ["alias"]}},
    }}));
    let out = map(own, json!({"name": "N", "alias": "A"})).unwrap();
    let Value::Map(m) = out else { panic!() };
    assert_eq!(m["name"], Value::Str("N".into()));
}

#[test]
fn an_own_slot_with_populated_from_ignores_its_sources() {
    let own = spec(json!({"Agent": {
        "populated_from": "Person",
        "slot_derivations": {"label": {"populated_from": "name", "sources": ["alias"]}},
    }}));
    let out = map(own, json!({"name": "N", "alias": "A"})).unwrap();
    let Value::Map(m) = out else { panic!() };
    assert_eq!(m["label"], Value::Str("N".into()));
}

#[test]
fn an_empty_populated_from_or_expr_is_falsy_so_sources_apply() {
    for falsy in [json!({"populated_from": ""}), json!({"expr": ""})] {
        let mut sd = falsy;
        sd["sources"] = json!(["name", "alias"]);
        let own = spec(json!({"Agent": {
            "populated_from": "Person",
            "slot_derivations": {"label": sd},
        }}));
        let out = map(own, json!({"alias": "A"})).unwrap();
        let Value::Map(m) = out else { panic!() };
        assert_eq!(m["label"], Value::Str("A".into()));
    }
}

#[test]
fn a_slot_inherited_from_a_mixin_keeps_its_sources() {
    let mixed = spec(json!({
        "M": {"slot_derivations": {"label": {"sources": ["name", "alias"]}}},
        "Agent": {"mixins": ["M"], "populated_from": "Person"},
    }));
    assert_eq!(
        label(mixed, json!({"alias": "b"})),
        Some(Value::Str("b".into()))
    );
}

#[test]
fn a_childs_own_slot_of_the_same_name_replaces_the_inherited_one() {
    // The child's own `label` (no populated_from, no expr, no value) is induced to
    // `populated_from: label`, which `Person` lacks: upstream fails before any row.
    let child = spec(json!({
        "Base": {"slot_derivations": {"label": {"sources": ["name"]}}},
        "Agent": {"is_a": "Base", "populated_from": "Person",
                  "slot_derivations": {"label": {"sources": ["alias"]}}},
    }));
    let err = map(child, json!({"name": "n"})).unwrap_err();
    assert!(err.contains("No such slot label"), "{err}");
}

#[test]
fn a_nested_slot_derivation_keeps_its_sources() {
    let nested = spec(json!({"Agent": {
        "populated_from": "Person",
        "slot_derivations": {"addr": {"class_derivations": {"Addr": {
            "slot_derivations": {"street": {"sources": ["name", "alias"]}},
        }}}},
    }}));
    // No target schema: the nested object cannot be shaped, but the arm runs first, so
    // reaching the target-schema error shows the sources were resolved.
    let err = map(nested, json!({"name": "a", "alias": "b"})).unwrap_err();
    assert!(
        err.contains("Multiple sources for street: {'name': 'a', 'alias': 'b'}"),
        "{err}"
    );
}

#[test]
fn sources_load_in_the_shapes_upstreams_reference_validator_accepts() {
    let load = |sources: serde_json::Value| {
        let mut obj = json!({"class_derivations": {"A": {"slot_derivations": {
            "s": {"sources": sources}}}}});
        normalise_spec_json(&mut obj);
        serde_json::from_value::<TransformationSpecification>(obj)
            .map(|s| {
                s.class_derivations.unwrap()[0]
                    .slot_derivations
                    .as_ref()
                    .unwrap()["s"]
                    .sources
                    .clone()
            })
            .map_err(|e| e.to_string())
    };
    let strs = |v: &[&str]| Some(v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>());
    assert_eq!(load(json!("alias")).unwrap(), strs(&["alias"]));
    assert_eq!(load(json!([])).unwrap(), strs(&[]));
    assert_eq!(
        load(json!([1, "a", true, 1.5])).unwrap(),
        strs(&["1", "a", "True", "1.5"])
    );
    assert_eq!(load(json!({"k": "v"})).unwrap(), strs(&["v"]));
    assert_eq!(load(json!(7)).unwrap(), strs(&["7"]));
    for bad in [json!(null), json!([null, "a"])] {
        let err = load(bad).unwrap_err();
        assert!(err.contains("Input should be a valid string"), "{err}");
    }
    let err = load(json!([["x"]])).unwrap_err();
    assert!(
        err.contains("NotImplementedError: List of Lists: [['x']]"),
        "{err}"
    );
}

// ── the induction `sources` interacts with ──────────────────────────────────────────

fn own(slot: &str, sd: serde_json::Value) -> TransformationSpecification {
    spec(json!({"Agent": {
        "populated_from": "Person",
        "slot_derivations": {slot: sd},
    }}))
}

#[test]
fn a_bare_own_slot_is_induced_so_its_mappings_and_missing_values_apply() {
    // `induce_missing_values`: `name: {value_mappings: ...}` is `populated_from: name`.
    let out = map(
        own(
            "name",
            json!({"value_mappings": {"Al": {"value": "ALICE"}}}),
        ),
        json!({"name": "Al"}),
    )
    .unwrap();
    let Value::Map(m) = out else { panic!() };
    assert_eq!(m["name"], Value::Str("ALICE".into()));
    let out = map(
        own("name", json!({"missing_values": ["Al"]})),
        json!({"name": "Al"}),
    )
    .unwrap();
    let Value::Map(m) = out else { panic!() };
    assert_eq!(m["name"], Value::Null);
}

#[test]
fn an_inherited_bare_slot_is_not_induced() {
    // The ancestor's slot derivation comes from the raw specification: no `populated_from`,
    // so `value_mappings` do not apply (the implicit copy only nullifies missing values).
    let inherited = spec(json!({
        "Base": {"slot_derivations": {"name": {"value_mappings": {"Al": {"value": "ALICE"}}}}},
        "Agent": {"is_a": "Base", "populated_from": "Person"},
    }));
    let out = map(inherited, json!({"name": "Al"})).unwrap();
    let Value::Map(m) = out else { panic!() };
    assert_eq!(m["name"], Value::Str("Al".into()));
}

#[test]
fn an_empty_populated_from_is_set_so_the_slot_is_not_induced() {
    // Not induced, and falsy: the implicit same-name copy, which has no mappings.
    let out = map(
        own(
            "name",
            json!({"populated_from": "", "value_mappings": {"Al": {"value": "ALICE"}}}),
        ),
        json!({"name": "Al"}),
    )
    .unwrap();
    let Value::Map(m) = out else { panic!() };
    assert_eq!(m["name"], Value::Str("Al".into()));
}
