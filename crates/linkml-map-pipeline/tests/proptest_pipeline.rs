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
    cfg.unrestricted_eval = fx.unrestricted;
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
    let mut plan = CompiledPlan::load(
        &fx.spec,
        &fx.source_schema,
        fx.target_schema.as_deref(),
        Some(fx.class),
    )
    .unwrap_or_else(|e| panic!("{}: {e:#}", fx.name));
    plan.set_unrestricted_eval(fx.unrestricted);
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
                .map(|v| jsonl_line(v).unwrap() + "\n")
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
    fn type_coercion_pipeline_is_deterministic(
        rows in prop::collection::vec(arb_row("type_coercion"), 0..30),
        format in arb_format(),
    ) {
        check_fixture(&fixtures::type_coercion(), rows, format)?;
    }
}

/// Guard against vacuous runs: every fixture accepts its own real row in the mode the tests
/// use, so the generated rows are not all filtered out.
#[test]
fn fixtures_accept_their_real_row() {
    // (`flattening` dereferences foreign keys, so it only works on a whole container: see
    // `flattening_container_pipeline_is_deterministic`.)
    for fx in fixtures::all()
        .into_iter()
        .filter(|f| f.name != "flattening")
    {
        let (_, rows) = accepted_rows(&fx, vec![fx.data_row.clone()]);
        assert_eq!(rows.len(), 1, "{} rejects its own data row", fx.name);
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

/// The flattening fixture resolves foreign keys (`subject.id`), which needs the whole
/// container: the pipeline buffers it, builds one object index and fans the rows out.
/// Output must not depend on the worker count or channel depth, and must equal the
/// hand-computed denormalisation.
fn flattening_case(
    entities: Vec<String>,
    links: Vec<(usize, usize, String)>,
    format: Format,
) -> Result<(), TestCaseError> {
    let fx = fixtures::flattening();
    let rt = runtime();
    let dir = tempfile::tempdir().unwrap();
    let name = |i: usize| format!("E:{i}");
    let container = serde_json::json!({
        "mappings": links.iter().map(|(s, o, p)| serde_json::json!({
            "subject": name(s % entities.len()),
            "object": name(o % entities.len()),
            "predicate": p,
        })).collect::<Vec<_>>(),
        "entities": entities.iter().enumerate()
            .map(|(i, n)| (name(i), serde_json::json!({ "name": n })))
            .collect::<serde_json::Map<_, _>>(),
    });
    let input = dir.path().join("container.json");
    std::fs::write(&input, container.to_string()).unwrap();

    let mut first: Option<String> = None;
    for workers in [0usize, 1, 2, 8] {
        for channel_depth in [1usize, 256] {
            let out = dir.path().join(format!(
                "out-{workers}-{channel_depth}.{}",
                format.extension()
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
            cfg.out_format = Some(format);
            cfg.source_class = Some("MappingSet".into());
            cfg.workers = workers;
            cfg.channel_depth = channel_depth;
            rt.block_on(run_pipeline(cfg)).map_err(|e| {
                TestCaseError::fail(format!("workers={workers} depth={channel_depth}: {e:#}"))
            })?;
            let text = std::fs::read_to_string(&out).unwrap();
            match &first {
                None => first = Some(text),
                Some(f) => prop_assert_eq!(f, &text, "workers={} depth={}", workers, channel_depth),
            }
        }
    }
    // The oracle (JSON output only).
    if format == Format::Json && !links.is_empty() {
        let got: serde_json::Value = serde_json::from_str(first.as_deref().unwrap()).unwrap();
        let want = serde_json::json!({
            "mappings": links.iter().map(|(s, o, p)| {
                let (s, o) = (s % entities.len(), o % entities.len());
                serde_json::json!({
                    "subject_id": name(s), "subject_name": entities[s],
                    "object_id": name(o), "object_name": entities[o],
                    "predicate_id": p,
                })
            }).collect::<Vec<_>>(),
        });
        prop_assert_eq!(got, want);
    }
    Ok(())
}

proptest! {
    #![proptest_config(config(10))]

    #[test]
    fn flattening_container_pipeline_is_deterministic(
        entities in prop::collection::vec("x[a-z]{1,5}", 1..6),
        links in prop::collection::vec((0usize..6, 0usize..6, "P:[0-9]{1,2}"), 0..25),
        format in prop::sample::select(vec![Format::Json, Format::Json, Format::Yaml]),
    ) {
        flattening_case(entities, links, format)?;
    }
}
