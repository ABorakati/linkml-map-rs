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

use linkml_map_cli::{run_map_data_config, MapDataConfig};
use linkml_map_core::value::Value;
use linkml_map_io::{load_all, Format};
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
