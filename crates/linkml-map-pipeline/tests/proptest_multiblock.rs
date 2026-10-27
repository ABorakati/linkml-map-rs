//! Multi-block streaming (`transform_spec`): generated specs of several row blocks and
//! constants-only blocks over a directory of tables in mixed formats.
//!
//! Properties: the output is exactly what an independent oracle computes block by block in
//! spec order (blocks whose table is absent are skipped), whatever the worker count and
//! channel depth; unordered mode permutes rows only *within* a block; the other output
//! formats are byte-identical across worker settings; and the stats add up.

use std::path::Path;

use linkml_map_io::Format;
use linkml_map_pipeline::{PipelineConfig, PipelineStats, run_pipeline_with_options};
use proptest::prelude::*;
use serde_json::{Value, json};

const SOURCE: &str = "\
id: https://example.org/s
name: s
prefixes:
  linkml: https://w3id.org/linkml/
imports:
  - linkml:types
default_range: string
classes:
  participants:
    attributes:
      pid:
        identifier: true
      name:
      site:
  sites:
    attributes:
      site_code:
        identifier: true
      site_name:
  visits:
    attributes:
      vid:
        identifier: true
      pid:
      age:
        range: integer
";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Table {
    Participants,
    Sites,
    Visits,
}

impl Table {
    fn stem(self) -> &'static str {
        match self {
            Table::Participants => "participants",
            Table::Sites => "sites",
            Table::Visits => "visits",
        }
    }
    fn columns(self) -> &'static [&'static str] {
        match self {
            Table::Participants => &["pid", "name", "site"],
            Table::Sites => &["site_code", "site_name"],
            Table::Visits => &["vid", "pid", "age"],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fmt {
    Tsv,
    Csv,
    Jsonl,
    Json,
    Yaml,
}

/// A row: one value per column (`age` is an integer, everything else a word).
type Row = Vec<Value>;

#[derive(Debug, Clone)]
struct TableData {
    table: Table,
    fmt: Fmt,
    rows: Vec<Row>,
}

#[derive(Debug, Clone)]
enum Block {
    /// A constants-only block `c<N>`.
    Constant(usize),
    /// A row block over a table.
    Rows(Table),
}

fn word() -> impl Strategy<Value = Value> {
    // Never a YAML/CSV-special word: a letter prefix keeps `no`, `null`, `1e3`, ... out.
    "x[a-z]{1,5}".prop_map(Value::from)
}

fn arb_table(table: Table) -> impl Strategy<Value = Option<TableData>> {
    let cell = move |col: &'static str| -> BoxedStrategy<Value> {
        if col == "age" {
            (0i64..120).prop_map(Value::from).boxed()
        } else {
            word().boxed()
        }
    };
    let row = table.columns().iter().map(|c| cell(c)).collect::<Vec<_>>();
    (
        proptest::option::weighted(0.8, Just(())),
        prop::sample::select(vec![Fmt::Tsv, Fmt::Csv, Fmt::Jsonl, Fmt::Json, Fmt::Yaml]),
        prop::collection::vec(row, 0..8),
    )
        .prop_map(move |(present, fmt, rows)| present.map(|()| TableData { table, fmt, rows }))
}

fn arb_blocks() -> impl Strategy<Value = Vec<Block>> {
    prop::collection::vec(
        prop_oneof![
            1 => Just(Block::Constant(0)),
            2 => Just(Block::Rows(Table::Participants)),
            2 => Just(Block::Rows(Table::Sites)),
            2 => Just(Block::Rows(Table::Visits)),
        ],
        1..7,
    )
    .prop_map(|mut blocks| {
        // Number the constants so every block name and value is distinct.
        let mut n = 0;
        for b in &mut blocks {
            if let Block::Constant(k) = b {
                *k = n;
                n += 1;
            }
        }
        blocks
    })
}

fn render(data: &TableData) -> (String, String) {
    let cols = data.table.columns();
    let name = |ext: &str| format!("{}.{ext}", data.table.stem());
    let as_text = |v: &Value| match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    let objects = || -> Vec<Value> {
        data.rows
            .iter()
            .map(|r| Value::Object(cols.iter().map(|c| c.to_string()).zip(r.clone()).collect()))
            .collect()
    };
    match data.fmt {
        Fmt::Tsv | Fmt::Csv => {
            let sep = if data.fmt == Fmt::Tsv { "\t" } else { "," };
            let mut s = cols.join(sep) + "\n";
            for r in &data.rows {
                s += &r.iter().map(as_text).collect::<Vec<_>>().join(sep);
                s.push('\n');
            }
            (name(if data.fmt == Fmt::Tsv { "tsv" } else { "csv" }), s)
        }
        Fmt::Jsonl => (
            name("jsonl"),
            objects().iter().map(|o| o.to_string() + "\n").collect(),
        ),
        Fmt::Json => (name("json"), Value::Array(objects()).to_string()),
        Fmt::Yaml => (name("yaml"), serde_yaml_ng::to_string(&objects()).unwrap()),
    }
}

fn spec_yaml(blocks: &[Block]) -> String {
    let mut s = String::new();
    for (i, b) in blocks.iter().enumerate() {
        match b {
            Block::Constant(k) => {
                s += &format!("  K{i}:\n    slot_derivations:\n      id:\n        value: c{k}\n")
            }
            Block::Rows(Table::Participants) => {
                s += &format!(
                    "  B{i}:\n    populated_from: participants\n    slot_derivations:\n      pid:\n      name:\n"
                )
            }
            Block::Rows(Table::Sites) => {
                s += &format!(
                    "  B{i}:\n    populated_from: sites\n    slot_derivations:\n      site_code:\n      site_name:\n"
                )
            }
            Block::Rows(Table::Visits) => {
                s += &format!(
                    "  B{i}:\n    populated_from: visits\n    slot_derivations:\n      vid:\n      who:\n        populated_from: pid\n      age:\n"
                )
            }
        }
    }
    s
}

/// The oracle: per block, in spec order, the objects it must emit.
fn expected_blocks(blocks: &[Block], tables: &[TableData]) -> Vec<Vec<Value>> {
    blocks
        .iter()
        .map(|b| match b {
            Block::Constant(k) => vec![json!({"id": format!("c{k}")})],
            Block::Rows(t) => tables
                .iter()
                .find(|d| d.table == *t)
                .map(|d| {
                    d.rows
                        .iter()
                        .map(|r| match t {
                            Table::Participants => json!({"pid": r[0], "name": r[1]}),
                            Table::Sites => json!({"site_code": r[0], "site_name": r[1]}),
                            Table::Visits => json!({"vid": r[0], "who": r[1], "age": r[2]}),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        })
        .collect()
}

struct Setup {
    tmp: tempfile::TempDir,
}

impl Setup {
    fn new(blocks: &[Block], tables: &[TableData]) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let t = tmp.path();
        std::fs::write(t.join("source.yaml"), SOURCE).unwrap();
        std::fs::write(
            t.join("spec.yaml"),
            format!(
                "id: https://example.org/spec\nclass_derivations:\n{}",
                spec_yaml(blocks)
            ),
        )
        .unwrap();
        let data = t.join("data");
        std::fs::create_dir_all(&data).unwrap();
        for d in tables {
            let (name, text) = render(d);
            std::fs::write(data.join(name), text).unwrap();
        }
        Self { tmp }
    }

    fn run(
        &self,
        rt: &tokio::runtime::Runtime,
        workers: usize,
        ordered: bool,
        channel_depth: usize,
        out_format: Format,
    ) -> Result<(String, PipelineStats), String> {
        let t = self.tmp.path();
        let out = t.join(format!("out.{}", out_format.extension()));
        let _ = std::fs::remove_file(&out);
        let mut cfg = PipelineConfig::new(
            t.join("data").to_str().unwrap(),
            t.join("source.yaml").to_str().unwrap(),
            None::<String>,
            t.join("spec.yaml").to_str().unwrap(),
            out.to_str().unwrap(),
        );
        cfg.out_format = Some(out_format);
        cfg.workers = workers;
        cfg.ordered = ordered;
        cfg.channel_depth = channel_depth;
        let stats = rt
            .block_on(run_pipeline_with_options(cfg, false))
            .map_err(|e| format!("{e:#}"))?;
        Ok((read(&out), stats))
    }
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

fn config(default_cases: u32) -> ProptestConfig {
    let mut c = ProptestConfig::default();
    if std::env::var_os("PROPTEST_CASES").is_none() {
        c.cases = default_cases;
    }
    c.failure_persistence = None;
    c
}

fn jsonl(text: &str) -> Vec<Value> {
    text.lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

proptest! {
    #![proptest_config(config(16))]

    #[test]
    fn multi_block_output_matches_the_oracle_in_every_mode(
        blocks in arb_blocks(),
        participants in arb_table(Table::Participants),
        sites in arb_table(Table::Sites),
        visits in arb_table(Table::Visits),
        // Half the time every table shares one format (JSONL then takes the raw-line path).
        uniform in proptest::option::of(prop::sample::select(vec![Fmt::Tsv, Fmt::Csv, Fmt::Jsonl, Fmt::Json, Fmt::Yaml])),
    ) {
        let mut tables: Vec<TableData> = [participants, sites, visits].into_iter().flatten().collect();
        if let Some(fmt) = uniform {
            for t in &mut tables {
                t.fmt = fmt;
            }
        }
        let per_block = expected_blocks(&blocks, &tables);
        let expected: Vec<Value> = per_block.iter().flatten().cloned().collect();
        let rows_in: usize = blocks.iter().map(|b| match b {
            Block::Rows(t) => tables.iter().find(|d| d.table == *t).map_or(0, |d| d.rows.len()),
            Block::Constant(_) => 0,
        }).sum();
        let setup = Setup::new(&blocks, &tables);
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();

        // Row blocks but no file for any of their tables: upstream raises "No input data
        // for any class derivation" (constant blocks cannot hide a missing input).
        let reads_a_table = |b: &Block| matches!(b, Block::Rows(t) if tables.iter().any(|d| d.table == *t));
        if blocks.iter().any(|b| matches!(b, Block::Rows(_))) && !blocks.iter().any(reads_a_table) {
            for workers in [0usize, 4] {
                let err = setup.run(&rt, workers, true, 256, Format::Jsonl).expect_err("no input data");
                prop_assert!(err.contains("No input data for any class derivation"), "{}", err);
            }
            return Ok(());
        }

        // Ordered: exact, for every worker count and channel depth.
        let mut jsonl_text: Option<String> = None;
        for workers in [0usize, 1, 2, 8] {
            for depth in [1usize, 256] {
                let (text, stats) = setup
                    .run(&rt, workers, true, depth, Format::Jsonl)
                    .map_err(|e| TestCaseError::fail(format!("workers={workers} depth={depth}: {e}")))?;
                prop_assert_eq!(&jsonl(&text), &expected, "workers={} depth={}", workers, depth);
                prop_assert_eq!((stats.rows_in, stats.rows_out), (rows_in, expected.len()),
                    "stats workers={} depth={}", workers, depth);
                match &jsonl_text {
                    None => jsonl_text = Some(text),
                    Some(first) => prop_assert_eq!(first, &text, "bytes differ workers={} depth={}", workers, depth),
                }
            }
        }

        // Unordered: rows may permute inside a block, never across a block boundary.
        let mut spans = Vec::new();
        let mut at = 0;
        for b in &per_block {
            spans.push((at, at + b.len()));
            at += b.len();
        }
        let normalise = |mut rows: Vec<Value>| {
            for &(a, b) in &spans {
                rows[a..b].sort_by_key(Value::to_string);
            }
            rows
        };
        for workers in [0usize, 2, 8] {
            for depth in [1usize, 3] {
                let (text, stats) = setup
                    .run(&rt, workers, false, depth, Format::Jsonl)
                    .map_err(|e| TestCaseError::fail(format!("unordered workers={workers} depth={depth}: {e}")))?;
                let got = jsonl(&text);
                prop_assert_eq!(got.len(), expected.len());
                prop_assert_eq!(normalise(got), normalise(expected.clone()), "unordered workers={} depth={}", workers, depth);
                prop_assert_eq!((stats.rows_in, stats.rows_out), (rows_in, expected.len()));
            }
        }

        // Other output formats: byte-identical across worker settings (ordered).
        for format in [Format::Yaml, Format::Tsv, Format::Json] {
            let mut first: Option<String> = None;
            for (workers, depth) in [(0usize, 256usize), (2, 1), (8, 256)] {
                let (text, _) = setup
                    .run(&rt, workers, true, depth, format)
                    .map_err(|e| TestCaseError::fail(format!("{format:?} workers={workers}: {e}")))?;
                match &first {
                    None => first = Some(text),
                    Some(f) => prop_assert_eq!(f, &text, "{:?} differs workers={} depth={}", format, workers, depth),
                }
            }
        }
    }
}
