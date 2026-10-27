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
        write_options: Default::default(),
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
        write_options: Default::default(),
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
        write_options: Default::default(),
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
        write_options: Default::default(),
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
    let result = linkml_map_cli::parse_format_str("xlsx");
    assert!(result.is_err(), "xlsx should not be a supported format");
}

/// Verify that "auto" returns None (meaning: detect from path).
#[test]
fn test_parse_format_str_auto_is_none() {
    let result = linkml_map_cli::parse_format_str("auto").unwrap();
    assert!(result.is_none(), "auto should map to None");
}

// ── Stdout output tests ─────────────────────────────────────────────────────

/// `-o -` with explicit `--output-format` must succeed (output goes to test-harness stdout).
#[tokio::test]
async fn test_map_data_stdout_with_explicit_format() {
    let fix = measurements_dir();

    let cfg = linkml_map_cli::MapDataConfig {
        write_options: Default::default(),
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
        output: "-".to_string(),
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

    let result = linkml_map_cli::run_map_data_config(cfg).await;
    assert!(
        result.is_ok(),
        "stdout output should succeed: {:?}",
        result.err()
    );
    let stats = result.unwrap();
    assert!(stats.rows_out > 0, "should produce at least one output row");
}

/// `-o -` WITHOUT `--output-format` defaults to YAML, as upstream's `map-data` does
/// when no output file (and so no extension) is given.
#[tokio::test]
async fn test_map_data_stdout_without_format_defaults_to_yaml() {
    let fix = measurements_dir();

    let cfg = linkml_map_cli::MapDataConfig {
        write_options: Default::default(),
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
        output: "-".to_string(),
        source_class: Some("Person".to_owned()),
        input_format: "auto".to_owned(),
        output_format: "auto".to_owned(), // "auto" maps to None → YAML on stdout
        workers: 2,
        unordered: false,
        input: fix
            .join("data/PersonQuantityValue-001.yaml")
            .to_str()
            .unwrap()
            .to_owned(),
    };

    let result = linkml_map_cli::run_map_data_config(cfg).await;
    let stats = result.expect("stdout without a format should default to YAML");
    assert_eq!(stats.rows_out, 1);
}

// ── Output writers: null stripping and layout (upstream output_streams.py) ──

/// Measurements fixture whose row (`PersonQuantityValue-002`) makes the engine emit a
/// null-valued slot (`height_in_cm: null`).  The row is fed as JSONL so the streaming
/// writers (not upstream's single-object `dump_output`) are used.
fn null_slot_config(output: &str, output_format: &str) -> (MapDataConfig, tempfile::TempDir) {
    let fix = measurements_dir();
    let tmp = tempdir().expect("tempdir");
    let input = tmp.path().join("Person.jsonl");
    std::fs::write(
        &input,
        "{\"id\": \"P:2\", \"height\": {\"value\": 1.72, \"unit\": \"m\"}}\n",
    )
    .expect("writing JSONL");
    let mut cfg = MapDataConfig::new(
        fix.join("transform/qv-to-scalar.transform.yaml")
            .to_str()
            .unwrap(),
        fix.join("source/quantity_value.yaml").to_str().unwrap(),
        input.to_str().unwrap(),
        tmp.path().join(output).to_str().unwrap(),
    );
    cfg.target_schema = Some(
        fix.join("target/quantity_value_flat.yaml")
            .to_str()
            .unwrap()
            .to_owned(),
    );
    cfg.source_class = Some("Person".to_owned());
    cfg.output_format = output_format.to_owned();
    cfg.workers = 2;
    (cfg, tmp)
}

/// Streaming output drops null slots in JSON, JSONL and YAML, and writes stream YAML
/// as a `---`-terminated document.  Port of upstream `test_writers_drop_null_slots`.
#[tokio::test]
async fn test_stream_outputs_drop_null_slots() {
    for (file, format, want) in [
        ("out.jsonl", "jsonl", "{\"id\": \"P:2\"}\n"),
        ("out.json", "json", "[\n{\n  \"id\": \"P:2\"\n}\n]\n"),
        ("out.yaml", "yaml", "id: P:2\n---\n"),
    ] {
        let (cfg, tmp) = null_slot_config(file, format);
        run_map_data_config(cfg).await.expect("map-data failed");
        let text = std::fs::read_to_string(tmp.path().join(file)).unwrap();
        assert_eq!(text, want, "{format}");
    }
}

/// `-O` fans the same records out to further files, format inferred per file.
#[tokio::test]
async fn test_additional_outputs_fan_out() {
    let (mut cfg, tmp) = null_slot_config("out.jsonl", "auto");
    let extra = tmp.path().join("extra.tsv");
    cfg.write_options.additional_outputs = vec![extra.clone()];
    run_map_data_config(cfg).await.expect("map-data failed");
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("out.jsonl")).unwrap(),
        "{\"id\": \"P:2\"}\n"
    );
    // CSV/TSV keep null slots as empty cells (only JSON/JSONL/YAML strip them).
    assert_eq!(
        std::fs::read_to_string(extra).unwrap(),
        "id\theight_in_cm\nP:2\t\n"
    );
}

/// A single YAML object goes through upstream's `dump_output`: one plain document,
/// insertion order, no `---`.
#[tokio::test]
async fn test_single_object_yaml_has_no_document_marker() {
    let fix = measurements_dir();
    let tmp = tempdir().expect("tempdir");
    let out = tmp.path().join("out.yaml");
    let mut cfg = MapDataConfig::new(
        fix.join("transform/qv-to-scalar.transform.yaml")
            .to_str()
            .unwrap(),
        fix.join("source/quantity_value.yaml").to_str().unwrap(),
        fix.join("data/PersonQuantityValue-001.yaml")
            .to_str()
            .unwrap(),
        out.to_str().unwrap(),
    );
    cfg.target_schema = Some(
        fix.join("target/quantity_value_flat.yaml")
            .to_str()
            .unwrap()
            .to_owned(),
    );
    cfg.source_class = Some("Person".to_owned());
    cfg.workers = 2;
    run_map_data_config(cfg).await.expect("map-data failed");
    assert_eq!(
        std::fs::read_to_string(&out).unwrap(),
        "id: P:1\nheight_in_cm: 172.0\n"
    );
}

// ── Parquet / DuckDB output (upstream 04dc4c8, tests/test_cli/test_cli_columnar.py) ──

/// Fixtures for the columnar tests: the shared tabular example.
fn tabular_config(output: &std::path::Path) -> MapDataConfig {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
    let fix = PathBuf::from(manifest)
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("tests/examples/tabular");
    let mut cfg = MapDataConfig::new(
        fix.join("transform/person_to_agent.transform.yaml")
            .to_str()
            .unwrap(),
        fix.join("source/person_flat.yaml").to_str().unwrap(),
        fix.join("data/Person.tsv").to_str().unwrap(),
        output.to_str().unwrap(),
    );
    cfg.source_class = Some("Person".to_owned());
    cfg.workers = 2;
    cfg
}

/// A default build has no DuckDB: asking for Parquet fails with a rebuild hint.
#[cfg(not(feature = "columnar"))]
#[tokio::test]
async fn test_parquet_without_columnar_feature_fails_with_hint() {
    let tmp = tempdir().expect("tempdir");
    let target = tmp.path().join("agent.parquet");
    let err = run_map_data_config(tabular_config(&target))
        .await
        .expect_err("parquet needs the columnar feature");
    assert!(
        format!("{err:#}").contains("--features columnar"),
        "{err:#}"
    );
}

/// `parquet`/`duckdb` cannot go to stdout, with or without the feature
/// (upstream: "{fmt} output requires an output file; pass -o/--output FILE").
#[tokio::test]
async fn test_columnar_to_stdout_is_rejected() {
    for format in ["parquet", "duckdb"] {
        let tmp = tempdir().expect("tempdir");
        let mut cfg = tabular_config(&tmp.path().join("unused"));
        cfg.output = "-".to_owned();
        cfg.output_format = format.to_owned();
        let err = run_map_data_config(cfg).await.expect_err("must fail");
        assert!(
            format!("{err:#}").contains(&format!(
                "{format} output requires an output file; pass -o/--output FILE"
            )),
            "{err:#}"
        );
    }
}

#[cfg(feature = "columnar")]
mod columnar {
    use super::*;
    use linkml_map_duckdb::columnar::duckdb::Connection;

    fn strings(con: &Connection, sql: &str) -> Vec<String> {
        let mut stmt = con.prepare(sql).unwrap();
        stmt.query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    /// Port of `test_table_name_applies_to_additional_duckdb_outputs`.
    #[tokio::test]
    async fn test_table_name_applies_to_additional_duckdb_outputs() {
        let tmp = tempdir().expect("tempdir");
        let primary = tmp.path().join("primary.duckdb");
        let extra = tmp.path().join("extra.duckdb");
        let mut cfg = tabular_config(&primary);
        cfg.write_options.table_name = Some("agent".to_owned());
        cfg.write_options.additional_outputs = vec![extra.clone()];
        run_map_data_config(cfg).await.expect("map-data failed");
        for path in [primary, extra] {
            let con = Connection::open(&path).unwrap();
            assert_eq!(strings(&con, "SHOW TABLES"), ["agent"]);
        }
    }

    /// Port of `test_single_object_input_writes_parquet`.
    #[tokio::test]
    async fn test_single_object_input_writes_parquet() {
        let tmp = tempdir().expect("tempdir");
        let data = tmp.path().join("person.yaml");
        std::fs::write(
            &data,
            "id: P:001\nname: fred bloggs\nprimary_email: fred@example.com\nage_in_years: 33\ngender: nonbinary man\n",
        )
        .unwrap();
        let target = tmp.path().join("agent.parquet");
        let mut cfg = tabular_config(&target);
        cfg.input = data.to_str().unwrap().to_owned();
        run_map_data_config(cfg).await.expect("map-data failed");

        let con = Connection::open_in_memory().unwrap();
        let rows = strings(
            &con,
            &format!(
                "SELECT id || '|' || label FROM read_parquet('{}')",
                target.display()
            ),
        );
        assert_eq!(rows, ["P:001|fred bloggs"]);
        // Only the artifact remains: the staging file is gone.
        let mut names: Vec<String> = std::fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["agent.parquet", "person.yaml"]);
    }

    /// Port of `test_single_object_input_writes_duckdb_with_table_name`.
    #[tokio::test]
    async fn test_single_object_input_writes_duckdb_with_table_name() {
        let tmp = tempdir().expect("tempdir");
        let data = tmp.path().join("person.yaml");
        std::fs::write(
            &data,
            "id: P:002\nname: jane doe\nprimary_email: jane@example.com\nage_in_years: 28\ngender: cisgender woman\n",
        )
        .unwrap();
        let target = tmp.path().join("study.duckdb");
        let mut cfg = tabular_config(&target);
        cfg.input = data.to_str().unwrap().to_owned();
        cfg.write_options.table_name = Some("agent".to_owned());
        run_map_data_config(cfg).await.expect("map-data failed");

        let con = Connection::open(&target).unwrap();
        assert_eq!(strings(&con, "SHOW TABLES"), ["agent"]);
        assert_eq!(strings(&con, "SELECT id FROM agent"), ["P:002"]);
    }

    /// Tabular (streaming) input to Parquet infers the format from the extension.
    #[tokio::test]
    async fn test_streaming_input_writes_parquet() {
        let tmp = tempdir().expect("tempdir");
        let target = tmp.path().join("agents.PARQUET");
        run_map_data_config(tabular_config(&target))
            .await
            .expect("map-data failed");
        let con = Connection::open_in_memory().unwrap();
        let ids = strings(
            &con,
            &format!(
                "SELECT id FROM read_parquet('{}') ORDER BY id",
                target.display()
            ),
        );
        assert_eq!(ids, ["P:001", "P:002", "P:003"]);
    }
}
