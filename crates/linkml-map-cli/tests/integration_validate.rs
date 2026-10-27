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

use linkml_map_cli::{Severity, ValidateConfig, run_validate_config};
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

// ── spec range overriding the target slot's range (upstream b332fc7) ──────────

/// Upstream `_RANGE_TARGET_SCHEMA` (`tests/test_validator.py`), loaded through the
/// real schemaview provider so `typeof` chains and `default_range` are exercised.
const RANGE_TARGET_SCHEMA: &str = "\
id: https://example.org/range-target
name: range-target
prefixes: {linkml: 'https://w3id.org/linkml/'}
imports: [linkml:types]
default_range: string
types:
  Score:
    typeof: decimal
enums:
  Relationship:
    permissible_values:
      ONESELF: {}
classes:
  Site:
    attributes:
      id: {identifier: true}
  Participant:
    attributes:
      relationship: {range: Relationship}
      amount: {range: decimal}
      score: {range: Score}
      ratio: {range: float}
      count: {range: integer}
      flag: {range: boolean}
      note: {range: string}
      site: {range: Site}
      untyped: {}
";

fn range_override_messages(
    slot_derivations: &str,
    strict: bool,
) -> Vec<linkml_map_cli::ValidationMessage> {
    let tmp = tempdir().expect("tempdir");
    let schema = tmp.path().join("target.yaml");
    let spec = tmp.path().join("spec.yaml");
    std::fs::write(&schema, RANGE_TARGET_SCHEMA).unwrap();
    std::fs::write(
        &spec,
        format!(
            "id: range-spec\nclass_derivations:\n  Participant:\n    slot_derivations:\n{slot_derivations}"
        ),
    )
    .unwrap();
    run_validate_config(ValidateConfig {
        spec: path_str(spec),
        source_schema: None,
        target_schema: Some(path_str(schema)),
        strict,
    })
    .expect("validation failed to run")
    .into_iter()
    .filter(|m| m.category.as_deref() == Some("range-override"))
    .collect()
}

#[test]
fn test_validate_range_override_warns_through_schemaview() {
    let cases = [
        (
            "      relationship: {value: ONESELF, range: string}\n",
            "range 'string' overrides the target slot's range 'Relationship', so its values \
             aren't resolved to the target enum's permissible values. Remove the range to use \
             the target schema's.",
        ),
        (
            "      amount: {value: '1.5', range: string}\n",
            "range 'string' overrides the target slot's range 'decimal', so its values aren't \
             coerced to decimal. Remove the range to use the target schema's.",
        ),
        (
            "      score: {value: '1', range: string}\n",
            "range 'string' overrides the target slot's range 'Score', so its values aren't \
             coerced to decimal. Remove the range to use the target schema's.",
        ),
        (
            "      untyped: {value: '1', range: integer}\n",
            "range 'integer' overrides the target slot's range 'string', so its values aren't \
             coerced to string. Remove the range to use the target schema's.",
        ),
    ];
    for (slot_derivations, message) in cases {
        let msgs = range_override_messages(slot_derivations, false);
        assert_eq!(msgs.len(), 1, "{slot_derivations}: {msgs:?}");
        assert_eq!(msgs[0].severity, Severity::Warning);
        assert_eq!(msgs[0].message, message);
        // CLI formatting is `{path}: [{severity}] {message}`; the category is not printed.
        let line = msgs[0].to_string();
        assert!(
            line.starts_with("class_derivations[Participant].slot_derivations["),
            "{line}"
        );
        assert!(line.contains(&format!("]: [warning] {message}")), "{line}");
    }
}

#[test]
fn test_validate_range_override_compatible_ranges_not_reported() {
    for slot_derivations in [
        "      score: {value: '0.1', range: decimal}\n",
        "      amount: {value: '1', range: integer}\n",
        "      ratio: {value: '1', range: integer}\n",
        "      note: {value: '2020-01-01', range: date}\n",
        "      note: {value: 'X:1', range: uriorcurie}\n",
        "      note: {value: ONESELF, range: Relationship}\n",
        "      site: {value: S1, range: string}\n",
    ] {
        let msgs = range_override_messages(slot_derivations, false);
        assert!(msgs.is_empty(), "{slot_derivations}: {msgs:?}");
    }
}

#[test]
fn test_validate_range_override_is_error_when_strict() {
    let msgs = range_override_messages("      amount: {value: '1.5', range: string}\n", true);
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].severity, Severity::Error);
}
