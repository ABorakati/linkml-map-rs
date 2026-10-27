//! YAML written by the CLI is byte-identical to upstream's PyYAML output.
//!
//! `tests/yaml_e2e/expected/` holds what upstream linkml-map v0.6.0-rc1
//! (`linkml-map map-data ...`) wrote for the inputs next to it; regenerate and
//! compare against a live install with `tests/yaml_e2e/run.sh`. The data
//! deliberately mixes strings PyYAML must quote (`yes`, `~`, `2024-01-01`,
//! `a: b`), long text it folds at 80 columns, multi-line text, unicode
//! (including non-BMP) and flow-style stringification.

use std::path::PathBuf;
use std::process::Command;

use tempfile::tempdir;

fn e2e_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/yaml_e2e")
}

fn run_map_data(input: &str, extra: &[&str]) -> String {
    let dir = e2e_dir();
    let out = tempdir().unwrap().keep().join("out.yaml");
    let output = Command::new(env!("CARGO_BIN_EXE_linkml-tr-rs"))
        .arg("map-data")
        .args(["-T", dir.join("spec.yaml").to_str().unwrap()])
        .args(["-s", dir.join("schema.yaml").to_str().unwrap()])
        .args(["--source-type", "Record"])
        .args(["-o", out.to_str().unwrap()])
        .args(extra)
        .arg(dir.join(input))
        .output()
        .expect("runs");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::read_to_string(out).unwrap()
}

fn expected(name: &str) -> String {
    std::fs::read_to_string(e2e_dir().join("expected").join(name)).unwrap()
}

/// Single-object output: upstream `dump_output` (`sort_keys=False`).
#[test]
fn single_object_yaml_matches_upstream() {
    assert_eq!(run_map_data("data.yaml", &[]), expected("data.yaml"));
}

/// Streamed rows: upstream `YAMLStreamWriter` (sorted keys, `---` per record).
#[test]
fn streamed_yaml_matches_upstream() {
    assert_eq!(
        run_map_data("data.tsv", &["-f", "yaml"]),
        expected("data.tsv.yaml")
    );
}
