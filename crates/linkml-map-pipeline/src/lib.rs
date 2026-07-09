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
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use futures::StreamExt;
use linkml_map_core::{
    datamodel::{ClassDerivation, TransformationSpecification},
    engine::{CompiledExprs, LookupIndex, ObjectIndex, ObjectTransformer},
    schema::{RangeKind, SchemaProvider},
    value::Value,
};
use linkml_map_io::{
    Format, NumericColumnHints, NumericColumnKind, load_all_with_numeric_hints,
    load_stream_with_numeric_hints, write_all,
};
use linkml_map_schemaview::SchemaViewProvider;
use tokio::sync::mpsc;

// ── Public types ──────────────────────────────────────────────────────────────

/// Configuration for a single pipeline run.
#[derive(Debug, Clone)]
pub struct PipelineConfig {
    /// Path to the source data.
    ///
    /// **Single-file mode (default):** a path to one data file — the historical
    /// behavior; every row is a source object of the resolved source class.
    ///
    /// **Directory mode (multi-table joins):** a path to a *directory* switches
    /// on the [`DataLoader`](https://github.com/linkml/linkml-map)-style
    /// convention mirrored from upstream Python. Each file in the directory is a
    /// named table matched by its **filename stem** to a source class/table name.
    /// The primary table (the resolved source class) is streamed as usual, and
    /// every table referenced by a synthesized/explicit `joins:` block in the
    /// spec is loaded into an in-memory [`LookupIndex`] so that `{Table.col}`
    /// join references actually resolve at runtime instead of collapsing to null.
    /// A join whose table has no file in the directory is skipped (its reference
    /// resolves to null), matching upstream `transform_spec`.
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
    /// Cross-table join lookup index, built once when the pipeline runs in
    /// directory mode (secondary tables loaded from sibling files). `None` in
    /// single-file mode — the historical behavior. Shared (`Arc`) into every
    /// per-row engine so lock-free join resolution runs across worker threads.
    lookup_index: Option<Arc<LookupIndex>>,
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

    /// Precompute CSV/TSV numeric column hints from the source schema already
    /// loaded into this plan. The loader reuses these hints across every row and
    /// never reopens or reparses the schema file.
    fn numeric_column_hints_for_class(&self, class_name: Option<&str>) -> NumericColumnHints {
        let Some(class_name) = class_name else {
            return NumericColumnHints::new();
        };

        self.source_provider
            .induced_slots(class_name)
            .map(|slots| {
                slots
                    .into_iter()
                    .filter_map(|slot| match slot.range {
                        RangeKind::Type(range) => {
                            numeric_column_kind(&range).map(|kind| (slot.name, kind))
                        }
                        // Enums and strings intentionally remain lexical, even
                        // where their concrete values happen to look numeric.
                        RangeKind::Class(_) | RangeKind::Enum(_) | RangeKind::None => None,
                    })
                    .collect()
            })
            // A class hint can be absent for a generic object mapping. Preserve
            // historical all-string tabular behaviour in that case.
            .unwrap_or_default()
    }

    fn source_numeric_column_hints(&self) -> NumericColumnHints {
        self.numeric_column_hints_for_class(self.source_class.as_deref())
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
        let mut spec = load_transform_spec(spec_path)
            .with_context(|| format!("loading spec {}", spec_path.display()))?;

        // Load source schema, applying any source_schema_patches from the spec
        // (e.g. FK range augmentation). `SchemaViewProvider` now resolves builtin
        // CURIE maps / `linkml:` import prefixes itself, so no fallback is needed.
        let source_provider: Box<dyn SchemaProvider> = Box::new(
            load_schema(schema_path, spec.source_schema_patches.as_ref())
                .with_context(|| format!("loading source schema {}", schema_path.display()))?,
        );

        // Synthesize implicit cross-table joins ONCE, now that the source schema
        // is available — mirrors upstream `Transformer.derived_specification`,
        // which rewrites implicit `{Table.col}` references into explicit joins
        // before any row is processed (synthesis walks `all_classes()`, so it
        // cannot run at raw spec-parse time when no schema is loaded yet). This
        // is the build-once layer, alongside `CompiledExprs::build`; the per-row
        // `new_borrowed` engines below share the resulting derived spec. Fails
        // loud on an un-keyable / un-hostable reference rather than letting it
        // silently resolve to null at runtime.
        linkml_map_core::normalize::synthesize_implicit_joins(&mut spec, source_provider.as_ref())
            .with_context(|| {
                format!(
                    "synthesizing implicit joins for spec {}",
                    spec_path.display()
                )
            })?;

        // Load target schema (may be same file — that's fine, second load is cheap)
        let target_provider: Box<dyn SchemaProvider> =
            Box::new(load_schema(target_schema_path, None).with_context(|| {
                format!("loading target schema {}", target_schema_path.display())
            })?);

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
            lookup_index: None,
        })
    }

    /// Attach a pre-built cross-table [`LookupIndex`] (directory mode).
    ///
    /// The pipeline builds one index from the secondary-table files, wraps it in
    /// `Arc`, and calls this once before wrapping the plan in `Arc<CompiledPlan>`.
    /// Every per-row engine then borrows a clone of the `Arc` via
    /// [`ObjectTransformer::with_lookup_index`], so join resolution is lock-free
    /// across worker threads.
    pub fn attach_lookup_index(&mut self, index: Arc<LookupIndex>) {
        self.lookup_index = Some(index);
    }

    /// Enumerate the secondary tables this spec joins to, mapping each to the
    /// column its rows must be indexed on. Mirrors upstream
    /// `engine._collect_all_joins`: walks every `class_derivation` and its nested
    /// descendants, and for each declared/synthesized `joins:` entry resolves the
    /// lookup key (`lookup_key` or `join_on`).
    ///
    /// The map key is the name the engine looks the table up under at runtime
    /// (`class_named` if set, else the join alias — matching the engine's
    /// `ac.class_named.unwrap_or(alias)` resolution), which is also the file stem
    /// used to find its data in directory mode.
    ///
    /// # Errors
    /// Fails loud (mirroring upstream's `ValueError`) when a join specifies
    /// neither `join_on` nor both of `source_key`/`lookup_key`, or when two joins
    /// resolve to the same table with conflicting lookup keys.
    fn collect_join_tables(&self) -> Result<BTreeMap<String, String>> {
        fn walk(cd: &ClassDerivation, out: &mut BTreeMap<String, String>) -> Result<()> {
            if let Some(joins) = cd.joins.as_ref() {
                for (join_name, ac) in joins {
                    let lookup_key = ac.lookup_key.as_deref().or(ac.join_on.as_deref());
                    let source_key = ac.source_key.as_deref().or(ac.join_on.as_deref());
                    let (Some(lookup_key), Some(_source_key)) = (lookup_key, source_key) else {
                        anyhow::bail!(
                            "join {join_name:?} must specify 'join_on' or both \
                             'source_key' and 'lookup_key'"
                        );
                    };
                    let table = ac.class_named.as_deref().unwrap_or(join_name).to_string();
                    if let Some(existing) = out.get(&table)
                        && existing != lookup_key
                    {
                        anyhow::bail!(
                            "conflicting join specs for {table:?}: \
                             lookup key {existing:?} vs {lookup_key:?}"
                        );
                    }
                    out.insert(table, lookup_key.to_string());
                }
            }
            for (_, sd) in cd.slot_derivations.iter().flatten() {
                for nested in sd.class_derivations.iter().flatten().map(|(_, v)| v) {
                    walk(nested, out)?;
                }
            }
            Ok(())
        }

        let mut out = BTreeMap::new();
        for cd in self.spec.class_derivations.iter().flatten() {
            walk(cd, &mut out)?;
        }
        Ok(out)
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
        let mut engine = ObjectTransformer::new_borrowed(
            &self.spec,
            Some(self.source_provider.as_ref()),
            Some(self.target_provider.as_ref()),
        )
        .with_compiled_exprs(&self.compiled_exprs);
        if let Some(li) = &self.lookup_index {
            engine = engine.with_lookup_index(Arc::clone(li));
        }
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
        let mut engine = ObjectTransformer::new_borrowed(
            &self.spec,
            Some(self.source_provider.as_ref()),
            Some(self.target_provider.as_ref()),
        )
        .with_compiled_exprs(&self.compiled_exprs);
        if let Some(li) = &self.lookup_index {
            engine = engine.with_lookup_index(Arc::clone(li));
        }
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
    run_pipeline_with_options(config, false).await
}

/// Run the pipeline, optionally keeping processing after row-level transform
/// failures.
///
/// When `continue_on_error` is enabled, every failed row is reported to stderr
/// as soon as the collector observes it.  A clean run that saw any such errors
/// still returns an error after output has been flushed.  Crucially, writer
/// errors are returned unchanged: already-emitted row diagnostics remain
/// visible rather than being deferred until a point a writer failure may never
/// reach.
pub async fn run_pipeline_with_options(
    mut config: PipelineConfig,
    continue_on_error: bool,
) -> Result<PipelineStats> {
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
    let mut plan = CompiledPlan::load(
        Path::new(&config.spec_path),
        Path::new(&config.schema_path),
        Path::new(&config.target_schema_path),
        config.source_class.as_deref(),
    )
    .context("compiling plan")?;

    // ── 1b. Directory-source mode: resolve the primary table's file and build
    // the cross-table LookupIndex from sibling files (multi-table joins). In
    // single-file mode this whole block is skipped and behavior is unchanged.
    if Path::new(&config.source_path).is_dir() {
        let dir = PathBuf::from(&config.source_path);
        let (primary_file, index) = load_directory_source(&dir, &plan)
            .await
            .with_context(|| format!("loading directory source {}", dir.display()))?;
        if let Some(index) = index {
            plan.attach_lookup_index(Arc::new(index));
        }
        // Redirect the streaming/buffering path at the resolved primary file so
        // format detection and the reader use the concrete data file.
        config.source_path = primary_file;
    }

    let plan = Arc::new(plan);

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
        let all_rows = load_all_with_numeric_hints(
            &config.source_path,
            src_format,
            plan.source_numeric_column_hints(),
        )
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
        let mut source_stream = load_stream_with_numeric_hints(
            &config.source_path,
            src_format,
            plan.source_numeric_column_hints(),
        )
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
        let (out_tx, out_rx) = mpsc::channel::<Result<Value>>(config.channel_depth);
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
                                if out_tx.send(Ok(v)).await.is_err() {
                                    return count;
                                }
                                count += 1;
                            }
                            Err(e) if continue_on_error => {
                                eprintln!("transform error at row {}: {e:#}", next_seq - 1);
                            }
                            Err(e) => {
                                let _ = out_tx
                                    .send(Err(e.context(format!(
                                        "transform error at row {}",
                                        next_seq - 1
                                    ))))
                                    .await;
                                return count;
                            }
                        }
                    }
                }
            } else {
                // Unordered: forward as they arrive (maximum throughput).
                while let Some((_seq, result)) = res_rx.recv().await {
                    match result {
                        Ok(v) => {
                            if out_tx.send(Ok(v)).await.is_err() {
                                return count;
                            }
                            count += 1;
                        }
                        Err(e) if continue_on_error => eprintln!("transform error: {e:#}"),
                        Err(e) => {
                            let _ = out_tx.send(Err(e.context("transform error"))).await;
                            return count;
                        }
                    }
                }
            }
            count
        });

        // Lazily turn the output channel into a stream for write_all.
        let out_stream = futures::stream::unfold(out_rx, |mut rx| async move {
            rx.recv().await.map(|result| (result, rx))
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

// ── Directory-source (multi-table) mode ──────────────────────────────────────
//
// Mirrors upstream Python's `DataLoader` convention (transformer/engine.py
// `transform_spec`): a directory of files maps table names → files by filename
// stem. We resolve the primary table's file (the resolved source class) and load
// every joined table referenced by the (post-synthesis) spec into one
// `LookupIndex`, so `{Table.col}` join references resolve at runtime instead of
// collapsing to null.

/// Resolve the primary data file and build the cross-table [`LookupIndex`] for a
/// directory source.
///
/// Returns `(primary_file_path, Some(index))` when the spec declares any joins
/// whose tables have data files in `dir`; `(primary_file_path, None)` when there
/// are no resolvable joins (an empty index would only add overhead, so the plain
/// single-file path is kept). Fails loud when the primary table's file cannot be
/// located — a directory source with no matching primary file is a user error,
/// not a silent empty run.
async fn load_directory_source(
    dir: &Path,
    plan: &CompiledPlan,
) -> Result<(String, Option<LookupIndex>)> {
    // Primary table = the resolved source class (populated_from / name of the
    // first class_derivation). Its data file is matched by stem, same as joins.
    let primary_table = plan.source_class.as_deref().ok_or_else(|| {
        anyhow::anyhow!(
            "directory source {} requires a resolvable source class, but the spec \
             declares no class_derivations to derive one from",
            dir.display()
        )
    })?;
    let primary_file = find_table_file(dir, primary_table)?.ok_or_else(|| {
        anyhow::anyhow!(
            "directory source {} has no data file for the primary table {primary_table:?} \
             (expected a file whose name stem is {primary_table:?})",
            dir.display()
        )
    })?;

    // Load every joined table that has a file in the directory. A join whose
    // table has no file is skipped (its reference resolves to null), mirroring
    // upstream `if join_name in data_loader`.
    let joins = plan.collect_join_tables()?;
    let mut index = LookupIndex::new();
    let mut any_registered = false;
    for (table, lookup_key) in joins {
        let Some(file) = find_table_file(dir, &table)? else {
            continue;
        };
        let fmt = Format::from_path(&file)
            .with_context(|| format!("detecting format of joined table file {}", file.display()))?;
        let rows = load_all_with_numeric_hints(
            &file,
            fmt,
            plan.numeric_column_hints_for_class(Some(&table)),
        )
        .await
        .with_context(|| format!("loading joined table {table:?} from {}", file.display()))?;
        index.register_table(&table, &rows, &lookup_key);
        any_registered = true;
    }

    let primary = primary_file
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("non-UTF-8 primary file path {}", primary_file.display()))?;
    Ok((primary.to_owned(), any_registered.then_some(index)))
}

/// Find the file in `dir` whose filename stem equals `table_name`.
///
/// Sub-directories are ignored; the first matching regular file is returned.
/// `Ok(None)` when no file matches (a missing joined table is a skip, not an
/// error — only a missing *primary* table is fatal, enforced by the caller).
fn find_table_file(dir: &Path, table_name: &str) -> Result<Option<PathBuf>> {
    for entry in std::fs::read_dir(dir)
        .with_context(|| format!("reading source directory {}", dir.display()))?
    {
        let entry = entry.with_context(|| format!("reading entry in {}", dir.display()))?;
        let path = entry.path();
        if path.is_dir() {
            continue;
        }
        if path.file_stem().and_then(|s| s.to_str()) == Some(table_name) {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

// ── Schema loader ────────────────────────────────────────────────────────────
//
// `SchemaViewProvider` resolves builtin CURIE maps (`default_curi_maps:
// [semweb_context]`) and builtin `linkml:` import prefixes before `add_schema`
// (see `linkml-map-schemaview`'s `resolve_default_curi_maps`), so schemas that
// used to need an in-memory YAML fallback now load through the strict backend
// directly.

/// Return the scalar coercion appropriate for a built-in LinkML numeric range.
/// Custom scalar types are intentionally left lexical: without the type's
/// `typeof` chain, guessing would reintroduce the ZIP-code/enum coercion bug.
fn numeric_column_kind(range: &str) -> Option<NumericColumnKind> {
    match range.to_ascii_lowercase().as_str() {
        "integer" | "nonnegativeinteger" | "positiveinteger" | "nonpositiveinteger"
        | "negativeinteger" | "unsignedinteger" => Some(NumericColumnKind::Integer),
        "float" | "double" | "decimal" => Some(NumericColumnKind::Float),
        _ => None,
    }
}

/// Load a source/target schema through `SchemaViewProvider`, applying any
/// `source_schema_patches` (FK range augmentation etc.) before indexing.
fn load_schema(path: &Path, patch: Option<&serde_json::Value>) -> Result<SchemaViewProvider> {
    SchemaViewProvider::load_from_path_with_patch(path, patch)
        .with_context(|| format!("loading schema: {}", path.display()))
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
    use linkml_map_io::{Format, write_vec};
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

    // ── Implicit-join synthesis is wired into the real load path ──────────────

    /// Minimal LinkML source schema with two tables sharing a non-identifier
    /// `subject_id` column, so an implicit `{Reading.score}` reference can be
    /// keyed on `subject_id`.
    fn write_join_schema(dir: &std::path::Path) -> std::path::PathBuf {
        let p = dir.join("schema.yaml");
        std::fs::write(
            &p,
            r#"id: https://example.org/join-test
name: join_test
prefixes:
  linkml: https://w3id.org/linkml/
default_range: string
imports:
  - linkml:types
classes:
  Measurement:
    attributes:
      id:
        identifier: true
      subject_id: {}
      method: {}
  Reading:
    attributes:
      id:
        identifier: true
      subject_id: {}
      score:
        range: float
"#,
        )
        .unwrap();
        p
    }

    /// `CompiledPlan::load` — the shared CLI/pipeline build-once entry — runs
    /// implicit-join synthesis, so an implicit `{Reading.score}` expression
    /// reference is rewritten into an explicit join on the hosting class
    /// derivation. Proves `synthesize_implicit_joins` is wired into the real
    /// path a CLI/pipeline user hits, not just reachable from its own unit test.
    #[test]
    fn compiled_plan_load_synthesizes_implicit_expr_join() {
        let tmp = tempfile::tempdir().unwrap();
        let schema = write_join_schema(tmp.path());
        let spec = tmp.path().join("spec.yaml");
        std::fs::write(
            &spec,
            r#"id: https://example.org/join-transform
title: implicit expr join
class_derivations:
  Result:
    populated_from: Measurement
    slot_derivations:
      id:
        populated_from: id
      reading_score:
        expr: "{Reading.score}"
"#,
        )
        .unwrap();

        let plan = CompiledPlan::load(&spec, &schema, &schema, Some("Measurement"))
            .expect("plan load (with join synthesis) failed");

        let cd = &plan.spec.class_derivations.as_ref().unwrap()[0];
        let join = cd
            .joins
            .as_ref()
            .expect("synthesis should have added a joins block")
            .get("Reading")
            .expect("synthesis should have added a Reading join");
        assert_eq!(join.join_on.as_deref(), Some("subject_id"));
    }

    /// A cross-table reference in a TOP-LEVEL `slot_derivations` entry has no
    /// class_derivation to host a join, so synthesis fails loud. Proves the
    /// reject path — the reason the top-level `slot_derivations` field was added
    /// — runs through the real `CompiledPlan::load` entry, surfacing as a hard
    /// load error instead of a silent null at runtime.
    #[test]
    fn compiled_plan_load_rejects_unhostable_cross_table_ref() {
        let tmp = tempfile::tempdir().unwrap();
        let schema = write_join_schema(tmp.path());
        let spec = tmp.path().join("spec.yaml");
        std::fs::write(
            &spec,
            r#"id: https://example.org/unhostable-transform
title: unhostable top-level slot
slot_derivations:
  bad:
    expr: "{Reading.score}"
class_derivations:
  Result:
    populated_from: Measurement
    slot_derivations:
      id:
        populated_from: id
"#,
        )
        .unwrap();

        let err = match CompiledPlan::load(&spec, &schema, &schema, Some("Measurement")) {
            Ok(_) => panic!("unhostable cross-table ref should fail loud at load"),
            Err(e) => e,
        };
        let full = format!("{err:#}");
        assert!(
            full.contains("only class_derivations can host joins"),
            "expected unhostable-join error, got: {full}"
        );
    }

    // ── Directory-mode join RESOLVES end-to-end (not just declared) ───────────

    /// Minimal target schema defining the `Result` class the join spec derives:
    /// `id` (identifier) + `reading_score` (float, populated from the joined
    /// `Reading` table).
    fn write_join_target_schema(dir: &std::path::Path) -> std::path::PathBuf {
        let p = dir.join("target.yaml");
        std::fs::write(
            &p,
            r#"id: https://example.org/join-target
name: join_target
prefixes:
  linkml: https://w3id.org/linkml/
default_range: string
imports:
  - linkml:types
classes:
  Result:
    attributes:
      id:
        identifier: true
      reading_score:
        range: float
"#,
        )
        .unwrap();
        p
    }

    /// End-to-end proof that an implicit-join-via-expression spec run through the
    /// REAL `run_pipeline` (directory-source mode) produces output whose joined
    /// value is actually populated — not null.
    ///
    /// This is the gap the prior pass left: synthesis *declares* the join, but
    /// nothing in the production pipeline loaded the joined table's data or
    /// attached a `LookupIndex`, so `{Reading.score}` silently resolved to null.
    /// Directory mode loads `Reading.jsonl` into the index and the reference
    /// resolves.
    #[tokio::test]
    async fn test_pipeline_directory_join_resolves_value() {
        let tmp = tempfile::tempdir().unwrap();
        let schema = write_join_schema(tmp.path());
        let target = write_join_target_schema(tmp.path());

        // Implicit cross-table reference: reading_score := {Reading.score}.
        // Measurement and Reading share the non-identifier `subject_id` column,
        // so synthesis infers a join keyed on subject_id.
        let spec = tmp.path().join("spec.yaml");
        std::fs::write(
            &spec,
            r#"id: https://example.org/join-transform
title: implicit expr join
class_derivations:
  Result:
    populated_from: Measurement
    slot_derivations:
      id:
        populated_from: id
      reading_score:
        expr: "{Reading.score}"
"#,
        )
        .unwrap();

        // Directory of source tables, matched by filename stem to class name.
        let data_dir = tmp.path().join("data");
        std::fs::create_dir(&data_dir).unwrap();
        std::fs::write(
            data_dir.join("Measurement.jsonl"),
            "{\"id\":\"M:1\",\"subject_id\":\"S:1\",\"method\":\"m1\"}\n",
        )
        .unwrap();
        std::fs::write(
            data_dir.join("Reading.jsonl"),
            "{\"id\":\"R:1\",\"subject_id\":\"S:1\",\"score\":9.5}\n",
        )
        .unwrap();

        let out_path = tmp.path().join("out.jsonl");
        let cfg = PipelineConfig {
            // Source path is the DIRECTORY — triggers multi-table mode.
            source_path: data_dir.to_str().unwrap().to_owned(),
            source_format: None,
            schema_path: schema.to_str().unwrap().to_owned(),
            target_schema_path: target.to_str().unwrap().to_owned(),
            spec_path: spec.to_str().unwrap().to_owned(),
            out_path: out_path.to_str().unwrap().to_owned(),
            out_format: Some(Format::Jsonl),
            source_class: Some("Measurement".to_owned()),
            workers: 2,
            ordered: true,
            channel_depth: 64,
        };

        let stats = run_pipeline(cfg)
            .await
            .expect("directory-mode pipeline failed");
        assert_eq!(stats.rows_in, 1, "expected 1 primary (Measurement) row in");
        assert_eq!(stats.rows_out, 1, "expected 1 row out");

        let loaded = linkml_map_io::load_all(&out_path, Format::Jsonl)
            .await
            .unwrap();
        assert_eq!(loaded.len(), 1);

        let m = match &loaded[0] {
            Value::Map(m) => m,
            other => panic!("expected Map, got {other:?}"),
        };
        assert_eq!(m.get("id"), Some(&Value::Str("M:1".into())));

        // The whole point: the joined value is populated, NOT null.
        let score = m
            .get("reading_score")
            .expect("reading_score missing from output");
        assert_ne!(
            score,
            &Value::Null,
            "joined value must resolve, not collapse to null: {loaded:?}"
        );
        match score {
            Value::Float(f) => assert!((f - 9.5).abs() < 1e-9, "reading_score = {f}, want 9.5"),
            Value::Int(i) => assert_eq!(*i, 9, "reading_score = {i}, want 9.5"),
            other => panic!("unexpected reading_score type: {other:?}"),
        }
    }

    /// The SAME implicit-join spec run against a single FILE (not a directory)
    /// builds no `LookupIndex` — single-file mode is untouched by directory mode.
    /// With the `Reading` alias unbound, the engine's existing unknown-root guard
    /// makes the `{Reading.score}` expression fail loud rather than silently
    /// null. This is pre-existing engine behavior (unchanged by this crate's
    /// work); the test pins it as the backward-compat contract: single-file mode
    /// neither scans a directory nor resolves cross-table joins.
    #[tokio::test]
    async fn test_pipeline_single_file_join_is_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        let schema = write_join_schema(tmp.path());
        let target = write_join_target_schema(tmp.path());
        let spec = tmp.path().join("spec.yaml");
        std::fs::write(
            &spec,
            r#"id: https://example.org/join-transform
title: implicit expr join
class_derivations:
  Result:
    populated_from: Measurement
    slot_derivations:
      id:
        populated_from: id
      reading_score:
        expr: "{Reading.score}"
"#,
        )
        .unwrap();

        // Single FILE source (not a directory) → no LookupIndex is built.
        let src = tmp.path().join("Measurement.jsonl");
        std::fs::write(
            &src,
            "{\"id\":\"M:1\",\"subject_id\":\"S:1\",\"method\":\"m1\"}\n",
        )
        .unwrap();

        let out_path = tmp.path().join("out.jsonl");
        let cfg = PipelineConfig {
            source_path: src.to_str().unwrap().to_owned(),
            source_format: Some(Format::Jsonl),
            schema_path: schema.to_str().unwrap().to_owned(),
            target_schema_path: target.to_str().unwrap().to_owned(),
            spec_path: spec.to_str().unwrap().to_owned(),
            out_path: out_path.to_str().unwrap().to_owned(),
            out_format: Some(Format::Jsonl),
            source_class: Some("Measurement".to_owned()),
            workers: 2,
            ordered: true,
            channel_depth: 64,
        };

        // No directory ⇒ no lookup index ⇒ the unbound join reference surfaces as
        // a hard transform error (the row fails, so run_pipeline returns Err).
        let err = run_pipeline(cfg)
            .await
            .expect_err("single-file join without data should not silently succeed");
        let full = format!("{err:#}");
        assert!(
            full.contains("failed to transform"),
            "expected a transform failure in single-file join mode, got: {full}"
        );
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
            if let Value::Map(m) = row
                && let Some(Value::Str(id)) = m.get("id")
            {
                seen_ids.insert(id.clone(), true);
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
            std::path::Path::new(&cfg.spec_path),
            std::path::Path::new(&cfg.schema_path),
            std::path::Path::new(&cfg.target_schema_path),
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
