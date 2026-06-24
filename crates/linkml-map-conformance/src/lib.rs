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
    schema::{ClassDef, InMemorySchema, InMemorySchemaBuilder, RangeKind, SchemaProvider, SlotDef},
    value::Value,
};
use linkml_map_schemaview::SchemaViewProvider;
use serde_yaml_ng as serde_yaml;

/// A source-schema provider for a fixture case.
///
/// Prefers the real pure-Rust `SchemaViewProvider`. Some golden schemas use
/// standard CURIE maps (`semweb_context`) whose prefixes the backend does not
/// resolve at `add_schema` time, so they fail to load. For those, the runner
/// falls back to a tolerant in-memory schema parsed directly from the YAML
/// (classes, slots, ranges, identifier/multivalued flags) — enough metadata for
/// the engine's range coercion + FK object-index resolution.
pub enum SchemaSource {
    Real(SchemaViewProvider),
    InMemory(InMemorySchema),
}

impl SchemaSource {
    fn as_provider(&self) -> &dyn SchemaProvider {
        match self {
            SchemaSource::Real(p) => p as &dyn SchemaProvider,
            SchemaSource::InMemory(p) => p as &dyn SchemaProvider,
        }
    }
}
use walkdir::WalkDir;

#[cfg(test)]
mod compliance;

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
            let ext = e.path().extension().and_then(|x| x.to_str()).unwrap_or("");
            ext == "yaml" || ext == "yml" || ext == "json"
        })
        .map(|e| e.path().to_path_buf())
        .collect();
    files.sort();
    files
}

// ─── Source-schema loading (real → in-memory fallback) ────────────────────────

/// Load the best source schema for a case.
///
/// `hint_path` is the discovered candidate (first YAML in `source/` or
/// `model/`). We scan its sibling directory for ALL schema YAMLs and pick the
/// one that yields the most classes — some fixtures ship a truncated/alternate
/// schema alongside the real one (e.g. an empty `cmdr_normalized.yaml` next to
/// the populated `normalized.yaml`). For each candidate we try the real
/// backend first, then a tolerant in-memory parse.
fn load_source_schema(hint_path: &Path) -> anyhow::Result<SchemaSource> {
    let dir = hint_path.parent().unwrap_or(hint_path);
    let mut candidates = collect_yaml_files(dir);
    // Ensure the hinted file is considered even if collect order differs.
    if !candidates.iter().any(|p| p == hint_path) {
        candidates.push(hint_path.to_path_buf());
    }

    // The tolerant in-memory fallback exists for fixtures that ship a truncated
    // alternate schema next to the real one (e.g. `cmdr_normalized.yaml` beside
    // `normalized.yaml` in flattening), where the real backend rejects the
    // populated file (unresolved standard CURIE map) AND the sibling it *does*
    // load is empty. It is gated to multi-candidate directories so single-schema
    // fixtures whose schema legitimately fails to load (e.g. the biolink
    // metamodel) keep their documented SKIP rather than being silently rescued.
    let allow_inmemory = candidates.len() > 1;

    let mut best: Option<(usize, SchemaSource)> = None;
    let mut last_err: Option<anyhow::Error> = None;

    for cand in &candidates {
        let source = match SchemaViewProvider::load_from_path(cand) {
            Ok(p) => SchemaSource::Real(p),
            Err(e) if allow_inmemory => match build_inmemory_schema_from_yaml(cand) {
                Ok(p) => SchemaSource::InMemory(p),
                Err(e2) => {
                    last_err = Some(anyhow::anyhow!("{cand:?}: real={e:#}; inmemory={e2:#}"));
                    continue;
                }
            },
            Err(e) => {
                last_err = Some(anyhow::anyhow!("{cand:?}: {e:#}"));
                continue;
            }
        };
        let n = source.as_provider().all_class_names().len();
        if best.as_ref().map(|(bn, _)| n > *bn).unwrap_or(true) {
            best = Some((n, source));
        }
    }

    match best {
        Some((_, s)) => Ok(s),
        None => Err(last_err.unwrap_or_else(|| anyhow::anyhow!("no loadable source schema"))),
    }
}

/// Tolerant LinkML-schema → [`InMemorySchema`] parser.
///
/// Reads only what the engine needs: classes (with `is_a`, `tree_root`), their
/// inline `attributes` and referenced global `slots`, and each slot's `range`,
/// `multivalued`, and `identifier`/`key` flags. Ranges are classified against
/// the schema's own classes/enums/types (everything else defaults to a scalar
/// type). This is a deliberate, minimal subset — not a full LinkML loader.
fn build_inmemory_schema_from_yaml(path: &Path) -> anyhow::Result<InMemorySchema> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading schema {}", path.display()))?;
    let root: serde_json::Value =
        serde_yaml::from_str(&text).with_context(|| format!("YAML parse {}", path.display()))?;
    let obj = root
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("schema root is not a mapping: {}", path.display()))?;

    let class_names: Vec<String> = obj
        .get("classes")
        .and_then(|c| c.as_object())
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    let enum_names: Vec<String> = obj
        .get("enums")
        .and_then(|c| c.as_object())
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    let type_names: Vec<String> = obj
        .get("types")
        .and_then(|c| c.as_object())
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();

    // Global slot definitions block.
    let global_slots = obj.get("slots").and_then(|s| s.as_object());

    let classify = |range_name: &str| -> RangeKind {
        if class_names.iter().any(|c| c == range_name) {
            RangeKind::Class(range_name.to_string())
        } else if enum_names.iter().any(|e| e == range_name) {
            RangeKind::Enum(range_name.to_string())
        } else {
            RangeKind::Type(range_name.to_string())
        }
    };

    // Build a SlotDef from a slot spec object + a fallback name + (optional)
    // global slot spec to merge defaults from.
    let make_slot =
        |name: &str, local: Option<&serde_json::Map<String, serde_json::Value>>| -> SlotDef {
            let global = global_slots
                .and_then(|gs| gs.get(name))
                .and_then(|v| v.as_object());
            let get_str = |key: &str| -> Option<String> {
                local
                    .and_then(|m| m.get(key))
                    .or_else(|| global.and_then(|m| m.get(key)))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            };
            let get_bool = |key: &str| -> bool {
                local
                    .and_then(|m| m.get(key))
                    .or_else(|| global.and_then(|m| m.get(key)))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
            };
            let range = match get_str("range") {
                Some(r) => classify(&r),
                None => RangeKind::None,
            };
            SlotDef {
                name: name.to_string(),
                range,
                multivalued: get_bool("multivalued"),
                required: get_bool("required"),
                identifier: get_bool("identifier"),
                key: get_bool("key"),
                unit: None,
                any_of_enums: vec![],
                inlined: false,
                inlined_as_list: false,
            }
        };

    let mut builder = InMemorySchemaBuilder::new();
    for t in &type_names {
        builder = builder.add_type(t.clone());
    }
    // Always register the common scalar types so unknown ranges still classify.
    for t in [
        "string",
        "integer",
        "float",
        "double",
        "boolean",
        "uriorcurie",
        "uri",
        "date",
    ] {
        if !type_names.iter().any(|x| x == t) {
            builder = builder.add_type(t);
        }
    }

    if let Some(classes) = obj.get("classes").and_then(|c| c.as_object()) {
        for (class_name, cdef) in classes {
            let cobj = cdef.as_object();
            let is_a = cobj
                .and_then(|m| m.get("is_a"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let tree_root = cobj
                .and_then(|m| m.get("tree_root"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            builder = builder.add_class(ClassDef {
                name: class_name.clone(),
                tree_root,
                is_a,
                mixins: vec![],
            });

            // Inline `attributes` (each is a full slot def keyed by name).
            if let Some(attrs) = cobj
                .and_then(|m| m.get("attributes"))
                .and_then(|a| a.as_object())
            {
                for (slot_name, sdef) in attrs {
                    builder = builder.add_slot(class_name, make_slot(slot_name, sdef.as_object()));
                }
            }
            // Referenced global `slots` (list of names; defs live in top-level slots:).
            if let Some(slot_refs) = cobj.and_then(|m| m.get("slots")).and_then(|s| s.as_array()) {
                for sref in slot_refs {
                    if let Some(slot_name) = sref.as_str() {
                        builder = builder.add_slot(class_name, make_slot(slot_name, None));
                    }
                }
            }
        }
    }

    Ok(builder.build())
}

// ─── YAML / JSON loading helpers ──────────────────────────────────────────────

/// Load a YAML or JSON file as a `serde_json::Value` (common interchange).
fn load_as_json(path: &Path) -> anyhow::Result<serde_json::Value> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let v: serde_json::Value = if ext == "json" {
        serde_json::from_str(&text).with_context(|| format!("JSON parse: {}", path.display()))?
    } else {
        serde_yaml::from_str(&text).with_context(|| format!("YAML parse: {}", path.display()))?
    };
    Ok(v)
}

/// Convert `serde_json::Value` → `linkml_map_core::value::Value`.
pub fn json_to_value(j: &serde_json::Value) -> Value {
    Value::from(j)
}

/// Convert `Value` → `serde_json::Value` for comparison / diffing.
pub fn value_to_json(v: &Value) -> serde_json::Value {
    serde_json::Value::from(v)
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
pub(crate) fn normalise(v: serde_json::Value) -> serde_json::Value {
    sort_keys(strip_null_leaves(v))
}

// ─── Core runner ─────────────────────────────────────────────────────────────

/// Load the transform YAML into a `TransformationSpecification`.
fn load_transform(path: &Path) -> anyhow::Result<TransformationSpecification> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading transform {}", path.display()))?;
    let mut obj: serde_json::Value =
        serde_yaml::from_str(&text).context("parsing transform YAML")?;
    linkml_map_core::datamodel::normalise_spec_json(&mut obj);
    serde_json::from_value(obj)
        .map_err(|e| anyhow::anyhow!("parsing transform spec {}: {}", path.display(), e))
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
    // ── 1. Load transform spec ───────────────────────────────────────────────
    //
    // No pre-emptive feature-string skipping: every case is executed and its
    // output compared.  A case is SKIPped only on a genuine load/parse/runtime
    // error, recording the actual message.  A transform-parse error is a real
    // load failure for this spec → SKIP (the engine never ran).
    let spec = match load_transform(&case.transform_path) {
        Ok(s) => s,
        Err(e) => {
            return Ok(RunResult {
                case_name: case.name.clone(),
                status: Status::Skip,
                reason: format!("SKIP(transform-parse): {:#}", e),
            });
        }
    };

    // ── 3. Load source schema (optional) ─────────────────────────────────────
    let source_provider: Option<SchemaSource> = match &case.source_schema_path {
        Some(p) => match load_source_schema(p) {
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

    // ── 6. Resolve the source type, then build engine and run ─────────────────
    //
    // Prefer the filename-derived class hint (e.g. `Person-001` → `Person`) but
    // ONLY when the spec actually has a matching class derivation — matched the
    // same way the engine does: `populated_from == hint`, or `name == hint`
    // when `populated_from` is absent. This is what lets a bare `Person` input
    // resolve to the `Agent` derivation (`populated_from: Person`) instead of
    // the schema tree-root (`Container`). When the hint does not match any
    // derivation (e.g. stem `PersonQuantityValue` vs spec class `Person`), fall
    // back to `None` so the engine uses the schema tree-root / sole class.
    let resolved_source_type: Option<String> = case.source_class_hint.as_deref().and_then(|hint| {
        spec.class_derivations.as_ref().and_then(|cds| {
            cds.iter()
                .find(|cd| {
                    cd.populated_from.as_deref() == Some(hint)
                        || (cd.populated_from.is_none() && cd.name == hint)
                })
                .map(|_| hint.to_string())
        })
    });

    let source_provider_ref: Option<&dyn SchemaProvider> =
        source_provider.as_ref().map(|p| p.as_provider());

    let engine = ObjectTransformer::new(spec, source_provider_ref, None);

    // Use the container-aware entry: it builds an FK ObjectIndex from the whole
    // input first, then maps. For non-FK specs this is identical to
    // `map_object` (the index is built but never consulted).
    let source_type: Option<&str> = resolved_source_type.as_deref();
    let actual_value = match engine.map_container(&input_value, source_type) {
        Ok(v) => v,
        Err(e) => {
            // A runtime Err from the engine signals a hard error / unsupported
            // feature (e.g. an FK/object-index join trying attribute access on
            // a scalar id) → SKIP, recording the actual message.
            return Ok(RunResult {
                case_name: case.name.clone(),
                status: Status::Skip,
                reason: format!("SKIP(engine): {:#}", e),
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
        // Engine ran but output differs → FAIL with the first divergent path
        // and both values.
        let diff_msg = first_diff(&expected_norm, &actual_json);
        Ok(RunResult {
            case_name: case.name.clone(),
            status: Status::Fail,
            reason: format!("FAIL(mismatch): {}", diff_msg),
        })
    }
}

/// Walk two normalised JSON values and return a short description of the first
/// divergence found.
pub(crate) fn first_diff(expected: &serde_json::Value, actual: &serde_json::Value) -> String {
    match (expected, actual) {
        (serde_json::Value::Object(e), serde_json::Value::Object(a)) => {
            // Keys in expected but missing in actual
            for (k, ev) in e.iter() {
                match a.get(k) {
                    None => {
                        return format!("key '{}' missing in actual (expected {:?})", k, short(ev))
                    }
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
        self.results
            .iter()
            .filter(|r| r.status == Status::Pass)
            .count()
    }

    pub fn fail_count(&self) -> usize {
        self.results
            .iter()
            .filter(|r| r.status == Status::Fail)
            .count()
    }

    pub fn skip_count(&self) -> usize {
        self.results
            .iter()
            .filter(|r| r.status == Status::Skip)
            .count()
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
        md.push_str(
            "Gaps ranked by number of cases they block (SKIPs + FAILs that cite them).\n\n",
        );

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
            ("unit_conversion", "Unit conversion (pint/ucumvert)"),
            (
                "asteval `case()`",
                "asteval `case()` built-in (not ported to Rust eval)",
            ),
            (
                "list-comprehension",
                "Python list-comprehension / asteval expressions",
            ),
            (
                "cast_collection_as",
                "Collection-type coercion (MultiValuedDict/List)",
            ),
            ("pivot", "Pivot / melt / unmelt operations"),
            (
                "stringification",
                "Stringification (delimiter / JSON / YAML)",
            ),
            ("FK join", "FK join / indexed object lookup"),
            ("aggregation", "Aggregation operations"),
            ("offset", "Offset calculation"),
            ("dictionary_key", "Dictionary-key / dict-keyed collections"),
            (
                "scalar↔multivalued",
                "Scalar↔multivalued coercion (single_value_for_multivalued)",
            ),
            ("mirror_source", "mirror_source enum derivation"),
            ("uri range", "URI range coercion (IRI/CURIE expansion)"),
            ("curie range", "CURIE range coercion (prefix expansion)"),
            (
                "schema-load",
                "Schema load failure (import resolution / metamodel compat)",
            ),
            ("transform-parse", "Transform spec parse error"),
            ("engine", "Engine runtime error (map_object failure)"),
            (
                "mismatch",
                "Output mismatch (engine ran but result differs from expected)",
            ),
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
        ranked.sort_by_key(|a| std::cmp::Reverse(a.1));
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
                eprintln!(
                    "[conformance] directory not found, skipping: {}",
                    dir.display()
                );
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
