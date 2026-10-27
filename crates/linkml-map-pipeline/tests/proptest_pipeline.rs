//! Pipeline determinism: for generated rows over real fixtures, the output bytes are
//! identical across worker counts, channel depths and output formats; equal to running
//! the engine sequentially row by row; and unordered mode yields the same multiset.

mod fixtures;

use std::path::Path;

use fixtures::{Fixture, arb_row};
use linkml_map_core::value::Value;
use linkml_map_io::{Format, WriteOptions, jsonl_line, write_vec};
use linkml_map_pipeline::{CompiledPlan, PipelineConfig, run_pipeline};
use proptest::prelude::*;

fn config(default_cases: u32) -> ProptestConfig {
    let mut c = ProptestConfig::default();
    if std::env::var_os("PROPTEST_CASES").is_none() {
        c.cases = default_cases;
    }
    c.failure_persistence = None;
    c
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime")
}

struct Run {
    workers: usize,
    ordered: bool,
    channel_depth: usize,
}

/// Run the pipeline over `input` (a JSONL file) and return the bytes written.
fn run(
    rt: &tokio::runtime::Runtime,
    fx: &Fixture,
    input: &Path,
    out_format: Format,
    r: &Run,
    out_dir: &Path,
) -> Result<String, String> {
    let out = out_dir.join(format!(
        "out-{}-{}-{}.{}",
        r.workers,
        r.ordered,
        r.channel_depth,
        out_format.extension()
    ));
    let mut cfg = PipelineConfig::new(
        input.to_str().unwrap(),
        fx.source_schema.to_str().unwrap(),
        fx.target_schema
            .as_ref()
            .map(|p| p.to_str().unwrap().to_string()),
        fx.spec.to_str().unwrap(),
        out.to_str().unwrap(),
    );
    cfg.source_format = Some(Format::Jsonl);
    cfg.out_format = Some(out_format);
    cfg.source_class = Some(fx.class.to_string());
    cfg.workers = r.workers;
    cfg.ordered = r.ordered;
    cfg.channel_depth = r.channel_depth;
    cfg.write_options = WriteOptions::default();
    rt.block_on(run_pipeline(cfg))
        .map_err(|e| format!("{e:#}"))?;
    std::fs::read_to_string(&out).map_err(|e| e.to_string())
}

/// Keep the rows the engine accepts one by one: a row that fails fails the whole run,
/// which is not what is being tested here.
fn accepted_rows(fx: &Fixture, rows: Vec<Value>) -> (CompiledPlan, Vec<Value>) {
    let plan = CompiledPlan::load(
        &fx.spec,
        &fx.source_schema,
        fx.target_schema.as_deref(),
        Some(fx.class),
    )
    .unwrap_or_else(|e| panic!("{}: {e:#}", fx.name));
    // The JSONL writer drops null map values, so the rows are what the pipeline will read.
    let rows = rows
        .iter()
        .map(linkml_map_io::pycompat::strip_nulls)
        .filter(|r| plan.transform(r.clone()).is_ok())
        .collect();
    (plan, rows)
}

fn check_fixture(fx: &Fixture, rows: Vec<Value>, out_format: Format) -> Result<(), TestCaseError> {
    let rt = runtime();
    let (plan, rows) = accepted_rows(fx, rows);
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("rows.jsonl");
    rt.block_on(write_vec(&input, Format::Jsonl, rows.clone()))
        .unwrap();

    // Reference: the engine, sequentially, row by row, through the same writer.
    let expected_values: Vec<Value> = rows
        .iter()
        .map(|r| plan.transform(r.clone()).expect("accepted row"))
        .collect();
    let reference_path = dir
        .path()
        .join(format!("reference.{}", out_format.extension()));
    rt.block_on(write_vec(
        &reference_path,
        out_format,
        expected_values.clone(),
    ))
    .unwrap();
    let reference = std::fs::read_to_string(&reference_path).unwrap();
    // FK specs resolve references through an object index the sequential reference lacks.
    let comparable_to_reference = !plan.requires_object_index();

    let mut first: Option<String> = None;
    for workers in [0usize, 1, 2, 8] {
        for channel_depth in [1usize, 256] {
            let r = Run {
                workers,
                ordered: true,
                channel_depth,
            };
            let got = run(&rt, fx, &input, out_format, &r, dir.path())
                .map_err(|e| TestCaseError::fail(format!("{}: pipeline failed: {e}", fx.name)))?;
            match &first {
                None => first = Some(got.clone()),
                Some(f) => prop_assert_eq!(
                    f,
                    &got,
                    "{}: output differs for workers={} depth={}",
                    fx.name,
                    workers,
                    channel_depth
                ),
            }
            if comparable_to_reference {
                prop_assert_eq!(
                    &reference,
                    &got,
                    "{}: pipeline differs from sequential engine (workers={}, depth={})",
                    fx.name,
                    workers,
                    channel_depth
                );
            }
        }
    }

    // Unordered mode: the same multiset of records (compare JSONL lines as a sorted list).
    if out_format == Format::Jsonl {
        let mut want: Vec<String> = first
            .as_deref()
            .unwrap_or("")
            .lines()
            .map(String::from)
            .collect();
        want.sort();
        for workers in [0usize, 2, 8] {
            for channel_depth in [1usize, 256] {
                let r = Run {
                    workers,
                    ordered: false,
                    channel_depth,
                };
                let got = run(&rt, fx, &input, out_format, &r, dir.path()).map_err(|e| {
                    TestCaseError::fail(format!("{}: unordered pipeline failed: {e}", fx.name))
                })?;
                let mut lines: Vec<String> = got.lines().map(String::from).collect();
                lines.sort();
                prop_assert_eq!(&want, &lines, "{}: unordered multiset differs", fx.name);
            }
        }
        // The JSONL reference text is exactly the record lines.
        if comparable_to_reference {
            let text: String = expected_values
                .iter()
                .map(|v| jsonl_line(v) + "\n")
                .collect();
            prop_assert_eq!(text, reference);
        }
    }
    Ok(())
}

fn arb_format() -> impl Strategy<Value = Format> {
    prop::sample::select(vec![
        Format::Jsonl,
        Format::Jsonl,
        Format::Yaml,
        Format::Tsv,
        Format::Json,
    ])
}

proptest! {
    #![proptest_config(config(12))]

    #[test]
    fn measurements_pipeline_is_deterministic(
        rows in prop::collection::vec(arb_row("measurements"), 0..40),
        format in arb_format(),
    ) {
        check_fixture(&fixtures::measurements(), rows, format)?;
    }

    #[test]
    fn personinfo_pipeline_is_deterministic(
        rows in prop::collection::vec(arb_row("personinfo"), 0..30),
        format in arb_format(),
    ) {
        check_fixture(&fixtures::personinfo(), rows, format)?;
    }

    #[test]
    fn flattening_pipeline_is_deterministic(
        rows in prop::collection::vec(arb_row("flattening"), 0..30),
        format in arb_format(),
    ) {
        check_fixture(&fixtures::flattening(), rows, format)?;
    }

    #[test]
    fn type_coercion_pipeline_is_deterministic(
        rows in prop::collection::vec(arb_row("type_coercion"), 0..30),
        format in arb_format(),
    ) {
        check_fixture(&fixtures::type_coercion(), rows, format)?;
    }
}

/// A larger fixed batch, so worker scheduling actually interleaves rows.
#[test]
fn large_batches_stay_ordered() {
    use proptest::strategy::{Strategy, ValueTree};
    let mut runner = proptest::test_runner::TestRunner::deterministic();
    let strat = prop::collection::vec(arb_row("measurements"), 1000..1001);
    let rows = strat.new_tree(&mut runner).unwrap().current();
    check_fixture(&fixtures::measurements(), rows, Format::Jsonl).unwrap();
}
