//! `linkml-tr-rs invert` writes exactly what upstream `linkml-map invert` writes.
//!
//! `tests/invert/expected/` holds the stdout of upstream linkml-map v0.6.0-rc1 for every
//! (specification, source schema, mode) triple listed in `tests/invert/cases.json`: the vendored
//! examples, golden fixtures and exported compliance combos, hand-written feature cases under
//! `tests/invert/inputs/` (every inverter branch), and Hypothesis-generated ones under
//! `tests/invert/inputs/fuzz/`. Each runs strict (upstream's default) and with `--no-strict`.
//! Triples on which upstream raises are marked `"error"` and must fail here too, with the same
//! message for the exceptions the inverter itself defines or lets escape
//! (`NonInvertibleSpecificationError`, `AttributeError`, `ValueError`). Regenerate with
//! `tests/invert/generate.py`.

use std::{path::PathBuf, process::Command};

use serde_json::Value;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn invert(spec: &str, schema: &str, strict: bool) -> std::process::Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_linkml-tr-rs"));
    cmd.arg("invert");
    if !strict {
        cmd.arg("--no-strict");
    }
    cmd.args(["-T", root().join(spec).to_str().unwrap()])
        .arg(root().join(schema))
        .output()
        .expect("runs")
}

#[test]
fn invert_matches_upstream_for_every_case() {
    let cases: Value = serde_json::from_str(
        &std::fs::read_to_string(root().join("tests/invert/cases.json")).unwrap(),
    )
    .unwrap();
    let cases = cases.as_array().unwrap();
    assert!(cases.len() > 200, "corpus went missing");
    let mut failures = Vec::new();
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let output = invert(
            case["spec"].as_str().unwrap(),
            case["schema"].as_str().unwrap(),
            case["strict"].as_bool().unwrap(),
        );
        if let Some(error) = case.get("error") {
            if output.status.code() != case["exit"].as_i64().map(|c| c as i32) {
                failures.push(format!(
                    "{name}: upstream exits {}, we exit {:?}",
                    case["exit"],
                    output.status.code()
                ));
                continue;
            }
            let kind = error["type"].as_str().unwrap();
            if matches!(
                kind,
                "NonInvertibleSpecificationError" | "AttributeError" | "ValueError"
            ) && !String::from_utf8_lossy(&output.stderr)
                .contains(error["message"].as_str().unwrap())
            {
                failures.push(format!(
                    "{name}: expected {kind}: {}, got stderr: {}",
                    error["message"],
                    String::from_utf8_lossy(&output.stderr)
                ));
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
                .join("tests/invert/expected")
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

/// `-o FILE` writes the YAML to the file and prints nothing; `--strict` is accepted.
#[test]
fn invert_output_file_and_strict_flag() {
    let dir = std::env::temp_dir().join(format!("invert-out-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("inv.yaml");
    let status = Command::new(env!("CARGO_BIN_EXE_linkml-tr-rs"))
        .args(["invert", "--strict", "-T"])
        .arg(root().join("tests/invert/inputs/src__rename.spec.yaml"))
        .arg("-o")
        .arg(&out)
        .arg(root().join("tests/invert/inputs/src.yaml"))
        .output()
        .unwrap();
    assert!(status.status.success());
    assert!(status.stdout.is_empty());
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(
        text.starts_with("class_derivations:\n- name: Person\n"),
        "{text}"
    );
    std::fs::remove_dir_all(&dir).ok();
}
