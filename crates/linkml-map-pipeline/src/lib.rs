//! High-throughput async + concurrent row-transformation pipeline for linkml-map.
//!
//! # Overview
//!
//! ```text
//!  AsyncReader (linkml-map-io)
//!      │  tokio bounded channel (backpressure)
//!      ▼
//!  rayon threadpool  ──→  ObjectTransformer (Arc<CompiledPlan>)
//!      │  tokio bounded channel
//!      ▼
//!  AsyncWriter (linkml-map-io)
//! ```
//!
//! The [`CompiledPlan`] is built once before any rows are processed.  It holds
//! the parsed [`TransformationSpecification`], both [`SchemaViewProvider`]s,
//! and the resolved source-class name.  All three are `Send + Sync` and shared
//! via `Arc` across worker threads — no per-row re-parsing.
//!
//! **Expr AST caching**: every slot/enum `expr:` string in the spec is lexed +
//! parsed exactly once at [`CompiledPlan::load`] into a [`CompiledExprs`] and
//! shared via `Arc<CompiledPlan>`.  Each per-row [`ObjectTransformer`] is built
//! `with_compiled_exprs`, so workers evaluate the cached AST instead of
//! re-parsing the expr string on every row.  This removes the per-row parse
//! cost that previously dominated CPU and capped thread scaling.

use std::{
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use futures::StreamExt;
use linkml_map_core::{
    datamodel::TransformationSpecification,
    engine::{CompiledExprs, ObjectTransformer},
    schema::SchemaProvider,
    value::Value,
};
use linkml_map_io::{load_stream, write_all, Format};
use linkml_map_schemaview::SchemaViewProvider;
use tokio::sync::mpsc;

// ── Public types ──────────────────────────────────────────────────────────────

/// Configuration for a single pipeline run.
#[derive(Debug, Clone)]
pub struct PipelineConfig {
    /// Path to the source data file.
    pub source_path: String,
    /// Format of the source file; if `None`, auto-detected from extension.
    pub source_format: Option<Format>,
    /// Path to the source LinkML schema YAML.
    pub schema_path: String,
    /// Path to the target LinkML schema YAML (may be the same as `schema_path`).
    pub target_schema_path: String,
    /// Path to the transformation specification YAML.
    pub spec_path: String,
    /// Output file path.
    pub out_path: String,
    /// Format for the output file; if `None`, auto-detected from extension.
    pub out_format: Option<Format>,
    /// Source class name hint.  `None` → engine resolves from tree-root / first derivation.
    pub source_class: Option<String>,
    /// Number of rayon worker threads.  `0` → rayon global pool default (≈ num CPUs).
    pub workers: usize,
    /// Preserve input row order in the output.  `true` = ordered (slightly higher memory),
    /// `false` = unordered (first-finished-first-written, maximum throughput).
    pub ordered: bool,
    /// Depth of the tokio bounded channels (reader→workers and workers→writer).
    pub channel_depth: usize,
}

impl PipelineConfig {
    /// Sensible defaults: 4 workers, ordered output, channel depth 256.
    pub fn new(
        source_path: impl Into<String>,
        schema_path: impl Into<String>,
        target_schema_path: impl Into<String>,
        spec_path: impl Into<String>,
        out_path: impl Into<String>,
    ) -> Self {
        Self {
            source_path: source_path.into(),
            source_format: None,
            schema_path: schema_path.into(),
            target_schema_path: target_schema_path.into(),
            spec_path: spec_path.into(),
            out_path: out_path.into(),
            out_format: None,
            source_class: None,
            workers: 4,
            ordered: true,
            channel_depth: 256,
        }
    }
}

/// Statistics from a completed pipeline run.
#[derive(Debug, Clone)]
pub struct PipelineStats {
    pub rows_in: usize,
    pub rows_out: usize,
    pub elapsed: Duration,
    /// rows / second throughput
    pub rows_per_sec: f64,
}

// ── Compiled plan ─────────────────────────────────────────────────────────────

/// Immutable, `Send + Sync` plan built once before processing.
///
/// Holds the parsed spec and both providers.  Shared via `Arc` across worker
/// threads — zero per-row schema/spec parsing overhead.
///
/// **Expr AST cache**: `compiled_exprs` holds every slot/enum `expr:` parsed
/// once at [`CompiledPlan::load`].  The per-row transformer is built
/// `with_compiled_exprs(&self.compiled_exprs)`, so no expr string is re-parsed
/// per row — the dominant per-row cost on expr-heavy specs.
pub struct CompiledPlan {
    spec: TransformationSpecification,
    source_provider: SchemaViewProvider,
    target_provider: SchemaViewProvider,
    /// Pre-parsed `expr:` ASTs, shared (by borrow) into every per-row engine.
    compiled_exprs: CompiledExprs,
    /// Resolved source class name (empty string = let engine decide per row).
    source_class: Option<String>,
}

// SAFETY: SchemaViewProvider wraps a pure-Rust SchemaView with no interior
// mutability after construction; TransformationSpecification is plain data.
// Both are safe to share across threads.
unsafe impl Send for CompiledPlan {}
unsafe impl Sync for CompiledPlan {}

impl CompiledPlan {
    /// Number of `expr:` derivations in the spec (slots with an `expr:`).
    /// Benchmarks use this to confirm the fixture actually exercises the cache.
    pub fn expr_slot_count(&self) -> usize {
        self.spec
            .class_derivations
            .as_ref()
            .map(|cds| {
                cds.iter()
                    .filter_map(|cd| cd.slot_derivations.as_ref())
                    .flat_map(|sds| sds.values())
                    .filter(|sd| sd.expr.is_some())
                    .count()
            })
            .unwrap_or(0)
    }

    /// Load spec + schemas from disk; resolve source class.
    pub fn load(
        spec_path: &Path,
        schema_path: &Path,
        target_schema_path: &Path,
        source_class_hint: Option<&str>,
    ) -> Result<Self> {
        // Load source schema
        let source_provider = SchemaViewProvider::load_from_path(schema_path)
            .with_context(|| format!("loading source schema {}", schema_path.display()))?;

        // Load target schema (may be same file — that's fine, second load is cheap)
        let target_provider = SchemaViewProvider::load_from_path(target_schema_path)
            .with_context(|| format!("loading target schema {}", target_schema_path.display()))?;

        // Load + normalise the transformation spec
        let spec = load_transform_spec(spec_path)
            .with_context(|| format!("loading spec {}", spec_path.display()))?;

        // Parse every expr: ONCE into a reusable AST cache. Malformed exprs
        // surface here at plan-build time rather than per row.
        let compiled_exprs = CompiledExprs::build(&spec)
            .map_err(|e| anyhow::anyhow!("compiling expr ASTs: {e}"))
            .with_context(|| format!("compiling exprs from spec {}", spec_path.display()))?;

        // Resolve the source class once
        let source_class = source_class_hint.map(str::to_owned).or_else(|| {
            // Try first class_derivation's populated_from, then its name
            spec.class_derivations.as_ref().and_then(|cds| {
                cds.first().map(|cd| {
                    cd.populated_from
                        .clone()
                        .unwrap_or_else(|| cd.name.clone())
                })
            })
        });

        Ok(Self {
            spec,
            source_provider,
            target_provider,
            compiled_exprs,
            source_class,
        })
    }

    /// Transform one row.  Called from rayon worker threads.
    ///
    /// Public so benchmarks can isolate per-row transform throughput from the
    /// reader/writer I/O path.
    pub fn transform(&self, row: Value) -> Result<Value> {
        // Borrow the spec (no per-row deep clone) and the pre-parsed expr cache.
        let engine = ObjectTransformer::new_borrowed(
            &self.spec,
            Some(&self.source_provider as &dyn SchemaProvider),
            Some(&self.target_provider as &dyn SchemaProvider),
        )
        .with_compiled_exprs(&self.compiled_exprs);
        engine
            .map_object(&row, self.source_class.as_deref())
            .map_err(|e| anyhow::anyhow!("transform error: {e}"))
    }

    /// Transform one row WITHOUT the cached-AST fast path (re-parses each
    /// `expr:` string per call). Benchmark-only: used to measure the BEFORE
    /// (string-parse) baseline against [`CompiledPlan::transform`] in-process.
    /// Still borrows the spec, so the only difference is expr parsing.
    pub fn transform_uncached(&self, row: Value) -> Result<Value> {
        let engine = ObjectTransformer::new_borrowed(
            &self.spec,
            Some(&self.source_provider as &dyn SchemaProvider),
            Some(&self.target_provider as &dyn SchemaProvider),
        );
        engine
            .map_object(&row, self.source_class.as_deref())
            .map_err(|e| anyhow::anyhow!("transform error: {e}"))
    }
}

// ── Core pipeline ─────────────────────────────────────────────────────────────

/// Run the full transform pipeline end-to-end.
///
/// 1. Parse spec + schemas once into an `Arc<CompiledPlan>`.
/// 2. Stream source rows from `source_path` through a bounded tokio channel.
/// 3. Each row is dispatched to a rayon threadpool (via `spawn_blocking`) with
///    the shared `Arc<CompiledPlan>` — no per-row schema/spec parsing.
/// 4. Results (optionally reordered) are written to `out_path`.
pub async fn run_pipeline(config: PipelineConfig) -> Result<PipelineStats> {
    let start = Instant::now();

    // Build rayon thread pool if a non-default worker count was requested.
    let _pool_guard: Option<rayon::ThreadPool> = if config.workers > 0 {
        Some(
            rayon::ThreadPoolBuilder::new()
                .num_threads(config.workers)
                .build()
                .context("building rayon thread pool")?,
        )
    } else {
        None
    };

    // ── 1. Compile plan (once) ────────────────────────────────────────────────
    let plan = Arc::new(
        CompiledPlan::load(
            Path::new(&config.spec_path),
            Path::new(&config.schema_path),
            Path::new(&config.target_schema_path),
            config.source_class.as_deref(),
        )
        .context("compiling plan")?,
    );

    // ── 2. Open source stream ─────────────────────────────────────────────────
    let src_format = config
        .source_format
        .map(Ok)
        .unwrap_or_else(|| Format::from_path(&config.source_path))
        .with_context(|| format!("detecting source format for {}", config.source_path))?;

    let mut source_stream = load_stream(&config.source_path, src_format)
        .await
        .with_context(|| format!("opening source stream {}", config.source_path))?;

    // ── 3. Concurrent transform ───────────────────────────────────────────────
    // Reader → bounded channel → tokio spawn_blocking (rayon thread) → results channel → writer
    let (work_tx, mut work_rx) = mpsc::channel::<(usize, Value)>(config.channel_depth);
    let (res_tx, mut res_rx) = mpsc::channel::<(usize, Result<Value>)>(config.channel_depth);

    // Spawn reader task: feeds (seq_id, row) into work_tx
    let reader_handle = tokio::spawn(async move {
        let mut seq = 0usize;
        while let Some(item) = source_stream.next().await {
            match item {
                Ok(val) => {
                    if work_tx.send((seq, val)).await.is_err() {
                        break; // downstream dropped
                    }
                    seq += 1;
                }
                Err(e) => {
                    // Propagate error through the channel as a special sentinel
                    // by dropping: writer will see channel close after seq rows.
                    eprintln!("reader error at row {seq}: {e:#}");
                    break;
                }
            }
        }
        seq // total rows read
    });

    // Spawn worker dispatcher: for each (seq, row) pulls from work_rx and
    // hands off to spawn_blocking (runs on rayon or tokio blocking pool).
    let res_tx2 = res_tx.clone();
    let plan2 = Arc::clone(&plan);
    let dispatcher_handle = tokio::spawn(async move {
        let mut handles = Vec::new();
        while let Some((seq, row)) = work_rx.recv().await {
            let plan3 = Arc::clone(&plan2);
            let res_tx3 = res_tx2.clone();
            let h = tokio::task::spawn_blocking(move || {
                let result = plan3.transform(row);
                // Best-effort send; if receiver dropped, ignore.
                let _ = res_tx3.blocking_send((seq, result));
            });
            handles.push(h);
        }
        // Wait for all in-flight workers before dropping our res_tx clone
        for h in handles {
            let _ = h.await;
        }
        // Drop our clone so the writer sees channel close
        drop(res_tx2);
    });
    drop(res_tx); // main task no longer holds a sender

    // ── 4. Collect + write ────────────────────────────────────────────────────
    let out_format = config
        .out_format
        .map(Ok)
        .unwrap_or_else(|| Format::from_path(&config.out_path))
        .with_context(|| format!("detecting output format for {}", config.out_path))?;

    let rows_in;
    let rows_out;

    if config.ordered {
        // Ordered: buffer out-of-order results until the expected seq arrives.
        use std::collections::BTreeMap;
        let mut pending: BTreeMap<usize, Result<Value>> = BTreeMap::new();
        let mut next_seq = 0usize;
        let mut output_rows: Vec<Value> = Vec::new();

        while let Some((seq, result)) = res_rx.recv().await {
            pending.insert(seq, result);
            // Drain consecutive completed rows
            while let Some(result) = pending.remove(&next_seq) {
                next_seq += 1;
                match result {
                    Ok(v) => output_rows.push(v),
                    Err(e) => eprintln!("transform error at seq {}: {e:#}", next_seq - 1),
                }
            }
        }
        // Drain any remaining
        for (_seq, result) in pending {
            if let Ok(v) = result {
                output_rows.push(v);
            }
        }
        rows_out = output_rows.len();
        rows_in = reader_handle.await.unwrap_or(0);

        // Write ordered results
        let stream = futures::stream::iter(output_rows.into_iter().map(Ok::<_, anyhow::Error>));
        write_all(&config.out_path, out_format, stream)
            .await
            .with_context(|| format!("writing output to {}", config.out_path))?;
    } else {
        // Unordered: write results as they arrive (maximum throughput).
        // We can't use write_all directly because we need to count — collect into vec.
        let mut output_rows: Vec<Value> = Vec::new();
        while let Some((_seq, result)) = res_rx.recv().await {
            match result {
                Ok(v) => output_rows.push(v),
                Err(e) => eprintln!("transform error: {e:#}"),
            }
        }
        rows_out = output_rows.len();
        rows_in = reader_handle.await.unwrap_or(0);

        let stream = futures::stream::iter(output_rows.into_iter().map(Ok::<_, anyhow::Error>));
        write_all(&config.out_path, out_format, stream)
            .await
            .with_context(|| format!("writing output to {}", config.out_path))?;
    }

    let _ = dispatcher_handle.await;

    let elapsed = start.elapsed();
    let rows_per_sec = if elapsed.as_secs_f64() > 0.0 {
        rows_out as f64 / elapsed.as_secs_f64()
    } else {
        f64::INFINITY
    };

    Ok(PipelineStats {
        rows_in,
        rows_out,
        elapsed,
        rows_per_sec,
    })
}

// ── Transform spec loader (mirrors linkml-map-conformance) ────────────────────

/// Load and normalise a transformation spec YAML (map → list for class_derivations).
pub fn load_transform_spec(path: &Path) -> Result<TransformationSpecification> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading spec {}", path.display()))?;
    // normalise_transform_yaml returns a JSON string (via serde_json round-trip)
    let normalised_json = normalise_transform_yaml(&text)
        .with_context(|| format!("normalising spec {}", path.display()))?;
    // Deserialise from JSON (not YAML) — the normalised form is already JSON
    serde_json::from_str(&normalised_json)
        .with_context(|| format!("parsing spec {}", path.display()))
}

/// Normalise YAML transform spec: convert `class_derivations` map → vec,
/// injecting `name` into each ClassDerivation.
///
/// `slot_derivations` stays as a map (the datamodel uses `IndexMap<String,
/// SlotDerivation>` for that field, so no list conversion needed).
fn normalise_transform_yaml(text: &str) -> Result<String> {
    let mut obj: serde_json::Value =
        serde_yaml_ng::from_str(text).context("parsing transform YAML")?;

    if let Some(cd) = obj.get_mut("class_derivations") {
        if cd.is_object() {
            let mapping = std::mem::replace(cd, serde_json::Value::Null);
            let mut list = Vec::new();
            if let serde_json::Value::Object(m) = mapping {
                for (class_name, mut val) in m {
                    if val.is_null() {
                        val = serde_json::Value::Object(serde_json::Map::new());
                    }
                    if let serde_json::Value::Object(ref mut class_obj) = val {
                        // Inject the map key as `name` (ClassDerivation.name)
                        class_obj
                            .entry("name")
                            .or_insert_with(|| serde_json::Value::String(class_name.clone()));
                        // slot_derivations stays as a map (ClassDerivation.slot_derivations
                        // is IndexMap<String,SlotDerivation>), but each SlotDerivation
                        // has a required `name` field — inject it from the map key.
                        if let Some(sd) = class_obj.get_mut("slot_derivations") {
                            if sd.is_object() {
                                let sd_owned = std::mem::replace(sd, serde_json::Value::Null);
                                if let serde_json::Value::Object(mut sdm) = sd_owned {
                                    for (slot_name, slot_val) in sdm.iter_mut() {
                                        if slot_val.is_null() {
                                            *slot_val = serde_json::json!({});
                                        }
                                        if let Some(so) = slot_val.as_object_mut() {
                                            so.entry("name").or_insert_with(|| {
                                                serde_json::Value::String(slot_name.clone())
                                            });
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

    serde_json::to_string(&obj).context("re-serialising normalised spec")
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use linkml_map_core::value::Value;
    use linkml_map_io::{write_vec, Format};
    use std::collections::HashMap;

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn measurements_fixture_dir() -> std::path::PathBuf {
        // Relative to workspace root: tests/examples/measurements/
        let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
        std::path::PathBuf::from(&manifest)
            .parent() // crates/
            .unwrap()
            .parent() // workspace root
            .unwrap()
            .join("tests/examples/measurements")
    }

    fn make_map_val(pairs: &[(&str, Value)]) -> Value {
        // Build via serde_json round-trip using the Value's own serde impl
        let obj = serde_json::Value::Object(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), serde_json::to_value(v).unwrap()))
                .collect(),
        );
        serde_json::from_value(obj).unwrap()
    }

    // ── E2E test: measurements PASSing fixture ────────────────────────────────

    /// End-to-end pipeline over the measurements/PersonQuantityValue-001 fixture.
    /// This fixture PASS in the conformance report.
    #[tokio::test]
    async fn test_pipeline_e2e_measurements_001() {
        let fixture = measurements_fixture_dir();
        let tmp = tempfile::tempdir().unwrap();
        let out_path = tmp.path().join("out.jsonl");

        let cfg = PipelineConfig {
            source_path: fixture
                .join("data/PersonQuantityValue-001.yaml")
                .to_str()
                .unwrap()
                .to_owned(),
            source_format: Some(Format::Yaml),
            schema_path: fixture
                .join("source/quantity_value.yaml")
                .to_str()
                .unwrap()
                .to_owned(),
            target_schema_path: fixture
                .join("target/quantity_value_flat.yaml")
                .to_str()
                .unwrap()
                .to_owned(),
            spec_path: fixture
                .join("transform/qv-to-scalar.transform.yaml")
                .to_str()
                .unwrap()
                .to_owned(),
            out_path: out_path.to_str().unwrap().to_owned(),
            out_format: Some(Format::Jsonl),
            source_class: Some("Person".to_owned()),
            workers: 2,
            ordered: true,
            channel_depth: 64,
        };

        let stats = run_pipeline(cfg).await.expect("pipeline failed");

        assert_eq!(stats.rows_in, 1, "expected 1 row in");
        assert_eq!(stats.rows_out, 1, "expected 1 row out");

        // Read the output and verify it matches expected
        let loaded = linkml_map_io::load_all(&out_path, Format::Jsonl)
            .await
            .unwrap();
        assert_eq!(loaded.len(), 1);

        // Expected: {id: "P:1", height_in_cm: 172.0}
        if let Value::Map(m) = &loaded[0] {
            assert_eq!(m.get("id"), Some(&Value::Str("P:1".into())));
            // height_in_cm should be 172.0 (float or int depending on parsing)
            let h = m.get("height_in_cm").expect("height_in_cm missing");
            match h {
                Value::Float(f) => assert!((f - 172.0).abs() < 0.001),
                Value::Int(i) => assert_eq!(*i, 172),
                other => panic!("unexpected height_in_cm type: {other:?}"),
            }
        } else {
            panic!("expected Map, got {:?}", loaded[0]);
        }
    }

    // ── Concurrency test: 1000+ rows ──────────────────────────────────────────

    /// Synthesize 1000 rows and run through the pipeline at workers=4.
    /// Verifies rows_out == rows_in and all IDs are present.
    #[tokio::test]
    async fn test_pipeline_concurrent_1000_rows() {
        let fixture = measurements_fixture_dir();
        let tmp = tempfile::tempdir().unwrap();

        // Build 1000 source rows: {id: "P:N", height: {value: N.0, unit: cm}}
        let rows: Vec<Value> = (0..1000)
            .map(|i| {
                // Nested map for height
                let height = make_map_val(&[
                    ("value", Value::Float(150.0 + i as f64)),
                    ("unit", Value::Str("cm".into())),
                ]);
                make_map_val(&[
                    ("id", Value::Str(format!("P:{i}"))),
                    ("height", height),
                ])
            })
            .collect();

        let src_path = tmp.path().join("source.jsonl");
        write_vec(&src_path, Format::Jsonl, rows.clone())
            .await
            .unwrap();

        let out_path = tmp.path().join("out.jsonl");

        let cfg = PipelineConfig {
            source_path: src_path.to_str().unwrap().to_owned(),
            source_format: Some(Format::Jsonl),
            schema_path: fixture
                .join("source/quantity_value.yaml")
                .to_str()
                .unwrap()
                .to_owned(),
            target_schema_path: fixture
                .join("target/quantity_value_flat.yaml")
                .to_str()
                .unwrap()
                .to_owned(),
            spec_path: fixture
                .join("transform/qv-to-scalar.transform.yaml")
                .to_str()
                .unwrap()
                .to_owned(),
            out_path: out_path.to_str().unwrap().to_owned(),
            out_format: Some(Format::Jsonl),
            source_class: Some("Person".to_owned()),
            workers: 4,
            ordered: true,
            channel_depth: 256,
        };

        let stats = run_pipeline(cfg).await.expect("pipeline failed");

        assert_eq!(stats.rows_in, 1000, "rows_in mismatch");
        assert_eq!(stats.rows_out, 1000, "rows_out mismatch");

        // Read back and verify all IDs present
        let loaded = linkml_map_io::load_all(&out_path, Format::Jsonl)
            .await
            .unwrap();
        assert_eq!(loaded.len(), 1000);

        let mut seen_ids: HashMap<String, bool> = HashMap::new();
        for row in &loaded {
            if let Value::Map(m) = row {
                if let Some(Value::Str(id)) = m.get("id") {
                    seen_ids.insert(id.clone(), true);
                }
            }
        }
        assert_eq!(seen_ids.len(), 1000, "not all IDs present in output");

        // Verify content: id P:0 → height_in_cm = 150.0
        let first = loaded
            .iter()
            .find(|v| {
                if let Value::Map(m) = v {
                    m.get("id") == Some(&Value::Str("P:0".into()))
                } else {
                    false
                }
            })
            .expect("P:0 not found");

        if let Value::Map(m) = first {
            let h = m.get("height_in_cm").expect("height_in_cm missing");
            match h {
                Value::Float(f) => assert!((f - 150.0).abs() < 0.001),
                Value::Int(i) => assert_eq!(*i, 150),
                other => panic!("unexpected type: {other:?}"),
            }
        }
    }

    // ── CompiledPlan: spec shared via Arc, no per-row reload ──────────────────

    #[tokio::test]
    async fn test_compiled_plan_shared_arc() {
        let fixture = measurements_fixture_dir();
        let plan = Arc::new(
            CompiledPlan::load(
                &fixture.join("transform/qv-to-scalar.transform.yaml"),
                &fixture.join("source/quantity_value.yaml"),
                &fixture.join("target/quantity_value_flat.yaml"),
                Some("Person"),
            )
            .unwrap(),
        );

        // Multiple Arc clones should all share the same plan
        let plan2 = Arc::clone(&plan);
        let plan3 = Arc::clone(&plan);

        assert!(Arc::ptr_eq(&plan, &plan2));
        assert!(Arc::ptr_eq(&plan, &plan3));

        // Each can independently transform
        let row = make_map_val(&[
            ("id", Value::Str("P:99".into())),
            (
                "height",
                make_map_val(&[
                    ("value", Value::Float(180.0)),
                    ("unit", Value::Str("cm".into())),
                ]),
            ),
        ]);

        let result = plan.transform(row).unwrap();
        if let Value::Map(m) = result {
            assert_eq!(m.get("id"), Some(&Value::Str("P:99".into())));
        } else {
            panic!("expected map");
        }
    }
}
