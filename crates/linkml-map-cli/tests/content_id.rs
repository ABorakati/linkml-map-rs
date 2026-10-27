//! CLI behaviour for identifier slots the spec doesn't derive.
//!
//! Port of upstream `tests/test_transformer/test_content_id.py`
//! (`test_map_data_refuses_records_without_identifiers`,
//! `test_continue_on_error_reports_once_and_still_writes`; linkml-map `76db207`)
//! plus the Rust-only cases for `--target-schema` omission and minting.

use std::path::Path;
use std::process::{Command, Output};

use tempfile::tempdir;

const SOURCE_SCHEMA: &str = r#"
id: https://example.org/source
name: source
prefixes: {linkml: 'https://w3id.org/linkml/'}
default_prefix: source
default_range: string
imports: [linkml:types]
classes:
  Row:
    attributes:
      participant: {}
      observation_type: {}
"#;

const TARGET_SCHEMA: &str = r#"
id: https://example.org/target
name: target
prefixes: {linkml: 'https://w3id.org/linkml/'}
default_prefix: target
default_range: string
imports: [linkml:types]
classes:
  Measurement:
    attributes:
      id: {identifier: true, required: true}
      associated_participant: {}
      observation_type: {}
"#;

const CLI_SPEC: &str = "\
id: content-id-cli
class_derivations:
  Measurement:
    populated_from: Row
    slot_derivations:
      associated_participant:
        populated_from: participant
      observation_type:
        populated_from: observation_type
";

fn map_data(
    dir: &Path,
    spec: &str,
    with_target: bool,
    extra: &[&str],
) -> (Output, std::path::PathBuf) {
    std::fs::write(dir.join("source.yaml"), SOURCE_SCHEMA).unwrap();
    std::fs::write(dir.join("target.yaml"), TARGET_SCHEMA).unwrap();
    std::fs::write(dir.join("spec.yaml"), spec).unwrap();
    std::fs::write(
        dir.join("Row.tsv"),
        "participant\tobservation_type\nP1\tOBA:1\nP2\tOBA:2\n",
    )
    .unwrap();
    let output = dir.join("out.jsonl");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_linkml-tr-rs"));
    cmd.arg("map-data")
        .args(["-T", dir.join("spec.yaml").to_str().unwrap()])
        .args(["-s", dir.join("source.yaml").to_str().unwrap()])
        .args(["--source-class", "Row"])
        .args(["-o", output.to_str().unwrap()]);
    if with_target {
        cmd.args(["--target-schema", dir.join("target.yaml").to_str().unwrap()]);
    }
    cmd.args(extra).arg(dir.join("Row.tsv"));
    (cmd.output().expect("runs"), output)
}

fn rows(path: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

// test_map_data_refuses_records_without_identifiers
#[test]
fn map_data_refuses_records_without_identifiers() {
    let dir = tempdir().unwrap();
    let (out, output) = map_data(dir.path(), CLI_SPEC, true, &[]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("'Measurement' does not derive identifier slot 'id'"),
        "{stderr}"
    );
    assert!(!output.exists(), "no output may be opened");
}

// test_continue_on_error_reports_once_and_still_writes
#[test]
fn continue_on_error_reports_once_and_still_writes() {
    let dir = tempdir().unwrap();
    let (out, output) = map_data(dir.path(), CLI_SPEC, true, &["--continue-on-error"]);
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        stderr
            .matches("  - Class derivation 'Measurement' does not derive identifier slot 'id'")
            .count(),
        1,
        "{stderr}"
    );
    assert!(stderr.contains("1 transformation error(s)"), "{stderr}");
    assert_eq!(
        rows(&output),
        vec![
            serde_json::json!({"associated_participant": "P1", "observation_type": "OBA:1"}),
            serde_json::json!({"associated_participant": "P2", "observation_type": "OBA:2"}),
        ]
    );
}

// `--target-schema` omitted means no target schema: nothing is checked or minted.
#[test]
fn no_target_schema_means_no_identifier_check() {
    let dir = tempdir().unwrap();
    let (out, output) = map_data(dir.path(), CLI_SPEC, false, &[]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        rows(&output)[0],
        serde_json::json!({"associated_participant": "P1", "observation_type": "OBA:1"})
    );
    // Even with minting on: there is no target schema to know the identifier slot.
    let spec = format!("mint_ids: true\n{CLI_SPEC}");
    let (out, output) = map_data(dir.path(), &spec, false, &[]);
    assert!(out.status.success());
    assert!(rows(&output)[0].get("id").is_none());
}

// With `mint_ids: true` and a target schema, every row gets a stable content id, first.
#[test]
fn mint_ids_true_mints_stable_ids_in_parallel() {
    let dir = tempdir().unwrap();
    let spec = format!("mint_ids: true\n{CLI_SPEC}");
    let (out, output) = map_data(dir.path(), &spec, true, &["--workers", "4"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let first = rows(&output);
    let text = std::fs::read_to_string(&output).unwrap();
    assert!(
        text.lines().next().unwrap().starts_with("{\"id\": \""),
        "{text}"
    );
    let ids: Vec<_> = first
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect();
    assert_ne!(ids[0], ids[1]);
    // A second run (different worker count / unordered) reproduces the ids.
    let (out, output) = map_data(dir.path(), &spec, true, &["--workers", "1", "--unordered"]);
    assert!(out.status.success());
    let mut again: Vec<_> = rows(&output)
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect();
    again.sort();
    let mut expected = ids.clone();
    expected.sort();
    assert_eq!(again, expected);
}
