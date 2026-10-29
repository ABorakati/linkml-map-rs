//! A foreign key in an expression is dereferenced only for single-object input.
//!
//! Upstream builds its `ObjectIndex` in `cli.py::_map_data_single` (a YAML / JSON file) and nowhere
//! on the streaming path (`_map_data_streaming`, a table or a directory), so there a reference
//! slot keeps its bare identifier: `{boss.label}` raises `'str' object has no attribute 'label'`
//! and `populated_from: boss.label` is null. The expected outcomes were produced by upstream
//! `linkml-map` v0.6.0-rc1 on the same inputs (`tests/differential/corpus/*fk_expr*`).

use linkml_map_io::Format;
use linkml_map_pipeline::{PipelineConfig, run_pipeline_with_options};

const SCHEMA: &str = "\
id: https://example.org/fk
name: fk
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
      label:
      boss:
        range: Person
  Container:
    tree_root: true
    attributes:
      persons:
        range: Person
        multivalued: true
        inlined: true
";

const PEOPLE_TSV: &str = "id\tlabel\tboss\nA\tLA\tB\nB\tLB\t\n";

const PEOPLE_YAML: &str = "persons:\n  - {id: A, label: LA, boss: B}\n  - {id: B, label: LB}\n";

fn spec(slot: &str) -> String {
    format!(
        "id: https://example.org/spec
class_derivations:
  Agent:
    populated_from: Person
    slot_derivations:
      id: {{populated_from: id}}
      {slot}
"
    )
}

/// Run `slot` over `file` (TSV or YAML) as the only `Person` input and return the output text.
async fn run(file: &str, content: &str, slot: &str, source_class: &str) -> anyhow::Result<String> {
    let tmp = tempfile::tempdir()?;
    let t = tmp.path();
    std::fs::write(t.join("source.yaml"), SCHEMA)?;
    std::fs::write(t.join("spec.yaml"), spec(slot))?;
    let input = if file.ends_with(".tsv") {
        // A directory of tables, as in the corpus: `Person.tsv`.
        std::fs::create_dir(t.join("data"))?;
        std::fs::write(t.join("data").join(file), content)?;
        t.join("data")
    } else {
        std::fs::write(t.join(file), content)?;
        t.join(file)
    };
    let out = t.join("out.jsonl");
    let config = PipelineConfig {
        source_path: input.to_str().unwrap().to_owned(),
        source_format: None,
        schema_path: t.join("source.yaml").to_str().unwrap().to_owned(),
        target_schema_path: None,
        spec_path: t.join("spec.yaml").to_str().unwrap().to_owned(),
        out_path: out.to_str().unwrap().to_owned(),
        out_format: Some(Format::Jsonl),
        source_class: Some(source_class.to_owned()),
        workers: 2,
        ordered: true,
        channel_depth: 16,
        strict: false,
        unrestricted_eval: false,
        write_options: Default::default(),
    };
    run_pipeline_with_options(config, false).await?;
    Ok(std::fs::read_to_string(out)?)
}

fn error_chain(e: &anyhow::Error) -> String {
    format!("{e:#}")
}

#[tokio::test]
async fn directory_expression_reads_the_bare_identifier() {
    // Row A's boss is `B`, a row of the same table; upstream still fails on `'B'.label`.
    let err = run(
        "Person.tsv",
        PEOPLE_TSV,
        "bl: {expr: '{boss.label}'}",
        "Person",
    )
    .await
    .unwrap_err();
    assert!(
        error_chain(&err).contains("'str' object has no attribute 'label'"),
        "{err:#}"
    );
}

#[tokio::test]
async fn directory_populated_from_path_is_null() {
    let out = run(
        "Person.tsv",
        PEOPLE_TSV,
        "bl: {populated_from: boss.label}",
        "Person",
    )
    .await
    .unwrap();
    // `bl` is null for every row, so it is not emitted.
    assert_eq!(out, "{\"id\": \"A\"}\n{\"id\": \"B\"}\n");
}

#[tokio::test]
async fn directory_bare_reference_is_the_identifier() {
    let out = run("Person.tsv", PEOPLE_TSV, "b: {expr: '{boss}'}", "Person")
        .await
        .unwrap();
    assert_eq!(out, "{\"id\": \"A\", \"b\": \"B\"}\n{\"id\": \"B\"}\n");
}

#[tokio::test]
async fn single_object_expression_dereferences_the_reference() {
    // The same data as one YAML container: `tr.index(...)` is called, so `boss` is the object.
    let spec_slot = "bl: {expr: '{boss.label}'}";
    let tmp = tempfile::tempdir().unwrap();
    let t = tmp.path();
    std::fs::write(t.join("source.yaml"), SCHEMA).unwrap();
    let container_spec = format!(
        "id: https://example.org/spec
class_derivations:
  Container:
    populated_from: Container
    slot_derivations:
      persons: {{populated_from: persons}}
  Agent:
    populated_from: Person
    slot_derivations:
      id: {{populated_from: id}}
      {spec_slot}
"
    );
    std::fs::write(t.join("spec.yaml"), container_spec).unwrap();
    std::fs::write(t.join("data.yaml"), PEOPLE_YAML).unwrap();
    let out = t.join("out.yaml");
    let config = PipelineConfig {
        source_path: t.join("data.yaml").to_str().unwrap().to_owned(),
        source_format: None,
        schema_path: t.join("source.yaml").to_str().unwrap().to_owned(),
        target_schema_path: None,
        spec_path: t.join("spec.yaml").to_str().unwrap().to_owned(),
        out_path: out.to_str().unwrap().to_owned(),
        out_format: Some(Format::Yaml),
        source_class: Some("Container".to_owned()),
        workers: 2,
        ordered: true,
        channel_depth: 16,
        strict: false,
        unrestricted_eval: false,
        write_options: Default::default(),
    };
    run_pipeline_with_options(config, false).await.unwrap();
    let text = std::fs::read_to_string(out).unwrap();
    assert!(text.contains("bl: LB"), "{text}");
}

#[tokio::test]
async fn single_object_input_rejects_a_key_the_schema_does_not_know() {
    // Even a spec that reads no reference: `tr.index(input_obj)` runs first upstream.
    let err = run(
        "person.yaml",
        "id: A\nlabel: LA\nzzz: 1\n",
        "l: {populated_from: label}",
        "Person",
    )
    .await
    .unwrap_err();
    assert!(
        error_chain(&err).contains("No such slot zzz as an attribute of Person ancestors"),
        "{err:#}"
    );
}

#[tokio::test]
async fn a_table_with_an_unknown_column_is_not_indexed() {
    // The streaming path never calls `index()`, so an unknown column is simply not read.
    let out = run(
        "Person.tsv",
        "id\tlabel\tzzz\nA\tLA\t1\n",
        "l: {populated_from: label}",
        "Person",
    )
    .await
    .unwrap();
    assert_eq!(out, "{\"id\": \"A\", \"l\": \"LA\"}\n");
}
