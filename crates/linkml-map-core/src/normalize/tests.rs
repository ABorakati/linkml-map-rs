//! Tests for implicit-join synthesis (ported from upstream
//! `test_expr_implicit_join.py`, `test_join_synthesis_completeness.py`,
//! `test_implicit_cross_table_join.py`).

use std::sync::Arc;

use indexmap::IndexMap;

use super::*;
use crate::datamodel::{
    AliasedClass, ClassDerivation, EnumDerivation, KeyVal, PermissibleValueDerivation,
    SlotDerivation, TransformationSpecification,
};
use crate::engine::{LookupIndex, ObjectTransformer};
use crate::schema::{ClassDef, InMemorySchema, InMemorySchemaBuilder, RangeKind, SlotDef};
use crate::value::Value;

// ── schema + spec builders ──────────────────────────────────────────────────

fn class(name: &str) -> ClassDef {
    ClassDef {
        name: name.into(),
        tree_root: false,
        is_a: None,
        mixins: vec![],
    }
}

fn slot(name: &str, range: RangeKind, identifier: bool) -> SlotDef {
    SlotDef {
        name: name.into(),
        range,
        multivalued: false,
        inlined: false,
        inlined_as_list: false,
        required: false,
        identifier,
        key: false,
        unit: None,
        any_of_enums: vec![],
    }
}

fn str_range() -> RangeKind {
    RangeKind::Type("string".into())
}

/// Measurement{id, subject_id, method} ⋈ Reading{id, subject_id, score} on the
/// shared non-identifier `subject_id`.
fn mr_schema() -> InMemorySchema {
    InMemorySchemaBuilder::new()
        .add_class(class("Measurement"))
        .add_slot("Measurement", slot("id", str_range(), true))
        .add_slot("Measurement", slot("subject_id", str_range(), false))
        .add_slot("Measurement", slot("method", str_range(), false))
        .add_class(class("Reading"))
        .add_slot("Reading", slot("id", str_range(), true))
        .add_slot("Reading", slot("subject_id", str_range(), false))
        .add_slot("Reading", slot("score", RangeKind::Type("float".into()), false))
        .build()
}

/// Measurement{id, method} and Reading{reading_id, score} share no column.
fn no_common_schema() -> InMemorySchema {
    InMemorySchemaBuilder::new()
        .add_class(class("Measurement"))
        .add_slot("Measurement", slot("id", str_range(), true))
        .add_slot("Measurement", slot("method", str_range(), false))
        .add_class(class("Reading"))
        .add_slot("Reading", slot("reading_id", str_range(), true))
        .add_slot("Reading", slot("score", RangeKind::Type("float".into()), false))
        .build()
}

fn sd(name: &str) -> SlotDerivation {
    SlotDerivation {
        name: name.into(),
        ..Default::default()
    }
}

fn sd_expr(name: &str, expr: &str) -> SlotDerivation {
    SlotDerivation {
        name: name.into(),
        expr: Some(expr.into()),
        ..Default::default()
    }
}

fn sd_pf(name: &str, pf: &str) -> SlotDerivation {
    SlotDerivation {
        name: name.into(),
        populated_from: Some(pf.into()),
        ..Default::default()
    }
}

fn slot_map(slots: Vec<SlotDerivation>) -> IndexMap<String, SlotDerivation> {
    slots.into_iter().map(|s| (s.name.clone(), s)).collect()
}

fn cd(name: &str, populated_from: &str, slots: Vec<SlotDerivation>) -> ClassDerivation {
    ClassDerivation {
        name: name.into(),
        populated_from: Some(populated_from.into()),
        slot_derivations: Some(slot_map(slots)),
        ..Default::default()
    }
}

fn spec_with(cds: Vec<ClassDerivation>) -> TransformationSpecification {
    TransformationSpecification {
        class_derivations: Some(cds),
        ..Default::default()
    }
}

fn result_cd(spec: &TransformationSpecification) -> &ClassDerivation {
    &spec.class_derivations.as_ref().unwrap()[0]
}

// ── expression-driven synthesis ─────────────────────────────────────────────

#[test]
fn expr_reference_synthesizes_join() {
    // A `{Reading.score}` in an expr — no joins block, no nested CD — must
    // synthesize the join on the shared `subject_id`.
    let mut spec = spec_with(vec![cd(
        "Result",
        "Measurement",
        vec![sd("id"), sd("method"), sd_expr("reading_score", "{Reading.score}")],
    )]);
    synthesize_implicit_joins(&mut spec, &mr_schema()).unwrap();
    let joins = result_cd(&spec).joins.as_ref().unwrap();
    assert_eq!(joins.get("Reading").unwrap().join_on.as_deref(), Some("subject_id"));
}

#[test]
fn expr_implicit_join_resolves_value() {
    // End to end: after synthesis + a registered lookup table, `{Reading.score}`
    // resolves to the joined value instead of silently returning null.
    let mut spec = spec_with(vec![cd(
        "Result",
        "Measurement",
        vec![sd_expr("reading_score", "{Reading.score}")],
    )]);
    let schema = mr_schema();
    synthesize_implicit_joins(&mut spec, &schema).unwrap();

    let reading = Value::Map(
        [
            ("id", Value::Str("R1".into())),
            ("subject_id", Value::Str("S1".into())),
            ("score", Value::Float(95.5)),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect(),
    );
    let mut li = LookupIndex::new();
    li.register_table("Reading", std::slice::from_ref(&reading), "subject_id");

    let measurement: IndexMap<String, Value> = [
        ("id", Value::Str("M1".into())),
        ("subject_id", Value::Str("S1".into())),
        ("method", Value::Str("spiro".into())),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();

    let out = ObjectTransformer::new(spec, Some(&schema), None)
        .with_lookup_index(Arc::new(li))
        .map_object(&Value::Map(measurement), Some("Measurement"))
        .unwrap();
    let got = match &out {
        Value::Map(m) => m.get("reading_score").cloned().unwrap_or(Value::Null),
        _ => Value::Null,
    };
    assert_eq!(got, Value::Float(95.5));
}

#[test]
fn nested_class_derivation_synthesizes_join() {
    // A nested class_derivation populated from a different table synthesizes the
    // join on the hosting (outer) class derivation.
    let mut inner = cd("Obs", "Reading", vec![sd_pf("value", "score")]);
    inner.populated_from = Some("Reading".into());
    let mut readings = sd("readings");
    readings.class_derivations = Some(
        std::iter::once(("Obs".to_string(), inner)).collect(),
    );
    let mut spec = spec_with(vec![cd("Result", "Measurement", vec![readings])]);
    synthesize_implicit_joins(&mut spec, &mr_schema()).unwrap();
    let joins = result_cd(&spec).joins.as_ref().unwrap();
    assert_eq!(joins.get("Reading").unwrap().join_on.as_deref(), Some("subject_id"));
}

// ── fail-loud cases ─────────────────────────────────────────────────────────

fn assert_join_err(spec: &mut TransformationSpecification, sv: &dyn SchemaProvider, needle: &str) {
    let err = synthesize_implicit_joins(spec, sv).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains(needle), "expected {needle:?} in error, got: {msg}");
}

#[test]
fn expr_cross_table_ref_unkeyable_fails_loud() {
    // An expr ref to a table with no inferable join key fails loud, not null.
    let mut spec = spec_with(vec![cd(
        "Result",
        "Measurement",
        vec![sd_expr("score", "{Reading.score}")],
    )]);
    assert_join_err(&mut spec, &no_common_schema(), "cannot be joined");
}

#[test]
fn expr_unknown_qualified_root_fails_loud() {
    let mut spec = spec_with(vec![cd(
        "Result",
        "Measurement",
        vec![sd_expr("x", "{Nonexistent.col}")],
    )]);
    assert_join_err(&mut spec, &mr_schema(), "cannot be resolved");
}

#[test]
fn expr_same_row_qualified_root_is_allowed() {
    // `{subject_id.upper}` is rooted in a source slot on Measurement → resolvable.
    let mut spec = spec_with(vec![cd(
        "Result",
        "Measurement",
        vec![sd_expr("x", "{subject_id.upper}")],
    )]);
    synthesize_implicit_joins(&mut spec, &mr_schema()).unwrap();
}

#[test]
fn expr_declared_join_alias_is_allowed() {
    // `{myreading.score}` rooted in a declared join alias (alias != class) → ok.
    let mut result = cd("Result", "Measurement", vec![sd_expr("s", "{myreading.score}")]);
    result.joins = Some(
        std::iter::once((
            "myreading".to_string(),
            AliasedClass {
                alias: "myreading".into(),
                class_named: Some("Reading".into()),
                join_on: Some("subject_id".into()),
                ..Default::default()
            },
        ))
        .collect(),
    );
    let mut spec = spec_with(vec![result]);
    synthesize_implicit_joins(&mut spec, &mr_schema()).unwrap();
}

// ── enum / permissible-value / top-level-slot: nowhere to host a join ────────

fn enum_deriv(name: &str, expr: Option<&str>, pf: Option<&str>) -> EnumDerivation {
    EnumDerivation {
        name: name.into(),
        expr: expr.map(String::from),
        populated_from: pf.map(String::from),
        ..Default::default()
    }
}

fn spec_with_enum(ed: EnumDerivation) -> TransformationSpecification {
    let mut spec = spec_with(vec![cd("Result", "Measurement", vec![sd("id")])]);
    spec.enum_derivations = Some(std::iter::once((ed.name.clone(), ed)).collect());
    spec
}

#[test]
fn enum_derivation_cross_table_ref_fails_loud() {
    let mut spec = spec_with_enum(enum_deriv("MyEnum", Some("{Reading.score}"), None));
    assert_join_err(&mut spec, &mr_schema(), "cannot be joined");
}

#[test]
fn enum_derivation_structural_populated_from_fails_loud() {
    let mut spec = spec_with_enum(enum_deriv("MyEnum", None, Some("Reading.score")));
    assert_join_err(&mut spec, &mr_schema(), "cannot be joined");
}

#[test]
fn enum_derivation_same_row_reference_is_allowed() {
    let mut spec = spec_with_enum(enum_deriv("MyEnum", Some("{subject_id}"), None));
    synthesize_implicit_joins(&mut spec, &mr_schema()).unwrap();
}

#[test]
fn permissible_value_derivation_cross_table_ref_fails_loud() {
    let pv = PermissibleValueDerivation {
        name: "PV1".into(),
        expr: Some("{Reading.score}".into()),
        ..Default::default()
    };
    let mut ed = enum_deriv("MyEnum", None, None);
    ed.permissible_value_derivations = Some(std::iter::once(("PV1".to_string(), pv)).collect());
    let mut spec = spec_with_enum(ed);
    assert_join_err(&mut spec, &mr_schema(), "cannot be joined");
}

#[test]
fn permissible_value_derivation_structural_populated_from_fails_loud() {
    // populated_from is list-form on permissible-value derivations.
    let pv = PermissibleValueDerivation {
        name: "PV1".into(),
        populated_from: Some(vec!["Reading.score".into()]),
        ..Default::default()
    };
    let mut ed = enum_deriv("MyEnum", None, None);
    ed.permissible_value_derivations = Some(std::iter::once(("PV1".to_string(), pv)).collect());
    let mut spec = spec_with_enum(ed);
    assert_join_err(&mut spec, &mr_schema(), "cannot be joined");
}

fn spec_with_top_slot(slot: SlotDerivation) -> TransformationSpecification {
    let mut spec = spec_with(vec![cd("Result", "Measurement", vec![sd("id")])]);
    spec.slot_derivations = Some(std::iter::once((slot.name.clone(), slot)).collect());
    spec
}

#[test]
fn top_level_slot_derivation_cross_table_ref_fails_loud() {
    let mut spec = spec_with_top_slot(sd_expr("loose", "{Reading.score}"));
    assert_join_err(&mut spec, &mr_schema(), "cannot be joined");
}

#[test]
fn top_level_slot_derivation_structural_populated_from_fails_loud() {
    let mut spec = spec_with_top_slot(sd_pf("loose", "Reading.score"));
    assert_join_err(&mut spec, &mr_schema(), "cannot be joined");
}

#[test]
fn top_level_slot_derivation_fk_path_populated_from_is_allowed() {
    // `subject_id.x` is an FK/inline path (subject_id is a slot, not a table),
    // so the unhostable-ref check must leave it alone.
    let mut spec = spec_with_top_slot(sd_pf("loose", "subject_id.x"));
    synthesize_implicit_joins(&mut spec, &mr_schema()).unwrap();
}

// ── flat top-level dotted populated_from (#279) ─────────────────────────────

#[test]
fn flat_dotted_populated_from_synthesizes_join() {
    // A flat top-level `populated_from: Reading.score` under a class_derivation
    // must synthesize its join (not just nested class_derivations).
    let mut spec = spec_with(vec![cd(
        "Result",
        "Measurement",
        vec![sd("id"), sd("method"), sd_pf("score", "Reading.score")],
    )]);
    synthesize_implicit_joins(&mut spec, &mr_schema()).unwrap();
    let joins = result_cd(&spec).joins.as_ref().unwrap();
    assert_eq!(joins.get("Reading").unwrap().join_on.as_deref(), Some("subject_id"));
}

#[test]
fn flat_dotted_populated_from_resolves_via_engine() {
    let mut spec = spec_with(vec![cd(
        "Result",
        "Measurement",
        vec![sd_pf("score", "Reading.score")],
    )]);
    let schema = mr_schema();
    synthesize_implicit_joins(&mut spec, &schema).unwrap();

    let reading = Value::Map(
        [
            ("id", Value::Str("R1".into())),
            ("subject_id", Value::Str("S1".into())),
            ("score", Value::Float(88.0)),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect(),
    );
    let mut li = LookupIndex::new();
    li.register_table("Reading", std::slice::from_ref(&reading), "subject_id");

    let measurement: IndexMap<String, Value> = [
        ("id", Value::Str("M1".into())),
        ("subject_id", Value::Str("S1".into())),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();

    let out = ObjectTransformer::new(spec, Some(&schema), None)
        .with_lookup_index(Arc::new(li))
        .map_object(&Value::Map(measurement), Some("Measurement"))
        .unwrap();
    let got = match &out {
        Value::Map(m) => m.get("score").cloned().unwrap_or(Value::Null),
        _ => Value::Null,
    };
    assert_eq!(got, Value::Float(88.0));
}

#[test]
fn fk_path_dotted_populated_from_is_not_flagged() {
    // A dotted `populated_from` whose root is a slot (subject_id), not a table,
    // is an FK/inline path — synthesis leaves it alone (no join, no error).
    let mut spec = spec_with(vec![cd(
        "Result",
        "Measurement",
        vec![sd_pf("x", "subject_id.upper")],
    )]);
    synthesize_implicit_joins(&mut spec, &mr_schema()).unwrap();
    assert!(result_cd(&spec).joins.is_none());
}

// ── idempotence: an explicit join is not overwritten ────────────────────────

#[test]
fn explicit_join_is_preserved() {
    let mut result = cd("Result", "Measurement", vec![sd_expr("s", "{Reading.score}")]);
    result.joins = Some(
        std::iter::once((
            "Reading".to_string(),
            AliasedClass {
                alias: "Reading".into(),
                source_key: Some("id".into()),
                ..Default::default()
            },
        ))
        .collect(),
    );
    let mut spec = spec_with(vec![result]);
    synthesize_implicit_joins(&mut spec, &mr_schema()).unwrap();
    // The declared join (keyed on `id`) is preserved, not replaced by a
    // synthesized one keyed on `subject_id`.
    let ac = result_cd(&spec).joins.as_ref().unwrap().get("Reading").unwrap();
    assert_eq!(ac.source_key.as_deref(), Some("id"));
    assert!(ac.join_on.is_none());
}

// ── expression completeness guard (mirrors test_expression_locations.py) ─────

#[test]
fn expression_mapping_reference_synthesizes_join() {
    // A `{Reading.score}` hiding in an expression_mappings value is scanned too.
    let mut sd = sd("computed");
    sd.expression_mappings = Some(
        std::iter::once((
            "M1".to_string(),
            KeyVal {
                key: "M1".into(),
                value: Some(serde_json::Value::String("{Reading.score}".into())),
            },
        ))
        .collect(),
    );
    let mut spec = spec_with(vec![cd("Result", "Measurement", vec![sd])]);
    synthesize_implicit_joins(&mut spec, &mr_schema()).unwrap();
    let joins = result_cd(&spec).joins.as_ref().unwrap();
    assert_eq!(joins.get("Reading").unwrap().join_on.as_deref(), Some("subject_id"));
}
