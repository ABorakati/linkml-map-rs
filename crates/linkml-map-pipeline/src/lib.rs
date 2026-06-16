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
    engine::{CompiledExprs, ObjectIndex, ObjectTransformer},
    schema::SchemaProvider,
    value::Value,
};
use linkml_map_io::{load_all, load_stream, write_all, Format};
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
    /// Source schema provider — either a real `SchemaViewProvider` or a
    /// tolerant `InMemorySchema` fallback for schemas with unresolvable CURIE maps.
    source_provider: Box<dyn SchemaProvider>,
    /// Target schema provider (same tolerance).
    target_provider: Box<dyn SchemaProvider>,
    /// Pre-parsed `expr:` ASTs, shared (by borrow) into every per-row engine.
    compiled_exprs: CompiledExprs,
    /// Resolved source class name (empty string = let engine decide per row).
    source_class: Option<String>,
}

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
        // Load + normalise the transformation spec first so we can extract
        // source_schema_patches before loading the source schema.
        let spec = load_transform_spec(spec_path)
            .with_context(|| format!("loading spec {}", spec_path.display()))?;

        // Load source schema with CURIE-map tolerance, applying any
        // source_schema_patches from the spec (e.g. FK range augmentation).
        // `SchemaViewProvider` fails on schemas that reference `semweb_context`
        // or other standard CURIE maps it can't resolve at load time. We fall
        // back to a minimal in-memory YAML parser that extracts the class/slot
        // metadata the engine needs for FK detection and range coercion.
        let source_provider: Box<dyn SchemaProvider> = {
            let ts = load_schema_tolerant(schema_path, spec.source_schema_patches.as_ref())
                .with_context(|| format!("loading source schema {}", schema_path.display()))?;
            match ts {
                TolerantSchema::Real(p) => Box::new(p),
                TolerantSchema::InMemory(p) => Box::new(p),
            }
        };

        // Load target schema (may be same file — that's fine, second load is cheap)
        let target_provider: Box<dyn SchemaProvider> = {
            let ts = load_schema_tolerant(target_schema_path, None).with_context(|| {
                format!("loading target schema {}", target_schema_path.display())
            })?;
            match ts {
                TolerantSchema::Real(p) => Box::new(p),
                TolerantSchema::InMemory(p) => Box::new(p),
            }
        };

        // Parse every expr: ONCE into a reusable AST cache. Malformed exprs
        // surface here at plan-build time rather than per row.
        let compiled_exprs = CompiledExprs::build(&spec)
            .map_err(|e| anyhow::anyhow!("compiling expr ASTs: {e}"))
            .with_context(|| format!("compiling exprs from spec {}", spec_path.display()))?;

        // Resolve the source class once
        let source_class = source_class_hint.map(str::to_owned).or_else(|| {
            // Try first class_derivation's populated_from, then its name
            spec.class_derivations.as_ref().and_then(|cds| {
                cds.first()
                    .map(|cd| cd.populated_from.clone().unwrap_or_else(|| cd.name.clone()))
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

    /// Returns `true` when the spec has any `expr:` that contains a `.`
    /// attribute access AND the source schema has at least one slot whose
    /// range is a class — meaning FK dereference is likely needed.
    ///
    /// This is a conservative over-approximation: it may return `true` when
    /// the dot-access is on an inlined object (not an FK), but that is safe
    /// (the FK pipeline buffering path degrades gracefully: the index just
    /// won't be consulted for inlined objects). It never returns `false` when
    /// FK resolution is actually needed.
    ///
    /// When `true`, [`run_pipeline`] will buffer the full dataset, build one
    /// `ObjectIndex`, and fan out per-row transforms sharing `Arc<ObjectIndex>`.
    /// When `false`, the streaming non-FK path is used unchanged.
    pub fn requires_object_index(&self) -> bool {
        // Fast path: no expr: slots → no FK access possible.
        let has_dot_expr = self
            .spec
            .class_derivations
            .as_ref()
            .map(|cds| {
                cds.iter()
                    .filter_map(|cd| cd.slot_derivations.as_ref())
                    .flat_map(|sds| sds.values())
                    .any(|sd| sd.expr.as_deref().map(|e| e.contains('.')).unwrap_or(false))
            })
            .unwrap_or(false);

        if !has_dot_expr {
            return false;
        }

        // A dotted expr needs the ObjectIndex only for a genuine cross-row
        // foreign key: a class-range slot that is NOT inlined (so the value is
        // an id, not a nested object) AND whose range class has an identifier
        // (so it can be referenced by id). In-row nested access — an inlined
        // slot, or a range class with no identifier (implicitly inlined) — is
        // resolved within the row and must stay on the fast streaming path.
        let source: &dyn SchemaProvider = self.source_provider.as_ref();
        source.all_class_names().iter().any(|class_name| {
            source
                .induced_slots(class_name)
                .unwrap_or_default()
                .iter()
                .any(|s| match s.range_class() {
                    Some(rc) => {
                        let inlined = s.inlined || s.inlined_as_list;
                        let range_has_id = source.identifier_slot(rc).ok().flatten().is_some();
                        !inlined && range_has_id
                    }
                    None => false,
                })
        })
    }

    /// Transform one row using a pre-built [`ObjectIndex`].
    ///
    /// The FK pipeline builds the index ONCE, wraps it in `Arc`, and passes
    /// a reference to each parallel worker — zero per-row index rebuild.
    pub fn transform_with_index(&self, row: Value, index: &ObjectIndex) -> Result<Value> {
        let engine = ObjectTransformer::new_borrowed(
            &self.spec,
            Some(self.source_provider.as_ref()),
            Some(self.target_provider.as_ref()),
        )
        .with_compiled_exprs(&self.compiled_exprs);
        engine
            .map_object_with_index(&row, self.source_class.as_deref(), index)
            .map_err(|e| anyhow::anyhow!("transform error: {e}"))
    }

    /// Transform one row.  Called from rayon worker threads.
    ///
    /// Public so benchmarks can isolate per-row transform throughput from the
    /// reader/writer I/O path.
    pub fn transform(&self, row: Value) -> Result<Value> {
        // Borrow the spec (no per-row deep clone) and the pre-parsed expr cache.
        let engine = ObjectTransformer::new_borrowed(
            &self.spec,
            Some(self.source_provider.as_ref()),
            Some(self.target_provider.as_ref()),
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
            Some(self.source_provider.as_ref()),
            Some(self.target_provider.as_ref()),
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

    // ── 2. Detect FK mode and dispatch ───────────────────────────────────────
    //
    // FK specs need the full dataset to build the ObjectIndex (referenced
    // objects may appear after the referencing rows). When FK is required:
    //   a. Buffer ALL source rows into memory.
    //   b. Build ONE ObjectIndex over the full set (wraps the container Value).
    //   c. Fan out per-row transforms in parallel, each sharing Arc<ObjectIndex>.
    // Non-FK specs keep the original streaming path unchanged.
    let src_format = config
        .source_format
        .map(Ok)
        .unwrap_or_else(|| Format::from_path(&config.source_path))
        .with_context(|| format!("detecting source format for {}", config.source_path))?;

    let out_format = config
        .out_format
        .map(Ok)
        .unwrap_or_else(|| Format::from_path(&config.out_path))
        .with_context(|| format!("detecting output format for {}", config.out_path))?;

    let rows_in;
    let rows_out;

    if plan.requires_object_index() {
        // ── FK mode: buffer → build index → parallel transform ────────────────
        //
        // IMPORTANT: FK resolution is NOT row-streamable. The referenced
        // collection (e.g. `entities`) may appear AFTER the referencing rows
        // (e.g. `mappings`) in the source file. We must buffer all rows first.
        //
        // Memory cost: O(N) in the number of source rows. For very large datasets
        // consider breaking the source into pre-joined chunks before using this
        // pipeline.
        let all_rows = load_all(&config.source_path, src_format)
            .await
            .with_context(|| format!("loading source file for FK mode: {}", config.source_path))?;

        rows_in = all_rows.len();

        // Build a container Value (Map) so ObjectIndex can scan all collections.
        // The source data is a sequence of top-level objects (container rows).
        // We wrap them into a synthetic container map keyed by their position,
        // so the index can discover the entity/reference collections.
        //
        // For the canonical flattening case the source is a SINGLE container
        // object (MappingSet) that already IS the container. When the source
        // is a list of containers (jsonl), we build the index from the first
        // row (the container that holds the entity sub-collections) and treat
        // ALL rows as the items to transform — this matches map_container
        // semantics.
        let source_class = plan.source_class.clone();
        let container_val = if all_rows.len() == 1 {
            all_rows[0].clone()
        } else {
            // Multiple rows: build index from a synthetic container that holds
            // all rows as a list under a generic key. This enables the index
            // to find referenced objects inlined within individual rows.
            // Use serde_json round-trip to build the Map (avoids a direct
            // indexmap dependency in this crate).
            let json_list: serde_json::Value =
                serde_json::to_value(&all_rows).unwrap_or(serde_json::Value::Array(vec![]));
            let json_container = serde_json::json!({ "items": json_list });
            serde_json::from_value::<Value>(json_container).unwrap_or(Value::Null)
        };

        // Build the index ONCE, wrap in Arc for sharing across workers.
        let index = Arc::new(ObjectIndex::build(
            &container_val,
            source_class.as_deref(),
            Some(plan.source_provider.as_ref()),
        ));

        // Fan out with a sized rayon pool: par_iter over rows sharing
        // Arc<ObjectIndex> + Arc<CompiledPlan> (both Send+Sync, lock-free reads).
        // Replaces per-row tokio spawn_blocking, which was overhead-bound on
        // tiny rows. `collect()` preserves input order, so output is ordered.
        let pool = build_rayon_pool(config.workers)?;

        if matches!(out_format, Format::Jsonl) {
            // JSONL: transform AND serialise in parallel into one buffer, then a
            // single bulk write (serial per-row serialise was the writer wall).
            use rayon::prelude::*;
            let plan2 = Arc::clone(&plan);
            let idx2 = Arc::clone(&index);
            let (buf, n): (String, usize) = tokio::task::spawn_blocking(move || {
                let work = || -> (String, usize) {
                    let outs: Vec<Option<String>> = all_rows
                        .par_iter()
                        .map(|row| match plan2.transform_with_index(row.clone(), &idx2) {
                            Ok(v) => linkml_map_io::value_to_json(&v)
                                .ok()
                                .and_then(|j| serde_json::to_string(&j).ok()),
                            Err(e) => {
                                eprintln!("FK transform error: {e:#}");
                                None
                            }
                        })
                        .collect();
                    let mut buf = String::with_capacity(outs.len() * 64);
                    let mut n = 0usize;
                    for o in outs.into_iter().flatten() {
                        buf.push_str(&o);
                        buf.push('\n');
                        n += 1;
                    }
                    (buf, n)
                };
                match pool.as_ref() {
                    Some(p) => p.install(work),
                    None => work(),
                }
            })
            .await
            .context("FK transform task")?;

            rows_out = n;
            use tokio::io::AsyncWriteExt;
            let out_file = tokio::fs::File::create(&config.out_path)
                .await
                .with_context(|| format!("creating FK output {}", config.out_path))?;
            let mut writer = tokio::io::BufWriter::with_capacity(1 << 20, out_file);
            writer.write_all(buf.as_bytes()).await?;
            writer.flush().await?;
        } else {
            // Other formats: parallel transform → Vec<Value>, then the generic
            // (serial) writer for that format.
            use rayon::prelude::*;
            let plan2 = Arc::clone(&plan);
            let idx2 = Arc::clone(&index);
            let values: Vec<Value> = tokio::task::spawn_blocking(move || {
                let work = || {
                    all_rows
                        .par_iter()
                        .filter_map(|row| match plan2.transform_with_index(row.clone(), &idx2) {
                            Ok(v) => Some(v),
                            Err(e) => {
                                eprintln!("FK transform error: {e:#}");
                                None
                            }
                        })
                        .collect::<Vec<Value>>()
                };
                match pool.as_ref() {
                    Some(p) => p.install(work),
                    None => work(),
                }
            })
            .await
            .context("FK transform task")?;

            rows_out = values.len();
            let stream = futures::stream::iter(values.into_iter().map(Ok::<_, anyhow::Error>));
            write_all(&config.out_path, out_format, stream)
                .await
                .with_context(|| format!("writing FK output to {}", config.out_path))?;
        }
    } else if src_format == Format::Jsonl && out_format == Format::Jsonl {
        // ── Fully-parallel JSONL fast path ────────────────────────────────────
        //
        // The generic streaming path parses every record in the serial reader
        // (`load_stream`) and serialises every record in the serial writer —
        // making tiny-per-row JSONL jobs I/O/serde-bound, not parallel. Here the
        // reader emits *raw lines* (cheap byte scan), workers parse+transform+
        // serialise in parallel, and the writer does raw byte appends. This moves
        // JSON parse + serialise off the serial path onto the worker pool.
        let (ri, ro) = transform_jsonl_parallel(&config, Arc::clone(&plan)).await?;
        rows_in = ri;
        rows_out = ro;
    } else {
        // ── Non-FK streaming mode (original path, unchanged) ──────────────────
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
        // hands off to spawn_blocking (runs on the tokio blocking pool).
        //
        // In-flight workers are capped by a semaphore so a slow transform can't
        // let the dispatcher spawn an unbounded number of tasks (memory spike).
        // The cap also bounds how far out-of-order the results can get, which in
        // turn bounds the reorder buffer below.  We keep no JoinHandle list:
        // each worker owns a `res_tx` clone, so the results channel closes
        // naturally once the last in-flight worker finishes.
        let res_tx2 = res_tx.clone();
        let plan2 = Arc::clone(&plan);
        let max_inflight = if config.workers > 0 {
            config.workers
        } else {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(8)
        };
        let sem = Arc::new(tokio::sync::Semaphore::new(max_inflight));
        let dispatcher_handle = tokio::spawn(async move {
            while let Some((seq, row)) = work_rx.recv().await {
                let plan3 = Arc::clone(&plan2);
                let res_tx3 = res_tx2.clone();
                // Back-pressure the dispatcher when all worker slots are busy.
                let permit = Arc::clone(&sem).acquire_owned().await.unwrap();
                tokio::task::spawn_blocking(move || {
                    let result = plan3.transform(row);
                    // Best-effort send; if receiver dropped, ignore.
                    let _ = res_tx3.blocking_send((seq, result));
                    drop(permit); // free the slot when this row is done
                });
            }
            drop(res_tx2); // in-flight workers still hold their own clones
        });
        drop(res_tx); // main task no longer holds a sender

        // ── 4. Collect + write (streamed, no whole-dataset buffering) ─────────────
        // A collector task feeds finished rows into an output channel; write_all
        // streams from it, so memory stays bounded to the channel depth (+ the
        // reorder window in ordered mode, itself bounded by `max_inflight`).
        let (out_tx, out_rx) = mpsc::channel::<Value>(config.channel_depth);
        let ordered = config.ordered;
        let collector = tokio::spawn(async move {
            let mut count = 0usize;
            if ordered {
                use std::collections::BTreeMap;
                // Bounded: at most `max_inflight` entries can be out of order,
                // since only that many rows are ever in flight concurrently.
                let mut pending: BTreeMap<usize, Result<Value>> = BTreeMap::new();
                let mut next_seq = 0usize;
                while let Some((seq, result)) = res_rx.recv().await {
                    pending.insert(seq, result);
                    // Drain consecutive completed rows (errors advance past gaps).
                    while let Some(result) = pending.remove(&next_seq) {
                        next_seq += 1;
                        match result {
                            Ok(v) => {
                                if out_tx.send(v).await.is_err() {
                                    return count;
                                }
                                count += 1;
                            }
                            Err(e) => eprintln!("transform error at seq {}: {e:#}", next_seq - 1),
                        }
                    }
                }
            } else {
                // Unordered: forward as they arrive (maximum throughput).
                while let Some((_seq, result)) = res_rx.recv().await {
                    match result {
                        Ok(v) => {
                            if out_tx.send(v).await.is_err() {
                                return count;
                            }
                            count += 1;
                        }
                        Err(e) => eprintln!("transform error: {e:#}"),
                    }
                }
            }
            count
        });

        // Lazily turn the output channel into a stream for write_all.
        let out_stream = futures::stream::unfold(out_rx, |mut rx| async move {
            rx.recv().await.map(|v| (Ok::<_, anyhow::Error>(v), rx))
        })
        .boxed();
        write_all(&config.out_path, out_format, out_stream)
            .await
            .with_context(|| format!("writing output to {}", config.out_path))?;

        rows_out = collector.await.unwrap_or(0);
        rows_in = reader_handle.await.unwrap_or(0);
        let _ = dispatcher_handle.await;
    }

    if rows_out < rows_in {
        anyhow::bail!(
            "{} of {} rows failed to transform (see stderr for details)",
            rows_in - rows_out,
            rows_in
        );
    }

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

/// Fully-parallel JSONL transform: raw-line reader → parallel
/// parse+transform+serialise workers → raw-byte writer.
///
/// Returns `(rows_in, rows_out)`. Used by [`run_pipeline`] when source and
/// output are both JSONL and the spec is not FK (no shared `ObjectIndex`).
/// Build a rayon pool sized to `workers` (0 → use the global pool ≈ num CPUs).
fn build_rayon_pool(workers: usize) -> Result<Arc<Option<rayon::ThreadPool>>> {
    let pool = if workers > 0 {
        Some(
            rayon::ThreadPoolBuilder::new()
                .num_threads(workers)
                .build()
                .map_err(|e| anyhow::anyhow!("building rayon pool: {e}"))?,
        )
    } else {
        None
    };
    Ok(Arc::new(pool))
}

async fn transform_jsonl_parallel(
    config: &PipelineConfig,
    plan: Arc<CompiledPlan>,
) -> Result<(usize, usize)> {
    use rayon::prelude::*;
    use tokio::io::{AsyncWriteExt, BufWriter};

    let pool = build_rayon_pool(config.workers)?;

    // Bulk read: one big async read, not per-line `next_line().await` (100k+
    // awaits were the real bottleneck — read dominated, not transform). Line
    // splitting + JSON parse + transform + serialise then run fully in parallel
    // on the rayon pool, off the async reactor.
    //
    // ponytail: reads the whole file into memory (O(file_size)). For inputs
    // larger than RAM, switch to a block-streaming reader (read fixed-size byte
    // blocks, split lines across block boundaries) — not needed for typical ETL.
    let dbg = std::env::var("PIPELINE_PHASE_TIMING").is_ok();
    let t0 = Instant::now();
    let data = tokio::fs::read_to_string(&config.source_path)
        .await
        .with_context(|| format!("reading JSONL source {}", config.source_path))?;
    let t_read = t0.elapsed();

    let t1 = Instant::now();
    let plan2 = Arc::clone(&plan);
    let (out_buf, rows_in, rows_out): (String, usize, usize) =
        tokio::task::spawn_blocking(move || {
            let work = || -> (String, usize, usize) {
                let lines: Vec<&str> = data.lines().filter(|l| !l.trim().is_empty()).collect();
                let n_in = lines.len();
                let outs: Vec<Option<String>> = lines
                    .par_iter()
                    .map(|l| parse_transform_serialize(&plan2, l))
                    .collect();
                let mut buf = String::with_capacity(outs.len() * 64);
                let mut n_out = 0usize;
                for o in outs.into_iter().flatten() {
                    buf.push_str(&o);
                    buf.push('\n');
                    n_out += 1;
                }
                (buf, n_in, n_out)
            };
            match pool.as_ref() {
                Some(p) => p.install(work),
                None => work(),
            }
        })
        .await
        .context("transform task")?;
    let t_xform = t1.elapsed();

    let t2 = Instant::now();
    let out_file = tokio::fs::File::create(&config.out_path)
        .await
        .with_context(|| format!("creating JSONL output {}", config.out_path))?;
    let mut writer = BufWriter::with_capacity(1 << 20, out_file);
    writer.write_all(out_buf.as_bytes()).await?;
    writer.flush().await?;
    let t_write = t2.elapsed();

    if dbg {
        eprintln!(
            "[phase] read={:.3}s transform={:.3}s write={:.3}s (workers={})",
            t_read.as_secs_f64(),
            t_xform.as_secs_f64(),
            t_write.as_secs_f64(),
            config.workers
        );
    }
    Ok((rows_in, rows_out))
}

/// Parse one JSONL line → transform → serialise back to a JSON string.
/// Returns `None` (dropping the row, with a logged error) on any failure.
fn parse_transform_serialize(plan: &CompiledPlan, line: &str) -> Option<String> {
    let v: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("JSONL parse error: {e}");
            return None;
        }
    };
    let out = match plan.transform(v) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("transform error: {e:#}");
            return None;
        }
    };
    match linkml_map_io::value_to_json(&out).and_then(|j| Ok(serde_json::to_string(&j)?)) {
        Ok(s) => Some(s),
        Err(e) => {
            eprintln!("serialise error: {e:#}");
            None
        }
    }
}

// ── Tolerant schema loader ──────────────────────────────────────────────────────
//
// `SchemaViewProvider::load_from_path` fails on schemas that reference
// unresolved standard CURIE maps (e.g. `semweb_context`).  The tolerant loader
// falls back to an in-memory schema parsed directly from the YAML — enough
// metadata for FK range classification and object-index resolution.

/// Wrapper that holds either a real `SchemaViewProvider` or a minimal
/// `InMemorySchema` parsed from YAML, exposing both as `&dyn SchemaProvider`.
enum TolerantSchema {
    Real(SchemaViewProvider),
    InMemory(linkml_map_core::schema::InMemorySchema),
}

impl TolerantSchema {}

/// Load a schema tolerantly: try `SchemaViewProvider` first; if it fails due
/// to CURIE resolution errors, fall back to the minimal YAML parser.
/// The fallback provides class/slot/range/identifier metadata sufficient for
/// FK detection and object-index resolution.
fn load_schema_tolerant(path: &Path, patch: Option<&serde_json::Value>) -> Result<TolerantSchema> {
    match SchemaViewProvider::load_from_path_with_patch(path, patch) {
        Ok(p) => Ok(TolerantSchema::Real(p)),
        Err(_) => {
            if patch.is_some() {
                eprintln!(
                    "warning: source_schema_patches could not be applied \
                     (schema fell back to in-memory parser): {}",
                    path.display()
                );
            }
            // Try the tolerant in-memory YAML parser as fallback.
            build_inmemory_schema_from_yaml(path)
                .map(TolerantSchema::InMemory)
                .with_context(|| {
                    format!(
                        "loading schema (real + inmemory fallback failed): {}",
                        path.display()
                    )
                })
        }
    }
}

/// Tolerant LinkML-schema → `InMemorySchema` parser.
///
/// Reads classes, slots (inline `attributes` + referenced global `slots`),
/// ranges, and `identifier`/`multivalued` flags. Everything else defaults
/// (unknown range → scalar type). Deliberately minimal — not a full LinkML loader.
fn build_inmemory_schema_from_yaml(path: &Path) -> Result<linkml_map_core::schema::InMemorySchema> {
    use linkml_map_core::schema::{
        ClassDef as SchemaDef, InMemorySchemaBuilder, RangeKind, SlotDef,
    };
    use serde_yaml_ng as serde_yaml;

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
    // Always register common scalar types so unknown ranges still classify.
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
            builder = builder.add_class(SchemaDef {
                name: class_name.clone(),
                tree_root,
                is_a,
                mixins: vec![],
            });

            // Inline `attributes`.
            if let Some(attrs) = cobj
                .and_then(|m| m.get("attributes"))
                .and_then(|a| a.as_object())
            {
                for (slot_name, sdef) in attrs {
                    builder = builder.add_slot(class_name, make_slot(slot_name, sdef.as_object()));
                }
            }
            // Referenced global `slots` list.
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

// ── Transform spec loader (mirrors linkml-map-conformance) ────────────────────

/// Load and normalise a transformation spec YAML (map → list for class_derivations).
pub fn load_transform_spec(path: &Path) -> Result<TransformationSpecification> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading spec {}", path.display()))?;
    let mut obj: serde_json::Value =
        serde_yaml_ng::from_str(&text).context("parsing transform YAML")?;
    linkml_map_core::datamodel::normalise_spec_json(&mut obj);
    serde_json::from_value(obj).with_context(|| format!("parsing spec {}", path.display()))
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
                make_map_val(&[("id", Value::Str(format!("P:{i}"))), ("height", height)])
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

    // ── FK pipeline: flattening golden fixture ─────────────────────────────────

    fn flattening_fixture_dir() -> std::path::PathBuf {
        let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
        std::path::PathBuf::from(&manifest)
            .parent() // crates/
            .unwrap()
            .parent() // workspace root
            .unwrap()
            .join("tests/examples/flattening")
    }

    /// Run the flattening/MappingSet-001 fixture through `run_pipeline` (FK mode)
    /// and assert the output matches the golden expected output.
    ///
    /// This fixture PASSES via `map_container` in the conformance runner.
    /// The pipeline should produce the same result via the FK buffering path.
    #[tokio::test]
    async fn test_pipeline_fk_flattening_golden() {
        let fixture = flattening_fixture_dir();
        let tmp = tempfile::tempdir().unwrap();
        let out_path = tmp.path().join("out.yaml");

        let cfg = PipelineConfig {
            source_path: fixture
                .join("data/MappingSet-001.yaml")
                .to_str()
                .unwrap()
                .to_owned(),
            source_format: Some(Format::Yaml),
            schema_path: fixture
                .join("source/normalized.yaml")
                .to_str()
                .unwrap()
                .to_owned(),
            target_schema_path: fixture
                .join("target/denormalized.yaml")
                .to_str()
                .unwrap()
                .to_owned(),
            spec_path: fixture
                .join("transform/denormalize.transform.yaml")
                .to_str()
                .unwrap()
                .to_owned(),
            out_path: out_path.to_str().unwrap().to_owned(),
            out_format: Some(Format::Yaml),
            source_class: Some("MappingSet".to_owned()),
            workers: 2,
            ordered: true,
            channel_depth: 64,
        };

        // Verify the plan detects FK need.
        let plan = CompiledPlan::load(
            &std::path::Path::new(&cfg.spec_path),
            &std::path::Path::new(&cfg.schema_path),
            &std::path::Path::new(&cfg.target_schema_path),
            cfg.source_class.as_deref(),
        )
        .expect("plan load failed");
        assert!(
            plan.requires_object_index(),
            "flattening spec should require FK index"
        );

        let stats = run_pipeline(cfg).await.expect("FK pipeline failed");
        assert_eq!(stats.rows_in, 1, "expected 1 container row in");
        assert_eq!(stats.rows_out, 1, "expected 1 container row out");

        // Read the output back and compare against golden.
        let output_rows = linkml_map_io::load_all(&out_path, Format::Yaml)
            .await
            .expect("reading output failed");
        assert_eq!(output_rows.len(), 1, "expected 1 output row");

        let output_text =
            std::fs::read_to_string(&out_path).expect("reading raw YAML output failed");
        let output_yaml: serde_json::Value =
            serde_yaml_ng::from_str(&output_text).expect("parsing raw YAML output failed");
        assert!(
            output_yaml.is_object(),
            "single transformed container should serialize as a top-level YAML object"
        );

        let expected_path = fixture.join("output/MappingSet-001.transformed.yaml");
        let expected_text =
            std::fs::read_to_string(&expected_path).expect("reading golden YAML output failed");
        let expected_yaml: serde_json::Value =
            serde_yaml_ng::from_str(&expected_text).expect("parsing golden YAML output failed");
        assert_eq!(
            output_yaml, expected_yaml,
            "raw YAML object shape should match the golden fixture"
        );

        // The output should be a MappingSet with a mappings list.
        let out_map = match &output_rows[0] {
            Value::Map(m) => m,
            other => panic!("expected Map, got {:?}", other),
        };

        let mappings = out_map.get("mappings").expect("output has no 'mappings'");
        let mapping_list = match mappings {
            Value::List(l) => l,
            other => panic!("'mappings' should be a List, got {:?}", other),
        };
        assert_eq!(mapping_list.len(), 1, "expected 1 mapping in output");

        let m = match &mapping_list[0] {
            Value::Map(m) => m,
            other => panic!("mapping should be a Map, got {:?}", other),
        };

        // Verify FK-resolved fields are present and correct.
        assert_eq!(
            m.get("subject_id"),
            Some(&Value::Str("X:1".into())),
            "subject_id mismatch"
        );
        assert_eq!(
            m.get("subject_name"),
            Some(&Value::Str("x1".into())),
            "subject_name mismatch — FK not resolved"
        );
        assert_eq!(
            m.get("object_id"),
            Some(&Value::Str("Y:1".into())),
            "object_id mismatch"
        );
        assert_eq!(
            m.get("object_name"),
            Some(&Value::Str("y1".into())),
            "object_name mismatch — FK not resolved"
        );
        assert_eq!(
            m.get("predicate_id"),
            Some(&Value::Str("P:1".into())),
            "predicate_id mismatch"
        );
    }

    /// Parallel FK test: synthesize N entity objects + M mappings, run
    /// through the FK pipeline at workers=4, assert all rows resolve their
    /// FKs and rows_out is correct.
    ///
    /// Note: we use the engine directly (not run_pipeline) because run_pipeline
    /// reads from a file and the synthetic FK container is built in memory.
    /// This tests Arc<ObjectIndex> sharing across parallel workers.
    #[tokio::test]
    async fn test_pipeline_fk_parallel_resolution() {
        use indexmap::IndexMap;
        use linkml_map_core::{
            datamodel::{ClassDerivation, SlotDerivation, TransformationSpecification},
            engine::{CompiledExprs, ObjectIndex},
            schema::{ClassDef, InMemorySchemaBuilder, RangeKind, SlotDef},
        };

        // ── Build a schema: Container { mappings: [Mapping], entities: {Entity} }
        //                               Mapping { subject: Entity, predicate: string }
        //                               Entity  { id: string (identifier), name: string }
        let schema = InMemorySchemaBuilder::new()
            .add_type("string")
            .add_class(ClassDef {
                name: "Container".into(),
                tree_root: true,
                is_a: None,
                mixins: vec![],
            })
            .add_slot(
                "Container",
                SlotDef {
                    name: "mappings".into(),
                    range: RangeKind::Class("Mapping".into()),
                    multivalued: true,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .add_slot(
                "Container",
                SlotDef {
                    name: "entities".into(),
                    range: RangeKind::Class("Entity".into()),
                    multivalued: true,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .add_class(ClassDef {
                name: "Mapping".into(),
                tree_root: false,
                is_a: None,
                mixins: vec![],
            })
            .add_slot(
                "Mapping",
                SlotDef {
                    name: "subject".into(),
                    range: RangeKind::Class("Entity".into()),
                    multivalued: false,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .add_slot(
                "Mapping",
                SlotDef {
                    name: "predicate".into(),
                    range: RangeKind::Type("string".into()),
                    multivalued: false,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .add_class(ClassDef {
                name: "Entity".into(),
                tree_root: false,
                is_a: None,
                mixins: vec![],
            })
            .add_slot(
                "Entity",
                SlotDef {
                    name: "id".into(),
                    range: RangeKind::Type("string".into()),
                    multivalued: false,
                    required: true,
                    identifier: true,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .add_slot(
                "Entity",
                SlotDef {
                    name: "name".into(),
                    range: RangeKind::Type("string".into()),
                    multivalued: false,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .build();

        // ── Build spec: Container→Container, Mapping→FlatMapping (subject.id, subject.name)
        let mut container_slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        container_slots.insert(
            "mappings".into(),
            SlotDerivation {
                name: "mappings".into(),
                populated_from: Some("mappings".into()),
                ..Default::default()
            },
        );

        let mut mapping_slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        mapping_slots.insert(
            "subject_id".into(),
            SlotDerivation {
                name: "subject_id".into(),
                expr: Some("subject.id".into()),
                ..Default::default()
            },
        );
        mapping_slots.insert(
            "subject_name".into(),
            SlotDerivation {
                name: "subject_name".into(),
                expr: Some("subject.name".into()),
                ..Default::default()
            },
        );
        mapping_slots.insert(
            "predicate_id".into(),
            SlotDerivation {
                name: "predicate_id".into(),
                populated_from: Some("predicate".into()),
                ..Default::default()
            },
        );

        let spec = TransformationSpecification {
            class_derivations: Some(vec![
                ClassDerivation {
                    name: "Container".into(),
                    populated_from: Some("Container".into()),
                    slot_derivations: Some(container_slots),
                    ..Default::default()
                },
                ClassDerivation {
                    name: "Mapping".into(),
                    populated_from: Some("Mapping".into()),
                    slot_derivations: Some(mapping_slots),
                    ..Default::default()
                },
            ]),
            ..Default::default()
        };

        let compiled_exprs = CompiledExprs::build(&spec).expect("compile exprs");

        // ── Synthesize N=20 Entity objects + M=50 Mapping rows
        const N_ENTITIES: usize = 20;
        const M_MAPPINGS: usize = 50;

        // Build entity dict: { "E:0": {name: "entity_0"}, ... }
        let entities_json: serde_json::Value = {
            let mut obj = serde_json::Map::new();
            for i in 0..N_ENTITIES {
                let key = format!("E:{i}");
                obj.insert(key, serde_json::json!({ "name": format!("entity_{i}") }));
            }
            serde_json::Value::Object(obj)
        };

        // Build mapping list: [{ subject: "E:i%N", predicate: "P:j" }, ...]
        let mappings_json: serde_json::Value = serde_json::Value::Array(
            (0..M_MAPPINGS)
                .map(|j| {
                    serde_json::json!({
                        "subject": format!("E:{}", j % N_ENTITIES),
                        "predicate": format!("P:{j}"),
                    })
                })
                .collect(),
        );

        // The container holds both collections.
        let container_json = serde_json::json!({
            "mappings": mappings_json,
            "entities": entities_json,
        });
        let container: Value = serde_json::from_value(container_json).unwrap();

        // Build the ObjectIndex ONCE from the container.
        let index = Arc::new(ObjectIndex::build(
            &container,
            Some("Container"),
            Some(&schema as &dyn linkml_map_core::schema::SchemaProvider),
        ));
        assert_eq!(index.len(), N_ENTITIES, "index should hold all entities");

        // Fan out: 4 parallel workers each sharing Arc<ObjectIndex> + Arc<spec>.
        let spec = Arc::new(spec);
        let compiled_exprs = Arc::new(compiled_exprs);
        let schema = Arc::new(schema);

        // Simulate M mapping rows through the engine with the shared index.
        let mappings = match &container {
            Value::Map(m) => match m.get("mappings") {
                Some(Value::List(l)) => l.clone(),
                _ => panic!("expected mappings list"),
            },
            _ => panic!("expected container map"),
        };

        let results: Vec<Value> = {
            use rayon::prelude::*;
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(4)
                .build()
                .unwrap();
            pool.install(|| {
                mappings
                    .par_iter()
                    .map(|row| {
                        use linkml_map_core::engine::ObjectTransformer;
                        use linkml_map_core::schema::SchemaProvider;
                        let engine = ObjectTransformer::new_borrowed(
                            &spec,
                            Some(schema.as_ref() as &dyn SchemaProvider),
                            None,
                        )
                        .with_compiled_exprs(&compiled_exprs);
                        engine
                            .map_object_with_index(row, Some("Mapping"), &index)
                            .expect("transform failed")
                    })
                    .collect()
            })
        };

        assert_eq!(results.len(), M_MAPPINGS, "all mappings should transform");

        // Verify FK resolution: each result should have subject_id + subject_name.
        for (j, result) in results.iter().enumerate() {
            let m = match result {
                Value::Map(m) => m,
                other => panic!("expected Map at row {j}, got {other:?}"),
            };
            let expected_entity_idx = j % N_ENTITIES;
            let expected_subject_id = format!("E:{expected_entity_idx}");
            let expected_subject_name = format!("entity_{expected_entity_idx}");

            assert_eq!(
                m.get("subject_id"),
                Some(&Value::Str(expected_subject_id.clone())),
                "FK subject_id mismatch at row {j}"
            );
            assert_eq!(
                m.get("subject_name"),
                Some(&Value::Str(expected_subject_name.clone())),
                "FK subject_name mismatch at row {j} (FK not resolved)"
            );
            assert_eq!(
                m.get("predicate_id"),
                Some(&Value::Str(format!("P:{j}"))),
                "predicate_id mismatch at row {j}"
            );
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
