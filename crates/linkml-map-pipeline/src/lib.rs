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

mod data_loader;
mod driver;
mod join_order;
mod join_rows;

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use futures::StreamExt;
use linkml_map_core::{
    datamodel::TransformationSpecification,
    engine::{
        CompiledExprs, ContentIds, LookupIndex, ObjectIndex, ObjectTransformer,
        ResolvedDerivations, SourceSlots, TargetCoercions,
    },
    normalize::constant_blocks::{Block, classify_blocks},
    schema::{RangeKind, SchemaProvider},
    value::Value,
};
use linkml_map_io::{
    Format, NumericColumnHints, WriteOptions, validate_additional_outputs, write_all_with,
    write_all_writer_with,
};
use linkml_map_schemaview::SchemaViewProvider;
use tokio::sync::mpsc;

pub use data_loader::DataLoader;
use driver::{ConstantBlocks, DriveInput, DriveStats, RowSegment, Sink, Step};

// ── Public types ──────────────────────────────────────────────────────────────

/// Configuration for a single pipeline run.
#[derive(Debug, Clone)]
pub struct PipelineConfig {
    /// Path to the source data.
    ///
    /// **Single-file mode (default):** a path to one data file — the historical
    /// behavior; every row is a source object of the resolved source class.
    ///
    /// **Tabular file (`.tsv`/`.csv`):** the file holds one table, named by
    /// [`PipelineConfig::source_class`] or, failing that, its file stem. Every
    /// class derivation (block) reading that table streams the file's rows; a
    /// block reading another table is skipped. The run fails if no block reads it.
    ///
    /// **Directory mode (multi-table):** a path to a *directory* switches on the
    /// [`DataLoader`] convention mirrored from upstream Python. Each file is a
    /// named table matched by its **filename stem** to a block's table
    /// (`populated_from`, else the block's name). Every block streams its own
    /// table, in spec order; a block whose table has no file is skipped. Every
    /// table referenced by a synthesized/explicit `joins:` block is loaded into an
    /// in-memory [`LookupIndex`] for that block, so `{Table.col}` join references
    /// resolve at runtime instead of collapsing to null; a join whose table has no
    /// file is skipped, as upstream `transform_spec` does (a reference then fails for a
    /// row with a key value, as DuckDB does there), and a YAML join table is an error
    /// (`LookupIndex does not yet support 'yaml' files`).
    ///
    /// **Any other file** (YAML, JSON, JSONL): one source object class
    /// (`source_class`, else the first row block's table), as upstream's
    /// single-object `map_data` route.
    pub source_path: String,
    /// Format of the source file; if `None`, auto-detected from extension.
    pub source_format: Option<Format>,
    /// Path to the source LinkML schema YAML.
    pub schema_path: String,
    /// Path to the target LinkML schema YAML. `None` mirrors upstream's
    /// `target_schema=None`: no target-schema-driven behaviour (target-range
    /// coercion, enum resolution, induced target slots) is applied.
    pub target_schema_path: Option<String>,
    /// Path to the transformation specification YAML.
    pub spec_path: String,
    /// Output file path.
    pub out_path: String,
    /// Format for the output file; if `None`, auto-detected from extension.
    pub out_format: Option<Format>,
    /// Upstream's `--source-type`. A tabular file: the table the file holds
    /// (default: its stem). A directory: the `source_type` handed to every block
    /// (default: each block's own table). Any other file: the source class.
    /// `None` → resolved from tree-root / first derivation for the latter.
    pub source_class: Option<String>,
    /// Number of rayon worker threads.  `0` → rayon global pool default (≈ num CPUs).
    pub workers: usize,
    /// Preserve input row order in the output.  `true` = ordered (slightly higher memory),
    /// `false` = unordered (first-finished-first-written, maximum throughput).
    pub ordered: bool,
    /// Depth of the tokio bounded channels (reader→workers and workers→writer).
    pub channel_depth: usize,
    /// Output-writer options: `--table-name`, `--chunk-size` and `-O` additional
    /// outputs. `single_object` is decided by the pipeline from the input kind and
    /// is overwritten here.
    pub write_options: WriteOptions,
    /// Strict mode (upstream `--strict`): an `expr:` that references an unbound
    /// name, or a value for an enum-ranged target slot that matches no
    /// permissible value (or more than one), fails the row instead of yielding
    /// null / passing through with a warning. Default `false`.
    pub strict: bool,
    /// Allow the unrestricted expression evaluator (upstream
    /// `--unrestricted-eval`): `expr:` statement blocks run with the source
    /// object as `src`, and an expression the restricted evaluator rejects
    /// yields null instead of failing the row. Default `false`: the safe,
    /// restricted evaluator, which has no `src` and evaluates one expression.
    /// Only enable it for trusted specs.
    pub unrestricted_eval: bool,
}

impl PipelineConfig {
    /// Sensible defaults: 4 workers, ordered output, channel depth 256.
    pub fn new(
        source_path: impl Into<String>,
        schema_path: impl Into<String>,
        target_schema_path: Option<impl Into<String>>,
        spec_path: impl Into<String>,
        out_path: impl Into<String>,
    ) -> Self {
        Self {
            source_path: source_path.into(),
            source_format: None,
            schema_path: schema_path.into(),
            target_schema_path: target_schema_path.map(Into::into),
            spec_path: spec_path.into(),
            out_path: out_path.into(),
            out_format: None,
            source_class: None,
            workers: 4,
            ordered: true,
            channel_depth: 256,
            write_options: WriteOptions::default(),
            strict: false,
            unrestricted_eval: false,
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
    target_provider: Option<Box<dyn SchemaProvider>>,
    /// Pre-parsed `expr:` ASTs, shared (by borrow) into every per-row engine.
    compiled_exprs: CompiledExprs,
    /// Target-schema lookups (slot datatypes, enum indices and the enum
    /// warn-once state), resolved once and shared (by borrow) into every
    /// per-row engine. `None` without a target schema.
    target_coercions: Option<TargetCoercions>,
    /// Class derivations resolved (and `is_a` / `mixins` merged) once per source
    /// type, shared (by borrow) into every per-row engine.
    resolved_derivations: ResolvedDerivations,
    /// The slots each source class declares, resolved once and shared (by borrow)
    /// into every per-row engine: an `expr:` reads a declared slot the row lacks
    /// as null.
    source_slots: SourceSlots,
    /// Strict mode, applied to every per-row engine. See [`PipelineConfig::strict`].
    strict: bool,
    /// Unrestricted evaluation, applied to every per-row engine. See
    /// [`PipelineConfig::unrestricted_eval`].
    unrestricted_eval: bool,
    /// Resolved source class name (empty string = let engine decide per row).
    source_class: Option<String>,
    /// Cross-table join lookup index, built once when the pipeline runs in
    /// directory mode (secondary tables loaded from sibling files). `None` in
    /// single-file mode — the historical behavior. Shared (`Arc`) into every
    /// per-row engine so lock-free join resolution runs across worker threads.
    lookup_index: Option<Arc<LookupIndex>>,
    /// Every class derivation as upstream `transform_spec` sees it: `is_a` /
    /// `mixins` ancestors merged, classified as a constants-only block or not.
    /// Built once at [`CompiledPlan::load`]; the engine itself never sees it.
    blocks: Vec<Block>,
    /// The explicit source class (`--source-class`), if one was given. Upstream
    /// passes it as `source_type` to `transform_spec`, which uses it for
    /// constant blocks (`source_type or class_deriv.name`).
    source_class_hint: Option<String>,
    /// Content-id synthesiser for identifier slots the spec doesn't derive
    /// (upstream #342), built once from the target schema and borrowed by every
    /// per-row engine. `None` without a target schema.
    content_ids: Option<ContentIds>,
    /// Emit records whose identifier is neither derived nor minted instead of
    /// failing (`--continue-on-error`; see [`CompiledPlan::missing_identifiers`]).
    emit_without_identifiers: bool,
}

/// Whether any slot derivation of `cd` (nested class derivations included) has a
/// dotted `expr:` or `populated_from:`.
fn class_derivation_reads_dotted_path(cd: &linkml_map_core::datamodel::ClassDerivation) -> bool {
    cd.slot_derivations
        .iter()
        .flat_map(|sds| sds.values())
        .any(|sd| {
            let dotted = |text: &Option<String>| text.as_deref().is_some_and(|t| t.contains('.'));
            dotted(&sd.expr)
                || dotted(&sd.populated_from)
                || sd
                    .class_derivations
                    .iter()
                    .flatten()
                    .any(class_derivation_reads_dotted_path)
        })
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
    ///
    /// Mirrors `data_loaders._numeric_slots_for`: a slot is numeric when its
    /// range is a type whose `typeof` ancestors include `integer`, `float`,
    /// `double` or `decimal`.
    fn numeric_column_hints_for_class(&self, class_name: Option<&str>) -> NumericColumnHints {
        let Some(class_name) = class_name else {
            return NumericColumnHints::new();
        };

        self.source_provider
            .induced_slots(class_name)
            .map(|slots| {
                slots
                    .into_iter()
                    .filter(|slot| match &slot.range {
                        RangeKind::Type(range) => self
                            .source_provider
                            .type_ancestors(range)
                            .iter()
                            .any(|ancestor| is_numeric_type_name(ancestor)),
                        // Enums and strings intentionally remain lexical, even
                        // where their concrete values happen to look numeric.
                        RangeKind::Class(_) | RangeKind::Enum(_) | RangeKind::None => false,
                    })
                    .map(|slot| slot.name)
                    .collect()
            })
            // A class hint can be absent for a generic object mapping. Preserve
            // historical all-string tabular behaviour in that case.
            .unwrap_or_default()
    }

    /// Load spec + schemas from disk; resolve source class.
    pub fn load(
        spec_path: &Path,
        schema_path: &Path,
        target_schema_path: Option<&Path>,
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

        // Load the target schema when one was given. Upstream leaves
        // `target_schemaview` as None otherwise; it does not fall back to the
        // source schema.
        let target_provider = target_schema_path
            .map(|path| -> Result<Box<dyn SchemaProvider>> {
                Ok(Box::new(load_schema(path, None).with_context(|| {
                    format!("loading target schema {}", path.display())
                })?))
            })
            .transpose()?;

        // Content-id synthesis needs the target schema, so it is built once here
        // and only when a target is present.
        let content_ids = target_provider
            .as_deref()
            .map(|target| ContentIds::for_spec(target, &spec));

        // Parse every expr: ONCE into a reusable AST cache. Malformed exprs
        // surface here at plan-build time rather than per row.
        let compiled_exprs = CompiledExprs::build(&spec)
            .map_err(|e| anyhow::anyhow!("compiling expr ASTs: {e}"))
            .with_context(|| format!("compiling exprs from spec {}", spec_path.display()))?;

        // Resolve every target slot's coercion datatype ONCE (no target schema,
        // no target-driven coercion).
        let target_coercions = target_provider
            .as_deref()
            .map(|target| TargetCoercions::build(&spec, target));

        // Resolve each source type's class derivation (with its `is_a` /
        // `mixins` ancestors merged) ONCE, so the per-row engines only look it up.
        let resolved_derivations = ResolvedDerivations::build(&spec);

        // Resolve the slots each source class declares ONCE, for the `expr:`
        // bindings (upstream `Bindings._schema_slots`).
        let source_slots = SourceSlots::build(source_provider.as_ref());

        // Classify every block once, as upstream `transform_spec` does before it
        // streams anything: merge `is_a`/`mixins`, then tell constants-only
        // blocks (emitted once, reading no input) from row blocks.
        let source_classes: BTreeSet<String> =
            source_provider.all_class_names().into_iter().collect();
        let blocks = classify_blocks(&spec, &source_classes)
            .with_context(|| format!("resolving is_a/mixins in spec {}", spec_path.display()))?;

        // Resolve the source class once, for single-object input (YAML / JSON /
        // JSONL), which has one class and so one derivation: the explicit hint,
        // else the first row block (a block that reads a table, and is not merely
        // a parent template for another block). Tabular and directory input never
        // use it: every block streams its own table (see `plan_steps`).
        let source_class = source_class_hint
            .map(str::to_owned)
            .or_else(|| {
                row_block_candidates(&blocks)
                    .next()
                    .map(|block| block.table().to_owned())
            })
            .or_else(|| {
                // No row block (a constants-only spec): keep the historical
                // default of the first class_derivation's populated_from / name.
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
            target_coercions,
            resolved_derivations,
            source_slots,
            strict: false,
            unrestricted_eval: false,
            source_class,
            lookup_index: None,
            blocks,
            source_class_hint: source_class_hint.map(str::to_owned),
            content_ids,
            emit_without_identifiers: false,
        })
    }

    /// Whether the spec has any block that reads a table. A spec made only of
    /// constants-only blocks (and templates) needs no input data at all.
    ///
    /// Mirrors upstream `tables = {cd.populated_from or cd.name for cd, constant
    /// in blocks if not constant}` being non-empty.
    fn has_row_blocks(&self) -> bool {
        self.blocks.iter().any(|b| !b.constant)
    }

    /// The tables every row block reads, sorted (upstream's `tables`).
    fn row_tables(&self) -> BTreeSet<String> {
        self.blocks
            .iter()
            .filter(|b| !b.constant)
            .map(|b| b.table().to_owned())
            .collect()
    }

    /// Build every constants-only block's object once, each from one empty row.
    ///
    /// Mirrors upstream `transform_spec`:
    /// `yield from _map_rows(transformer, [{}], source_type or cd.name, cd, on_error)`.
    /// The engine stays a pure per-object function; "exactly once" is a property
    /// of this driver-side call, made before any worker is spawned. The driver
    /// then emits each object at its block's position.
    ///
    /// With `continue_on_error` a failing block is reported on stderr and
    /// counted in [`ConstantBlocks::failed`]; otherwise it fails the run before
    /// any row streams.
    fn constant_block_values(&self, continue_on_error: bool) -> Result<ConstantBlocks> {
        let mut out = ConstantBlocks {
            values: vec![None; self.blocks.len()],
            failed: 0,
        };
        if !self.blocks.iter().any(|b| b.constant) {
            return Ok(out);
        }
        let engine = self.row_engine(None);
        let empty_row = Value::Map(indexmap::IndexMap::new());
        for (i, block) in self.blocks.iter().enumerate().filter(|(_, b)| b.constant) {
            let cd = &block.class_derivation;
            let source_type = self.source_class_hint.as_deref().unwrap_or(&cd.name);
            match engine.map_object_with_derivation(&empty_row, Some(source_type), cd) {
                Ok(v) => out.values[i] = Some(v),
                Err(e) if continue_on_error => {
                    // PARITY: transform_spec routes the error to `on_error` with
                    // `row_index = 0` and the class derivation's name.
                    eprintln!(
                        "transform error at row 0; class_derivation={}: {e:#}",
                        cd.name
                    );
                    out.failed += 1;
                }
                Err(e) => {
                    return Err(anyhow::anyhow!(
                        "transform error in constant block '{}': {e}",
                        cd.name
                    ));
                }
            }
        }
        Ok(out)
    }

    /// Match every block to its data, in spec order, as `transform_spec` does.
    ///
    /// - A constants-only block is a [`Step::Constant`].
    /// - Any other block reads the table `populated_from or name`; when the
    ///   loader has no data for it the block is skipped ("Skipping
    ///   class_derivation …: no data found"), otherwise it is a [`Step::Rows`]
    ///   over the file the loader resolves, with source type
    ///   `source_type or table`.
    ///
    /// # Errors
    /// Upstream's `FileNotFoundError` when there are row blocks and none of their
    /// tables has data: `No input data for any class derivation: expected one of
    /// [...], found [...]`. Also an undetectable file format.
    fn plan_steps(
        &self,
        loader: &DataLoader,
        source_type: Option<&str>,
        format_override: Option<Format>,
    ) -> Result<Vec<Step>> {
        let tables = self.row_tables();
        if !tables.is_empty() && !tables.iter().any(|table| loader.contains(table)) {
            anyhow::bail!(
                "No input data for any class derivation: expected one of {}, found {}",
                py_str_list(tables.iter()),
                py_str_list(loader.describe_available()?.iter()),
            );
        }
        let mut steps = Vec::with_capacity(self.blocks.len());
        for (i, block) in self.blocks.iter().enumerate() {
            if block.constant {
                steps.push(Step::Constant(i));
                continue;
            }
            let table = block.table();
            let Some(file) = loader.find(table) else {
                continue;
            };
            let format = format_override
                .map(Ok)
                .unwrap_or_else(|| Format::from_path(&file))
                .with_context(|| format!("detecting source format for {}", file.display()))?;
            steps.push(Step::Rows(Arc::new(RowSegment {
                block: Some(i),
                table: Some(table.to_owned()),
                source_type: Some(source_type.unwrap_or(table).to_owned()),
                file,
                format,
            })));
        }
        Ok(steps)
    }

    /// A per-row engine over the shared plan: borrowed spec, cached `expr:` ASTs,
    /// plan-wide settings and the join index (the block's own, else the plan's).
    fn row_engine<'a>(&'a self, lookup: Option<&Arc<LookupIndex>>) -> ObjectTransformer<'a> {
        let engine = ObjectTransformer::new_borrowed(
            &self.spec,
            Some(self.source_provider.as_ref()),
            self.target_provider.as_deref(),
        )
        .with_compiled_exprs(&self.compiled_exprs);
        let mut engine = self.configure(engine);
        if let Some(li) = lookup.or(self.lookup_index.as_ref()) {
            engine = engine.with_lookup_index(Arc::clone(li));
        }
        engine
    }

    /// Transform one row of a block through that block's class derivation
    /// (`map_object(row, source_type, class_derivation=cd)`), or — for
    /// single-object input — through the derivation found by source class.
    /// `prejoined` is whether upstream's set-based join engine reads the block
    /// (see [`JoinEngine`](join_order::JoinEngine)): its joins are resolved up front.
    fn transform_segment(
        &self,
        seg: &RowSegment,
        row: Value,
        lookup: Option<&Arc<LookupIndex>>,
        prejoined: bool,
    ) -> Result<Value> {
        let engine = self.row_engine(lookup).with_prejoined_joins(prejoined);
        match seg.block {
            Some(b) => engine.map_object_with_derivation(
                &row,
                seg.source_type.as_deref(),
                &self.blocks[b].class_derivation,
            ),
            // PARITY: single-object input is indexed (`tr.index(...)`) before it is mapped,
            // which rejects a key the schema does not know even if the spec reads no reference.
            None => engine
                .check_indexable(&row, self.source_class_hint.as_deref())
                .and_then(|()| engine.map_object(&row, self.source_class.as_deref())),
        }
        .map_err(anyhow::Error::from)
    }

    /// [`CompiledPlan::transform_segment`] with a pre-built [`ObjectIndex`] (FK).
    fn transform_segment_with_index(
        &self,
        seg: &RowSegment,
        row: &Value,
        index: &ObjectIndex,
        lookup: Option<&Arc<LookupIndex>>,
        prejoined: bool,
    ) -> Result<Value> {
        let engine = self.row_engine(lookup).with_prejoined_joins(prejoined);
        match seg.block {
            Some(b) => engine.map_object_with_derivation_and_index(
                row,
                seg.source_type.as_deref(),
                &self.blocks[b].class_derivation,
                index,
            ),
            None => engine
                .check_indexable(row, self.source_class_hint.as_deref())
                .and_then(|()| {
                    engine.map_object_with_index(row, self.source_class.as_deref(), index)
                }),
        }
        .map_err(anyhow::Error::from)
    }

    /// Apply the plan-wide engine settings (shared content-id synthesiser,
    /// identifier policy) to a per-row engine.
    fn configure<'a>(&'a self, mut engine: ObjectTransformer<'a>) -> ObjectTransformer<'a> {
        if let Some(coercions) = &self.target_coercions {
            engine = engine.with_target_coercions(coercions);
        }
        engine = engine
            .with_resolved_derivations(&self.resolved_derivations)
            .with_source_slots(&self.source_slots)
            .with_strict_exprs(self.strict)
            .with_unrestricted_eval(self.unrestricted_eval);
        if let Some(ids) = &self.content_ids {
            engine = engine.with_content_ids(ids);
        }
        engine.with_emit_without_identifiers(self.emit_without_identifiers)
    }

    /// Describe every class derivation that would emit records without their
    /// identifier (neither derived nor minted). Empty without a target schema.
    /// Mirrors upstream `ObjectTransformer.missing_identifiers`.
    pub fn missing_identifiers(&self) -> Result<Vec<String>> {
        self.configure(ObjectTransformer::new_borrowed(
            &self.spec,
            Some(self.source_provider.as_ref()),
            self.target_provider.as_deref(),
        ))
        .missing_identifiers()
        .map_err(|e| anyhow::anyhow!("{e}"))
    }

    /// Let per-row engines emit records without their identifier instead of
    /// failing; set after the caller has reported
    /// [`CompiledPlan::missing_identifiers`] (`--continue-on-error`).
    pub fn set_emit_without_identifiers(&mut self, emit: bool) {
        self.emit_without_identifiers = emit;
    }

    /// Set strict mode for every per-row engine (see [`PipelineConfig::strict`]).
    pub fn set_strict(&mut self, strict: bool) {
        self.strict = strict;
    }

    /// Set unrestricted evaluation for every per-row engine (see
    /// [`PipelineConfig::unrestricted_eval`]).
    pub fn set_unrestricted_eval(&mut self, unrestricted: bool) {
        self.unrestricted_eval = unrestricted;
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
        // Fast path: no dotted `expr:` or `populated_from:` → no FK access possible
        // (`populated_from: org_id.name` reads through the index like `{org_id.name}`).
        let has_dot_expr = self
            .spec
            .class_derivations
            .as_deref()
            .is_some_and(|cds| cds.iter().any(class_derivation_reads_dotted_path));

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

    /// The [`ObjectIndex`] over a single-object input (upstream's `tr.index(input_obj,
    /// source_type)`), followed as far as this spec's expressions reach.
    pub(crate) fn build_object_index(
        &self,
        container: &Value,
        source_type: Option<&str>,
    ) -> ObjectIndex {
        self.row_engine(None).build_index(container, source_type)
    }

    /// Transform one row using a pre-built [`ObjectIndex`].
    ///
    /// The FK pipeline builds the index ONCE, wraps it in `Arc`, and passes
    /// a reference to each parallel worker — zero per-row index rebuild.
    pub fn transform_with_index(&self, row: Value, index: &ObjectIndex) -> Result<Value> {
        self.row_engine(None)
            .map_object_with_index(&row, self.source_class.as_deref(), index)
            .map_err(|e| anyhow::anyhow!("transform error: {e}"))
    }

    /// Transform one row.  Called from rayon worker threads.
    ///
    /// Public so benchmarks can isolate per-row transform throughput from the
    /// reader/writer I/O path.
    pub fn transform(&self, row: Value) -> Result<Value> {
        // Borrows the spec (no per-row deep clone) and the pre-parsed expr cache.
        self.row_engine(None)
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
            self.target_provider.as_deref(),
        );
        self.configure(engine)
            .map_object(&row, self.source_class.as_deref())
            .map_err(|e| anyhow::anyhow!("transform error: {e}"))
    }
}

/// Whether `path` names a tabular file (`.tsv` / `.csv`), the condition under
/// which upstream `map_data` takes the streaming `transform_spec` route.
fn is_tabular_path(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("tsv") || e.eq_ignore_ascii_case("csv"))
}

/// Row blocks that can stand for the source class of single-object input, in spec
/// order: blocks that read a table and that no other block inherits from, then the
/// inherited-from ones. Picking the first block blindly would take a parent
/// template (or a constants-only block) for the source class.
fn row_block_candidates(blocks: &[Block]) -> impl Iterator<Item = &Block> {
    let rows = || blocks.iter().filter(|b| !b.constant);
    rows()
        .filter(|b| !b.is_parent)
        .chain(rows().filter(|b| b.is_parent))
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
/// # Blocks (upstream `transform_spec`)
///
/// For tabular (`.tsv`/`.csv`) or directory input every class derivation is a
/// *block*, visited in spec order, exactly as upstream's `transform_spec` does
/// (see the `driver` module):
///
/// - a block built only from constants is emitted **once**, at its position, and
///   reads no input; a spec made only of such blocks needs no data;
/// - every other block streams the rows of its own table (`populated_from`, else
///   its name), matched like upstream's `DataLoader`: for a directory the file
///   whose stem is the table name (`.tsv`, `.csv`, `.yaml`, `.yml`, `.json`, then
///   Rust's `.jsonl`); for a single file only the table named by `--source-type`
///   (`source_class`) or, failing that, the file stem. A block with no table is
///   skipped; if no block has one the run fails with upstream's "No input data
///   for any class derivation" error.
///
/// Blocks run one after another, so output is in spec order, each block's rows in
/// input order when `ordered`; within a block the rows are transformed by the
/// rayon workers. Row errors under `continue_on_error` are reported with the
/// block's row index and class derivation, and `rows_in`/`rows_out` count rows
/// across blocks (constant objects count towards `rows_out` only).
///
/// PARITY: engine.py::transform_spec passes `source_type or table_name` to every
/// block, so `--source-type` overrides the source type of every block, not just
/// the one reading the file it names. Rust follows.
///
/// PARITY: cli.py::map_data — any other input (a YAML / JSON / JSONL file) is
/// single-object input, upstream's `_map_data_single`: one source class, one
/// derivation looked up by it, no constant-block handling. JSONL input is a Rust
/// extension on that route.
///
/// When `continue_on_error` is enabled, every failed row is reported to stderr
/// as soon as the collector observes it.  A clean run that saw any such errors
/// still returns an error after output has been flushed.  Crucially, writer
/// errors are returned unchanged: already-emitted row diagnostics remain
/// visible rather than being deferred until a point a writer failure may never
/// reach.
pub async fn run_pipeline_with_options(
    config: PipelineConfig,
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
        config.target_schema_path.as_deref().map(Path::new),
        config.source_class.as_deref(),
    )
    .context("compiling plan")?;
    plan.set_strict(config.strict);
    plan.set_unrestricted_eval(config.unrestricted_eval);

    // ── 1a. Identifier slots must be derived or minted (upstream
    // `cli._require_identifiers`), checked before any output is opened. Without
    // `--continue-on-error` a problem refuses the run; with it each problem is
    // reported once, records are emitted without ids, and the run ends non-zero.
    let identifier_problems = plan.missing_identifiers()?;
    if !identifier_problems.is_empty() && !continue_on_error {
        anyhow::bail!("{}", identifier_problems.join("\n"));
    }
    for problem in &identifier_problems {
        eprintln!("  - {problem}");
    }
    plan.set_emit_without_identifiers(!identifier_problems.is_empty());
    let identifier_errors = identifier_problems.len();

    // ── 1b. Constants-only blocks (upstream `1323cc8`).
    //
    // Upstream applies the "emit once, read no input" rule only in
    // `transform_spec`, which `map_data` uses for tabular (`.tsv`/`.csv`) input
    // or a directory; single-object input goes through `_map_data_single`.
    // They are built here, on this thread, before any worker starts, so each
    // appears exactly once whatever the worker count, chunking or ordering.
    let source_is_dir = Path::new(&config.source_path).is_dir();
    let streaming_input = source_is_dir || is_tabular_path(&config.source_path);
    let constants = if streaming_input {
        plan.constant_block_values(continue_on_error)?
    } else {
        ConstantBlocks::default()
    };
    let (constants_ok, constants_failed, constants_attempted) =
        (constants.emitted(), constants.failed, constants.attempted());
    // A spec made only of constants-only blocks reads nothing: no source file is
    // required, opened or format-detected.
    let reads_no_input = streaming_input && !plan.has_row_blocks();

    // PARITY: cli.py::map_data — with no `--output-format`, the format comes from the
    // output extension and falls back to YAML for an unknown extension or for stdout
    // (`EXTENSION_FORMAT_MAP.get(ext, OutputFormat.YAML)`).
    let out_format = config.out_format.unwrap_or_else(|| {
        if config.out_path == "-" {
            Format::Yaml
        } else {
            Format::from_path(&config.out_path).unwrap_or(Format::Yaml)
        }
    });

    // PARITY: cli.py::map_data — Parquet/DuckDB are binary artifacts built from a
    // staged file, so they cannot go to stdout.
    if out_format.is_columnar() && config.out_path == "-" {
        anyhow::bail!(linkml_map_io::columnar_needs_file(out_format));
    }

    // Streaming input (tabular or directory) always uses the stream writers and
    // honours `-O`; the single-object decision for other inputs is made once the
    // source format is known (see `single_object` below).
    let write_options = WriteOptions {
        single_object: false,
        ..config.write_options.clone()
    };
    if streaming_input {
        let primary = (config.out_path != "-").then(|| Path::new(&config.out_path));
        validate_additional_outputs(primary, &write_options)?;
    }

    if reads_no_input {
        let n_out = constants_ok;
        let stream = futures::stream::iter(
            constants
                .values
                .into_iter()
                .flatten()
                .map(Ok::<_, anyhow::Error>),
        );
        write_stream_to_output(&config.out_path, out_format, &write_options, stream)
            .await
            .with_context(|| format!("writing output to {}", config.out_path))?;
        if constants_failed > 0 {
            anyhow::bail!(
                "{constants_failed} of {constants_attempted} rows failed to transform \
                 (see stderr for details)"
            );
        }
        let elapsed = start.elapsed();
        return Ok(PipelineStats {
            rows_in: 0,
            rows_out: n_out,
            elapsed,
            rows_per_sec: if elapsed.as_secs_f64() > 0.0 {
                n_out as f64 / elapsed.as_secs_f64()
            } else {
                f64::INFINITY
            },
        });
    }

    // ── 2. Plan the steps: which block reads which file ──────────────────────
    //
    // Tabular / directory input: every block, matched to its table the way
    // upstream's `DataLoader` does. Any other input is one source class read
    // from the one file, as before.
    let (steps, loader, single_object) = if streaming_input {
        let loader = DataLoader::new(&config.source_path, config.source_class.as_deref())?;
        let steps = plan
            .plan_steps(
                &loader,
                config.source_class.as_deref(),
                config.source_format,
            )
            .with_context(|| format!("loading source {}", config.source_path))?;
        // PARITY: `transform_spec` registers a block's joins `if join_name in data_loader`
        // in single-file mode too, where only the file's own table is "in" the loader.
        (steps, Some(loader), false)
    } else {
        let format = config
            .source_format
            .map(Ok)
            .unwrap_or_else(|| Format::from_path(&config.source_path))
            .with_context(|| format!("detecting source format for {}", config.source_path))?;
        let segment = RowSegment {
            block: None,
            table: plan.source_class.clone(),
            source_type: plan.source_class.clone(),
            file: PathBuf::from(&config.source_path),
            format,
        };
        // PARITY: cli.py::map_data — only tabular (.tsv/.csv) or directory input
        // streams; any other input is "single object" input and is written by
        // `dump_output` (`_map_data_single`), which also ignores `-O`.  JSONL
        // input is a Rust extension and keeps the streaming writers.
        let single_object = matches!(format, Format::Yaml | Format::Json);
        (vec![Step::Rows(Arc::new(segment))], None, single_object)
    };
    let write_options = WriteOptions {
        single_object,
        ..config.write_options.clone()
    };
    if !streaming_input && !single_object {
        let primary = (config.out_path != "-").then(|| Path::new(&config.out_path));
        validate_additional_outputs(primary, &write_options)?;
    }
    // The JSONL fast paths bypass the stream writers, so they cannot fan out to `-O`.
    let fast_paths_ok = single_object || write_options.additional_outputs.is_empty();

    // ── 3. Run ───────────────────────────────────────────────────────────────
    //
    // FK specs need the whole block in memory to build one `ObjectIndex`
    // (referenced objects may appear after the referencing rows); the others
    // stream. JSONL output from JSONL input (or from an FK block) skips the
    // serial reader/writer for raw-line byte appends — only possible when every
    // block can take it, as the output is then one buffer. See `driver`.
    let bytes_mode = out_format == Format::Jsonl
        && fast_paths_ok
        && steps.iter().all(|step| match step {
            Step::Constant(_) => true,
            Step::Rows(seg) => seg.is_indexed(&plan) || seg.format == Format::Jsonl,
        });

    let input = DriveInput {
        pool: build_rayon_pool(config.workers)?,
        plan: Arc::new(plan),
        steps,
        constants,
        loader,
        workers: config.workers,
        ordered: config.ordered,
        channel_depth: config.channel_depth,
        continue_on_error,
    };

    let stats = if bytes_mode {
        let (stats, text) = driver::drive(input, Sink::Jsonl(String::new())).await?;
        let text = text.unwrap_or_default();
        write_bytes_to_output(&config.out_path, text.as_bytes(), "JSONL output")
            .await
            .with_context(|| format!("writing JSONL output to {}", config.out_path))?;
        stats
    } else {
        // The driver feeds a bounded channel that the writer drains, so memory
        // stays bounded to the channel depth (+ the reorder window in ordered
        // mode, itself bounded by the worker count).
        let (out_tx, out_rx) = mpsc::channel::<Result<Value>>(config.channel_depth);
        let driver = tokio::spawn(driver::drive(input, Sink::Values(out_tx)));
        let out_stream = futures::stream::unfold(out_rx, |mut rx| async move {
            rx.recv().await.map(|result| (result, rx))
        })
        .boxed();
        write_stream_to_output(&config.out_path, out_format, &write_options, out_stream)
            .await
            .with_context(|| format!("writing output to {}", config.out_path))?;
        let (stats, _) = driver.await.context("driver task")??;
        stats
    };
    let DriveStats { rows_in, rows_out } = stats;

    // Constant objects are not input rows, so they must not mask failed rows.
    let rows_failed = rows_in.saturating_sub(rows_out) + constants_failed;
    if rows_failed > 0 {
        anyhow::bail!(
            "{rows_failed} of {} rows failed to transform (see stderr for details)",
            rows_in + constants_attempted
        );
    }
    let rows_out = rows_out + constants_ok;

    // The identifier problems reported up front count toward the exit status.
    if identifier_errors > 0 {
        anyhow::bail!("{identifier_errors} transformation error(s)");
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

// ── Output writing helpers (file vs stdout) ───────────────────────────────────

/// Write a raw byte buffer to the output destination.
///
/// When `out_path` is `"-"` the data is written to stdout; BrokenPipe is
/// treated as normal termination (downstream pipe closed).
async fn write_bytes_to_output(out_path: &str, buf: &[u8], label: &str) -> Result<()> {
    use tokio::io::AsyncWriteExt;
    if out_path == "-" {
        let mut stdout = tokio::io::stdout();
        if let Err(e) = stdout.write_all(buf).await {
            if e.kind() == std::io::ErrorKind::BrokenPipe {
                return Ok(()); // normal: downstream closed
            }
            return Err(e).with_context(|| format!("writing {} to stdout", label));
        }
        if let Err(e) = stdout.flush().await {
            if e.kind() == std::io::ErrorKind::BrokenPipe {
                return Ok(());
            }
            return Err(e).with_context(|| format!("flushing {} to stdout", label));
        }
    } else {
        let file = tokio::fs::File::create(out_path)
            .await
            .with_context(|| format!("creating {} {}", label, out_path))?;
        let mut writer = tokio::io::BufWriter::with_capacity(1 << 20, file);
        writer.write_all(buf).await?;
        writer.flush().await?;
    }
    Ok(())
}

/// Write a [`Value`] stream to the output destination.
///
/// When `out_path` is `"-"` the stream is written to stdout via
/// [`write_all_writer`]; BrokenPipe is treated as normal termination.
async fn write_stream_to_output<S>(
    out_path: &str,
    out_format: Format,
    options: &WriteOptions,
    stream: S,
) -> Result<()>
where
    S: futures::Stream<Item = Result<Value>> + Unpin,
{
    if out_path == "-" {
        match write_all_writer_with(tokio::io::stdout(), out_format, options, stream).await {
            Ok(()) => Ok(()),
            Err(e) => {
                if e.downcast_ref::<std::io::Error>()
                    .map(|io| io.kind() == std::io::ErrorKind::BrokenPipe)
                    .unwrap_or(false)
                {
                    return Ok(());
                }
                Err(e)
            }
        }
    } else {
        write_all_with(out_path, out_format, options, stream).await
    }
}

// ── Helpers shared with the driver ───────────────────────────────────────────

/// Format names as a Python list repr (`['a', 'b']`), as upstream's f-string of
/// a `list[str]` renders them.
fn py_str_list<'a>(items: impl Iterator<Item = &'a String>) -> String {
    let inner = items
        .map(|s| format!("'{s}'"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{inner}]")
}

// ── Schema loader ────────────────────────────────────────────────────────────
//
// `SchemaViewProvider` resolves builtin CURIE maps (`default_curi_maps:
// [semweb_context]`) and builtin `linkml:` import prefixes before `add_schema`
// (see `linkml-map-schemaview`'s `resolve_default_curi_maps`), so schemas that
// used to need an in-memory YAML fallback now load through the strict backend
// directly.

/// `_NUMERIC_TYPE_NAMES` (`data_loaders.py`, `delimited_file_loader.py`). Custom
/// scalar types are numeric only through their `typeof` chain, so a ZIP-code
/// type derived from `string` stays lexical.
fn is_numeric_type_name(name: &str) -> bool {
    matches!(name, "integer" | "float" | "double" | "decimal")
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
    // `yaml.safe_load` (`transformer.py::load_transformer_specification`): YAML 1.1,
    // so an unquoted `yes`, `017`, `1_000` or `2024-01-01` in a `value:` is not a string.
    let mut obj = match linkml_map_io::yaml_load::safe_load_all(&text)
        .with_context(|| format!("parsing transform YAML {}", path.display()))?
        .as_slice()
    {
        [] => serde_json::Value::Null,
        [doc] => serde_json::Value::from(doc),
        _ => anyhow::bail!(
            "parsing transform YAML {}: expected a single document in the stream",
            path.display()
        ),
    };
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
            target_schema_path: Some(
                fixture
                    .join("target/quantity_value_flat.yaml")
                    .to_str()
                    .unwrap()
                    .to_owned(),
            ),
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
            write_options: Default::default(),
            strict: false,
            unrestricted_eval: false,
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

        let plan = CompiledPlan::load(&spec, &schema, Some(&schema), Some("Measurement"))
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

        let err = match CompiledPlan::load(&spec, &schema, Some(&schema), Some("Measurement")) {
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
            target_schema_path: Some(target.to_str().unwrap().to_owned()),
            spec_path: spec.to_str().unwrap().to_owned(),
            out_path: out_path.to_str().unwrap().to_owned(),
            out_format: Some(Format::Jsonl),
            source_class: Some("Measurement".to_owned()),
            workers: 2,
            ordered: true,
            channel_depth: 64,
            write_options: Default::default(),
            strict: false,
            unrestricted_eval: false,
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
            target_schema_path: Some(target.to_str().unwrap().to_owned()),
            spec_path: spec.to_str().unwrap().to_owned(),
            out_path: out_path.to_str().unwrap().to_owned(),
            out_format: Some(Format::Jsonl),
            source_class: Some("Measurement".to_owned()),
            workers: 2,
            ordered: true,
            channel_depth: 64,
            write_options: Default::default(),
            strict: false,
            unrestricted_eval: false,
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
            target_schema_path: Some(
                fixture
                    .join("target/quantity_value_flat.yaml")
                    .to_str()
                    .unwrap()
                    .to_owned(),
            ),
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
            write_options: Default::default(),
            strict: false,
            unrestricted_eval: false,
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
            target_schema_path: Some(
                fixture
                    .join("target/denormalized.yaml")
                    .to_str()
                    .unwrap()
                    .to_owned(),
            ),
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
            write_options: Default::default(),
            strict: false,
            unrestricted_eval: false,
        };

        // Verify the plan detects FK need.
        let plan = CompiledPlan::load(
            std::path::Path::new(&cfg.spec_path),
            std::path::Path::new(&cfg.schema_path),
            cfg.target_schema_path.as_deref().map(std::path::Path::new),
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
                Some(&fixture.join("target/quantity_value_flat.yaml")),
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
