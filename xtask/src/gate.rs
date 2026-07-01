//! Pure, testable core of the harness gate: manifest model, glob matching, and
//! the rule evaluation. Kept free of `git`/IO so the unit tests below ARE the
//! gate self-test (`cargo test -p xtask`).

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Manifest {
    #[serde(default)]
    pub owners: Vec<Owner>,
    pub sync: Sync,
}

#[derive(Debug, Deserialize)]
pub struct Owner {
    pub name: String,
    pub paths: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct Sync {
    pub bypass_tag: String,
    pub rust_api: Vec<String>,
    pub binding: Vec<String>,
    pub py_surface: Vec<String>,
}

/// In-source opt-out marker. A changed `pub ` line in a `rust_api` file that
/// carries this marker is public to the Rust workspace but NOT part of the
/// binding-consumed surface, so it needs no Python-surface sync. This keeps the
/// gate strict-by-default (an unmarked new `pub` still trips) while letting the
/// broad `engine/mod.rs` / `schema/mod.rs` files hold engine-internal `pub`
/// items without false-positive sync violations.
pub const INTERNAL_MARKER: &str = "gate:internal";

/// True if a unified diff (`git diff -U0`) of a `rust_api` file adds or removes
/// a **binding-relevant** `pub ` declaration. Evaluated per hunk: a changed
/// `pub ` line trips the sync rule UNLESS its hunk also carries a
/// `gate:internal` marker on some changed line (the item is public to the Rust
/// workspace but not the binding-consumed surface). Hunk scope — rather than
/// same-line — keeps the exemption robust to rustfmt relocating the marker
/// comment onto an adjacent line. Pure so the tests below are the gate
/// self-test; the caller supplies the raw diff text.
pub fn binding_pub_changed_in_diff(diff: &str) -> bool {
    let mut hunk: Vec<&str> = Vec::new();
    let mut tripped = false;
    for line in diff.lines() {
        if line.starts_with("@@") {
            tripped |= hunk_trips(&hunk);
            hunk.clear();
            continue;
        }
        if line.starts_with("+++") || line.starts_with("---") {
            continue;
        }
        if let Some(body) = line.strip_prefix('+').or_else(|| line.strip_prefix('-')) {
            hunk.push(body);
        }
    }
    tripped | hunk_trips(&hunk)
}

/// A hunk trips iff it changes a `pub ` line and carries NO `gate:internal`
/// marker among its changed lines.
fn hunk_trips(changed: &[&str]) -> bool {
    let has_pub = changed.iter().any(|b| b.trim_start().starts_with("pub "));
    let marked = changed.iter().any(|b| b.contains(INTERNAL_MARKER));
    has_pub && !marked
}

impl Manifest {
    pub fn parse(s: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(s)
    }

    /// The owner whose globs match `path`, if any.
    fn owner_of(&self, path: &str) -> Option<&str> {
        self.owners
            .iter()
            .find(|o| o.paths.iter().any(|p| glob_match(p, path)))
            .map(|o| o.name.as_str())
    }
}

/// Result of a gate run. `ok()` is the pass/fail; `render()` is human output.
#[derive(Debug, Default)]
pub struct Report {
    pub orphans: Vec<String>,
    pub sync_violation: Option<String>,
}

impl Report {
    pub fn ok(&self) -> bool {
        self.orphans.is_empty() && self.sync_violation.is_none()
    }

    pub fn render(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.ok() {
            return out;
        }
        out.push("harness-gate: violations found".to_string());
        for o in &self.orphans {
            out.push(format!(
                "  [orphan] {o} is under crates/** but no owner in harness-ownership.toml claims it"
            ));
        }
        if let Some(msg) = &self.sync_violation {
            out.push(format!("  [sync] {msg}"));
        }
        out.push("  fix: assign an owner / update the Python surface, or tag the commit [skip-harness] for non-behavioural edits".to_string());
        out
    }
}

/// Evaluate both rules. `api_sig_changed` is supplied by the caller (it needs
/// git diff content); everything else is decided from the path list alone.
pub fn evaluate(m: &Manifest, changed: &[String], api_sig_changed: bool) -> Report {
    let mut report = Report::default();

    // Rule A — orphan check, scoped to crates/** source paths.
    for path in changed {
        if path.starts_with("crates/") && m.owner_of(path).is_none() {
            report.orphans.push(path.clone());
        }
    }

    // Rule B — three-surface sync.
    let any = |globs: &[String]| {
        changed
            .iter()
            .any(|c| globs.iter().any(|g| glob_match(g, c)))
    };
    let binding_changed = any(&m.sync.binding);
    let py_changed = any(&m.sync.py_surface);

    if (api_sig_changed || binding_changed) && !py_changed {
        let trigger = if binding_changed {
            "the PyO3 binding changed"
        } else {
            "a public Rust API signature changed"
        };
        report.sync_violation = Some(format!(
            "{trigger} but no Python-facing surface (.pyi / shim __all__ / docs) was touched"
        ));
    }

    report
}

/// Minimal path-glob matcher over `/`-separated paths.
///
/// Supported: `**` (zero or more whole segments), `*` (any run within one
/// segment), and literals. This is all the manifest uses; a full glob crate
/// would be overkill for an internal, unit-tested matcher.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    let pat: Vec<&str> = pattern.split('/').collect();
    let txt: Vec<&str> = path.split('/').collect();
    seg_match(&pat, &txt)
}

fn seg_match(pat: &[&str], txt: &[&str]) -> bool {
    match pat.first() {
        None => txt.is_empty(),
        Some(&"**") => {
            // `**` consumes zero or more segments.
            (0..=txt.len()).any(|skip| seg_match(&pat[1..], &txt[skip..]))
        }
        Some(seg) => match txt.first() {
            Some(t) if wildcard_match(seg, t) => seg_match(&pat[1..], &txt[1..]),
            _ => false,
        },
    }
}

/// `*`-wildcard match within a single path segment (no `/`).
fn wildcard_match(pat: &str, txt: &str) -> bool {
    if !pat.contains('*') {
        return pat == txt;
    }
    // Split on '*'; each literal piece must appear in order; first/last anchor ends.
    let parts: Vec<&str> = pat.split('*').collect();
    let mut pos = 0usize;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if i == 0 {
            if !txt[pos..].starts_with(part) {
                return false;
            }
            pos += part.len();
        } else if i == parts.len() - 1 {
            return txt[pos..].ends_with(part);
        } else {
            match txt[pos..].find(part) {
                Some(idx) => pos += idx + part.len(),
                None => return false,
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = r#"
        [[owners]]
        name = "engine"
        paths = ["crates/linkml-map-core/**", "crates/linkml-map-schemaview/**"]
        [[owners]]
        name = "pyo3"
        paths = ["crates/linkml-map-py/**"]
        [sync]
        bypass_tag = "[skip-harness]"
        rust_api = ["crates/linkml-map-core/src/lib.rs"]
        binding = ["crates/linkml-map-py/src/lib.rs"]
        py_surface = [
          "crates/linkml-map-py/python/linkml_map_rs/_native.pyi",
          "crates/linkml-map-py/python/linkml_map_rs/__init__.py",
          "README.md",
        ]
    "#;

    fn m() -> Manifest {
        Manifest::parse(MANIFEST).unwrap()
    }

    #[test]
    fn glob_doublestar_matches_nested() {
        assert!(glob_match(
            "crates/linkml-map-core/**",
            "crates/linkml-map-core/src/engine/mod.rs"
        ));
        assert!(glob_match(
            "crates/linkml-map-py/**",
            "crates/linkml-map-py/src/lib.rs"
        ));
    }

    #[test]
    fn glob_doublestar_matches_zero_segments() {
        // `a/**` should match `a` itself and `a/b`.
        assert!(glob_match("crates/**", "crates"));
        assert!(glob_match("crates/**", "crates/x"));
    }

    #[test]
    fn glob_star_within_segment() {
        assert!(glob_match("*.md", "README.md"));
        assert!(glob_match(
            "crates/*/src/lib.rs",
            "crates/linkml-map-io/src/lib.rs"
        ));
        assert!(!glob_match("*.md", "src/README.md")); // single-segment star, no nesting
        assert!(!glob_match("*.md", "README.rs"));
    }

    #[test]
    fn orphan_when_new_crate_unowned() {
        let changed = vec!["crates/linkml-map-newthing/src/lib.rs".to_string()];
        let r = evaluate(&m(), &changed, false);
        assert_eq!(r.orphans, changed);
        assert!(!r.ok());
    }

    #[test]
    fn no_orphan_for_owned_paths_or_noncrate() {
        let changed = vec![
            "crates/linkml-map-core/src/engine/mod.rs".to_string(),
            "README.md".to_string(),
            "Cargo.lock".to_string(),
        ];
        let r = evaluate(&m(), &changed, false);
        assert!(r.orphans.is_empty());
    }

    #[test]
    fn sync_fails_when_binding_changes_alone() {
        let changed = vec!["crates/linkml-map-py/src/lib.rs".to_string()];
        let r = evaluate(&m(), &changed, false);
        assert!(r.sync_violation.is_some());
        assert!(!r.ok());
    }

    #[test]
    fn sync_passes_when_binding_and_pyi_change_together() {
        let changed = vec![
            "crates/linkml-map-py/src/lib.rs".to_string(),
            "crates/linkml-map-py/python/linkml_map_rs/_native.pyi".to_string(),
        ];
        let r = evaluate(&m(), &changed, false);
        assert!(r.ok());
    }

    #[test]
    fn sync_fails_when_rust_api_sig_changes_alone() {
        let changed = vec!["crates/linkml-map-core/src/lib.rs".to_string()];
        let r = evaluate(&m(), &changed, /* api_sig_changed */ true);
        assert!(r.sync_violation.is_some());
    }

    #[test]
    fn sync_ok_when_rust_api_file_touched_without_pub_change() {
        // Path changed but no `pub ` line changed → api_sig_changed=false → no trip.
        let changed = vec!["crates/linkml-map-core/src/lib.rs".to_string()];
        let r = evaluate(&m(), &changed, false);
        assert!(r.ok());
    }

    #[test]
    fn engine_internal_change_does_not_trip_sync() {
        let changed = vec!["crates/linkml-map-core/src/expr/eval.rs".to_string()];
        let r = evaluate(&m(), &changed, false);
        assert!(r.ok());
    }

    // ── binding_pub_changed_in_diff: distinguish binding-consumed pub from
    //    engine-internal pub in the broad rust_api files.

    #[test]
    fn unmarked_added_pub_is_binding_relevant() {
        let diff = "@@ -1,0 +2,1 @@\n+    pub fn transform(&self) -> Value {";
        assert!(binding_pub_changed_in_diff(diff));
    }

    #[test]
    fn pub_marked_gate_internal_is_exempt() {
        // The real false positive we fixed: an engine-internal helper added to a
        // broad rust_api file, tagged internal via a comment in the same hunk →
        // must NOT trip the sync rule. Marker on a line ABOVE the `pub` line
        // (rustfmt-stable placement).
        let diff = "@@ -30,0 +32,3 @@\n\
                    +    // gate:internal — engine-only helper, not binding-consumed.\n\
                    +    pub fn name(&self) -> Option<&str> {";
        assert!(!binding_pub_changed_in_diff(diff));
    }

    #[test]
    fn marker_in_one_hunk_does_not_exempt_pub_in_another() {
        // Strict-by-default across hunks: a marked internal pub in hunk 1 must
        // not launder an unmarked binding pub added in a separate hunk.
        let diff = "@@ -30,0 +32,2 @@\n\
                    +    // gate:internal\n\
                    +    pub fn helper() {}\n\
                    @@ -80,0 +90,1 @@\n\
                    +    pub fn transform(&self) -> Value {";
        assert!(binding_pub_changed_in_diff(diff));
    }

    #[test]
    fn removed_binding_pub_still_trips() {
        let diff = "@@ -5,1 +5,0 @@\n-    pub struct Transformer;";
        assert!(binding_pub_changed_in_diff(diff));
    }

    #[test]
    fn non_pub_and_header_lines_are_ignored() {
        let diff = "+++ b/crates/linkml-map-core/src/schema/mod.rs\n\
                    --- a/crates/linkml-map-core/src/schema/mod.rs\n\
                    @@ -1,0 +1,3 @@\n\
                    +    // pub in a comment, not a declaration\n\
                    +    fn private_helper() {}\n\
                    +    let pub_ish = 1;";
        assert!(!binding_pub_changed_in_diff(diff));
    }
}
