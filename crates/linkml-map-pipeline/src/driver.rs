//! The multi-block streaming driver.
//!
//! Port of upstream `transformer/engine.py::transform_spec`: every class
//! derivation is a *block*, visited in spec order. A constants-only block yields
//! its one object at its position; every other block streams the rows of its own
//! table through the engine, and is skipped when that table has no data. See
//! [`CompiledPlan::plan_steps`] for the block-to-table matching.
//!
//! Blocks run **one after another**, so the output order is upstream's: spec
//! order, each block's rows in input order (`ordered`) or first-finished-first
//! (`unordered`). Within a block the rows fan out over the rayon workers exactly
//! as a single-block run always did.
//!
//! # Strategies
//!
//! A row block is run by one of four strategies, chosen once per run from the
//! output sink and the spec ([`drive`]):
//!
//! | sink        | spec            | strategy                                          |
//! |-------------|-----------------|---------------------------------------------------|
//! | values      | no FK           | [`stream_segment`]: reader → workers → collector  |
//! | values      | FK (`requires_object_index`) | [`fk_values_segment`]: buffer, index once |
//! | JSONL bytes | FK              | [`fk_jsonl_segment`]                              |
//! | JSONL bytes | no FK, JSONL in | [`jsonl_segment`]: raw lines, parallel serialise  |

use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Instant};

use anyhow::{Context, Result};
use futures::{StreamExt, TryStreamExt};
use linkml_map_core::{
    datamodel::ClassDerivation,
    engine::{LookupIndex, ObjectIndex},
    value::Value,
};
use linkml_map_io::{
    Format, jsonl_line, load_all_single_object, load_all_with_numeric_hints,
    load_stream_single_object, load_stream_with_numeric_hints, parse_numeric,
};
use tokio::sync::mpsc;

use crate::{CompiledPlan, DataLoader, join_order::JoinEngine, join_rows};

// ── Steps ─────────────────────────────────────────────────────────────────────

/// One row block to stream: which derivation, which file, which source type.
#[derive(Debug, Clone)]
pub(crate) struct RowSegment {
    /// Index into [`CompiledPlan::blocks`] of the derivation to apply. `None`
    /// for single-object input (YAML / JSON / JSONL file), which upstream routes
    /// through `_map_data_single` and so looks the derivation up by source class.
    pub block: Option<usize>,
    /// The table the block reads; keys the numeric column hints.
    pub table: Option<String>,
    /// The `source_type` handed to the engine: `source_type or table_name`.
    pub source_type: Option<String>,
    /// The file holding the table.
    pub file: PathBuf,
    pub format: Format,
}

/// Open a segment's rows. Single-object input (`block: None`, a `.yaml` / `.json`
/// file) is read as `cli.py::_map_data_single` does (`yaml.safe_load`, then
/// `json.loads`); a table is read as `DataLoader` does (per-format loaders,
/// schema-aware numeric CSV/TSV columns).
async fn open_stream(
    plan: &CompiledPlan,
    seg: &RowSegment,
) -> Result<futures::stream::BoxStream<'static, Result<Value>>> {
    if seg.block.is_none() {
        load_stream_single_object(&seg.file, seg.format).await
    } else {
        load_stream_with_numeric_hints(
            &seg.file,
            seg.format,
            plan.numeric_column_hints_for_class(seg.table.as_deref()),
        )
        .await
    }
}

/// [`open_stream`], collected.
async fn open_all(plan: &CompiledPlan, seg: &RowSegment) -> Result<Vec<Value>> {
    if seg.block.is_none() {
        load_all_single_object(&seg.file, seg.format).await
    } else {
        load_all_with_numeric_hints(
            &seg.file,
            seg.format,
            plan.numeric_column_hints_for_class(seg.table.as_deref()),
        )
        .await
    }
}

impl RowSegment {
    /// The error context for a failed row, as upstream's `_map_rows` enriches a
    /// `TransformationError` with `row_index` and `class_derivation_name`.
    fn error_context(&self, plan: &CompiledPlan, row: usize) -> String {
        match self.block {
            Some(b) => format!(
                "transform error at row {row}; class_derivation={}",
                plan.blocks[b].class_derivation.name
            ),
            None => format!("transform error at row {row}"),
        }
    }
}

/// One step of the run, in spec order.
#[derive(Debug, Clone)]
pub(crate) enum Step {
    /// A constants-only block: the object built once before the run (index into
    /// [`CompiledPlan::blocks`]).
    Constant(usize),
    /// A block that streams a table.
    Rows(Arc<RowSegment>),
}

/// Objects built by the constants-only blocks, by block index.
#[derive(Debug, Default)]
pub(crate) struct ConstantBlocks {
    pub values: Vec<Option<Value>>,
    /// Blocks whose transform failed under `continue_on_error`.
    pub failed: usize,
}

impl ConstantBlocks {
    /// Objects that will be emitted.
    pub fn emitted(&self) -> usize {
        self.values.iter().flatten().count()
    }

    /// Constant blocks attempted, failed or not.
    pub fn attempted(&self) -> usize {
        self.emitted() + self.failed
    }
}

// ── Sinks ─────────────────────────────────────────────────────────────────────

/// Where the driver puts what it produces.
pub(crate) enum Sink {
    /// Values for a stream writer, through a bounded channel (backpressure).
    Values(mpsc::Sender<Result<Value>>),
    /// JSONL text accumulated for one bulk write (the raw-byte fast paths).
    Jsonl(String),
}

impl Sink {
    /// Emit one value. `false` when the writer has gone away.
    async fn push(&mut self, value: Value) -> bool {
        match self {
            Sink::Values(tx) => tx.send(Ok(value)).await.is_ok(),
            Sink::Jsonl(buf) => {
                match jsonl_line(&value) {
                    Ok(line) => {
                        buf.push_str(&line);
                        buf.push('\n');
                    }
                    // Not JSON serialisable (a set): the row is dropped with the
                    // error logged, as a row that fails to transform is.
                    Err(e) => eprintln!("cannot write the row as JSON: {e}"),
                }
                true
            }
        }
    }

    /// End the run with a fatal error: in-band for a stream writer (which then
    /// fails with it), returned for the bulk-write sink.
    async fn fail(self, error: anyhow::Error, stats: DriveStats) -> Result<Driven> {
        match self {
            Sink::Values(tx) => {
                let _ = tx.send(Err(error)).await;
                Ok((stats, None))
            }
            Sink::Jsonl(_) => Err(error),
        }
    }

    /// Finish the run, closing a values channel (which ends the writer's stream)
    /// and handing back the accumulated JSONL text, if that is the sink.
    fn finish(self, stats: DriveStats) -> Driven {
        match self {
            Sink::Values(_) => (stats, None),
            Sink::Jsonl(buf) => (stats, Some(buf)),
        }
    }
}

/// What [`drive`] returns: the row counts, and the JSONL text for the bulk sink.
pub(crate) type Driven = (DriveStats, Option<String>);

/// Row counts of a driven run. Constant objects are not input rows and are
/// counted by [`ConstantBlocks`].
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct DriveStats {
    pub rows_in: usize,
    pub rows_out: usize,
}

/// What one row block did.
#[derive(Debug, Default)]
struct SegmentStats {
    rows_in: usize,
    rows_out: usize,
    /// The writer went away: stop.
    closed: bool,
}

/// Everything the driver needs that is not the sink.
pub(crate) struct DriveInput {
    pub plan: Arc<CompiledPlan>,
    pub steps: Vec<Step>,
    pub constants: ConstantBlocks,
    /// Directory mode: where the join tables are.
    pub loader: Option<DataLoader>,
    pub workers: usize,
    pub ordered: bool,
    pub channel_depth: usize,
    pub continue_on_error: bool,
    pub pool: Arc<Option<rayon::ThreadPool>>,
}

// ── Join tables ───────────────────────────────────────────────────────────────

/// Collect all join specs from a class derivation and its nested descendants:
/// `{table: lookup_key}`. Mirrors upstream `engine._collect_all_joins`, which
/// walks the slot-derivation tree for joins declared or synthesised during
/// normalisation.
///
/// The key is the name the engine looks the table up under at runtime
/// (`class_named` if set, else the join alias — the engine's
/// `ac.class_named.unwrap_or(alias)` resolution), which is also the file stem
/// used to find its data.
///
/// # Errors
/// As upstream's `ValueError`: a join with neither `join_on` nor both of
/// `source_key` / `lookup_key`, or two joins of one table with different lookup
/// keys.
pub(crate) fn collect_all_joins(class_deriv: &ClassDerivation) -> Result<BTreeMap<String, String>> {
    fn collect_from(cd: &ClassDerivation, out: &mut BTreeMap<String, String>) -> Result<()> {
        for (join_name, ac) in cd.joins.iter().flatten() {
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
        for (_, sd) in cd.slot_derivations.iter().flatten() {
            for nested in sd.class_derivations.iter().flatten() {
                collect_from(nested, out)?;
            }
        }
        Ok(())
    }

    let mut out = BTreeMap::new();
    collect_from(class_deriv, &mut out)?;
    Ok(out)
}

/// The last block's joins and the index built for them.
type JoinCache = (BTreeMap<String, String>, bool, Option<Arc<LookupIndex>>);

/// The join tables a block needs, loaded into one shared [`LookupIndex`].
///
/// Mirrors the registration in `transform_spec`: every table of
/// [`collect_all_joins`] that has data is registered, and a join whose table has
/// none is skipped: a reference then fails for a row with a join key value (the engine
/// raises, as DuckDB does upstream) and is null for one without. A YAML table cannot be
/// registered (upstream's `LookupIndex` reads CSV, TSV and JSON). Upstream registers them for
/// the block and drops them when it is done; here the index lives until the next
/// block needs a different one, so consecutive blocks with the same joins share
/// it. `None` when the block joins nothing.
async fn lookup_index_for(
    plan: &CompiledPlan,
    loader: Option<&DataLoader>,
    seg: &RowSegment,
    cache: &mut Option<JoinCache>,
) -> Result<Option<Arc<LookupIndex>>> {
    let (Some(block), Some(loader)) = (seg.block, loader) else {
        // PARITY: a table read from a single file has no other table to join, but
        // `transform_spec` still gives its block an (empty) `LookupIndex`.
        if let Some(block) = seg.block
            && plan.lookup_index.is_none()
            && !collect_all_joins(&plan.blocks[block].class_derivation)?.is_empty()
        {
            return Ok(Some(Arc::new(LookupIndex::new())));
        }
        return Ok(plan.lookup_index.clone());
    };
    let joins = collect_all_joins(&plan.blocks[block].class_derivation)?;
    // Whether upstream runs this block's joins in the set-based engine (see `join_rows`).
    let engine = matches!(seg.format, Format::Csv | Format::Tsv | Format::Json);
    if let Some((key, engine_key, index)) = cache
        && *key == joins
        && *engine_key == engine
    {
        return Ok(index.clone());
    }
    let mut index = LookupIndex::new();
    for (table, lookup_key) in &joins {
        // PARITY: engine.py::transform_spec — `if join_name in data_loader`.
        let Some(file) = loader.find(table) else {
            continue;
        };
        let fmt = Format::from_path(&file)
            .with_context(|| format!("detecting format of joined table file {}", file.display()))?;
        // PARITY: `lookup_index._duckdb_read_expr` reads CSV, TSV and JSON only and raises
        // `NotImplementedError("LookupIndex does not yet support 'yaml' files")` for a YAML
        // table, when the block registers its joins (so even with no rows to look up).
        // JSONL tables are a Rust extension (upstream's loader never finds them).
        if fmt == Format::Yaml {
            anyhow::bail!("LookupIndex does not yet support 'yaml' files");
        }
        // PARITY: `LookupIndex` reads a CSV/TSV table as all-varchar text and runs
        // `_parse_numeric` over *every* cell of a returned row, whatever the schema
        // says about the column (`"007"` reads as 7); the key itself is matched as
        // text. A JSON table keeps the types the loader gives it.
        if matches!(fmt, Format::Csv | Format::Tsv) {
            let rows = load_all_with_numeric_hints(&file, fmt, Default::default())
                .await
                .with_context(|| {
                    format!("loading joined table {table:?} from {}", file.display())
                })?;
            let coerce = |cell: &Value| match cell {
                Value::Str(text) => parse_numeric(text),
                other => other.clone(),
            };
            if engine {
                // PARITY: the engine joins on the keys' text, but the primary row it hands
                // the transformer has had `_parse_numeric` run over its key cell.
                index.register_table_with_parsed_keys(table, &rows, lookup_key, coerce);
            } else {
                index.register_table_with(table, &rows, lookup_key, coerce);
            }
            continue;
        }
        let rows = load_all_with_numeric_hints(
            &file,
            fmt,
            plan.numeric_column_hints_for_class(Some(table)),
        )
        .await
        .with_context(|| format!("loading joined table {table:?} from {}", file.display()))?;
        // PARITY: `LookupIndex` and the join engine read a JSON table with every column cast
        // to `VARCHAR` and run `_parse_numeric` over each cell (see `join_rows`).
        let rows = if fmt == Format::Json {
            join_rows::json_rows(rows)
        } else {
            rows
        };
        index.register_table(table, &rows, lookup_key);
    }
    // PARITY: `transform_spec` creates the `LookupIndex` for any block that is not run by the
    // join engine, so a join whose table has no file reaches DuckDB (a `CatalogException`)
    // rather than finding no index at all (a `ValueError`).
    let index = (!joins.is_empty()).then(|| Arc::new(index));
    *cache = Some((joins, engine, index.clone()));
    Ok(index)
}

// ── The driver ────────────────────────────────────────────────────────────────

/// Run every step in order into `sink`. Mirrors `transform_spec`'s
/// `for class_deriv, constant in blocks`.
///
/// A fatal error ends the run: with a values sink it is sent through the channel
/// (the writer fails with it) and the stats so far are returned; with the
/// JSONL-bytes sink it is returned.
pub(crate) async fn drive(mut input: DriveInput, mut sink: Sink) -> Result<Driven> {
    let mut stats = DriveStats::default();
    let mut join_cache = None;
    let fk = input.plan.requires_object_index();
    let steps = std::mem::take(&mut input.steps);
    for step in steps {
        match step {
            Step::Constant(block) => {
                if let Some(value) = input.constants.values[block].take()
                    && !sink.push(value).await
                {
                    break;
                }
            }
            Step::Rows(seg) => {
                let lookup = match lookup_index_for(
                    &input.plan,
                    input.loader.as_ref(),
                    &seg,
                    &mut join_cache,
                )
                .await
                {
                    Ok(lookup) => lookup,
                    Err(e) => return sink.fail(e, stats).await,
                };
                let done = match (&sink, fk) {
                    (Sink::Values(tx), false) => stream_segment(&input, &seg, lookup, tx).await,
                    (Sink::Values(_), true) => {
                        fk_values_segment(&input, &seg, lookup, &mut sink).await
                    }
                    (Sink::Jsonl(_), true) => {
                        fk_jsonl_segment(&input, &seg, lookup, &mut sink).await
                    }
                    (Sink::Jsonl(_), false) => jsonl_segment(&input, &seg, lookup, &mut sink).await,
                };
                match done {
                    Ok(s) => {
                        stats.rows_in += s.rows_in;
                        stats.rows_out += s.rows_out;
                        if s.closed {
                            break;
                        }
                    }
                    Err(e) => return sink.fail(e, stats).await,
                }
            }
        }
    }
    Ok(sink.finish(stats))
}

// ── Strategy: streaming (reader → workers → collector) ───────────────────────

/// Stream one table through the rayon workers into the values channel.
///
/// Reader → bounded channel → `spawn_blocking` workers (capped by a semaphore)
/// → results channel → collector, which restores input order when `ordered` and
/// forwards to `out_tx`. Memory stays bounded by the channel depth plus the
/// reorder window (at most one entry per in-flight worker).
async fn stream_segment(
    input: &DriveInput,
    seg: &Arc<RowSegment>,
    lookup: Option<Arc<LookupIndex>>,
    out_tx: &mpsc::Sender<Result<Value>>,
) -> Result<SegmentStats> {
    let plan = &input.plan;
    // PARITY: a block with joins over CSV/TSV/JSON tables is run by upstream's set-based
    // join engine, which reads the primary table through DuckDB (see `join_rows`) and whose
    // query returns the primary rows in DuckDB's probe order.
    let engine = JoinEngine::for_segment(plan, input.loader.as_ref(), seg, lookup.as_ref());
    let mut source_stream = match &engine {
        Some(engine) => engine.primary_stream(seg).await,
        None => open_stream(plan, seg).await,
    }
    .with_context(|| format!("opening source stream {}", seg.file.display()))?;
    if let (Some(engine), Some(index)) = (engine, lookup.as_ref()) {
        source_stream = engine.reorder_stream(source_stream, Arc::clone(index));
    }

    let (work_tx, mut work_rx) = mpsc::channel::<(usize, Value)>(input.channel_depth);
    let (res_tx, mut res_rx) = mpsc::channel::<(usize, Result<Value>)>(input.channel_depth);

    // Reader: feeds (row index, row) into the work channel.
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
                    eprintln!("reader error at row {seq}: {e:#}");
                    break;
                }
            }
        }
        seq // total rows read
    });

    // Dispatcher: hands each row to the blocking pool. In-flight workers are
    // capped by a semaphore so a slow transform cannot make it spawn an
    // unbounded number of tasks; the cap also bounds the reorder buffer. Each
    // worker owns a `res_tx` clone, so the results channel closes once the last
    // in-flight worker finishes.
    let res_tx2 = res_tx.clone();
    let plan2 = Arc::clone(plan);
    let seg2 = Arc::clone(seg);
    let max_inflight = if input.workers > 0 {
        input.workers
    } else {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(8)
    };
    let sem = Arc::new(tokio::sync::Semaphore::new(max_inflight));
    let dispatcher_handle = tokio::spawn(async move {
        while let Some((seq, row)) = work_rx.recv().await {
            let plan3 = Arc::clone(&plan2);
            let seg3 = Arc::clone(&seg2);
            let lookup3 = lookup.clone();
            let res_tx3 = res_tx2.clone();
            // Back-pressure the dispatcher when all worker slots are busy.
            let permit = Arc::clone(&sem).acquire_owned().await.unwrap();
            tokio::task::spawn_blocking(move || {
                let result = plan3.transform_segment(&seg3, row, lookup3.as_ref());
                // Best-effort send; if the receiver dropped, ignore.
                let _ = res_tx3.blocking_send((seq, result));
                drop(permit); // free the slot when this row is done
            });
        }
        drop(res_tx2);
    });
    drop(res_tx);

    // Collector: reports row errors against this block's row index and forwards
    // finished rows. Without `--continue-on-error` the first error is fatal.
    let ordered = input.ordered;
    let continue_on_error = input.continue_on_error;
    let out_tx = out_tx.clone();
    let seg_c = Arc::clone(seg);
    let plan_c = Arc::clone(plan);
    let collector = tokio::spawn(async move {
        let mut count = 0usize;
        let mut fatal = None;
        let mut closed = false;
        if ordered {
            let mut pending: BTreeMap<usize, Result<Value>> = BTreeMap::new();
            let mut next_seq = 0usize;
            'recv: while let Some((seq, result)) = res_rx.recv().await {
                pending.insert(seq, result);
                // Drain consecutive completed rows (errors advance past gaps).
                while let Some(result) = pending.remove(&next_seq) {
                    next_seq += 1;
                    match result {
                        Ok(v) => {
                            if out_tx.send(Ok(v)).await.is_err() {
                                closed = true;
                                break 'recv;
                            }
                            count += 1;
                        }
                        Err(e) => {
                            let ctx = seg_c.error_context(&plan_c, next_seq - 1);
                            if continue_on_error {
                                eprintln!("{ctx}: {e:#}");
                            } else {
                                fatal = Some(e.context(ctx));
                                break 'recv;
                            }
                        }
                    }
                }
            }
        } else {
            // Unordered: forward as they arrive (maximum throughput).
            while let Some((seq, result)) = res_rx.recv().await {
                match result {
                    Ok(v) => {
                        if out_tx.send(Ok(v)).await.is_err() {
                            closed = true;
                            break;
                        }
                        count += 1;
                    }
                    Err(e) => {
                        let ctx = seg_c.error_context(&plan_c, seq);
                        if continue_on_error {
                            eprintln!("{ctx}: {e:#}");
                        } else {
                            fatal = Some(e.context(ctx));
                            break;
                        }
                    }
                }
            }
        }
        (count, fatal, closed)
    });

    let (rows_out, fatal, closed) = collector.await.context("collector task")?;
    if let Some(e) = fatal {
        reader_handle.abort();
        dispatcher_handle.abort();
        return Err(e);
    }
    let rows_in = reader_handle.await.unwrap_or(0);
    let _ = dispatcher_handle.await;
    Ok(SegmentStats {
        rows_in,
        rows_out,
        closed,
    })
}

// ── Strategy: FK (buffer every row, one shared ObjectIndex) ──────────────────

/// What an FK block needs once its rows are buffered.
struct FkBlock {
    rows: Vec<Value>,
    index: Arc<ObjectIndex>,
}

/// FK resolution is NOT row-streamable: the referenced collection may appear
/// after the referencing rows, so every row of the block is buffered (memory
/// O(N)) and ONE [`ObjectIndex`] is built over them, then shared by every
/// parallel worker. For a single container object the index is built from it;
/// for several rows from a synthetic container holding them all, which lets the
/// index find referenced objects inlined within individual rows.
async fn load_fk_block(
    input: &DriveInput,
    seg: &RowSegment,
    lookup: Option<&Arc<LookupIndex>>,
) -> Result<FkBlock> {
    let plan = &input.plan;
    let rows = open_all(plan, seg)
        .await
        .with_context(|| format!("loading source file for FK mode: {}", seg.file.display()))?;

    let container_val = if rows.len() == 1 {
        rows[0].clone()
    } else {
        // serde_json round-trip builds the Map without a direct indexmap dependency.
        let json_list: serde_json::Value =
            serde_json::to_value(&rows).unwrap_or(serde_json::Value::Array(vec![]));
        let json_container = serde_json::json!({ "items": json_list });
        serde_json::from_value::<Value>(json_container).unwrap_or(Value::Null)
    };
    let index = Arc::new(ObjectIndex::build(
        &container_val,
        seg.source_type.as_deref(),
        Some(plan.source_provider.as_ref()),
    ));
    // PARITY: as in `stream_segment`, upstream's set-based join engine returns the rows of a
    // block with joins in DuckDB's probe order, whether or not a foreign key is also followed.
    let rows = match (
        JoinEngine::for_segment(plan, input.loader.as_ref(), seg, lookup),
        lookup,
    ) {
        (Some(engine), Some(lookup)) => {
            let rows = engine.primary_stream(seg).await?;
            engine
                .reorder_stream(rows, Arc::clone(lookup))
                .try_collect()
                .await?
        }
        _ => rows,
    };
    Ok(FkBlock { rows, index })
}

/// Run `work` on the sized rayon pool when there is one, else the global pool.
fn install<R: Send>(pool: &Option<rayon::ThreadPool>, work: impl FnOnce() -> R + Send) -> R {
    match pool {
        Some(p) => p.install(work),
        None => work(),
    }
}

/// FK block into a values sink: parallel transform into a `Vec`, then forwarded
/// in input order. A failed row is reported and dropped (counted as failed).
async fn fk_values_segment(
    input: &DriveInput,
    seg: &Arc<RowSegment>,
    lookup: Option<Arc<LookupIndex>>,
    sink: &mut Sink,
) -> Result<SegmentStats> {
    let FkBlock { rows, index } = load_fk_block(input, seg, lookup.as_ref()).await?;
    let rows_in = rows.len();
    let (plan, seg2, pool) = (
        Arc::clone(&input.plan),
        Arc::clone(seg),
        Arc::clone(&input.pool),
    );
    let results: Vec<Result<Value>> = tokio::task::spawn_blocking(move || {
        use rayon::prelude::*;
        install(&pool, || {
            rows.par_iter()
                .map(|row| plan.transform_segment_with_index(&seg2, row, &index, lookup.as_ref()))
                .collect()
        })
    })
    .await
    .context("FK transform task")?;

    let mut rows_out = 0;
    for (i, result) in results.into_iter().enumerate() {
        match result {
            Ok(v) => {
                if !sink.push(v).await {
                    return Ok(SegmentStats {
                        rows_in,
                        rows_out,
                        closed: true,
                    });
                }
                rows_out += 1;
            }
            Err(e) => eprintln!("{}: {e:#}", seg.error_context(&input.plan, i)),
        }
    }
    Ok(SegmentStats {
        rows_in,
        rows_out,
        closed: false,
    })
}

/// FK block into the JSONL-bytes sink: transform AND serialise in parallel, then
/// one bulk append (serial per-row serialisation was the writer wall).
async fn fk_jsonl_segment(
    input: &DriveInput,
    seg: &Arc<RowSegment>,
    lookup: Option<Arc<LookupIndex>>,
    sink: &mut Sink,
) -> Result<SegmentStats> {
    let Sink::Jsonl(buf) = sink else {
        anyhow::bail!("internal error: JSONL strategy without a JSONL sink");
    };
    let FkBlock { rows, index } = load_fk_block(input, seg, lookup.as_ref()).await?;
    let rows_in = rows.len();
    let (plan, seg2, pool) = (
        Arc::clone(&input.plan),
        Arc::clone(seg),
        Arc::clone(&input.pool),
    );
    let mut out = std::mem::take(buf);
    let (out, rows_out) = tokio::task::spawn_blocking(move || {
        use rayon::prelude::*;
        let outs: Vec<Option<String>> = install(&pool, || {
            rows.par_iter()
                .enumerate()
                .map(|(i, row)| {
                    match plan.transform_segment_with_index(&seg2, row, &index, lookup.as_ref()) {
                        Ok(v) => jsonl_line(&v).map_err(|e| eprintln!("{e}")).ok(),
                        Err(e) => {
                            eprintln!("{}: {e:#}", seg2.error_context(&plan, i));
                            None
                        }
                    }
                })
                .collect()
        });
        let mut n = 0usize;
        out.reserve(outs.len() * 64);
        for o in outs.into_iter().flatten() {
            out.push_str(&o);
            out.push('\n');
            n += 1;
        }
        (out, n)
    })
    .await
    .context("FK transform task")?;
    *buf = out;
    Ok(SegmentStats {
        rows_in,
        rows_out,
        closed: false,
    })
}

// ── Strategy: JSONL in, JSONL out (raw lines, parallel parse + serialise) ────

/// The generic streaming path parses every record in the serial reader and
/// serialises every record in the serial writer, making tiny-per-row JSONL jobs
/// I/O/serde-bound. Here the whole file is read at once, workers parse +
/// transform + serialise in parallel, and the output is appended as raw bytes.
///
/// ponytail: reads the whole file into memory (O(file_size)). For inputs larger
/// than RAM, switch to a block-streaming reader (fixed-size byte blocks, lines
/// split across block boundaries) — not needed for typical ETL.
async fn jsonl_segment(
    input: &DriveInput,
    seg: &Arc<RowSegment>,
    lookup: Option<Arc<LookupIndex>>,
    sink: &mut Sink,
) -> Result<SegmentStats> {
    let Sink::Jsonl(buf) = sink else {
        anyhow::bail!("internal error: JSONL strategy without a JSONL sink");
    };
    let dbg = std::env::var("PIPELINE_PHASE_TIMING").is_ok();
    let t0 = Instant::now();
    let data = tokio::fs::read_to_string(&seg.file)
        .await
        .with_context(|| format!("reading JSONL source {}", seg.file.display()))?;
    let t_read = t0.elapsed();

    let t1 = Instant::now();
    let (plan, seg2, pool) = (
        Arc::clone(&input.plan),
        Arc::clone(seg),
        Arc::clone(&input.pool),
    );
    let mut out = std::mem::take(buf);
    let (out, rows_in, rows_out) = tokio::task::spawn_blocking(move || {
        use rayon::prelude::*;
        let lines: Vec<&str> = data.lines().filter(|l| !l.trim().is_empty()).collect();
        let n_in = lines.len();
        let outs: Vec<Option<String>> = install(&pool, || {
            lines
                .par_iter()
                .enumerate()
                .map(|(i, l)| parse_transform_serialize(&plan, &seg2, lookup.as_ref(), i, l))
                .collect()
        });
        out.reserve(outs.len() * 64);
        let mut n_out = 0usize;
        for o in outs.into_iter().flatten() {
            out.push_str(&o);
            out.push('\n');
            n_out += 1;
        }
        (out, n_in, n_out)
    })
    .await
    .context("transform task")?;
    *buf = out;
    if dbg {
        eprintln!(
            "[phase] read={:.3}s transform+serialise={:.3}s (workers={})",
            t_read.as_secs_f64(),
            t1.elapsed().as_secs_f64(),
            input.workers
        );
    }
    Ok(SegmentStats {
        rows_in,
        rows_out,
        closed: false,
    })
}

/// Parse one JSONL line → transform → serialise back to a JSON string.
/// Returns `None` (dropping the row, with a logged error) on any failure.
fn parse_transform_serialize(
    plan: &CompiledPlan,
    seg: &RowSegment,
    lookup: Option<&Arc<LookupIndex>>,
    row: usize,
    line: &str,
) -> Option<String> {
    let v: Value = match linkml_map_core::value::parse_json(line) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("JSONL parse error at row {row}: {e}");
            return None;
        }
    };
    match plan.transform_segment(seg, v, lookup) {
        Ok(out) => jsonl_line(&out).map_err(|e| eprintln!("{e}")).ok(),
        Err(e) => {
            eprintln!("{}: {e:#}", seg.error_context(plan, row));
            None
        }
    }
}
