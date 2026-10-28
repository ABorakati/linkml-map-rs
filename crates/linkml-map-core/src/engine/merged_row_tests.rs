//! Tests of [`super::merged_row`]: a nested `class_derivations` over a join reads the
//! merge of the parent and joined rows (upstream `MergedRow`, `_merge_rows`).

use std::sync::Arc;

use indexmap::IndexMap;

use super::{LookupIndex, MergedRow, ObjectTransformer};
use crate::{
    datamodel::TransformationSpecification,
    schema::{ClassDef, InMemorySchemaBuilder, RangeKind, SlotDef},
    value::Value,
};

fn row(pairs: &[(&str, Value)]) -> IndexMap<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

fn s(text: &str) -> Value {
    Value::Str(text.to_owned())
}

/// Person rows join Org on `org_id`, Org rows join Country on `country_id`.
fn index() -> Arc<LookupIndex> {
    let mut li = LookupIndex::new();
    let org = |id: &str, label: &str, size: i64, country: &str| {
        Value::Map(row(&[
            ("id", s(id)),
            ("label", s(label)),
            ("size", Value::Int(size)),
            ("country_id", s(country)),
        ]))
    };
    li.register_table("Org", &[org("o1", "ol1", 5, "c1")], "id");
    li.register_table(
        "Country",
        &[Value::Map(row(&[
            ("id", s("c1")),
            ("name", s("Norway")),
            ("label", s("cl1")),
        ]))],
        "id",
    );
    Arc::new(li)
}

fn person() -> Value {
    Value::Map(row(&[
        ("id", s("p1")),
        ("org_id", s("o1")),
        ("country_id", s("c1")),
        ("label", s("pl1")),
        ("age", Value::Int(30)),
    ]))
}

fn target() -> crate::schema::InMemorySchema {
    let slot = |name: &str, range: RangeKind, multivalued: bool| SlotDef {
        name: name.into(),
        range,
        multivalued,
        required: false,
        identifier: false,
        key: false,
        unit: None,
        any_of_enums: vec![],
        inlined: true,
        inlined_as_list: multivalued,
    };
    let class = |name: &str, tree_root: bool| ClassDef {
        name: name.into(),
        tree_root,
        is_a: None,
        mixins: vec![],
    };
    let mut builder = InMemorySchemaBuilder::new()
        .add_type("string")
        .add_type("integer")
        .add_class(class("Agent", true))
        .add_class(class("Inner", false))
        .add_class(class("Deep", false))
        .add_slot(
            "Agent",
            slot("inner", RangeKind::Class("Inner".into()), false),
        )
        .add_slot(
            "Agent",
            slot("inners", RangeKind::Class("Inner".into()), true),
        )
        .add_slot(
            "Inner",
            slot("deep", RangeKind::Class("Deep".into()), false),
        );
    for name in ["a", "b", "c", "label", "sz"] {
        builder = builder.add_slot("Inner", slot(name, RangeKind::Type("string".into()), false));
    }
    for name in ["nm", "ol", "pl"] {
        builder = builder.add_slot("Deep", slot(name, RangeKind::Type("string".into()), false));
    }
    builder.build()
}

/// A spec as the loader reads it (`class_derivations` mapping form included).
fn load(yaml: &str) -> TransformationSpecification {
    let mut v: serde_json::Value = serde_yaml_ng::from_str(yaml).unwrap();
    crate::datamodel::normalise_spec_json(&mut v);
    serde_json::from_value(v).unwrap()
}

/// A spec whose `Agent` joins Org and Country, with the given slot derivations.
fn spec(slots: &str) -> TransformationSpecification {
    let yaml = format!(
        "id: https://example.org/spec\n\
         class_derivations:\n  Agent:\n    populated_from: Person\n    joins:\n      \
         Org: {{source_key: org_id, lookup_key: id}}\n      \
         Country: {{source_key: country_id, lookup_key: id}}\n    slot_derivations:\n{slots}"
    );
    load(&yaml)
}

/// The `inner` object of the first row, or the error text.
fn run(slots: &str, prejoined: bool) -> Result<Value, String> {
    let target = target();
    let spec = spec(slots);
    let cd = spec.class_derivations.as_ref().unwrap()[0].clone();
    let engine = ObjectTransformer::new(spec, None, Some(&target))
        .with_lookup_index(index())
        .with_prejoined_joins(prejoined);
    // The set-based engine hands `map_object` the block's derivation.
    let out = if prejoined {
        engine.map_object_with_derivation(&person(), Some("Person"), &cd)
    } else {
        engine.map_object(&person(), Some("Person"))
    }
    .map_err(|e| e.to_string())?;
    match out {
        Value::Map(mut m) => Ok(m.shift_remove("inner").unwrap_or(Value::Null)),
        other => panic!("expected a map, got {other:?}"),
    }
}

fn inner(slots: &str) -> String {
    format!(
        "      inner:\n        class_derivations:\n          Inner:\n            \
         populated_from: Org\n            slot_derivations:\n{slots}"
    )
}

#[test]
fn parent_and_joined_columns_are_both_readable() {
    let got = run(
        &inner(
            "              sz: {populated_from: size}\n\
             \x20             a: {populated_from: Org.label}\n\
             \x20             b: {populated_from: Person.label}\n\
             \x20             c: {expr: '{Person.age} + {Org.size}'}\n",
        ),
        false,
    )
    .unwrap();
    assert_eq!(
        got,
        Value::Map(row(&[
            ("sz", Value::Int(5)),
            ("a", s("ol1")),
            ("b", s("pl1")),
            ("c", s("35")),
        ]))
    );
}

#[test]
fn a_column_both_rows_have_is_ambiguous_when_read() {
    // `label` is in both rows, and not the join key.
    for slot in [
        "a: {populated_from: label}",
        "label: {}",
        "a: {expr: '{label}'}",
        "a: {expr: '{label}.upper()'}",
    ] {
        let err = run(&inner(&format!("              {slot}\n")), false).unwrap_err();
        assert!(err.contains("ambiguous"), "{slot}: {err}");
    }
    // A name an expression does not read is not read.
    let got = run(
        &inner("              a: {expr: \"'x' if True else {label}\"}\n"),
        false,
    )
    .unwrap();
    assert_eq!(got, Value::Map(row(&[("a", s("x"))])));
}

#[test]
fn the_parent_side_join_key_is_not_ambiguous() {
    // Person joins Org on `id == id`: the key keeps the parent's value.
    let slots = format!(
        "{}{}",
        "      id: {populated_from: id}\n",
        "      inner:\n        class_derivations:\n          Inner:\n            populated_from: Org\n            \
         slot_derivations:\n              a: {populated_from: id}\n"
    );
    let yaml = format!(
        "id: https://example.org/spec\n\
         class_derivations:\n  Agent:\n    populated_from: Person\n    joins:\n      \
         Org: {{source_key: id, lookup_key: id}}\n    slot_derivations:\n{slots}"
    );
    let spec = load(&yaml);
    let mut li = LookupIndex::new();
    li.register_table(
        "Org",
        &[Value::Map(row(&[("id", s("p1")), ("size", Value::Int(1))]))],
        "id",
    );
    let target = target();
    let out = ObjectTransformer::new(spec, None, Some(&target))
        .with_lookup_index(Arc::new(li))
        .map_object(&person(), Some("Person"))
        .unwrap();
    let Value::Map(m) = out else { panic!() };
    assert_eq!(m["inner"], Value::Map(row(&[("a", s("p1"))])));
}

#[test]
fn a_join_miss_emits_no_object() {
    let target = target();
    let mut missing = row(&[("id", s("p9")), ("org_id", s("nope")), ("label", s("x"))]);
    let out = ObjectTransformer::new(
        spec(&inner("              a: {populated_from: Org.label}\n")),
        None,
        Some(&target),
    )
    .with_lookup_index(index())
    .map_object(&Value::Map(missing.clone()), Some("Person"))
    .unwrap();
    let Value::Map(m) = out else { panic!() };
    assert_eq!(m["inner"], Value::Null);
    missing.shift_remove("org_id");
}

#[test]
fn an_offset_field_that_is_ambiguous_fails() {
    let err = run(
        &inner(
            "              sz: {populated_from: size, offset: {offset_field: label, offset_value: 1}}\n",
        ),
        false,
    )
    .unwrap_err();
    assert!(err.contains("_AmbiguousType"), "{err}");
}

#[test]
fn a_nested_class_with_no_join_is_an_error() {
    let yaml = "id: https://example.org/spec\n\
                class_derivations:\n  Agent:\n    populated_from: Person\n    slot_derivations:\n      \
                inner:\n        class_derivations:\n          Inner:\n            populated_from: Org\n            \
                slot_derivations:\n              sz: {populated_from: size}\n";
    let target = target();
    let err = ObjectTransformer::new(load(yaml), None, Some(&target))
        .map_object(&person(), Some("Person"))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("no implicit join could be synthesized"),
        "{err}"
    );
}

#[test]
fn nested_within_nested_keeps_ancestor_tables_and_ambiguity() {
    // Level two is populated from Country, joined by the nested derivation itself on
    // `country_id` -- a column both Person and Org have, so ambiguous at level one.
    let slots = "      inner:\n        class_derivations:\n          Inner:\n            populated_from: Org\n            \
                 joins:\n              Country: {source_key: country_id, lookup_key: id}\n            \
                 slot_derivations:\n              deep:\n                class_derivations:\n                  \
                 Deep:\n                    populated_from: Country\n                    slot_derivations:\n                      \
                 nm: {populated_from: name}\n                      \
                 ol: {populated_from: Org.label}\n                      \
                 pl: {populated_from: Person.label}\n";
    // Without the set-based engine the level-two join reads `country_id` from the
    // level-one merge, where it is ambiguous: no key, no row, no object.
    let per_row = run(slots, false).unwrap();
    assert_eq!(per_row, Value::Map(row(&[("deep", Value::Null)])));
    // With it, the joins were resolved from the primary row up front.
    let prejoined = run(slots, true).unwrap();
    assert_eq!(
        prejoined,
        Value::Map(row(&[(
            "deep",
            Value::Map(row(&[
                ("nm", s("Norway")),
                ("ol", s("ol1")),
                ("pl", s("pl1"))
            ]))
        )]))
    );
}

#[test]
fn merge_carries_ambiguity_and_tables_down() {
    let parent = row(&[("id", s("p1")), ("k", s("o1")), ("x", Value::Int(1))]);
    let org = row(&[("id", s("o1")), ("x", Value::Int(2)), ("z", Value::Int(3))]);
    let country = row(&[("id", s("c1")), ("z", Value::Int(4)), ("w", Value::Int(5))]);
    let level1 = MergedRow::merge(&parent, None, &org, "k", "Person", "Org");
    assert!(level1.is_ambiguous("id") && level1.is_ambiguous("x"));
    assert!(!level1.is_ambiguous("z") && !level1.is_ambiguous("k"));
    assert_eq!(level1.map.get("z"), Some(&Value::Int(3)));
    assert!(!level1.map.contains_key("id") && !level1.map.contains_key("x"));

    let level2 = MergedRow::merge(&level1.map, Some(&level1), &country, "k", "Org", "Country");
    // `id` and `x` stay ambiguous; `z` becomes so; the tables of all three levels remain.
    assert!(level2.is_ambiguous("id") && level2.is_ambiguous("x") && level2.is_ambiguous("z"));
    assert_eq!(level2.map.get("w"), Some(&Value::Int(5)));
    assert_eq!(level2.table("Person"), Some(Some(&parent)));
    assert_eq!(level2.table("Org"), Some(Some(&org)));
    assert_eq!(level2.table("Country"), Some(Some(&country)));
    assert_eq!(level2.table("Other"), None);
}
