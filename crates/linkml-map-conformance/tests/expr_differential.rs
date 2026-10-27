//! Replay of the expression differential corpus (`tests/differential/expr/corpus/`).
//!
//! Each line holds a case plus the result upstream `linkml-map` v0.6.0-rc1
//! produced for it (recorded by `tests/differential/expr/campaign.py`), so no
//! Python is needed here. `status: "pass"` entries must match; `status:
//! "divergence"` entries document known gaps and are replayed by the ignored
//! test below so a fix shows up as soon as someone runs
//! `cargo test -p linkml-map-conformance --test expr_differential -- --ignored`.

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use linkml_map_conformance::exprfuzz::{check_entry, install_quiet_panic_hook};
use serde_json::Value as J;

fn corpus_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/differential/expr/corpus")
}

fn entries() -> Vec<(String, J)> {
    let mut out = Vec::new();
    let mut files: Vec<_> = std::fs::read_dir(corpus_dir())
        .expect("corpus directory")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .collect();
    files.sort();
    for f in files {
        let name = f.file_name().unwrap().to_string_lossy().into_owned();
        for (i, line) in std::fs::read_to_string(&f).unwrap().lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let v: J =
                serde_json::from_str(line).unwrap_or_else(|e| panic!("{name}:{}: {e}", i + 1));
            out.push((format!("{name}:{}", i + 1), v));
        }
    }
    out
}

/// `check_entry` on a worker thread with a deadline: a few known divergences are
/// hangs (unbounded allocation), which must not stall the suite.
fn check_with_timeout(entry: &J) -> Result<(), String> {
    let (tx, rx) = mpsc::channel();
    let entry = entry.clone();
    std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(move || {
            let _ = tx.send(check_entry(&entry));
        })
        .expect("spawn");
    rx.recv_timeout(Duration::from_secs(20))
        .unwrap_or_else(|_| Err("timed out (hang)".to_owned()))
}

fn run_with_status(status: &str) -> Vec<String> {
    install_quiet_panic_hook();
    let mut failures = Vec::new();
    let mut n = 0;
    for (id, e) in entries() {
        if e["status"].as_str() != Some(status) {
            continue;
        }
        if e.get("hazard").is_some() {
            continue; // known to hang or exhaust memory: documented, never replayed
        }
        n += 1;
        if let Err(msg) = check_with_timeout(&e) {
            let what = e
                .get("expr")
                .or_else(|| e.get("leaf"))
                .map(J::to_string)
                .unwrap_or_default();
            let class = e["class"].as_str().unwrap_or("");
            failures.push(format!("{id} [{class}] {what}: {msg}"));
        }
    }
    assert!(n > 0 || status != "pass", "no `{status}` entries found");
    failures
}

#[test]
fn corpus_agreeing_entries_still_match_upstream() {
    let failures = run_with_status("pass");
    assert!(
        failures.is_empty(),
        "{} regression(s):\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
#[ignore = "divergence: known gaps vs upstream, see tests/differential/expr/corpus (status: divergence)"]
fn corpus_known_divergences_match_upstream() {
    let failures = run_with_status("divergence");
    assert!(
        failures.is_empty(),
        "{} divergence(s) remain:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// Not an assertion on the divergences themselves: prints entries that now agree so
/// they can be promoted to `status: "pass"`.
#[test]
fn corpus_reports_divergences_that_have_been_fixed() {
    install_quiet_panic_hook();
    let fixed: Vec<String> = entries()
        .into_iter()
        .filter(|(_, e)| {
            e["status"].as_str() == Some("divergence")
                && e.get("hazard").is_none()
                && check_with_timeout(e).is_ok()
        })
        .map(|(id, e)| format!("{id} [{}]", e["class"].as_str().unwrap_or("")))
        .collect();
    if !fixed.is_empty() {
        eprintln!(
            "{} divergence entries now agree with upstream (promote to `pass`):\n{}",
            fixed.len(),
            fixed.join("\n")
        );
    }
}
