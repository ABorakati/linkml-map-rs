//! `cargo xtask` — repo automation. Currently one task: `harness-gate`.
//!
//! The gate is the single, machine-readable enforcement of the harness rules
//! defined in `harness-ownership.toml`:
//!
//!   A. **Ownership** — every changed path under `crates/**` must map to exactly
//!      one owner. A new crate with no manifest entry is an "orphan" → fail.
//!   B. **Three-surface sync** — a change to the binding-consumed public Rust
//!      API, or to the PyO3 binding, must be accompanied by a change to the
//!      Python-facing surface (.pyi / shim / docs).
//!
//! Both the GitHub Actions CI job and the in-session Stop hook invoke THIS
//! binary, so the matching logic exists exactly once.
//!
//!   cargo xtask harness-gate                 # CI mode: exit 1 on violation
//!   cargo xtask harness-gate --base origin/master
//!   cargo xtask harness-gate --hook          # advisory: print nudge, exit 0

use std::process::Command;

mod gate;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("harness-gate") => {
            let hook = args.iter().any(|a| a == "--hook");
            let base = arg_value(&args, "--base").unwrap_or_else(|| "HEAD".to_string());
            std::process::exit(run_gate(&base, hook));
        }
        Some(other) => {
            eprintln!("xtask: unknown task '{other}'. Available: harness-gate");
            std::process::exit(2);
        }
        None => {
            eprintln!("xtask: usage: cargo xtask harness-gate [--base <ref>] [--hook]");
            std::process::exit(2);
        }
    }
}

fn arg_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1).cloned())
}

/// Returns the process exit code: 0 = ok, 1 = violation (CI mode only).
fn run_gate(base: &str, hook: bool) -> i32 {
    let manifest_path = repo_root().join("harness-ownership.toml");
    let manifest = match std::fs::read_to_string(&manifest_path) {
        Ok(s) => match gate::Manifest::parse(&s) {
            Ok(m) => m,
            Err(e) => {
                eprintln!(
                    "harness-gate: cannot parse {}: {e}",
                    manifest_path.display()
                );
                return if hook { 0 } else { 1 };
            }
        },
        Err(_) => {
            // No manifest (e.g. shallow checkout of an old commit) — do not block.
            return 0;
        }
    };

    if bypassed(&manifest.sync.bypass_tag) {
        if hook {
            println!("harness-gate: skipped (bypass tag / HARNESS_SKIP).");
        }
        return 0;
    }

    let changed = match changed_files(base) {
        Some(c) => c,
        None => {
            // git unavailable — never break the build over tooling.
            if hook {
                println!("harness-gate: git unavailable, skipped.");
            }
            return 0;
        }
    };
    if changed.is_empty() {
        return 0;
    }

    // Rust-API signature changes need diff *content*, not just the path list.
    let api_sig_changed = manifest
        .sync
        .rust_api
        .iter()
        .filter(|p| changed.iter().any(|c| c == *p))
        .any(|p| pub_line_changed(base, p));

    let report = gate::evaluate(&manifest, &changed, api_sig_changed);

    for line in report.render() {
        if hook {
            println!("{line}");
        } else {
            eprintln!("{line}");
        }
    }

    if report.ok() {
        if hook {
            println!("harness-gate: ok ({} changed path(s)).", changed.len());
        }
        return 0;
    }
    if hook {
        // In-session nudge only — never block the assistant from stopping.
        return 0;
    }
    1
}

fn bypassed(tag: &str) -> bool {
    if std::env::var("HARNESS_SKIP").is_ok() {
        return true;
    }
    last_commit_message().is_some_and(|m| m.contains(tag))
}

fn last_commit_message() -> Option<String> {
    let out = Command::new("git")
        .args(["log", "-1", "--pretty=%B"])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// `git diff --name-only <base>` — working tree (staged + unstaged) vs `base`.
/// With base=HEAD this catches uncommitted edits (the Stop-hook case).
fn changed_files(base: &str) -> Option<Vec<String>> {
    let out = Command::new("git")
        .args(["diff", "--name-only", base])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(|s| s.replace('\\', "/"))
            .collect(),
    )
}

/// True if the diff of `path` vs `base` adds or removes a binding-relevant
/// `pub ` declaration — a public-signature change on the binding-consumed
/// surface. A changed `pub ` line marked `// gate:internal` is exempt (engine
/// internal); see `gate::binding_pub_changed_in_diff`.
fn pub_line_changed(base: &str, path: &str) -> bool {
    let Ok(out) = Command::new("git")
        .args(["diff", "-U0", base, "--", path])
        .output()
    else {
        return false;
    };
    gate::binding_pub_changed_in_diff(&String::from_utf8_lossy(&out.stdout))
}

fn repo_root() -> std::path::PathBuf {
    Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| std::path::PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()))
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}
