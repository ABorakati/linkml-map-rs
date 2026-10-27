//! Input handling is byte-identical to upstream's.
//!
//! `tests/input_e2e/expected/` holds what upstream linkml-map v0.6.0-rc1
//! (`linkml-map map-data -f <format> ...`) wrote for every file in
//! `tests/input_e2e/cases/`, in each output format; this replays the same
//! invocations through the Rust CLI and requires identical bytes. Regenerate
//! and compare against a live install with `tests/input_e2e/run.sh`.
//!
//! The cases cover what the loaders decide before the engine sees a value:
//!
//! * YAML is YAML 1.1 (PyYAML `safe_load`): `yes`/`on`, `017`, `0x1F`, `1_000`,
//!   `1:30`, `~`, `.inf`, merge keys, and dates that must stay dates (so YAML
//!   output writes `2024-01-01` unquoted) while `1.0e20`, `1e3` and `0o7` are
//!   strings; JSON single-object input goes through the same loader first.
//! * TSV/CSV rows are read as linkml's `TsvLoader`/`CsvLoader` do: empty cells
//!   dropped, short rows padded with `None`, `skipinitialspace`, universal
//!   newlines, a BOM kept in the first header, `int()`/`float()` coercion of
//!   numeric-ranged columns whatever numeric range they declare.
//! * `stringification: {syntax: JSON}` is `ensure_ascii=True`.
//!
//! A case/format upstream itself refuses (`expected/<case>.<format>.error`) is
//! not compared; the date-as-JSON ones we deliberately write anyway are pinned
//! by their own test below.

use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::tempdir;

const FORMATS: [&str; 4] = ["yaml", "json", "jsonl", "tsv"];

fn e2e_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/input_e2e")
}

/// Run `map-data` on a case; `Ok(bytes)` or `Err(stderr)`.
fn run(case: &Path, format: &str) -> Result<Vec<u8>, String> {
    let dir = e2e_dir();
    let out = tempdir().unwrap();
    let out_path = out.path().join(format!("out.{format}"));
    let output = Command::new(env!("CARGO_BIN_EXE_linkml-tr-rs"))
        .arg("map-data")
        .args(["-T", dir.join("spec.yaml").to_str().unwrap()])
        .args(["-s", dir.join("schema.yaml").to_str().unwrap()])
        .args(["--source-type", "Rec"])
        .args(["-f", format])
        .args(["-o", out_path.to_str().unwrap()])
        .arg(case)
        .output()
        .expect("runs");
    if output.status.success() {
        Ok(std::fs::read(out_path).unwrap())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).into_owned())
    }
}

fn cases() -> Vec<PathBuf> {
    let mut cases: Vec<PathBuf> = std::fs::read_dir(e2e_dir().join("cases"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    cases.sort();
    cases
}

#[test]
fn inputs_match_upstream_in_every_output_format() {
    let expected_dir = e2e_dir().join("expected");
    let mut compared = 0;
    let mut failures = Vec::new();
    for case in cases() {
        let name = case.file_name().unwrap().to_str().unwrap().to_owned();
        for format in FORMATS {
            let expected = expected_dir.join(format!("{name}.{format}"));
            if !expected.exists() {
                assert!(
                    expected_dir.join(format!("{name}.{format}.error")).exists(),
                    "no expected output for {name} -> {format}; run tests/input_e2e/run.sh with REGEN=1"
                );
                continue;
            }
            compared += 1;
            match run(&case, format) {
                Ok(got) if got == std::fs::read(&expected).unwrap() => {}
                Ok(got) => failures.push(format!(
                    "{name} -> {format}\n--- upstream\n{}--- rust\n{}",
                    std::fs::read_to_string(&expected).unwrap(),
                    String::from_utf8_lossy(&got)
                )),
                Err(stderr) => failures.push(format!("{name} -> {format}: failed: {stderr}")),
            }
        }
    }
    assert!(compared >= 60, "only {compared} comparisons ran");
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// Upstream cannot write the `datetime.date` PyYAML builds from an unquoted
/// `2024-01-01` as JSON (`simplejson` raises `TypeError: Object of type date is
/// not JSON serializable`, part-way through the run). We write the `str()` text
/// instead of failing; this pins that choice. (PARITY, deliberately lenient.)
#[test]
fn json_output_of_a_yaml_date_is_the_text_where_upstream_raises() {
    let case = e2e_dir().join("cases/yaml11_scalars.yaml");
    // Upstream's last stderr line ("when serializing dict item 'd'") is recorded.
    assert!(
        e2e_dir()
            .join("expected/yaml11_scalars.yaml.json.error")
            .exists()
    );
    let json = String::from_utf8(run(&case, "json").unwrap()).unwrap();
    assert!(json.contains("\"d\": \"2024-01-01\""), "{json}");
    let jsonl = String::from_utf8(run(&case, "jsonl").unwrap()).unwrap();
    assert!(jsonl.contains("\"d\": \"2024-01-01\""), "{jsonl}");
}
