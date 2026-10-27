//! `linkml-tr-rs derive-schema` writes exactly what upstream `linkml-map derive-schema` writes.
//!
//! `tests/derive_schema/expected/` holds the stdout of upstream linkml-map v0.6.0-rc1 for every
//! (specification, source schema) pair listed in `tests/derive_schema/cases.json`: the vendored
//! examples and golden fixtures, plus hand-written feature cases under
//! `tests/derive_schema/inputs/` (copy directives, enum derivations, `overrides`, rewiring, ...).
//! Pairs on which upstream raises (a Python exception, no output) are marked `"error": true`
//! and must fail here too. Regenerate with `tests/derive_schema/generate.py`.

use std::{path::PathBuf, process::Command};

use serde_json::Value;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn derive(spec: &str, schema: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_linkml-tr-rs"))
        .arg("derive-schema")
        .args(["-T", root().join(spec).to_str().unwrap()])
        .args(["-s", root().join(schema).to_str().unwrap()])
        .output()
        .expect("runs")
}

#[test]
fn derive_schema_matches_upstream_for_every_case() {
    let cases: Value = serde_json::from_str(
        &std::fs::read_to_string(root().join("tests/derive_schema/cases.json")).unwrap(),
    )
    .unwrap();
    let cases = cases.as_array().unwrap();
    assert!(cases.len() > 20, "corpus went missing");
    let mut failures = Vec::new();
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let output = derive(
            case["spec"].as_str().unwrap(),
            case["schema"].as_str().unwrap(),
        );
        if case["error"].as_bool().unwrap_or(false) {
            if output.status.success() {
                failures.push(format!("{name}: upstream raises, but we succeeded"));
            }
            continue;
        }
        if !output.status.success() {
            failures.push(format!(
                "{name}: failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
            continue;
        }
        let expected = std::fs::read_to_string(
            root()
                .join("tests/derive_schema/expected")
                .join(format!("{name}.yaml")),
        )
        .unwrap();
        let actual = String::from_utf8(output.stdout).unwrap();
        if actual != expected {
            let line = actual
                .lines()
                .zip(expected.lines())
                .position(|(a, e)| a != e)
                .map_or_else(|| "length".to_string(), |n| format!("line {}", n + 1));
            failures.push(format!("{name}: output differs from upstream at {line}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
