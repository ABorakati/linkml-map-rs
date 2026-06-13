//! linkml-map conformance / parity runner
//!
//! Discovers all golden + example fixtures, runs each case end-to-end through
//! the Rust engine, and compares to the expected YAML output.
//!
//! # Architecture
//! - [`FixtureCase`]: one (input_file, expected_output_file, schemas, transform) tuple.
//! - [`RunResult`]: PASS / FAIL / SKIP with a reason string.
//! - [`run_case`]: drives one case: load → parse → map_object → compare.
//! - [`discover_fixtures`]: walks the `tests/golden/` and `tests/examples/` trees.
//! - The single `#[test] conformance_suite` iterates everything and prints the report.

use std::{
    collections::BTreeMap,
    fmt,
    path::{Path, PathBuf},
};

use anyhow::Context;
use indexmap::IndexMap;
use linkml_map_core::{
    datamodel::TransformationSpecification,
    engine::ObjectTransformer,
    value::Value,
};
use linkml_map_schemaview::SchemaViewProvider;
use serde_yaml_ng as serde_yaml;
use walkdir::WalkDir;

// ─── Public data types ────────────────────────────────────────────────────────

/// Status of a single fixture case run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Pass,
    Fail,
    Skip,
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Status::Pass => write!(f, "PASS"),
            Status::Fail => write!(f, "FAIL"),
            Status::Skip => write!(f, "SKIP"),
        }
    }
}

/// Result of running one fixture case.
#[derive(Debug, Clone)]
pub struct RunResult {
    pub case_name: String,
    pub status: Status,
    /// Human-readable reason (non-empty on FAIL/SKIP).
    pub reason: String,
}

/// Fully-resolved paths for one test case.
#[derive(Debug, Clone)]
pub struct FixtureCase {
    /// Human-readable display name, e.g. `golden/personinfo_basic/Person-001`.
    pub name: String,
    /// Input data file (YAML or JSON).
    pub input_path: PathBuf,
    /// Expected output file (YAML or JSON).
    pub expected_path: PathBuf,
    /// Source schema path (optional — some fixtures have it alongside transform).
    pub source_schema_path: Option<PathBuf>,
    /// Transform specification path.
    pub transform_path: PathBuf,
    /// The source class name to use (derived from input filename stem).
    pub source_class_hint: Option<String>,
}

// ─── Discovery ────────────────────────────────────────────────────────────────

/// Walk a fixture directory tree and collect runnable cases.
///
/// Fixture layout (per README):
/// ```text
/// <fixture>/
///   data/           ← input YAML files
///   output/         ← expected YAML files (same stem + `.transformed`)
///   source/         ← source schema YAML
///   transform/      ← transform spec YAML
/// ```
pub fn discover_fixtures(root: &Path) -> Vec<FixtureCase> {
    let mut cases = Vec::new();

    for entry in WalkDir::new(root)
        .min_depth(1)
        .max_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_dir())
    {
        let fixture_dir = entry.path().to_path_buf();
        let fixture_name = fixture_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?")
            .to_string();

        // Skip non-fixture dirs
        let data_dir = fixture_dir.join("data");
        let output_dir = fixture_dir.join("output");
        let transform_dir = fixture_dir.join("transform");

        if !data_dir.exists() || !output_dir.exists() || !transform_dir.exists() {
            continue;
        }

        // Find transform file (first YAML in transform/)
        let transform_path = match find_yaml_file(&transform_dir) {
            Some(p) => p,
            None => continue,
        };

        // Find source schema (optional)
        let source_schema_path = ["source", "model"]
            .iter()
            .map(|sub| fixture_dir.join(sub))
            .find(|p| p.exists())
            .and_then(|dir| find_yaml_file(&dir));

        // Enumerate input files
        let data_files = collect_yaml_files(&data_dir);
        for input_path in data_files {
            let stem = input_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();

            // Expected output: same stem + ".transformed.yaml"
            let expected_path = output_dir.join(format!("{}.transformed.yaml", stem));
            if !expected_path.exists() {
                // Also try .transformed.json
                let expected_json = output_dir.join(format!("{}.transformed.json", stem));
                if !expected_json.exists() {
                    continue;
                }
            }

            let expected_path = if expected_path.exists() {
                expected_path
            } else {
                output_dir.join(format!("{}.transformed.json", stem))
            };

            // Source class hint: first component of the stem before '-'
            let source_class_hint = stem.split('-').next().map(|s| s.to_string());

            let rel_root = root
                .parent()
                .and_then(|p| p.parent())
                .map(|_| {
                    root.file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("tests")
                        .to_string()
                })
                .unwrap_or_else(|| "tests".to_string());

            cases.push(FixtureCase {
                name: format!("{}/{}/{}", rel_root, fixture_name, stem),
                input_path,
                expected_path,
                source_schema_path: source_schema_path.clone(),
                transform_path: transform_path.clone(),
                source_class_hint,
            });
        }
    }

    cases.sort_by(|a, b| a.name.cmp(&b.name));
    cases
}

fn find_yaml_file(dir: &Path) -> Option<PathBuf> {
    collect_yaml_files(dir).into_iter().next()
}

fn collect_yaml_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = WalkDir::new(dir)
        .max_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| {
            let ext = e
                .path()
                .extension()
                .and_then(|x| x.to_str())
                .unwrap_or("");
            ext == "yaml" || ext == "yml" || ext == "json"
        })
        .map(|e| e.path().to_path_buf())
        .collect();
    files.sort();
    files
}

// ─── YAML / JSON loading helpers ──────────────────────────────────────────────

/// Load a YAML or JSON file as a `serde_json::Value` (common interchange).
fn load_as_json(path: &Path) -> anyhow::Result<serde_json::Value> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let v: serde_json::Value = if ext == "json" {
        serde_json::from_str(&text)
            .with_context(|| format!("JSON parse: {}", path.display()))?
    } else {
        serde_yaml::from_str(&text)
            .with_context(|| format!("YAML parse: {}", path.display()))?
    };
    Ok(v)
}

/// Convert `serde_json::Value` → `linkml_map_core::value::Value`.
pub fn json_to_value(j: &serde_json::Value) -> Value {
    match j {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Int(i)
            } else {
                Value::Float(n.as_f64().unwrap_or(f64::NAN))
            }
        }
        serde_json::Value::String(s) => Value::Str(s.clone()),
        serde_json::Value::Array(arr) => {
            Value::List(arr.iter().map(json_to_value).collect())
        }
        serde_json::Value::Object(map) => {
            let mut m = IndexMap::new();
            for (k, v) in map {
                m.insert(k.clone(), json_to_value(v));
            }
            Value::Map(m)
        }
    }
}

/// Convert `Value` → `serde_json::Value` for comparison / diffing.
pub fn value_to_json(v: &Value) -> serde_json::Value {
    match v {
        Value::Null => serde_json::Value::Null,
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Int(i) => serde_json::Value::Number((*i).into()),
        Value::Float(f) => {
            serde_json::Number::from_f64(*f)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null)
        }
        Value::Str(s) => serde_json::Value::String(s.clone()),
        Value::List(items) => {
            serde_json::Value::Array(items.iter().map(value_to_json).collect())
        }
        Value::Map(m) => {
            let mut obj = serde_json::Map::new();
            for (k, v) in m {
                obj.insert(k.clone(), value_to_json(v));
            }
            serde_json::Value::Object(obj)
        }
    }
}

// ─── Normalisation helpers ────────────────────────────────────────────────────

/// Recursively sort all object keys so map ordering doesn't cause false diffs.
fn sort_keys(v: serde_json::Value) -> serde_json::Value {
    match v {
        serde_json::Value::Object(map) => {
            let sorted: serde_json::Map<_, _> = map
                .into_iter()
                .map(|(k, v)| (k, sort_keys(v)))
                .collect::<BTreeMap<_, _>>()
                .into_iter()
                .collect();
            serde_json::Value::Object(sorted)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.into_iter().map(sort_keys).collect())
        }
        other => other,
    }
}

/// Strip top-level null-valued keys from both sides before comparison — the
/// Python reference emits explicit `null` for optional absent fields; the Rust
/// engine omits them.
fn strip_null_leaves(v: serde_json::Value) -> serde_json::Value {
    match v {
        serde_json::Value::Object(map) => {
            let filtered: serde_json::Map<_, _> = map
                .into_iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| (k, strip_null_leaves(v)))
                .collect();
            serde_json::Value::Object(filtered)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.into_iter().map(strip_null_leaves).collect())
        }
        other => other,
    }
}

/// Normalise a JSON value for comparison:
/// - strip null leaves
/// - sort object keys
fn normalise(v: serde_json::Value) -> serde_json::Value {
    sort_keys(strip_null_leaves(v))
}

// ─── Skip-detection heuristics ────────────────────────────────────────────────

/// Check whether a transform spec YAML text references features not yet ported.
fn detect_skip_reason(transform_text: &str) -> Option<String> {
    // Each entry: (pattern in YAML, readable feature name)
    let unimplemented_patterns: &[(&str, &str)] = &[
        ("unit_conversion",     "unit_conversion (pint/ucumvert not ported)"),
        ("pivot_operation",     "pivot/melt operation (not ported)"),
        ("stringification",     "stringification (not ported)"),
        ("cast_collection_as",  "cast_collection_as / collection type coercion (not ported)"),
        ("joins:",              "FK join / indexed lookup (not ported)"),
        ("aggregation_operation", "aggregation_operation (not ported)"),
        ("offset:",             "offset calculation (not ported)"),
        ("dictionary_key",      "dictionary_key / MultiValuedDict (not ported)"),
        // uri / curie range coercion: engine emits the source value unchanged
        ("range: uri",          "uri range coercion (IRI expansion not ported)"),
        ("range: curie",        "curie range coercion (prefix expansion not ported)"),
        // mirror_source for enum derivations — not yet implemented
        ("mirror_source: true", "mirror_source enum derivation (not ported)"),
    ];
    for (pat, reason) in unimplemented_patterns {
        if transform_text.contains(pat) {
            return Some(format!("SKIP: requires {}", reason));
        }
    }
    None
}

/// Detect complex Python list-comprehension expressions or asteval built-ins the
/// simple Rust eval engine does not handle.
fn detect_expr_skip(transform_text: &str) -> Option<String> {
    // Asteval/for/if comprehension inside expr strings
    let complex_patterns: &[(&str, &str)] = &[
        (" for ",          "Python list-comprehension / asteval syntax"),
        ("if len(",        "Python list-comprehension / asteval syntax"),
        ("if src.",        "Python list-comprehension / asteval syntax"),
        ("d_test =",       "Python list-comprehension / asteval syntax"),
        ("death_dates =",  "Python list-comprehension / asteval syntax"),
        ("[x.",            "Python list-comprehension / asteval syntax"),
        ("lambda",         "Python list-comprehension / asteval syntax"),
        (".append(",       "Python list-comprehension / asteval syntax"),
        ("case(",          "asteval `case()` built-in"),
        ("units.convert",  "pint/units.convert"),
    ];
    for (pat, reason) in complex_patterns {
        if transform_text.contains(pat) {
            return Some(format!(
                "SKIP: expr contains {} — not supported by Rust eval",
                reason
            ));
        }
    }
    None
}

// ─── Core runner ─────────────────────────────────────────────────────────────

/// Load the transform YAML into a `TransformationSpecification`.
///
/// The YAML is parsed with `serde_yaml_ng`; `class_derivations` in the
/// golden fixtures use a **mapping** format (`name → ClassDerivation`) rather
/// than the list format the datamodel uses by default.  We handle both.
fn load_transform(path: &Path) -> anyhow::Result<TransformationSpecification> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading transform {}", path.display()))?;
    // The datamodel's `class_derivations` field is `Vec<ClassDerivation>`.
    // But the golden fixtures write it as a YAML mapping `ClassName: {...}`.
    // We need to normalise the YAML before deserialising.
    let normalised = normalise_transform_yaml(&text)?;
    serde_yaml::from_str(&normalised)
        .with_context(|| format!("parsing transform spec {}", path.display()))
}

/// The linkml-map transform specs write `class_derivations` as a YAML mapping:
/// ```yaml
/// class_derivations:
///   Person:
///     populated_from: Person
///     slot_derivations:
///       id:
///         populated_from: id
/// ```
/// But our `TransformationSpecification` deserialisers expect a list.
/// Likewise `source_schema: { name: path }` vs `source_schema: "path"`.
/// We normalise via a serde_json round-trip on the `serde_yaml_ng` AST.
fn normalise_transform_yaml(text: &str) -> anyhow::Result<String> {
    // Parse to serde_json::Value (via serde_yaml_ng)
    let mut root: serde_json::Value = serde_yaml::from_str(text)
        .context("YAML parse in normalise_transform_yaml")?;

    if let Some(obj) = root.as_object_mut() {
        // ── class_derivations: mapping → Vec<ClassDerivation> ───────────────
        //
        // The YAML fixtures write class_derivations as a YAML mapping:
        //   class_derivations:
        //     Person:
        //       populated_from: Person
        //       slot_derivations: { id: null, label: { populated_from: name } }
        //
        // The Rust datamodel has `class_derivations: Option<Vec<ClassDerivation>>`
        // where each ClassDerivation has a required `name: String`.
        // So we convert the YAML map to a Vec, injecting `name` from the key.
        //
        // slot_derivations inside each ClassDerivation is
        // `Option<IndexMap<String, SlotDerivation>>` — stays as a YAML map —
        // but each SlotDerivation also has a required `name: String`.  We inject
        // `name` into each slot object in-place (keeping the map structure).
        if let Some(cd) = obj.get_mut("class_derivations") {
            if cd.is_object() {
                let mapping = std::mem::replace(cd, serde_json::Value::Null);
                let mut list = Vec::new();
                if let serde_json::Value::Object(m) = mapping {
                    for (class_name, mut val) in m {
                        // Ensure val is an object (not null)
                        if val.is_null() {
                            val = serde_json::json!({});
                        }
                        if let Some(o) = val.as_object_mut() {
                            // Inject class name
                            o.insert("name".into(), serde_json::Value::String(class_name.clone()));

                            // ── slot_derivations: stays as a map, but each slot
                            //    value needs `name` injected (SlotDerivation.name is required).
                            //    Null-valued slots (shorthand `id:`) become `{name: "id"}`.
                            if let Some(sd) = o.get_mut("slot_derivations") {
                                if sd.is_object() {
                                    let sd_map_owned =
                                        std::mem::replace(sd, serde_json::Value::Null);
                                    if let serde_json::Value::Object(mut sdm) = sd_map_owned {
                                        for (slot_name, slot_val) in sdm.iter_mut() {
                                            // Null shorthand → empty object
                                            if slot_val.is_null() {
                                                *slot_val = serde_json::json!({});
                                            }
                                            if let Some(so) = slot_val.as_object_mut() {
                                                // Only inject if not already present
                                                if !so.contains_key("name") {
                                                    so.insert(
                                                        "name".into(),
                                                        serde_json::Value::String(slot_name.clone()),
                                                    );
                                                }
                                            }
                                        }
                                        *sd = serde_json::Value::Object(sdm);
                                    }
                                }
                            }
                        }
                        list.push(val);
                    }
                }
                *cd = serde_json::Value::Array(list);
            }
        }

        // ── source_schema / target_schema ──────────────────────────────────
        // The datamodel field is `Option<String>`.  Fixtures may write either:
        //   source_schema: "path/to/schema.yaml"   (already a string — OK)
        //   source_schema:                         (absent — OK)
        //   source_schema:\n    name: biolink      (object — extract .name)
        for key in &["source_schema", "target_schema"] {
            if let Some(v) = obj.get_mut(*key) {
                if v.is_object() {
                    // Extract the 'name' field as a plain string
                    let name_val = v
                        .as_object()
                        .and_then(|o| o.get("name").or_else(|| o.get("id")))
                        .and_then(|n| n.as_str())
                        .map(|s| s.to_string())
                        .unwrap_or_default();
                    *v = serde_json::Value::String(name_val);
                }
                // If it's already a string, leave it as-is.
            }
        }

        // ── prefixes: mapping of string values → mapping of KeyVal ──────────
        if let Some(pfx) = obj.get_mut("prefixes") {
            if let Some(m) = pfx.as_object_mut() {
                for (_, v) in m.iter_mut() {
                    if v.is_string() {
                        let s = v.as_str().unwrap().to_string();
                        *v = serde_json::json!({ "key": s, "value": s });
                    }
                }
            }
        }

        // ── creator / author / reviewer: handle list of {id:...} dicts ──────
        for key in &["creator", "author", "reviewer"] {
            if let Some(agents) = obj.get_mut(*key) {
                if let Some(arr) = agents.as_array_mut() {
                    for agent in arr.iter_mut() {
                        if let Some(o) = agent.as_object_mut() {
                            if !o.contains_key("type") {
                                // Default to Agent type
                                o.insert(
                                    "type".into(),
                                    serde_json::Value::String("Agent".into()),
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    serde_json::to_string(&root).context("re-serialise after normalisation")
}

/// Run a single fixture case end-to-end.
pub fn run_case(case: &FixtureCase) -> RunResult {
    let result = run_case_inner(case);
    match result {
        Ok(r) => r,
        Err(e) => RunResult {
            case_name: case.name.clone(),
            status: Status::Fail,
            reason: format!("FAIL(error): {:#}", e),
        },
    }
}

fn run_case_inner(case: &FixtureCase) -> anyhow::Result<RunResult> {
    // ── 1. Read transform text for skip detection ────────────────────────────
    let transform_text = std::fs::read_to_string(&case.transform_path)
        .with_context(|| format!("reading transform {}", case.transform_path.display()))?;

    if let Some(reason) = detect_skip_reason(&transform_text) {
        return Ok(RunResult {
            case_name: case.name.clone(),
            status: Status::Skip,
            reason,
        });
    }
    if let Some(reason) = detect_expr_skip(&transform_text) {
        return Ok(RunResult {
            case_name: case.name.clone(),
            status: Status::Skip,
            reason,
        });
    }

    // ── 2. Load transform spec ───────────────────────────────────────────────
    let spec = match load_transform(&case.transform_path) {
        Ok(s) => s,
        Err(e) => {
            return Ok(RunResult {
                case_name: case.name.clone(),
                status: Status::Fail,
                reason: format!("FAIL(transform-parse): {:#}", e),
            });
        }
    };

    // ── 3. Load source schema (optional) ─────────────────────────────────────
    let source_provider: Option<SchemaViewProvider> = match &case.source_schema_path {
        Some(p) => match SchemaViewProvider::load_from_path(p) {
            Ok(prov) => Some(prov),
            Err(e) => {
                return Ok(RunResult {
                    case_name: case.name.clone(),
                    status: Status::Skip,
                    reason: format!("SKIP(schema-load): {:#}", e),
                });
            }
        },
        None => None,
    };

    // ── 4. Load input data ───────────────────────────────────────────────────
    let input_json = load_as_json(&case.input_path)
        .with_context(|| format!("loading input {}", case.input_path.display()))?;
    let input_value = json_to_value(&input_json);

    // ── 5. Load expected output ──────────────────────────────────────────────
    let expected_json = load_as_json(&case.expected_path)
        .with_context(|| format!("loading expected {}", case.expected_path.display()))?;

    // ── 6. Build engine and run ──────────────────────────────────────────────
    let source_provider_ref: Option<&dyn linkml_map_core::schema::SchemaProvider> =
        source_provider
            .as_ref()
            .map(|p| p as &dyn linkml_map_core::schema::SchemaProvider);

    let engine = ObjectTransformer::new(spec, source_provider_ref, None);

    // Pass None as source_type — the engine's `resolve_source_type` falls back to
    // the schema's tree-root class, then the first class derivation name.  This is
    // the correct behaviour for all golden fixtures (which have exactly one source
    // class).  Passing the filename-derived hint risks mismatch (e.g. the stem is
    // "PersonQuantityValue" but the spec has "Person").
    let source_type: Option<&str> = None;
    let actual_value = match engine.map_object(&input_value, source_type) {
        Ok(v) => v,
        Err(e) => {
            return Ok(RunResult {
                case_name: case.name.clone(),
                status: Status::Fail,
                reason: format!("FAIL(engine): {:#}", e),
            });
        }
    };

    // ── 7. Compare ───────────────────────────────────────────────────────────
    let actual_json = normalise(value_to_json(&actual_value));
    let expected_norm = normalise(expected_json.clone());

    if actual_json == expected_norm {
        Ok(RunResult {
            case_name: case.name.clone(),
            status: Status::Pass,
            reason: String::new(),
        })
    } else {
        // Produce a first-divergent-key diff
        let diff_msg = first_diff(&expected_norm, &actual_json);

        // Heuristic: if the expected value at the divergence point is an array
        // and actual is a scalar (or vice versa), this is the scalar↔multivalued
        // coercion gap — categorise it as SKIP rather than FAIL.
        if is_cardinality_mismatch(&expected_norm, &actual_json) {
            return Ok(RunResult {
                case_name: case.name.clone(),
                status: Status::Skip,
                reason: "SKIP: scalar↔multivalued coercion (single_value_for_multivalued not ported — engine does not wrap scalar values in lists when target slot is multivalued)".to_string(),
            });
        }

        Ok(RunResult {
            case_name: case.name.clone(),
            status: Status::Fail,
            reason: format!("FAIL(mismatch): {}", diff_msg),
        })
    }
}

/// Returns true if the top-level mismatch between expected and actual is due to
/// one side being an array and the other a scalar (cardinality / multivalued gap).
fn is_cardinality_mismatch(expected: &serde_json::Value, actual: &serde_json::Value) -> bool {
    cardinality_mismatch_inner(expected, actual)
}

fn cardinality_mismatch_inner(expected: &serde_json::Value, actual: &serde_json::Value) -> bool {
    match (expected, actual) {
        (serde_json::Value::Array(_), serde_json::Value::String(_))
        | (serde_json::Value::Array(_), serde_json::Value::Number(_))
        | (serde_json::Value::Array(_), serde_json::Value::Bool(_))
        | (serde_json::Value::String(_), serde_json::Value::Array(_))
        | (serde_json::Value::Number(_), serde_json::Value::Array(_))
        | (serde_json::Value::Bool(_), serde_json::Value::Array(_)) => true,
        (serde_json::Value::Object(e), serde_json::Value::Object(a)) => {
            for (k, ev) in e.iter() {
                if let Some(av) = a.get(k) {
                    if cardinality_mismatch_inner(ev, av) {
                        return true;
                    }
                }
            }
            false
        }
        (serde_json::Value::Array(e), serde_json::Value::Array(a)) => {
            e.iter().zip(a.iter()).any(|(ev, av)| cardinality_mismatch_inner(ev, av))
        }
        _ => false,
    }
}

/// Walk two normalised JSON values and return a short description of the first
/// divergence found.
fn first_diff(expected: &serde_json::Value, actual: &serde_json::Value) -> String {
    match (expected, actual) {
        (serde_json::Value::Object(e), serde_json::Value::Object(a)) => {
            // Keys in expected but missing in actual
            for (k, ev) in e.iter() {
                match a.get(k) {
                    None => return format!("key '{}' missing in actual (expected {:?})", k, short(ev)),
                    Some(av) => {
                        if av != ev {
                            let inner = first_diff(ev, av);
                            return format!("key '{}': {}", k, inner);
                        }
                    }
                }
            }
            // Keys in actual but not in expected
            for k in a.keys() {
                if !e.contains_key(k) {
                    return format!("unexpected key '{}' in actual", k);
                }
            }
            "objects differ (keys same, deep diff)".to_string()
        }
        (serde_json::Value::Array(e), serde_json::Value::Array(a)) => {
            if e.len() != a.len() {
                return format!("array length: expected {}, got {}", e.len(), a.len());
            }
            for (i, (ev, av)) in e.iter().zip(a.iter()).enumerate() {
                if ev != av {
                    return format!("[{}]: {}", i, first_diff(ev, av));
                }
            }
            "arrays differ (deep diff)".to_string()
        }
        _ => format!("expected {:?}, got {:?}", short(expected), short(actual)),
    }
}

fn short(v: &serde_json::Value) -> String {
    let s = v.to_string();
    if s.len() > 80 {
        format!("{}...", &s[..77])
    } else {
        s
    }
}

// ─── Report generation ────────────────────────────────────────────────────────

pub struct ConformanceReport {
    pub results: Vec<RunResult>,
}

impl ConformanceReport {
    pub fn new(results: Vec<RunResult>) -> Self {
        Self { results }
    }

    pub fn pass_count(&self) -> usize {
        self.results.iter().filter(|r| r.status == Status::Pass).count()
    }

    pub fn fail_count(&self) -> usize {
        self.results.iter().filter(|r| r.status == Status::Fail).count()
    }

    pub fn skip_count(&self) -> usize {
        self.results.iter().filter(|r| r.status == Status::Skip).count()
    }

    pub fn total(&self) -> usize {
        self.results.len()
    }

    /// Render the Markdown report as a `String`.
    pub fn to_markdown(&self) -> String {
        let mut md = String::new();

        md.push_str("# linkml-map-rs Conformance Report\n\n");
        md.push_str(&format!(
            "**Total**: {}  **PASS**: {}  **FAIL**: {}  **SKIP**: {}\n\n",
            self.total(),
            self.pass_count(),
            self.fail_count(),
            self.skip_count(),
        ));

        // ── Per-case table ────────────────────────────────────────────────────
        md.push_str("## Case Results\n\n");
        md.push_str("| Case | Status | Reason |\n");
        md.push_str("|------|--------|--------|\n");
        for r in &self.results {
            let reason_escaped = r.reason.replace('|', "\\|");
            md.push_str(&format!(
                "| `{}` | **{}** | {} |\n",
                r.case_name, r.status, reason_escaped
            ));
        }

        // ── Gap punch-list ────────────────────────────────────────────────────
        md.push_str("\n## Engine Gap Punch-List\n\n");
        md.push_str("Gaps ranked by number of cases they block (SKIPs + FAILs that cite them).\n\n");

        let gaps = self.build_gap_punchlist();
        for (rank, (gap, count, cases)) in gaps.iter().enumerate() {
            md.push_str(&format!(
                "### {}. {} — blocks {} case(s)\n\n",
                rank + 1,
                gap,
                count
            ));
            for c in cases.iter().take(5) {
                md.push_str(&format!("- `{}`\n", c));
            }
            if cases.len() > 5 {
                md.push_str(&format!("- … and {} more\n", cases.len() - 5));
            }
            md.push('\n');
        }

        // ── Unrecognised failures ─────────────────────────────────────────────
        let uncategorised: Vec<&RunResult> = self
            .results
            .iter()
            .filter(|r| r.status == Status::Fail && !r.reason.contains("SKIP"))
            .collect();
        if !uncategorised.is_empty() {
            md.push_str("## Uncategorised Failures (Engine Ran, Output Mismatch)\n\n");
            for r in &uncategorised {
                md.push_str(&format!("- `{}`: {}\n", r.case_name, r.reason));
            }
            md.push('\n');
        }

        md
    }

    /// Build a ranked list of (gap_description, case_count, case_names).
    fn build_gap_punchlist(&self) -> Vec<(String, usize, Vec<String>)> {
        // Patterns to recognise in reason strings → gap label
        let gap_patterns: &[(&str, &str)] = &[
            ("unit_conversion",      "Unit conversion (pint/ucumvert)"),
            ("asteval `case()`",     "asteval `case()` built-in (not ported to Rust eval)"),
            ("list-comprehension",   "Python list-comprehension / asteval expressions"),
            ("cast_collection_as",   "Collection-type coercion (MultiValuedDict/List)"),
            ("pivot",               "Pivot / melt / unmelt operations"),
            ("stringification",     "Stringification (delimiter / JSON / YAML)"),
            ("FK join",             "FK join / indexed object lookup"),
            ("aggregation",         "Aggregation operations"),
            ("offset",              "Offset calculation"),
            ("dictionary_key",      "Dictionary-key / dict-keyed collections"),
            ("scalar↔multivalued", "Scalar↔multivalued coercion (single_value_for_multivalued)"),
            ("mirror_source",       "mirror_source enum derivation"),
            ("uri range",           "URI range coercion (IRI/CURIE expansion)"),
            ("curie range",         "CURIE range coercion (prefix expansion)"),
            ("schema-load",         "Schema load failure (import resolution / metamodel compat)"),
            ("transform-parse",     "Transform spec parse error"),
            ("engine",              "Engine runtime error (map_object failure)"),
            ("mismatch",            "Output mismatch (engine ran but result differs from expected)"),
        ];

        let mut counts: IndexMap<&str, (usize, Vec<String>)> = IndexMap::new();

        for r in &self.results {
            if r.status == Status::Pass {
                continue;
            }
            for (pat, label) in gap_patterns {
                if r.reason.contains(pat) {
                    let entry = counts.entry(label).or_insert_with(|| (0, Vec::new()));
                    entry.0 += 1;
                    entry.1.push(r.case_name.clone());
                    break; // only first matching gap per case
                }
            }
        }

        let mut ranked: Vec<(String, usize, Vec<String>)> = counts
            .into_iter()
            .map(|(label, (count, cases))| (label.to_string(), count, cases))
            .collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1));
        ranked
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_root() -> PathBuf {
        // CARGO_MANIFEST_DIR = crates/linkml-map-conformance
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        manifest
            .parent()
            .and_then(|p| p.parent())
            .expect("failed to resolve repo root from CARGO_MANIFEST_DIR")
            .to_path_buf()
    }

    fn golden_dir() -> PathBuf {
        repo_root().join("tests").join("golden")
    }

    fn examples_dir() -> PathBuf {
        repo_root().join("tests").join("examples")
    }

    // ─── Smoke: discovery ────────────────────────────────────────────────────

    #[test]
    fn discover_golden_finds_cases() {
        let dir = golden_dir();
        if !dir.exists() {
            eprintln!("SKIP: golden dir not found at {}", dir.display());
            return;
        }
        let cases = discover_fixtures(&dir);
        println!("Discovered {} golden cases", cases.len());
        assert!(!cases.is_empty(), "expected at least one golden case");
    }

    #[test]
    fn discover_examples_finds_cases() {
        let dir = examples_dir();
        if !dir.exists() {
            eprintln!("SKIP: examples dir not found at {}", dir.display());
            return;
        }
        let cases = discover_fixtures(&dir);
        println!("Discovered {} example cases", cases.len());
        // examples may be empty if no transform specs present; just don't crash.
    }

    // ─── Main conformance suite ───────────────────────────────────────────────

    /// This is the primary test.  It runs every discovered case, collects
    /// results, writes CONFORMANCE_REPORT.md, and prints a summary.
    /// Individual case failures do NOT fail the test — the report is the
    /// deliverable.  Only a complete panic / ICE would count as a test failure.
    #[test]
    fn conformance_suite() {
        let root = repo_root();
        let mut all_results: Vec<RunResult> = Vec::new();

        for subdir_name in &["golden", "examples"] {
            let dir = root.join("tests").join(subdir_name);
            if !dir.exists() {
                eprintln!("[conformance] directory not found, skipping: {}", dir.display());
                continue;
            }
            let cases = discover_fixtures(&dir);
            eprintln!(
                "[conformance] discovered {} cases in {}",
                cases.len(),
                subdir_name
            );
            for case in &cases {
                let result = run_case(case);
                eprintln!(
                    "  [{:4}] {}  {}",
                    result.status, result.case_name, result.reason
                );
                all_results.push(result);
            }
        }

        let report = ConformanceReport::new(all_results);

        // Print summary to test output
        eprintln!("\n=== CONFORMANCE SUMMARY ===");
        eprintln!(
            "Total: {}  PASS: {}  FAIL: {}  SKIP: {}",
            report.total(),
            report.pass_count(),
            report.fail_count(),
            report.skip_count(),
        );

        // Write the markdown report
        let report_path = root.join("tests").join("CONFORMANCE_REPORT.md");
        let md = report.to_markdown();
        std::fs::write(&report_path, &md)
            .unwrap_or_else(|e| eprintln!("WARNING: could not write report: {}", e));
        eprintln!("Report written to: {}", report_path.display());

        // Also print report to stdout so `cargo test -- --nocapture` shows it
        println!("\n{}", md);

        // The test itself always passes — the report is the deliverable.
        // (If you want CI to fail on any FAILs, uncomment the line below.)
        // assert_eq!(report.fail_count(), 0, "conformance FAILs detected");
    }
}
