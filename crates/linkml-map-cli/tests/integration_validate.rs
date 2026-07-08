//! Integration tests for the `validate` subcommand.
//!
//! Calls `linkml_map_cli::run_validate_config` directly (no subprocess needed),
//! so tests assert on message content/severity, not just an exit code.
//!
//! Reuses the golden measurements fixture
//! (`tests/examples/measurements`) that is marked PASS in
//! tests/CONFORMANCE_REPORT.md:
//!   spec:   transform/qv-to-scalar.transform.yaml
//!   source: source/quantity_value.yaml   (Person{id, height}, QuantityValue{value, unit})
//!   target: target/quantity_value_flat.yaml (Person{id, height_in_cm})

use std::path::PathBuf;

use linkml_map_cli::{run_validate_config, Severity, ValidateConfig};
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

fn path_str(p: PathBuf) -> String {
    p.to_str().unwrap().to_owned()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Known-good fixture validated against both source and target schemas should
/// produce zero errors (the fixture is byte-parity PASS in conformance).
#[test]
fn test_validate_known_good_has_no_errors() {
    let fix = measurements_dir();

    let messages = run_validate_config(ValidateConfig {
        spec: path_str(fix.join("transform/qv-to-scalar.transform.yaml")),
        source_schema: Some(path_str(fix.join("source/quantity_value.yaml"))),
        target_schema: Some(path_str(fix.join("target/quantity_value_flat.yaml"))),
        strict: false,
    })
    .expect("validation failed to run");

    let errors: Vec<_> = messages
        .iter()
        .filter(|m| m.severity == Severity::Error)
        .collect();
    assert!(
        errors.is_empty(),
        "known-good fixture should have zero errors, got: {errors:?}"
    );
}

/// A spec with a deliberately typo'd slot-level `populated_from` should be
/// flagged as an error naming the bad source slot.
#[test]
fn test_validate_broken_populated_from_is_flagged() {
    let fix = measurements_dir();
    let tmp = tempdir().expect("tempdir");

    // Copy of the good spec, but the `id` slot's populated_from is a typo
    // ("idd") that does not exist on source class Person.
    let broken_spec = "\
id: qv-to-scalar
class_derivations:
  Person:
    populated_from: Person
    slot_derivations:
      id:
        populated_from: idd
      height_in_cm:
        expr: \"case( ({height}.unit == 'cm', {height}.value), (True, NULL) )\"
";
    let spec_path = tmp.path().join("broken.transform.yaml");
    std::fs::write(&spec_path, broken_spec).expect("writing broken spec");

    let messages = run_validate_config(ValidateConfig {
        spec: path_str(spec_path),
        source_schema: Some(path_str(fix.join("source/quantity_value.yaml"))),
        target_schema: Some(path_str(fix.join("target/quantity_value_flat.yaml"))),
        strict: false,
    })
    .expect("validation failed to run");

    let errors: Vec<_> = messages
        .iter()
        .filter(|m| m.severity == Severity::Error)
        .collect();

    assert!(
        errors
            .iter()
            .any(|m| m.message.contains("idd") && m.message.contains("populated_from")),
        "expected an error naming the bad source slot 'idd', got: {messages:?}"
    );
}
