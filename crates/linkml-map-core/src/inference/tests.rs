use indexmap::IndexMap;

use super::TransformationSpecificationInverter;
use crate::{
    datamodel::{
        ClassDerivation, SlotDerivation, StringificationConfiguration, TransformationSpecification,
        UnitConversionConfiguration,
    },
    engine::ObjectTransformer,
    schema::{ClassDef, InMemorySchemaBuilder, RangeKind, SlotDef, UnitRef, UnitSystem},
    value::Value,
};

/// Convenience: a `ClassDerivation` with a single named slot derivation.
fn cd_with(
    name: &str,
    populated_from: Option<&str>,
    slots: Vec<SlotDerivation>,
) -> ClassDerivation {
    let mut sds = IndexMap::new();
    for s in slots {
        sds.insert(s.name.clone(), s);
    }
    ClassDerivation {
        name: name.into(),
        populated_from: populated_from.map(Into::into),
        slot_derivations: Some(sds),
        ..Default::default()
    }
}

fn spec_of(cds: Vec<ClassDerivation>) -> TransformationSpecification {
    TransformationSpecification {
        class_derivations: Some(cds),
        ..Default::default()
    }
}

fn slot(name: &str) -> SlotDef {
    SlotDef {
        name: name.into(),
        range: RangeKind::None,
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

// ── Structural: name/populated_from swap ──────────────────────────────────────

#[test]
fn invert_swaps_class_and_slot_names() {
    let schema = InMemorySchemaBuilder::new()
        .add_class(ClassDef {
            name: "C".into(),
            tree_root: false,
            is_a: None,
            mixins: vec![],
        })
        .add_slot("C", slot("name"))
        .build();

    let fwd = spec_of(vec![cd_with(
        "D",
        Some("C"),
        vec![SlotDerivation {
            name: "label".into(),
            populated_from: Some("name".into()),
            ..Default::default()
        }],
    )]);

    let inv = TransformationSpecificationInverter::new(&schema)
        .invert(&fwd)
        .unwrap();
    let cds = inv.class_derivations.unwrap();
    assert_eq!(cds.len(), 1);
    // Class: D(populated_from C) → C(populated_from D)
    assert_eq!(cds[0].name, "C");
    assert_eq!(cds[0].populated_from.as_deref(), Some("D"));
    // Slot: label(populated_from name) → name(populated_from label)
    let sd = &cds[0].slot_derivations.as_ref().unwrap()["name"];
    assert_eq!(sd.name, "name");
    assert_eq!(sd.populated_from.as_deref(), Some("label"));
}

// ── Structural: bare-identifier expr becomes populated_from ────────────────────

#[test]
fn invert_bare_expr_is_reversible() {
    let schema = InMemorySchemaBuilder::new().build();
    let fwd = spec_of(vec![cd_with(
        "D",
        Some("C"),
        vec![SlotDerivation {
            name: "out".into(),
            expr: Some("s3".into()),
            ..Default::default()
        }],
    )]);
    let inv = TransformationSpecificationInverter::new(&schema)
        .invert(&fwd)
        .unwrap();
    let cds = inv.class_derivations.unwrap();
    let sd = &cds[0].slot_derivations.as_ref().unwrap()["s3"];
    assert_eq!(sd.name, "s3");
    assert_eq!(sd.populated_from.as_deref(), Some("out"));
}

#[test]
fn invert_complex_expr_errors_in_strict_mode() {
    let schema = InMemorySchemaBuilder::new().build();
    let fwd = spec_of(vec![cd_with(
        "D",
        Some("C"),
        vec![SlotDerivation {
            name: "out".into(),
            expr: Some("s1 + s2".into()),
            ..Default::default()
        }],
    )]);
    assert!(TransformationSpecificationInverter::new(&schema)
        .invert(&fwd)
        .is_err());
    // Non-strict drops the slot instead.
    let inv = TransformationSpecificationInverter::non_strict(&schema)
        .invert(&fwd)
        .unwrap();
    assert!(inv.class_derivations.unwrap()[0]
        .slot_derivations
        .as_ref()
        .unwrap()
        .is_empty());
}

// ── Structural: stringification toggles `reversed` ────────────────────────────

#[test]
fn invert_stringification_toggles_reversed() {
    let schema = InMemorySchemaBuilder::new().build();
    let fwd = spec_of(vec![cd_with(
        "D",
        Some("C"),
        vec![SlotDerivation {
            name: "s1_verbatim".into(),
            populated_from: Some("s1".into()),
            stringification: Some(StringificationConfiguration {
                delimiter: Some(",".into()),
                reversed: Some(false),
                ..Default::default()
            }),
            ..Default::default()
        }],
    )]);
    let inv = TransformationSpecificationInverter::new(&schema)
        .invert(&fwd)
        .unwrap();
    let cds = inv.class_derivations.unwrap();
    let sd = &cds[0].slot_derivations.as_ref().unwrap()["s1"];
    assert_eq!(sd.stringification.as_ref().unwrap().reversed, Some(true));
}

// ── Structural: unit_conversion reverses, target unit pulled from schema ───────

#[test]
fn invert_unit_conversion_reverses() {
    let mut height = slot("height");
    height.range = RangeKind::Type("float".into());
    height.unit = Some(UnitRef {
        code: "m".into(),
        system: UnitSystem::Ucum,
    });
    let schema = InMemorySchemaBuilder::new()
        .add_class(ClassDef {
            name: "C".into(),
            tree_root: false,
            is_a: None,
            mixins: vec![],
        })
        .add_slot("C", height)
        .build();

    let fwd = spec_of(vec![cd_with(
        "D",
        Some("C"),
        vec![SlotDerivation {
            name: "height_cm".into(),
            populated_from: Some("height".into()),
            unit_conversion: Some(UnitConversionConfiguration {
                target_unit: Some("cm".into()),
                ..Default::default()
            }),
            ..Default::default()
        }],
    )]);

    let inv = TransformationSpecificationInverter::new(&schema)
        .invert(&fwd)
        .unwrap();
    let cds = inv.class_derivations.unwrap();
    let sd = &cds[0].slot_derivations.as_ref().unwrap()["height"];
    let uc = sd.unit_conversion.as_ref().unwrap();
    assert_eq!(uc.target_unit.as_deref(), Some("m")); // from schema unit
    assert_eq!(uc.target_unit_scheme.as_deref(), Some("ucum_code"));
    assert_eq!(uc.source_unit.as_deref(), Some("cm")); // forward target → inverse source
}

// ── End-to-end: forward then inverse recovers the source object ───────────────

#[test]
fn roundtrip_rename_through_engine() {
    let schema = InMemorySchemaBuilder::new()
        .add_class(ClassDef {
            name: "Person".into(),
            tree_root: true,
            is_a: None,
            mixins: vec![],
        })
        .add_slot("Person", slot("name"))
        .build();

    // Forward: Person.name → label
    let fwd = spec_of(vec![cd_with(
        "Person",
        Some("Person"),
        vec![SlotDerivation {
            name: "label".into(),
            populated_from: Some("name".into()),
            ..Default::default()
        }],
    )]);

    let mut src = IndexMap::new();
    src.insert("name".to_string(), Value::Str("Alice".into()));
    let src = Value::Map(src);

    let fwd_engine = ObjectTransformer::new(fwd.clone(), Some(&schema), None);
    let target = fwd_engine.map_object(&src, Some("Person")).unwrap();
    // {label: Alice}
    assert_eq!(target, {
        let mut m = IndexMap::new();
        m.insert("label".to_string(), Value::Str("Alice".into()));
        Value::Map(m)
    });

    // Invert and map back → recovers {name: Alice}
    let inv = TransformationSpecificationInverter::new(&schema)
        .invert(&fwd)
        .unwrap();
    let inv_engine = ObjectTransformer::new(inv, Some(&schema), None);
    let roundtrip = inv_engine.map_object(&target, Some("Person")).unwrap();
    assert_eq!(roundtrip, src);
}

// ── Enum derivation inversion ─────────────────────────────────────────────────

#[test]
fn invert_enum_derivation_swaps_pv() {
    use crate::datamodel::{EnumDerivation, PermissibleValueDerivation};

    let schema = InMemorySchemaBuilder::new().build();
    let mut pvds = IndexMap::new();
    pvds.insert(
        "B".to_string(),
        PermissibleValueDerivation {
            name: "B".into(),
            populated_from: Some("A".into()),
            ..Default::default()
        },
    );
    let mut eds = IndexMap::new();
    eds.insert(
        "E".to_string(),
        EnumDerivation {
            name: "E".into(),
            populated_from: Some("ESource".into()),
            permissible_value_derivations: Some(pvds),
            ..Default::default()
        },
    );
    let fwd = TransformationSpecification {
        class_derivations: Some(vec![]),
        enum_derivations: Some(eds),
        ..Default::default()
    };

    let inv = TransformationSpecificationInverter::new(&schema)
        .invert(&fwd)
        .unwrap();
    let ed = &inv.enum_derivations.unwrap()["ESource"];
    assert_eq!(ed.name, "ESource");
    assert_eq!(ed.populated_from.as_deref(), Some("E"));
    let pv = &ed.permissible_value_derivations.as_ref().unwrap()["A"];
    assert_eq!(pv.name, "A");
    assert_eq!(pv.populated_from.as_deref(), Some("B"));
}
