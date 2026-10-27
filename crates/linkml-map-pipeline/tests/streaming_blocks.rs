//! Streaming a directory of tables through the pipeline: `is_a` inheritance
//! between class derivations and constants-only blocks.
//!
//! Ports of upstream `tests/test_transformer/test_streaming_inheritance.py`
//! (commit `cd395ff`) and `tests/test_transformer/test_constant_blocks.py`
//! (commit `1323cc8`). Upstream drives `transform_spec` over every block; the
//! Rust pipeline streams one primary block plus the constants-only blocks (see
//! the PARITY note on `run_pipeline_with_options`), so the multi-block upstream
//! cases are restricted to one row block alongside the constant blocks.

use std::path::Path;

use linkml_map_io::Format;
use linkml_map_pipeline::{PipelineConfig, PipelineStats, run_pipeline_with_options};
use serde_json::{Value, json};

// ── Fixtures ─────────────────────────────────────────────────────────────────

/// `test_streaming_inheritance.py::SOURCE`.
const INHERITANCE_SOURCE: &str = "\
id: https://example.org/s
name: s
prefixes:
  linkml: https://w3id.org/linkml/
imports:
  - linkml:types
default_range: string
classes:
  samples:
    attributes:
      sample_id:
        identifier: true
      site:
  sites:
    attributes:
      site_code:
        identifier: true
      site_name:
";

/// `test_constant_blocks.py::SOURCE`.
const CONSTANT_SOURCE: &str = "\
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
  Study:
    attributes:
      name:
";

/// `test_constant_blocks.py::TARGET`.
const CONSTANT_TARGET: &str = "\
id: https://example.org/t
name: t
prefixes:
  linkml: https://w3id.org/linkml/
imports:
  - linkml:types
default_range: string
classes:
  ResearchStudy:
    attributes:
      id:
        identifier: true
      name:
      project:
        range: Project
        inlined: true
  Project:
    attributes:
      code:
  Participant:
    attributes:
      pid:
        identifier: true
      tag:
  Study:
    attributes:
      id:
      name:
  Base:
    attributes:
      name:
      tag:
";

const STUDY: &str = r#"  ResearchStudy:
    slot_derivations:
      id:
        value: phs000810
      name:
        value: HCHS
"#;

const PARTICIPANT: &str = r#"  Participant:
    populated_from: participants
    slot_derivations:
      pid:
"#;

const PARTICIPANTS_TSV: &str = "pid\tname\nP1\tAda\nP2\tBo\nP3\tCy\n";

/// How a run is configured, beyond the spec and the data directory.
#[derive(Clone)]
struct Run {
    workers: usize,
    ordered: bool,
    channel_depth: usize,
    source_class: Option<String>,
    out_format: Format,
    continue_on_error: bool,
}

impl Default for Run {
    fn default() -> Self {
        Self {
            workers: 4,
            ordered: true,
            channel_depth: 256,
            source_class: None,
            out_format: Format::Jsonl,
            continue_on_error: false,
        }
    }
}

/// Everything a case needs on disk: schemas, a spec and a data directory.
struct Case {
    tmp: tempfile::TempDir,
    source_schema: &'static str,
    target_schema: Option<&'static str>,
}

impl Case {
    fn new(source_schema: &'static str, target_schema: Option<&'static str>) -> Self {
        Self {
            tmp: tempfile::tempdir().unwrap(),
            source_schema,
            target_schema,
        }
    }

    fn data_dir(&self) -> std::path::PathBuf {
        let dir = self.tmp.path().join("data");
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_data(&self, name: &str, content: &str) {
        std::fs::write(self.data_dir().join(name), content).unwrap();
    }

    /// Run `class_derivations_yaml` (the body under `class_derivations:`) over
    /// the data directory and return the parsed JSONL output lines.
    async fn run(&self, class_derivations_yaml: &str, run: &Run) -> anyhow::Result<Vec<Value>> {
        self.run_on(&self.data_dir(), class_derivations_yaml, run)
            .await
            .map(|(rows, _)| rows)
    }

    async fn run_on(
        &self,
        source: &Path,
        class_derivations_yaml: &str,
        run: &Run,
    ) -> anyhow::Result<(Vec<Value>, PipelineStats)> {
        let t = self.tmp.path();
        let schema = t.join("source.yaml");
        std::fs::write(&schema, self.source_schema).unwrap();
        let target = self.target_schema.map(|ts| {
            let p = t.join("target.yaml");
            std::fs::write(&p, ts).unwrap();
            p.to_str().unwrap().to_owned()
        });
        let spec = t.join("spec.yaml");
        std::fs::write(
            &spec,
            format!("id: https://example.org/spec\nclass_derivations:\n{class_derivations_yaml}"),
        )
        .unwrap();
        let ext = match run.out_format {
            Format::Jsonl => "jsonl",
            Format::Json => "json",
            Format::Tsv => "tsv",
            _ => "yaml",
        };
        let out = t.join(format!("out.{ext}"));
        let _ = std::fs::remove_file(&out);

        let config = PipelineConfig {
            source_path: source.to_str().unwrap().to_owned(),
            source_format: None,
            schema_path: schema.to_str().unwrap().to_owned(),
            target_schema_path: target,
            spec_path: spec.to_str().unwrap().to_owned(),
            out_path: out.to_str().unwrap().to_owned(),
            out_format: Some(run.out_format),
            source_class: run.source_class.clone(),
            workers: run.workers,
            ordered: run.ordered,
            channel_depth: run.channel_depth,
        };
        let stats = run_pipeline_with_options(config, run.continue_on_error).await?;
        let text = std::fs::read_to_string(&out).unwrap_or_default();
        let rows = if matches!(run.out_format, Format::Jsonl) {
            text.lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .collect()
        } else {
            vec![]
        };
        Ok((rows, stats))
    }
}

fn inheritance_case() -> Case {
    let case = Case::new(INHERITANCE_SOURCE, None);
    case.write_data("samples.tsv", "sample_id\tsite\nS1\tA\nS2\tB\n");
    case.write_data("sites.tsv", "site_code\tsite_name\nA\tAlpha\nB\tBeta\n");
    case
}

fn constant_case() -> Case {
    let case = Case::new(CONSTANT_SOURCE, Some(CONSTANT_TARGET));
    case.write_data("participants.tsv", PARTICIPANTS_TSV);
    case
}

fn pids() -> Vec<Value> {
    vec![
        json!({"pid": "P1"}),
        json!({"pid": "P2"}),
        json!({"pid": "P3"}),
    ]
}

fn study() -> Value {
    json!({"id": "phs000810", "name": "HCHS"})
}

fn with_study(rest: Vec<Value>) -> Vec<Value> {
    std::iter::once(study()).chain(rest).collect()
}

// ── cd395ff: is_a inheritance when streaming tabular input ──────────────────

const BASE_TAG: &str = r#"  Base:
    slot_derivations:
      tag:
        expr: "'from-base'"
"#;

/// The join declared on the parent / the child's own copy of it. The key
/// columns are named differently, so the join cannot be synthesised from a
/// shared column.
const JOIN_BLOCK: &str = r#"    joins:
      sites:
        source_key: site
        lookup_key: site_code
"#;

const CHILD_SLOTS: &str = r#"    slot_derivations:
      sample_id:
      site:
        expr: "{sites.site_name}"
"#;

/// `test_per_row_path_applies_inherited_slots`.
#[tokio::test]
async fn per_row_path_applies_inherited_slots() {
    let spec = format!(
        "{BASE_TAG}  Sample:
    is_a: Base
    populated_from: samples
    slot_derivations:
      sample_id:
"
    );
    let rows = inheritance_case()
        .run(&spec, &Run::default())
        .await
        .unwrap();
    assert_eq!(
        rows,
        vec![
            json!({"sample_id": "S1", "tag": "from-base"}),
            json!({"sample_id": "S2", "tag": "from-base"}),
        ]
    );
}

/// `test_join_engine_applies_inherited_slots` (Rust has one join path: a block
/// with joins reads its secondary table through the lookup index).
#[tokio::test]
async fn join_block_applies_inherited_slots() {
    let spec = format!(
        "{BASE_TAG}  Sample:
    is_a: Base
    populated_from: samples
{JOIN_BLOCK}{CHILD_SLOTS}"
    );
    let rows = inheritance_case()
        .run(&spec, &Run::default())
        .await
        .unwrap();
    assert_eq!(
        rows,
        vec![
            json!({"sample_id": "S1", "site": "Alpha", "tag": "from-base"}),
            json!({"sample_id": "S2", "site": "Beta", "tag": "from-base"}),
        ]
    );
}

/// `test_joins_inherited_from_parent`: a join declared on the parent applies to
/// the child's slots. Failed at plan build with "cannot be joined: no shared
/// join key could be inferred" before `joins` were inherited.
#[tokio::test]
async fn joins_inherited_from_parent() {
    let spec = format!(
        "  Base:
{JOIN_BLOCK}  Sample:
    is_a: Base
    populated_from: samples
{CHILD_SLOTS}"
    );
    let rows = inheritance_case()
        .run(&spec, &Run::default())
        .await
        .unwrap();
    assert_eq!(
        rows,
        vec![
            json!({"sample_id": "S1", "site": "Alpha"}),
            json!({"sample_id": "S2", "site": "Beta"}),
        ]
    );
}

/// The same join inherited through a mixin, not `is_a`.
#[tokio::test]
async fn joins_inherited_from_mixin() {
    let spec = format!(
        "  Base:
{JOIN_BLOCK}  Sample:
    mixins: [Base]
    populated_from: samples
{CHILD_SLOTS}"
    );
    let rows = inheritance_case()
        .run(&spec, &Run::default())
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["site"], "Alpha");
}

/// Without a parent declaring the join, the un-keyable reference still fails
/// loudly (the ancestor check only suppresses the error when a parent has it).
#[tokio::test]
async fn undeclared_join_still_fails() {
    let spec = format!(
        "  Sample:
    populated_from: samples
{CHILD_SLOTS}"
    );
    let err = inheritance_case()
        .run(&spec, &Run::default())
        .await
        .unwrap_err();
    assert!(
        format!("{err:#}").contains("no shared join key could be inferred"),
        "{err:#}"
    );
}

/// An unknown `is_a` parent is reported, as upstream's `KeyError` is.
#[tokio::test]
async fn unknown_parent_is_reported() {
    let spec = "  Sample:
    is_a: Nope
    populated_from: samples
    slot_derivations:
      sample_id:
";
    let err = inheritance_case()
        .run(spec, &Run::default())
        .await
        .unwrap_err();
    assert!(
        format!("{err:#}").contains("no class derivation for source type 'Nope'"),
        "{err:#}"
    );
}

/// Without `--source-class`, a parent template that has no table is skipped when
/// choosing the streamed block (upstream skips blocks with no data).
#[tokio::test]
async fn parent_without_a_table_is_not_the_primary_block() {
    let spec = format!(
        "{BASE_TAG}  Sample:
    is_a: Base
    populated_from: samples
    slot_derivations:
      sample_id:
"
    );
    let rows = inheritance_case()
        .run(&spec, &Run::default())
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
}

// ── 1323cc8: a constants-only block is emitted once, reading no input ───────

/// `test_constant_block_emits_once`: one constant object, then a row per participant.
#[tokio::test]
async fn constant_block_emits_once() {
    let spec = format!("{STUDY}{PARTICIPANT}");
    let rows = constant_case().run(&spec, &Run::default()).await.unwrap();
    assert_eq!(rows, with_study(pids()));
}

/// `test_nested_constant_block_emits_once`.
#[tokio::test]
async fn nested_constant_block_emits_once() {
    let spec = format!(
        "  ResearchStudy:
    slot_derivations:
      id:
        value: phs000810
      project:
        class_derivations:
          - name: Project
            slot_derivations:
              code:
                value: STUDY
{PARTICIPANT}"
    );
    let rows = constant_case().run(&spec, &Run::default()).await.unwrap();
    assert_eq!(
        rows[0],
        json!({"id": "phs000810", "project": {"code": "STUDY"}})
    );
    assert_eq!(rows.len(), 4);
}

/// `test_only_constant_blocks_need_no_data`: runs against an empty directory.
#[tokio::test]
async fn only_constant_blocks_need_no_data() {
    let case = Case::new(CONSTANT_SOURCE, Some(CONSTANT_TARGET));
    let (rows, stats) = case
        .run_on(&case.data_dir(), STUDY, &Run::default())
        .await
        .unwrap();
    assert_eq!(rows, vec![study()]);
    assert_eq!((stats.rows_in, stats.rows_out), (0, 1));
}

/// A constants-only spec does not even need the input path to exist.
#[tokio::test]
async fn only_constant_blocks_ignore_a_missing_tabular_input() {
    let case = Case::new(CONSTANT_SOURCE, Some(CONSTANT_TARGET));
    let missing = case.tmp.path().join("nope.tsv");
    let (rows, _) = case.run_on(&missing, STUDY, &Run::default()).await.unwrap();
    assert_eq!(rows, vec![study()]);
}

/// `test_constant_block_does_not_hide_missing_input`: same text as upstream's
/// `FileNotFoundError`.
#[tokio::test]
async fn constant_block_does_not_hide_missing_input() {
    let case = Case::new(CONSTANT_SOURCE, Some(CONSTANT_TARGET));
    let spec = format!("{STUDY}{PARTICIPANT}");
    let err = case.run(&spec, &Run::default()).await.unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains(
            "No input data for any class derivation: expected one of ['participants'], found []"
        ),
        "{msg}"
    );
}

/// `test_block_that_reads_a_row_is_not_emitted_once` (the four parametrisations):
/// a block that reads anything from a row still needs a table; with none under
/// its name it is skipped, so only the participant rows come out.
#[tokio::test]
async fn block_that_reads_a_row_is_not_emitted_once() {
    // Upstream drives `transform_spec` directly, which has no identifier
    // pre-check. The Rust pipeline mirrors the CLI, where
    // `cli._require_identifiers` would refuse a `ResearchStudy` block that
    // neither derives nor mints its `id`; `mint_ids: true` satisfies the check
    // without changing what is emitted (the block is skipped either way).
    let cases = [
        // bare-slot
        "  ResearchStudy:
    mint_ids: true
    slot_derivations:
      name:
",
        // expr
        "  ResearchStudy:
    mint_ids: true
    slot_derivations:
      name:
        expr: name
",
        // inherits-row-slot
        "  Base:
    slot_derivations:
      name:
  ResearchStudy:
    is_a: Base
    slot_derivations:
      id:
        value: phs000810
",
        // nested-row-slot
        "  ResearchStudy:
    slot_derivations:
      id:
        value: phs000810
      project:
        class_derivations:
          - name: Project
            slot_derivations:
              code:
                expr: name
",
    ];
    for (i, block) in cases.iter().enumerate() {
        let spec = format!("{block}{PARTICIPANT}");
        let rows = constant_case().run(&spec, &Run::default()).await.unwrap();
        assert_eq!(rows, pids(), "case {i}");
    }
}

/// `test_block_named_after_source_class_maps_every_row`: inference gives a block
/// named after a source class that table, so its constants repeat per row.
///
/// Divergence: upstream also streams the `Participant` block that follows; the
/// Rust pipeline streams only the first row block (the `Study` table here).
#[tokio::test]
async fn block_named_after_source_class_maps_every_row() {
    let case = constant_case();
    case.write_data("Study.tsv", "name\nX\nY\n");
    let spec = STUDY.replace("ResearchStudy", "Study") + PARTICIPANT;
    let rows = case.run(&spec, &Run::default()).await.unwrap();
    assert_eq!(rows, vec![study(), study()]);
}

/// `test_constant_parent_is_not_emitted`: a constant block that another block
/// inherits from is a template, not a record.
#[tokio::test]
async fn constant_parent_is_not_emitted() {
    let spec = "  Base:
    slot_derivations:
      tag:
        value: x
  Participant:
    is_a: Base
    populated_from: participants
    slot_derivations:
      pid:
";
    let rows = constant_case().run(spec, &Run::default()).await.unwrap();
    assert_eq!(
        rows,
        vec![
            json!({"pid": "P1", "tag": "x"}),
            json!({"pid": "P2", "tag": "x"}),
            json!({"pid": "P3", "tag": "x"}),
        ]
    );
}

// ── Rust-specific: exactly once on every path, in spec order ────────────────

/// The constant object appears exactly once whatever the worker count, ordering
/// or channel depth, and is ahead of the rows.
#[tokio::test]
async fn constant_block_once_across_worker_modes() {
    let spec = format!("{STUDY}{PARTICIPANT}");
    let case = constant_case();
    for (workers, ordered, channel_depth) in [
        (1, true, 1),
        (1, false, 1),
        (8, true, 2),
        (8, false, 2),
        (0, false, 256),
    ] {
        let run = Run {
            workers,
            ordered,
            channel_depth,
            ..Run::default()
        };
        let rows = case.run(&spec, &run).await.unwrap();
        assert_eq!(rows[0], study(), "workers={workers} ordered={ordered}");
        let mut pid_rows: Vec<Value> = rows[1..].to_vec();
        pid_rows.sort_by_key(|v| v["pid"].as_str().unwrap().to_owned());
        assert_eq!(
            (rows.len(), pid_rows),
            (4, pids()),
            "workers={workers} ordered={ordered}"
        );
    }
}

/// Upstream emits blocks in spec order: a constant block declared after the row
/// block comes after its rows.
#[tokio::test]
async fn constant_block_declared_after_the_row_block_follows_its_rows() {
    let spec = format!("{PARTICIPANT}{STUDY}");
    let mut expected = pids();
    expected.push(study());
    for ordered in [true, false] {
        let run = Run {
            ordered,
            ..Run::default()
        };
        let mut rows = constant_case().run(&spec, &run).await.unwrap();
        // Unordered mode may shuffle the rows themselves, never the epilogue.
        let n = rows.len() - 1;
        rows[..n].sort_by_key(|v| v["pid"].as_str().map(str::to_owned));
        assert_eq!(rows, expected, "ordered={ordered}");
    }
}

/// Constant blocks before and after the row block bracket it, in spec order.
#[tokio::test]
async fn constant_blocks_bracket_the_row_block() {
    let first = "  Before:
    slot_derivations:
      id:
        value: first
";
    let last = "  After:
    slot_derivations:
      id:
        value: last
";
    let spec = format!("{first}{PARTICIPANT}{last}");
    let rows = constant_case().run(&spec, &Run::default()).await.unwrap();
    assert_eq!(rows.first(), Some(&json!({"id": "first"})));
    assert_eq!(rows.last(), Some(&json!({"id": "last"})));
    assert_eq!(rows.len(), 5);
}

/// JSONL fast path (jsonl in, jsonl out): the constant block is written once,
/// into the same buffer as the rows.
#[tokio::test]
async fn constant_block_once_on_the_jsonl_fast_path() {
    let case = Case::new(CONSTANT_SOURCE, Some(CONSTANT_TARGET));
    case.write_data(
        "participants.jsonl",
        "{\"pid\":\"P1\"}\n{\"pid\":\"P2\"}\n{\"pid\":\"P3\"}\n",
    );
    let spec = format!("{STUDY}{PARTICIPANT}");
    for workers in [1, 8] {
        let run = Run {
            workers,
            ..Run::default()
        };
        let rows = case.run(&spec, &run).await.unwrap();
        assert_eq!(rows, with_study(pids()), "workers={workers}");
    }
    let spec = format!("{PARTICIPANT}{STUDY}");
    let rows = case.run(&spec, &Run::default()).await.unwrap();
    assert_eq!(rows.last(), Some(&study()));
}

/// Non-JSONL output goes through the generic stream writer.
#[tokio::test]
async fn constant_block_once_in_tsv_output() {
    // No target schema: this case is about TSV framing, not identifiers (with
    // the target schema, `ResearchStudy` would fail the identifier pre-check).
    let case = Case::new(CONSTANT_SOURCE, None);
    case.write_data("participants.tsv", PARTICIPANTS_TSV);
    let spec = "  ResearchStudy:
    slot_derivations:
      pid:
        value: STUDY
  Participant:
    populated_from: participants
    slot_derivations:
      pid:
";
    let run = Run {
        out_format: Format::Tsv,
        ..Run::default()
    };
    case.run(spec, &run).await.unwrap();
    let text = std::fs::read_to_string(case.tmp.path().join("out.tsv")).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines, ["pid", "STUDY", "P1", "P2", "P3"]);
}

/// FK mode (buffers every row, builds one `ObjectIndex`): the constant block is
/// still emitted once.
#[tokio::test]
async fn constant_block_once_in_fk_mode() {
    const FK_SOURCE: &str = "\
id: https://example.org/fk
name: fk
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
      site:
        range: sites
  sites:
    attributes:
      site_code:
        identifier: true
      site_name:
";
    let case = Case::new(FK_SOURCE, None);
    case.write_data(
        "participants.jsonl",
        "{\"pid\":\"P1\",\"site\":\"A\"}\n{\"pid\":\"P2\",\"site\":\"B\"}\n",
    );
    let spec = "  ResearchStudy:
    slot_derivations:
      id:
        value: phs000810
  Participant:
    populated_from: participants
    slot_derivations:
      pid:
      tagged:
        expr: pid + \".x\"
";
    for (workers, out_format) in [(1, Format::Jsonl), (8, Format::Jsonl)] {
        let run = Run {
            workers,
            out_format,
            ..Run::default()
        };
        let rows = case.run(spec, &run).await.unwrap();
        assert_eq!(rows[0], json!({"id": "phs000810"}), "workers={workers}");
        assert_eq!(rows.len(), 3, "workers={workers}");
    }
    // After the row block: epilogue in FK mode too.
    let spec = "  Participant:
    populated_from: participants
    slot_derivations:
      pid:
      tagged:
        expr: pid + \".x\"
  ResearchStudy:
    slot_derivations:
      id:
        value: phs000810
";
    let rows = case.run(spec, &Run::default()).await.unwrap();
    assert_eq!(rows.last(), Some(&json!({"id": "phs000810"})));
    assert_eq!(rows.len(), 3);
}

/// The constant block is excluded from `rows_in`, included in `rows_out`.
#[tokio::test]
async fn constant_objects_count_as_output_not_input() {
    let case = constant_case();
    let spec = format!("{STUDY}{PARTICIPANT}");
    let (_, stats) = case
        .run_on(&case.data_dir(), &spec, &Run::default())
        .await
        .unwrap();
    assert_eq!((stats.rows_in, stats.rows_out), (3, 4));
}

/// Single-object input (not tabular, not a directory) is unchanged: upstream
/// takes `_map_data_single`, which never emits constant blocks once.
#[tokio::test]
async fn jsonl_file_input_is_not_a_streaming_source() {
    let case = Case::new(CONSTANT_SOURCE, Some(CONSTANT_TARGET));
    let file = case.tmp.path().join("participants.jsonl");
    std::fs::write(&file, "{\"pid\":\"P1\"}\n{\"pid\":\"P2\"}\n").unwrap();
    let spec = format!("{STUDY}{PARTICIPANT}");
    let run = Run {
        source_class: Some("participants".into()),
        ..Run::default()
    };
    let (rows, _) = case.run_on(&file, &spec, &run).await.unwrap();
    assert_eq!(rows, vec![json!({"pid": "P1"}), json!({"pid": "P2"})]);
}
