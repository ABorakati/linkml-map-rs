//! Replays the differential-testing corpus (`tests/differential/corpus/<hash>/`) without Python.
//!
//! Each case directory holds a schema, a spec, optional target schema, input data and, per output format, the bytes
//! upstream Python `linkml-map` produced (`expected/<fmt>.out`) or the fact that it failed
//! (`expected/<fmt>.error`).  The corpus is written by `tests/differential/fuzz.py` (shrunk Hypothesis
//! counterexamples) and `tests/differential/examples_sweep.py`; see `tests/differential/README.md`.
//!
//! * [`corpus_matches_upstream`] must pass: every case without an `expected_divergence.md` has to reproduce
//!   upstream byte for byte (or fail with a non-zero exit where upstream failed).
//! * Cases carrying `expected_divergence.md` are *known* divergences (parity gaps or in-flight ports).  They are
//!   listed with their reason on every run, and [`known_divergences_now_match`] (ignored by default) asserts that
//!   they match, so `cargo test -p linkml-map-cli --test differential_corpus -- --ignored` shows what is still open
//!   and which markers can be deleted.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

const FORMATS: [&str; 5] = ["yaml", "json", "jsonl", "tsv", "csv"];

fn corpus_dir() -> PathBuf {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
    PathBuf::from(manifest)
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .join("tests/differential/corpus")
}

/// All case directories, sorted, with their divergence note (if any).
fn cases() -> Vec<(PathBuf, Option<String>)> {
    let Ok(rd) = fs::read_dir(corpus_dir()) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = rd
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.join("case.json").is_file())
        .collect();
    dirs.sort();
    dirs.into_iter()
        .map(|d| {
            let note = fs::read_to_string(d.join("expected_divergence.md")).ok();
            (d, note)
        })
        .collect()
}

/// Run one case in every format it lists; return one message per mismatching format.
fn replay(dir: &Path) -> Vec<String> {
    let meta: Value =
        serde_json::from_str(&fs::read_to_string(dir.join("case.json")).expect("case.json"))
            .expect("case.json is valid JSON");
    let get = |k: &str| meta.get(k).and_then(Value::as_str).map(str::to_owned);
    let schema = dir.join(get("schema").expect("schema"));
    let spec = dir.join(get("spec").expect("spec"));
    let input = dir.join(get("input").expect("input"));
    let target = get("target_schema").map(|t| dir.join(t));
    let source_type = get("source_type");
    let flags: Vec<String> = meta
        .get("flags")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let formats: Vec<String> = meta
        .get("formats")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_else(|| FORMATS.iter().map(|s| (*s).to_owned()).collect());

    let mut problems = Vec::new();
    for fmt in formats {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_linkml-tr-rs"));
        cmd.arg("map-data")
            .arg("-T")
            .arg(&spec)
            .arg("-s")
            .arg(&schema);
        if let Some(t) = &target {
            cmd.arg("--target-schema").arg(t);
        }
        if let Some(st) = &source_type {
            cmd.arg("--source-type").arg(st);
        }
        cmd.arg("-f").arg(&fmt).args(&flags).arg(&input);
        cmd.env("RUST_BACKTRACE", "0");
        let out = cmd.output().expect("spawn linkml-tr-rs");
        let code = out.status.code();
        let stderr = String::from_utf8_lossy(&out.stderr);
        let expected_out = dir.join("expected").join(format!("{fmt}.out"));
        let expected_err = dir.join("expected").join(format!("{fmt}.error"));
        if expected_out.is_file() {
            let want = fs::read(&expected_out).expect("read expected");
            if code != Some(0) {
                problems.push(format!(
                    "{fmt}: upstream succeeded but Rust exited {code:?}: {}",
                    stderr.lines().last().unwrap_or("")
                ));
            } else if out.stdout != want {
                problems.push(format!(
                    "{fmt}: output differs from upstream\n--- upstream ---\n{}\n--- rust ---\n{}",
                    String::from_utf8_lossy(&want),
                    String::from_utf8_lossy(&out.stdout)
                ));
            }
        } else if expected_err.is_file() {
            let info = fs::read_to_string(&expected_err).unwrap_or_default();
            if code == Some(0) {
                problems.push(format!(
                    "{fmt}: upstream failed ({}) but Rust succeeded",
                    info.replace('\n', " ")
                ));
            } else if code == Some(101) || stderr.contains("panicked at") {
                problems.push(format!("{fmt}: Rust panicked: {stderr}"));
            }
        } else {
            problems.push(format!("{fmt}: case has no expected/{fmt}.out or .error"));
        }
    }
    problems
}

fn reason(note: &str) -> String {
    note.lines().next().unwrap_or("").trim().to_owned()
}

#[test]
fn corpus_matches_upstream() {
    let all = cases();
    assert!(
        !all.is_empty(),
        "no corpus cases under {}",
        corpus_dir().display()
    );
    let (known, regular): (Vec<_>, Vec<_>) = all.into_iter().partition(|(_, note)| note.is_some());

    let mut failures = Vec::new();
    for (dir, _) in &regular {
        let problems = replay(dir);
        if !problems.is_empty() {
            failures.push(format!("{}:\n  {}", dir.display(), problems.join("\n  ")));
        }
    }

    for (dir, note) in &known {
        let name = dir.file_name().unwrap().to_string_lossy();
        let note = note.as_deref().unwrap_or("");
        if replay(dir).is_empty() {
            eprintln!(
                "NOTE {name}: now matches upstream; delete expected_divergence.md ({})",
                reason(note)
            );
        } else {
            eprintln!(
                "SKIP {name}: known divergence from upstream ({})",
                reason(note)
            );
        }
    }
    eprintln!(
        "differential corpus: {} replayed, {} known divergences skipped",
        regular.len(),
        known.len()
    );
    assert!(
        failures.is_empty(),
        "differential regressions:\n{}",
        failures.join("\n")
    );
}

#[test]
#[ignore = "divergence: cases with tests/differential/corpus/*/expected_divergence.md still differ from upstream"]
fn known_divergences_now_match() {
    let mut failures = Vec::new();
    for (dir, note) in cases().into_iter().filter(|(_, n)| n.is_some()) {
        let problems = replay(&dir);
        if !problems.is_empty() {
            failures.push(format!(
                "{} [{}]:\n  {}",
                dir.display(),
                reason(note.as_deref().unwrap_or("")),
                problems.join("\n  ")
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} divergent case(s):\n{}",
        failures.len(),
        failures.join("\n")
    );
}
