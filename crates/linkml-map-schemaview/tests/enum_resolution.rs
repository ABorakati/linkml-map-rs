//! Enum-ranged slot values resolve to the target enum's permissible value keys
//! (upstream 209f794; port of `tests/test_transformer/test_target_enum_resolution.py`).
//!
//! A spec may spell a permissible value by its key, `meaning`, `title` or an
//! alias; the output always carries the key.

use linkml_map_core::{
    datamodel::{TransformationSpecification, normalise_spec_json},
    engine::{ObjectTransformer, TargetCoercions},
    enum_index::enum_index,
    schema::SchemaProvider,
    value::Value,
};
use linkml_map_schemaview::SchemaViewProvider;

const SOURCE_SCHEMA: &str = r"
id: https://example.org/enum-source
name: enum_source
prefixes: {linkml: 'https://w3id.org/linkml/'}
default_prefix: enum_source
default_range: string
imports: [linkml:types]
classes:
  Person:
    attributes:
      id: {identifier: true}
      gender: {}
      genders: {multivalued: true}
";

/// Target schema with a given `enums:` body and a single enum-ranged `sex`
/// slot (plus the other slots the setups use).
fn target_schema(enums_yaml: &str) -> String {
    format!(
        r"
id: https://example.org/enum-target
name: enum_target
prefixes:
  linkml: 'https://w3id.org/linkml/'
  OMOP:
    prefix_prefix: OMOP
    prefix_reference: https://athena.ohdsi.org/search-terms/terms/
default_prefix: enum_target
default_range: string
imports: [linkml:types]
enums:
{enums_yaml}
classes:
  Agent:
    attributes:
      id: {{identifier: true}}
      sex: {{range: SexEnum}}
      sex_literal: {{range: SexEnum}}
      sex_expr: {{range: SexEnum}}
      sexes: {{range: SexEnum, multivalued: true}}
"
    )
}

const SEX_ENUM: &str = r#"
  SexEnum:
    permissible_values:
      "OMOP:8532":
        title: FEMALE
        meaning: OMOP:8532
        aliases: [F, female]
      "OMOP:8507":
        title: MALE
        meaning: OMOP:8507
        aliases: [M, male]
"#;

fn s(v: &str) -> Value {
    Value::Str(v.into())
}

fn spec(slot_yaml: &str) -> TransformationSpecification {
    let yaml = format!(
        "id: enum-resolution\nclass_derivations:\n  Agent:\n    populated_from: Person\n    slot_derivations:\n      id:\n        populated_from: id\n{slot_yaml}"
    );
    let mut obj: serde_json::Value = serde_yaml::from_str(&yaml).unwrap();
    normalise_spec_json(&mut obj);
    serde_json::from_value(obj).unwrap()
}

fn person(pairs: Vec<(&str, Value)>) -> Value {
    let mut m: indexmap::IndexMap<String, Value> =
        [("id".to_owned(), s("P:001"))].into_iter().collect();
    m.extend(pairs.into_iter().map(|(k, v)| (k.to_owned(), v)));
    Value::Map(m)
}

fn get(v: &Value, key: &str) -> Value {
    match v {
        Value::Map(m) => m.get(key).cloned().unwrap_or(Value::Null),
        other => panic!("expected a map, got {other:?}"),
    }
}

fn run(enums: &str, slot_yaml: &str, strict: bool, row: &Value) -> Result<Value, String> {
    let source = SchemaViewProvider::from_yaml_str(SOURCE_SCHEMA).unwrap();
    let target = SchemaViewProvider::from_yaml_str(&target_schema(enums)).unwrap();
    ObjectTransformer::new(spec(slot_yaml), Some(&source), Some(&target))
        .with_strict_exprs(strict)
        .map_object(row, Some("Person"))
        .map_err(|e| e.to_string())
}

// setup_value_mapping_target_by_title
#[test]
fn value_mapping_target_by_title_resolves_to_key() {
    let out = run(
        SEX_ENUM,
        "      sex:\n        populated_from: gender\n        value_mappings:\n          f: FEMALE\n          m: MALE\n",
        false,
        &person(vec![("gender", s("f"))]),
    )
    .unwrap();
    assert_eq!(get(&out, "sex"), s("OMOP:8532"));
}

// setup_value_literal_by_expanded_meaning
#[test]
fn value_literal_by_expanded_meaning() {
    let out = run(
        SEX_ENUM,
        "      sex_literal:\n        value: https://athena.ohdsi.org/search-terms/terms/8507\n",
        false,
        &person(vec![]),
    )
    .unwrap();
    assert_eq!(get(&out, "sex_literal"), s("OMOP:8507"));
}

// setup_expr_result_by_alias
#[test]
fn expr_result_by_alias() {
    let out = run(
        SEX_ENUM,
        "      sex_expr:\n        expr: \"case(({id} == 'P:001', 'female'), (True, 'male'))\"\n",
        false,
        &person(vec![]),
    )
    .unwrap();
    assert_eq!(get(&out, "sex_expr"), s("OMOP:8532"));
}

// setup_multivalued_resolves_each_element
#[test]
fn multivalued_resolves_each_element() {
    let out = run(
        SEX_ENUM,
        "      sexes:\n        populated_from: genders\n",
        false,
        &person(vec![(
            "genders",
            Value::List(vec![s("F"), s("OMOP:8507"), s("FEMALE")]),
        )]),
    )
    .unwrap();
    assert_eq!(
        get(&out, "sexes"),
        Value::List(vec![s("OMOP:8532"), s("OMOP:8507"), s("OMOP:8532")])
    );
}

const SEX_SLOT: &str = "      sex:\n        value: \"unknown\"\n";

#[test]
fn unmatched_value_warns_once_and_passes_through() {
    let source = SchemaViewProvider::from_yaml_str(SOURCE_SCHEMA).unwrap();
    let target = SchemaViewProvider::from_yaml_str(&target_schema(SEX_ENUM)).unwrap();
    let sp = spec(SEX_SLOT);
    // One shared table for the run: the warn-once state lives in it, as in the
    // pipeline, where a fresh engine is built per row.
    let shared = TargetCoercions::build(&sp, &target);
    for _ in 0..3 {
        let engine = ObjectTransformer::new_borrowed(&sp, Some(&source), Some(&target))
            .with_target_coercions(&shared);
        let out = engine.map_object(&person(vec![]), Some("Person")).unwrap();
        assert_eq!(get(&out, "sex"), s("unknown"));
    }
    assert_eq!(shared.warned_enum_value_count(), 1);
}

#[test]
fn unmatched_value_raises_in_strict_mode() {
    let err = run(SEX_ENUM, SEX_SLOT, true, &person(vec![])).unwrap_err();
    assert!(
        err.contains("'unknown' is not a permissible value of enum SexEnum"),
        "{err}"
    );
}

#[test]
fn ambiguous_value_raises_in_strict_mode() {
    let enums = "  SexEnum:\n    permissible_values:\n      F1:\n        title: FEMALE\n      F2:\n        title: FEMALE\n";
    let err = run(
        enums,
        "      sex:\n        value: \"FEMALE\"\n",
        true,
        &person(vec![]),
    )
    .unwrap_err();
    assert!(
        err.contains("matches more than one permissible value of enum SexEnum: ['F1', 'F2']"),
        "{err}"
    );
}

#[test]
fn key_wins_over_another_values_title() {
    let enums = "  SexEnum:\n    permissible_values:\n      FEMALE:\n        title: female (legacy)\n      \"OMOP:8532\":\n        title: FEMALE\n";
    let out = run(
        enums,
        "      sex:\n        value: \"FEMALE\"\n",
        true,
        &person(vec![]),
    )
    .unwrap();
    assert_eq!(get(&out, "sex"), s("FEMALE"));
}

#[test]
fn spec_range_overrides_target_enum() {
    let out = run(
        SEX_ENUM,
        "      sex:\n        value: \"female\"\n        range: string\n",
        true,
        &person(vec![]),
    )
    .unwrap();
    assert_eq!(get(&out, "sex"), s("female"));
}

#[test]
fn no_target_schema_leaves_enum_values_alone() {
    let source = SchemaViewProvider::from_yaml_str(SOURCE_SCHEMA).unwrap();
    let out = ObjectTransformer::new(spec(SEX_SLOT), Some(&source), None)
        .with_strict_exprs(true)
        .map_object(&person(vec![]), Some("Person"))
        .unwrap();
    assert_eq!(get(&out, "sex"), s("unknown"));
}

#[test]
fn open_enums_have_no_index() {
    for enum_yaml in [
        "Dynamic:\n    reachable_from:\n      source_ontology: obo:mondo\n      source_nodes: [MONDO:0000001]",
        "Dynamic:\n    description: no permissible values",
    ] {
        let sv = SchemaViewProvider::from_yaml_str(&format!(
            "id: https://example.org/e\nname: e\nprefixes:\n  obo: http://purl.obolibrary.org/obo/\nenums:\n  {enum_yaml}\n"
        ))
        .unwrap();
        assert!(sv.all_enum_names().iter().any(|e| e == "Dynamic"));
        assert!(enum_index(&sv, "Dynamic").is_none(), "{enum_yaml}");
    }
}
