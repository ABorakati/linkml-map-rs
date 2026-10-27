//! A constant `value:` slot derivation is emitted as-is and needs no source slot.
//!
//! Regression for upstream `d2297e5` (#338 / #326): `tag: {value: 7}` must not be
//! treated as an implicit same-name copy of a source slot `tag`. Upstream pins this
//! on the derived specification (`test_constant_value_gets_no_populated_from`);
//! Rust never writes `populated_from` onto a spec, so the observable contract is the
//! transformed output.

use linkml_map_core::datamodel::{TransformationSpecification, normalise_spec_json};
use linkml_map_core::engine::ObjectTransformer;
use linkml_map_core::schema::{ClassDef, InMemorySchemaBuilder, RangeKind, SlotDef};
use linkml_map_core::value::Value;
use serde_json::json;

fn string_slot(name: &str) -> SlotDef {
    SlotDef {
        name: name.into(),
        range: RangeKind::Type("string".into()),
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

#[test]
fn constant_value_is_emitted_without_a_source_slot() {
    // Source class `Row{person_id, note}`: no `tag` slot.
    let source = InMemorySchemaBuilder::new()
        .add_type("string")
        .add_class(ClassDef {
            name: "Row".into(),
            tree_root: true,
            is_a: None,
            mixins: vec![],
        })
        .add_slot("Row", string_slot("person_id"))
        .add_slot("Row", string_slot("note"))
        .build();

    let mut spec_json = json!({
        "class_derivations": {
            "Out": {
                "populated_from": "Row",
                "slot_derivations": {
                    "person_id": {"populated_from": "person_id"},
                    "note": {"populated_from": "note"},
                    "tag": {"value": 7},
                },
            }
        }
    });
    normalise_spec_json(&mut spec_json);
    let spec: TransformationSpecification = serde_json::from_value(spec_json).unwrap();

    let transformer = ObjectTransformer::new(spec, Some(&source), None);
    let input = Value::from(&json!({"person_id": "p1", "note": "hi"}));
    let out = transformer.map_object(&input, Some("Row")).unwrap();

    let Value::Map(map) = out else {
        panic!("expected a mapping, got {out:?}");
    };
    assert_eq!(map.get("person_id"), Some(&Value::Str("p1".into())));
    assert_eq!(map.get("note"), Some(&Value::Str("hi".into())));
    assert_eq!(map.get("tag"), Some(&Value::Int(7)));
    assert_eq!(map.len(), 3);
}
