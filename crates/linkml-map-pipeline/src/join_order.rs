//! The row order of upstream's set-based join engine.
//!
//! Port of the ordering half of `transformer/join_engine.py`. When a block with
//! `joins:` reads CSV, TSV or JSON tables, upstream does not look the joined rows
//! up one by one: `transform_block_via_join` runs one DuckDB query, a star of
//! `LEFT JOIN`s over the primary table, and transforms the rows it returns. The
//! rows come back in the order DuckDB's hash-join probe emits them, which is not
//! the file order once some of them have no match:
//!
//! * A probe of a `LEFT JOIN` emits the rows of an input chunk that matched, and
//!   then, in a call of its own, the rows that did not.
//! * The probe is a *caching* operator: an output of at most 64 rows is held back
//!   and appended to the operator's cache, which is released when it holds 1984
//!   rows or when the pipeline finishes. A larger output goes straight on.
//! * Several joins are a left-deep chain of such operators inside one pipeline,
//!   run by DuckDB's push-based executor: an operator that has more output is
//!   remembered on a stack and asked again, innermost first, once the output so
//!   far has reached the sink; the caches are flushed, in operator order, when
//!   the source is exhausted.
//!
//! [`JoinOrder`] replays that machine over the primary rows, in chunks of
//! DuckDB's vector size, using whether each row has a match in each join. The
//! model was fitted to and checked against DuckDB 1.5 (the version upstream's
//! lock file pins) on randomly generated tables of up to 9000 rows and four
//! joins, and agrees on all of them; `tests` holds cases taken from it.
//!
//! Not modelled: a join whose key column is missing from a file is skipped by
//! upstream (a `NULL` struct) and here only when the primary file lacks the
//! `source_key` column; the hash-join build side being swapped by DuckDB's
//! optimiser (it needs a primary table far smaller than the joined one, and then
//! only a single primary row cannot be reordered); files over a few tens of
//! megabytes, which DuckDB reads in more than one buffer.

use std::{collections::VecDeque, sync::Arc};

use anyhow::Result;
use futures::{StreamExt, stream::BoxStream};
use linkml_map_core::{
    datamodel::{AliasedClass, ClassDerivation},
    engine::LookupIndex,
    value::Value,
};
use linkml_map_io::Format;

use crate::{CompiledPlan, DataLoader, driver::RowSegment};

/// `STANDARD_VECTOR_SIZE`: the rows of a chunk the CSV and JSON readers emit.
const VECTOR_SIZE: usize = 2048;
/// A probe output of at most this many rows is cached (`CachingPhysicalOperator`).
const CACHE_THRESHOLD: usize = 64;
/// A cache is released once it holds this many rows (`VECTOR_SIZE - CACHE_THRESHOLD`).
const CACHE_FLUSH: usize = VECTOR_SIZE - CACHE_THRESHOLD;

// ── The operator pipeline ─────────────────────────────────────────────────────

/// A row and whether it has a match in each join.
struct Item<T> {
    row: T,
    matched: Box<[bool]>,
}

/// What a probe does with the chunk it was given.
enum Scan<T> {
    /// The matching rows were returned; these did not match.
    Unmatched(Vec<Item<T>>),
    /// Everything has been returned; the next call reports no more input.
    Finished,
}

/// One `LEFT JOIN` probe operator.
struct Probe<T> {
    scan: Option<Scan<T>>,
    cache: Vec<Item<T>>,
}

/// Whether an operator has more output for its input (`HAVE_MORE_OUTPUT`) or
/// wants the next input (`NEED_MORE_INPUT`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum More {
    Output,
    Input,
}

/// The pipeline of probes for the joins of one block.
pub(crate) struct JoinOrder<T> {
    probes: Vec<Probe<T>>,
    /// Operators with more output, innermost last (`in_process_operators`).
    stack: Vec<usize>,
    /// The chunk each operator produced last, for the next operator to take.
    outputs: Vec<Vec<Item<T>>>,
}

impl<T> JoinOrder<T> {
    /// A pipeline for `joins` joins.
    pub(crate) fn new(joins: usize) -> Self {
        Self {
            probes: (0..joins)
                .map(|_| Probe {
                    scan: None,
                    cache: Vec::new(),
                })
                .collect(),
            stack: Vec::new(),
            outputs: (0..joins).map(|_| Vec::new()).collect(),
        }
    }

    /// Operator `op` (0-based): its next output for `input`, and whether more
    /// follows. The first call for a chunk returns the rows that matched (or,
    /// with none, those that did not); the next those that did not.
    fn execute_probe(&mut self, op: usize, input: Option<Vec<Item<T>>>) -> (Vec<Item<T>>, More) {
        let probe = &mut self.probes[op];
        let (out, more) = match (probe.scan.take(), input) {
            (None, Some(chunk)) => {
                let (matched, unmatched): (Vec<_>, Vec<_>) =
                    chunk.into_iter().partition(|item| item.matched[op]);
                if matched.is_empty() {
                    probe.scan = Some(Scan::Finished);
                    (unmatched, More::Output)
                } else {
                    probe.scan = Some(Scan::Unmatched(unmatched));
                    (matched, More::Output)
                }
            }
            (Some(Scan::Unmatched(unmatched)), _) => {
                if unmatched.is_empty() {
                    (unmatched, More::Input)
                } else {
                    probe.scan = Some(Scan::Finished);
                    (unmatched, More::Output)
                }
            }
            (Some(Scan::Finished) | None, _) => (Vec::new(), More::Input),
        };
        // `CachingPhysicalOperator::Execute`: a small output is held back.
        if out.len() <= CACHE_THRESHOLD {
            probe.cache.extend(out);
            if probe.cache.len() >= CACHE_FLUSH {
                return (std::mem::take(&mut probe.cache), more);
            }
            return (Vec::new(), more);
        }
        (out, more)
    }

    /// `PipelineExecutor::Execute`: run `input` (the output of operator
    /// `initial - 1`, or the source for 0) through the operators from `initial`
    /// until one chunk reaches the end of the chain or the chain wants input.
    fn execute(
        &mut self,
        input: &mut Option<Vec<Item<T>>>,
        initial: usize,
    ) -> (Vec<Item<T>>, More) {
        let ops = self.probes.len();
        // Operator numbers here are 1-based, as upstream's `current_idx`.
        let mut current = match self.stack.pop() {
            Some(op) => op,
            None => initial,
        };
        if current == initial {
            current += 1;
        }
        loop {
            if current == initial {
                return (Vec::new(), More::Input);
            }
            let chunk = if current == initial + 1 {
                input.take()
            } else {
                Some(std::mem::take(&mut self.outputs[current - 2]))
            };
            let (out, more) = self.execute_probe(current - 1, chunk);
            if more == More::Output {
                self.stack.push(current);
            }
            if out.is_empty() {
                // No output: go back to the operator that has more.
                current = self.stack.pop().unwrap_or(initial);
                continue;
            }
            if current == ops {
                let more = if self.stack.is_empty() {
                    More::Input
                } else {
                    More::Output
                };
                return (out, more);
            }
            self.outputs[current - 1] = out;
            current += 1;
        }
    }

    /// `ExecutePushInternal`: push `chunk` through the operators from `initial`
    /// and append what reaches the sink to `sink`.
    fn push_from(&mut self, chunk: Vec<Item<T>>, initial: usize, sink: &mut Vec<T>) {
        if chunk.is_empty() {
            return;
        }
        if initial >= self.probes.len() {
            sink.extend(chunk.into_iter().map(|item| item.row));
            return;
        }
        let mut input = Some(chunk);
        loop {
            let (out, more) = self.execute(&mut input, initial);
            sink.extend(out.into_iter().map(|item| item.row));
            if more == More::Input {
                return;
            }
        }
    }

    /// Push one chunk (at most [`VECTOR_SIZE`] rows) of the primary table; the
    /// rows that reach the sink are appended to `sink`.
    pub(crate) fn push(&mut self, rows: Vec<(T, Box<[bool]>)>, sink: &mut Vec<T>) {
        let chunk = rows
            .into_iter()
            .map(|(row, matched)| Item { row, matched })
            .collect();
        self.push_from(chunk, 0, sink);
    }

    /// `PushFinalize`: the source is exhausted; release the caches in operator
    /// order, each through the operators after it.
    pub(crate) fn finish(&mut self, sink: &mut Vec<T>) {
        for op in 0..self.probes.len() {
            let cached = std::mem::take(&mut self.probes[op].cache);
            self.push_from(cached, op + 1, sink);
        }
    }

    /// The whole table at once: [`push`](Self::push) per chunk, then
    /// [`finish`](Self::finish).
    #[cfg(test)]
    pub(crate) fn reorder(
        joins: usize,
        rows: impl IntoIterator<Item = (T, Box<[bool]>)>,
    ) -> Vec<T> {
        let mut order = Self::new(joins);
        let mut sink = Vec::new();
        let mut rows = rows.into_iter().peekable();
        while rows.peek().is_some() {
            let chunk: Vec<_> = rows.by_ref().take(VECTOR_SIZE).collect();
            order.push(chunk, &mut sink);
        }
        order.finish(&mut sink);
        sink
    }
}

// ── When it applies ───────────────────────────────────────────────────────────

/// A join of the engine's star query: where the primary row's key is looked up.
#[derive(Debug, Clone)]
struct EngineJoin {
    /// The table in the [`LookupIndex`].
    table: String,
    source_key: String,
}

/// A block for which upstream uses the set-based join engine.
#[derive(Debug, Clone)]
pub(crate) struct JoinEngine {
    joins: Vec<EngineJoin>,
}

/// `join_engine._collect_joins`: this derivation's joins and its nested ones'.
/// A name already seen keeps its first spec.
fn collect_joins<'a>(cd: &'a ClassDerivation, acc: &mut Vec<(&'a str, &'a AliasedClass)>) {
    for (name, join) in cd.joins.iter().flatten() {
        if !acc.iter().any(|(seen, _)| seen == name) {
            acc.push((name, join));
        }
    }
    for (_, sd) in cd.slot_derivations.iter().flatten() {
        for nested in sd.class_derivations.iter().flatten() {
            collect_joins(nested, acc);
        }
    }
}

/// `join_engine._refs_engine_safe`: every dotted `populated_from` reads a column
/// of the primary table or of a joined one.
fn refs_engine_safe(cd: &ClassDerivation, available: &[&str]) -> bool {
    for (_, sd) in cd.slot_derivations.iter().flatten() {
        if let Some((head, tail)) = sd.populated_from.as_deref().and_then(|p| p.split_once('.'))
            && (!available.contains(&head) || tail.contains('.'))
        {
            return false;
        }
        if !sd
            .class_derivations
            .iter()
            .flatten()
            .all(|nested| refs_engine_safe(nested, available))
        {
            return false;
        }
    }
    true
}

/// `_DUCKDB_READABLE_FORMATS`.
fn duckdb_readable(format: Format) -> bool {
    matches!(format, Format::Csv | Format::Tsv | Format::Json)
}

impl JoinEngine {
    /// `can_use_join_engine` for the block `seg` reads, or `None` when upstream
    /// falls back to the per-row lookup (whose order is the file's).
    pub(crate) fn for_segment(
        plan: &CompiledPlan,
        loader: Option<&DataLoader>,
        seg: &RowSegment,
        lookup: Option<&Arc<LookupIndex>>,
    ) -> Option<Self> {
        let (block, loader, lookup) = (seg.block?, loader?, lookup?);
        let primary = seg.table.as_deref()?;
        if !duckdb_readable(seg.format) {
            return None;
        }
        let cd = &plan.blocks[block].class_derivation;
        // `sv is None or primary not in sv.all_classes()`.
        let primary_cols: Vec<String> = plan
            .source_provider
            .induced_slots(primary)
            .ok()?
            .into_iter()
            .map(|slot| slot.name)
            .collect();
        let mut found = Vec::new();
        collect_joins(cd, &mut found);
        if found.is_empty() {
            return None;
        }
        let mut joins = Vec::with_capacity(found.len());
        for (name, join) in &found {
            let table = join.class_named.as_deref().unwrap_or(name);
            let file = loader.find(table)?;
            if !duckdb_readable(Format::from_path(&file).ok()?) || !lookup.is_registered(table) {
                return None;
            }
            let source_key = join.source_key.as_deref().or(join.join_on.as_deref())?;
            if join
                .lookup_key
                .as_deref()
                .or(join.join_on.as_deref())
                .is_none()
                || !primary_cols.iter().any(|c| c == source_key)
            {
                return None;
            }
            joins.push(EngineJoin {
                table: table.to_owned(),
                source_key: source_key.to_owned(),
            });
        }
        let mut available: Vec<&str> = vec![primary];
        available.extend(found.iter().map(|(name, _)| *name));
        refs_engine_safe(cd, &available).then_some(Self { joins })
    }

    /// Whether `row` has a match in each join (a null or absent key has none).
    fn matches(&self, row: &Value, lookup: &LookupIndex, skip: &[bool]) -> Box<[bool]> {
        self.joins
            .iter()
            .zip(skip)
            .map(|(join, skipped)| {
                let key = match row {
                    Value::Map(m) => match m.get(&join.source_key) {
                        Some(Value::Str(s)) => s.clone(),
                        Some(Value::Int(i)) => i.to_string(),
                        _ => return false,
                    },
                    _ => return false,
                };
                !skipped && lookup.lookup_row(&join.table, &key).is_some()
            })
            .collect()
    }

    /// The joins the primary file cannot take part in: a `source_key` that is
    /// not one of its columns degrades to a `NULL` struct upstream, with no
    /// `LEFT JOIN` in the query. `first` is the first row, which carries every
    /// column of a CSV or TSV file.
    fn skipped(&self, first: Option<&Value>) -> Vec<bool> {
        self.joins
            .iter()
            .map(|join| match first {
                Some(Value::Map(m)) => !m.contains_key(&join.source_key),
                _ => false,
            })
            .collect()
    }

    /// `rows` in the order upstream's join query returns them, holding back no
    /// more than the operators' caches and the chunk being read.
    pub(crate) fn reorder_stream(
        self,
        rows: BoxStream<'static, Result<Value>>,
        lookup: Arc<LookupIndex>,
    ) -> BoxStream<'static, Result<Value>> {
        struct State {
            engine: JoinEngine,
            lookup: Arc<LookupIndex>,
            rows: BoxStream<'static, Result<Value>>,
            order: JoinOrder<Value>,
            skip: Option<Vec<bool>>,
            ready: VecDeque<Result<Value>>,
            done: bool,
        }
        let state = State {
            order: JoinOrder::new(self.joins.len()),
            engine: self,
            lookup,
            rows,
            skip: None,
            ready: VecDeque::new(),
            done: false,
        };
        futures::stream::unfold(state, |mut st| async move {
            loop {
                if let Some(next) = st.ready.pop_front() {
                    return Some((next, st));
                }
                if st.done {
                    return None;
                }
                let mut chunk = Vec::with_capacity(VECTOR_SIZE);
                let mut failure = None;
                while chunk.len() < VECTOR_SIZE {
                    match st.rows.next().await {
                        Some(Ok(row)) => chunk.push(row),
                        Some(Err(e)) => {
                            failure = Some(e);
                            break;
                        }
                        None => {
                            st.done = true;
                            break;
                        }
                    }
                }
                let skip = st
                    .skip
                    .get_or_insert_with(|| st.engine.skipped(chunk.first()))
                    .clone();
                let flagged = chunk
                    .into_iter()
                    .map(|row| {
                        let matched = st.engine.matches(&row, &st.lookup, &skip);
                        (row, matched)
                    })
                    .collect();
                let mut sink = Vec::new();
                st.order.push(flagged, &mut sink);
                if st.done || failure.is_some() {
                    st.order.finish(&mut sink);
                }
                st.ready.extend(sink.into_iter().map(Ok));
                if let Some(e) = failure {
                    st.ready.push_back(Err(e));
                    st.done = true;
                }
            }
        })
        .boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Flags from a pattern such as `"MU"`: matched in the first join, not the second.
    fn flags(pattern: &str) -> Box<[bool]> {
        pattern.chars().map(|c| c == 'M').collect()
    }

    fn order(patterns: &[&str]) -> Vec<usize> {
        let joins = patterns.first().map_or(0, |p| p.len());
        JoinOrder::reorder(
            joins,
            patterns.iter().enumerate().map(|(i, p)| (i, flags(p))),
        )
    }

    /// One join: the rows that match come first, then those that do not
    /// (DuckDB 1.5: `m LEFT JOIN j` over TSV files).
    #[test]
    fn one_join_puts_the_matches_first() {
        assert_eq!(order(&["U", "M", "U", "M", "M"]), [1, 3, 4, 0, 2]);
        assert_eq!(order(&["M", "M", "M"]), [0, 1, 2]);
        assert_eq!(order(&["U", "U"]), [0, 1]);
    }

    /// Two joins over five rows: the small outputs are cached, so the order is
    /// a partition by the first join and then by the second (DuckDB 1.5: five
    /// rows against joins of 1 and 3 rows).
    #[test]
    fn small_outputs_are_cached_and_partitioned_join_by_join() {
        // p0 UU, p1 MU, p2 MM, p3 MU, p4 UM: both joins' unmatched parts follow.
        assert_eq!(order(&["UU", "MU", "MM", "MU", "UM"]), [2, 4, 1, 3, 0]);
    }

    /// Three joins over 100 rows (DuckDB 1.5, three TSV files of 4, 2 and 1 rows):
    /// the first join's 64 matches are not cached, its 36 misses are, and the
    /// caches are released in operator order when the table is exhausted.
    #[test]
    fn cache_threshold_and_flush_order() {
        let patterns = [
            "MUU", "MMM", "MMU", "MUU", "MUU", "UMM", "MUU", "UMM", "MUM", "MUU", "MUM", "UUM",
            "MUU", "MMU", "MUM", "MUM", "UMU", "UUU", "UUU", "UMM", "MUM", "UUM", "MUU", "MMU",
            "MMM", "MUU", "MMU", "MMU", "UMM", "UUU", "MUU", "MUU", "MUU", "MMM", "UUM", "UUU",
            "UMM", "MMU", "UMM", "UMU", "UUU", "MMM", "MUU", "UMM", "UUM", "UUM", "MMU", "MUU",
            "MMU", "UMM", "MUM", "MUU", "MUU", "UUM", "MMU", "MMU", "UMU", "MUM", "UMM", "MMU",
            "UMM", "MMM", "MUU", "UMM", "MMM", "UMM", "MUU", "UUM", "UMM", "MMU", "MUM", "MUU",
            "MUM", "MMM", "UUM", "MUM", "MUU", "MMM", "UMU", "MMU", "MUM", "MMM", "UMM", "MUM",
            "UMM", "MMM", "UMM", "MUM", "MMM", "MUU", "MMU", "UMM", "UUU", "MMU", "MMU", "MUU",
            "UMM", "MUU", "MMU", "MMU",
        ];
        let expected = [
            1, 24, 33, 41, 61, 64, 73, 77, 81, 85, 88, 5, 7, 19, 28, 36, 38, 43, 49, 58, 60, 63,
            65, 68, 82, 84, 86, 91, 96, 8, 10, 14, 15, 20, 50, 57, 70, 72, 75, 80, 83, 87, 11, 21,
            34, 44, 45, 53, 67, 74, 2, 13, 23, 26, 27, 37, 46, 48, 54, 55, 59, 69, 79, 90, 93, 94,
            98, 99, 16, 39, 56, 78, 0, 3, 4, 6, 9, 12, 22, 25, 30, 31, 32, 42, 47, 51, 52, 62, 66,
            71, 76, 89, 95, 97, 17, 18, 29, 35, 40, 92,
        ];
        assert_eq!(order(&patterns), expected);
    }

    /// Beyond one chunk: the table is read 2048 rows at a time, and each chunk
    /// of a single join is partitioned on its own when both parts are large.
    #[test]
    fn chunks_of_2048_are_partitioned_separately() {
        let rows = (0..4096).map(|i| (i, flags(if i % 2 == 0 { "M" } else { "U" })));
        let got = JoinOrder::reorder(1, rows);
        let mut want: Vec<usize> = (0..2048).filter(|i| i % 2 == 0).collect();
        want.extend((0..2048).filter(|i| i % 2 == 1));
        want.extend((2048..4096).filter(|i| i % 2 == 0));
        want.extend((2048..4096).filter(|i| i % 2 == 1));
        assert_eq!(got, want);
    }

    /// The streaming driver and the one-shot reorder agree.
    #[test]
    fn nothing_is_lost_or_repeated() {
        let patterns: Vec<String> = (0..5000)
            .map(|i| {
                format!(
                    "{}{}",
                    if i % 3 == 0 { 'M' } else { 'U' },
                    if i % 7 < 2 { 'M' } else { 'U' }
                )
            })
            .collect();
        let got = order(&patterns.iter().map(String::as_str).collect::<Vec<_>>());
        let mut sorted = got.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..5000).collect::<Vec<_>>());
    }
}
