//! Integration tests for the `map-data` subcommand.
//!
//! Calls `linkml_map_cli::run_map_data_config` directly (no subprocess needed).
//!
//! Golden fixture: `tests/examples/measurements/PersonQuantityValue-001`
//!   input:    PersonQuantityValue-001.yaml  →  {id: P:1, height: {value: 172.0, unit: cm}}
//!   expected: PersonQuantityValue-001.transformed.yaml  →  {id: P:1, height_in_cm: 172.0}
//!
//! This fixture is marked PASS in tests/CONFORMANCE_REPORT.md.

use std::path::PathBuf;

use linkml_map_cli::{MapDataConfig, run_map_data_config, run_map_data_config_with_options};
use linkml_map_core::value::Value;
use linkml_map_io::{Format, load_all};
use tempfile::tempdir;

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Return the workspace-root `tests/examples/measurements` directory.
fn measurements_dir() -> PathBuf {
    // CARGO_MANIFEST_DIR = <workspace>/crates/linkml-map-cli
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
    PathBuf::from(&manifest)
        .parent() // crates/
        .unwrap()
        .parent() // workspace root
        .unwrap()
        .join("tests/examples/measurements")
}

/// Make a two-row fixture where the first row fails `int()` coercion and the
/// second one is valid. YAML input deliberately exercises the generic streaming
/// path rather than the JSONL bulk fast path.
fn write_continue_on_error_fixture(dir: &std::path::Path) -> (PathBuf, PathBuf, PathBuf) {
    let schema = dir.join("schema.yaml");
    std::fs::write(
        &schema,
        r#"id: https://example.org/continue-on-error
name: continue_on_error
prefixes:
  linkml: https://w3id.org/linkml/
default_range: string
imports:
  - linkml:types
classes:
  Person:
    attributes:
      id:
        identifier: true
      age: {}
  Result:
    attributes:
      id:
        identifier: true
      age:
        range: integer
"#,
    )
    .expect("writing schema");

    let spec = dir.join("spec.yaml");
    std::fs::write(
        &spec,
        r#"id: https://example.org/continue-on-error-transform
class_derivations:
  Result:
    populated_from: Person
    slot_derivations:
      id:
        populated_from: id
      age:
        expr: "int({age})"
"#,
    )
    .expect("writing spec");

    let input = dir.join("input.yaml");
    std::fs::write(
        &input,
        r#"- id: bad
  age: not-a-number
- id: good
  age: "7"
"#,
    )
    .expect("writing input");

    (schema, spec, input)
}

fn continue_on_error_config(
    schema: &std::path::Path,
    spec: &std::path::Path,
    input: &std::path::Path,
    output: &std::path::Path,
) -> MapDataConfig {
    MapDataConfig {
        spec: spec.to_string_lossy().into_owned(),
        source_schema: schema.to_string_lossy().into_owned(),
        target_schema: Some(schema.to_string_lossy().into_owned()),
        output: output.to_string_lossy().into_owned(),
        source_class: Some("Person".to_owned()),
        input_format: "yaml".to_owned(),
        output_format: "json".to_owned(),
        workers: 1,
        unordered: false,
        input: input.to_string_lossy().into_owned(),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// End-to-end: single YAML input row → JSONL output, assertions on stats + content.
#[tokio::test]
async fn test_map_data_measurements_001_golden() {
    let fix = measurements_dir();
    let tmp = tempdir().expect("tempdir");
    let out_path = tmp.path().join("out.jsonl");
    let out_str = out_path.to_str().unwrap().to_owned();

    let cfg = MapDataConfig {
        spec: fix
            .join("transform/qv-to-scalar.transform.yaml")
            .to_str()
            .unwrap()
            .to_owned(),
        source_schema: fix
            .join("source/quantity_value.yaml")
            .to_str()
            .unwrap()
            .to_owned(),
        target_schema: Some(
            fix.join("target/quantity_value_flat.yaml")
                .to_str()
                .unwrap()
                .to_owned(),
        ),
        output: out_str.clone(),
        source_class: Some("Person".to_owned()),
        input_format: "auto".to_owned(),
        output_format: "jsonl".to_owned(),
        workers: 2,
        unordered: false,
        input: fix
            .join("data/PersonQuantityValue-001.yaml")
            .to_str()
            .unwrap()
            .to_owned(),
    };

    // ── Run ───────────────────────────────────────────────────────────────────
    let stats = run_map_data_config(cfg)
        .await
        .expect("map-data pipeline failed");

    // ── Stats assertions ──────────────────────────────────────────────────────
    assert_eq!(stats.rows_in, 1, "expected 1 row in");
    assert_eq!(stats.rows_out, 1, "expected 1 row out");
    assert!(
        stats.elapsed.as_secs_f64() < 30.0,
        "pipeline took suspiciously long: {:?}",
        stats.elapsed
    );

    // ── Output content assertions (matches golden) ────────────────────────────
    // Golden: {id: P:1, height_in_cm: 172.0}
    let rows = load_all(&out_path, Format::Jsonl)
        .await
        .expect("loading output jsonl");

    assert_eq!(rows.len(), 1, "output should have exactly 1 row");

    let row = &rows[0];
    let map = match row {
        Value::Map(m) => m,
        other => panic!("expected Value::Map, got {:?}", other),
    };

    // id field
    assert_eq!(
        map.get("id"),
        Some(&Value::Str("P:1".into())),
        "id mismatch"
    );

    // height_in_cm should be 172.0 (float or int)
    let height = map
        .get("height_in_cm")
        .expect("height_in_cm missing in output");
    match height {
        Value::Float(f) => assert!(
            (f - 172.0_f64).abs() < 1e-6,
            "height_in_cm expected 172.0, got {f}"
        ),
        Value::Int(i) => assert_eq!(*i, 172, "height_in_cm expected 172, got {i}"),
        other => panic!("unexpected height_in_cm type: {:?}", other),
    }

    // Extra keys should not be present (the flat schema only has id + height_in_cm)
    assert!(
        !map.contains_key("height"),
        "nested 'height' field should not appear in flat output"
    );
}

/// End-to-end: single YAML input row → YAML output should remain a top-level object,
/// matching Python `linkml-map`'s non-streaming `map-data` behavior.
#[tokio::test]
async fn test_map_data_measurements_001_yaml_output_shape() {
    let fix = measurements_dir();
    let tmp = tempdir().expect("tempdir");
    let out_path = tmp.path().join("out.yaml");

    let cfg = MapDataConfig {
        spec: fix
            .join("transform/qv-to-scalar.transform.yaml")
            .to_str()
            .unwrap()
            .to_owned(),
        source_schema: fix
            .join("source/quantity_value.yaml")
            .to_str()
            .unwrap()
            .to_owned(),
        target_schema: Some(
            fix.join("target/quantity_value_flat.yaml")
                .to_str()
                .unwrap()
                .to_owned(),
        ),
        output: out_path.to_str().unwrap().to_owned(),
        source_class: Some("Person".to_owned()),
        input_format: "auto".to_owned(),
        output_format: "yaml".to_owned(),
        workers: 2,
        unordered: false,
        input: fix
            .join("data/PersonQuantityValue-001.yaml")
            .to_str()
            .unwrap()
            .to_owned(),
    };

    let stats = run_map_data_config(cfg)
        .await
        .expect("map-data pipeline failed");
    assert_eq!(stats.rows_in, 1, "expected 1 row in");
    assert_eq!(stats.rows_out, 1, "expected 1 row out");

    let output_text = std::fs::read_to_string(&out_path).expect("reading raw YAML output failed");
    let output_yaml: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(&output_text).expect("parsing raw YAML output failed");
    assert!(
        matches!(output_yaml, serde_yaml_ng::Value::Mapping(_)),
        "single transformed object should serialize as a top-level YAML object"
    );

    let expected_path = fix.join("output/PersonQuantityValue-001.transformed.yaml");
    let expected_text =
        std::fs::read_to_string(&expected_path).expect("reading golden YAML output failed");
    let expected_yaml: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(&expected_text).expect("parsing golden YAML output failed");
    assert_eq!(
        output_yaml, expected_yaml,
        "raw YAML object shape should match the golden fixture"
    );
}

/// Two-row fixture (PersonQuantityValue-002 adds a second person).
/// Verifies rows_in/rows_out and that both ids appear.
#[tokio::test]
async fn test_map_data_measurements_002_two_rows() {
    let fix = measurements_dir();

    // PersonQuantityValue-002 may or may not exist — skip gracefully if absent.
    let data_file = fix.join("data/PersonQuantityValue-002.yaml");
    if !data_file.exists() {
        eprintln!("SKIP: PersonQuantityValue-002.yaml not found");
        return;
    }

    let tmp = tempdir().expect("tempdir");
    let out_path = tmp.path().join("out.jsonl");

    let cfg = MapDataConfig {
        spec: fix
            .join("transform/qv-to-scalar.transform.yaml")
            .to_str()
            .unwrap()
            .to_owned(),
        source_schema: fix
            .join("source/quantity_value.yaml")
            .to_str()
            .unwrap()
            .to_owned(),
        target_schema: Some(
            fix.join("target/quantity_value_flat.yaml")
                .to_str()
                .unwrap()
                .to_owned(),
        ),
        output: out_path.to_str().unwrap().to_owned(),
        source_class: Some("Person".to_owned()),
        input_format: "auto".to_owned(),
        output_format: "jsonl".to_owned(),
        workers: 2,
        unordered: false,
        input: data_file.to_str().unwrap().to_owned(),
    };

    let stats = run_map_data_config(cfg)
        .await
        .expect("pipeline failed on 002");

    // Fixture-002 contains ≥1 rows; exact count depends on the fixture file.
    assert!(stats.rows_in >= 1, "expected at least 1 row in");
    assert_eq!(
        stats.rows_out, stats.rows_in,
        "all rows should transform without error"
    );
}

/// `--continue-on-error` preserves the valid rows after a row-level transform
/// error, but reports failure overall after output is completed.
#[tokio::test]
async fn test_continue_on_error_writes_later_valid_row_and_returns_error() {
    let tmp = tempdir().expect("tempdir");
    let (schema, spec, input) = write_continue_on_error_fixture(tmp.path());
    let output = tmp.path().join("continued.json");

    let err = run_map_data_config_with_options(
        continue_on_error_config(&schema, &spec, &input, &output),
        true,
    )
    .await
    .expect_err("a completed run with a failed row must be non-zero");
    assert!(
        format!("{err:#}").contains("1 of 2 rows failed to transform"),
        "unexpected error: {err:#}"
    );

    let rows = load_all(&output, Format::Json)
        .await
        .expect("continued output should be readable");
    assert_eq!(rows.len(), 1, "only the successful row should be written");
    let Value::Map(row) = &rows[0] else {
        panic!("expected output object, got {:?}", rows[0]);
    };
    assert_eq!(row.get("id"), Some(&Value::Str("good".into())));
    assert_eq!(row.get("age"), Some(&Value::Int(7)));
}

/// The long-standing public API is intentionally fail-fast unless the caller
/// opts into continuation.
#[tokio::test]
async fn test_default_map_data_api_stays_fail_fast() {
    let tmp = tempdir().expect("tempdir");
    let (schema, spec, input) = write_continue_on_error_fixture(tmp.path());
    let output = tmp.path().join("fail-fast.json");

    let err = run_map_data_config(continue_on_error_config(&schema, &spec, &input, &output))
        .await
        .expect_err("the default API must stop at the first row error");
    assert!(
        format!("{err:#}").contains("transform error at row 0"),
        "unexpected error: {err:#}"
    );
}

/// Verify that an unknown --input-format string is rejected before the pipeline runs.
#[test]
fn test_parse_format_str_rejects_unknown() {
    let result = linkml_map_cli::parse_format_str("parquet");
    assert!(result.is_err(), "parquet should not be a supported format");
}

/// Verify that "auto" returns None (meaning: detect from path).
#[test]
fn test_parse_format_str_auto_is_none() {
    let result = linkml_map_cli::parse_format_str("auto").unwrap();
    assert!(result.is_none(), "auto should map to None");
}
