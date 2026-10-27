//! Streaming a directory of tables through the pipeline: `is_a` inheritance
//! between class derivations and constants-only blocks.
//!
//! Ports of upstream `tests/test_transformer/test_streaming_inheritance.py`
//! (commit `cd395ff`), `tests/test_transformer/test_constant_blocks.py`
//! (commit `1323cc8`) and the `transform_spec` cases of
//! `tests/test_cli/test_cli_tabular.py`. Like upstream's `transform_spec`, the
//! pipeline visits every class derivation (block) in spec order; the second half
//! of this file covers that driver. Expected outputs marked "oracle" were
//! produced by running upstream linkml-map v0.6.0-rc1 on the same inputs.

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

/// `test_constant_blocks.py::TARGET`, plus the `Before` / `After` classes of the local bracket test
/// (upstream looks every derived slot up on the target class, so each block's class must exist).
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
  Before:
    attributes:
      id:
  After:
    attributes:
      id:
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
            strict: false,
            unrestricted_eval: false,
            write_options: Default::default(),
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

/// A parent template that has no table is skipped, not streamed as a table
/// (upstream skips blocks with no data).
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
/// named after a source class that table, so its constants repeat per row; the
/// `Participant` block that follows streams its table too.
#[tokio::test]
async fn block_named_after_source_class_maps_every_row() {
    let case = constant_case();
    case.write_data("Study.tsv", "name\nX\nY\n");
    let spec = STUDY.replace("ResearchStudy", "Study") + PARTICIPANT;
    let rows = case.run(&spec, &Run::default()).await.unwrap();
    assert_eq!(rows[..2], [study(), study()]);
    assert_eq!(rows.len(), 2 + 3);
    assert_eq!(rows[2..], pids());
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

// ── Every block streams its own table (transform_spec) ──────────────────────

/// Three tables, one of them with a multivalued slot.
const MULTI_SOURCE: &str = "\
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
      tags:
        multivalued: true
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

const PARTICIPANTS_MULTI: &str =
    "pid\tname\tsite\ttags\nP1\tAda\tA\tx|y\nP2\tBo\tB\tz\nP3\tCy\tA\tq\n";
const SITES_TSV: &str = "site_code\tsite_name\nA\tAlpha\nB\tBeta\n";
const VISITS_TSV: &str = "vid\tpid\tage\nV1\tP1\t30\nV2\tP2\t41\nV3\tP1\t52\nV4\tP9\t7\n";

fn multi_case() -> Case {
    let case = Case::new(MULTI_SOURCE, None);
    case.write_data("participants.tsv", PARTICIPANTS_MULTI);
    case.write_data("sites.tsv", SITES_TSV);
    case.write_data("visits.tsv", VISITS_TSV);
    case
}

const SPEC_INTERLEAVED: &str = r#"  First:
    slot_derivations:
      id:
        value: first
  Person:
    populated_from: participants
    slot_derivations:
      pid:
      name:
  Mid:
    slot_derivations:
      id:
        value: mid
  Visit:
    populated_from: visits
    slot_derivations:
      vid:
      who:
        populated_from: pid
      age:
  Last:
    slot_derivations:
      id:
        value: last
"#;

fn interleaved_expected() -> Vec<Value> {
    vec![
        json!({"id": "first"}),
        json!({"pid": "P1", "name": "Ada"}),
        json!({"pid": "P2", "name": "Bo"}),
        json!({"pid": "P3", "name": "Cy"}),
        json!({"id": "mid"}),
        json!({"vid": "V1", "who": "P1", "age": 30}),
        json!({"vid": "V2", "who": "P2", "age": 41}),
        json!({"vid": "V3", "who": "P1", "age": 52}),
        json!({"vid": "V4", "who": "P9", "age": 7}),
        json!({"id": "last"}),
    ]
}

/// Two row blocks with constant blocks before, between and after them come out
/// in spec order (oracle: `linkml-map map-data -f jsonl` on the same inputs).
#[tokio::test]
async fn row_and_constant_blocks_interleave_in_spec_order() {
    let rows = multi_case()
        .run(SPEC_INTERLEAVED, &Run::default())
        .await
        .unwrap();
    assert_eq!(rows, interleaved_expected());
}

/// Worker count, ordering and channel depth change neither which blocks run,
/// nor their order. Ordered runs are exact; unordered runs may shuffle the rows
/// *within* a block but never move a row across a block boundary.
#[tokio::test]
async fn multi_block_order_holds_across_worker_modes() {
    let case = multi_case();
    let expected = interleaved_expected();
    // (start, end) of each block in the expected output.
    let spans = [(0, 1), (1, 4), (4, 5), (5, 9), (9, 10)];
    for workers in [0, 1, 2, 3, 8] {
        for ordered in [true, false] {
            for channel_depth in [1, 3, 256] {
                let run = Run {
                    workers,
                    ordered,
                    channel_depth,
                    ..Run::default()
                };
                let mut rows = case.run(SPEC_INTERLEAVED, &run).await.unwrap();
                let label = format!("workers={workers} ordered={ordered} depth={channel_depth}");
                if !ordered {
                    for (a, b) in spans {
                        rows[a..b].sort_by_key(|v| v.to_string());
                    }
                    let mut want = expected.clone();
                    for (a, b) in spans {
                        want[a..b].sort_by_key(|v| v.to_string());
                    }
                    assert_eq!(rows, want, "{label}");
                } else {
                    assert_eq!(rows, expected, "{label}");
                }
            }
        }
    }
}

/// `rows_in` counts rows read across every block; `rows_out` also counts the
/// constant objects.
#[tokio::test]
async fn stats_count_rows_across_blocks() {
    let case = multi_case();
    let (_, stats) = case
        .run_on(&case.data_dir(), SPEC_INTERLEAVED, &Run::default())
        .await
        .unwrap();
    assert_eq!((stats.rows_in, stats.rows_out), (3 + 4, 3 + 4 + 3));
}

/// `test_multiple_derivations_same_populated_from` (test_cli_tabular.py): two
/// derivations of one table each stream it, one after the other.
#[tokio::test]
async fn blocks_sharing_a_table_each_stream_it() {
    let spec = r#"  NameRecord:
    populated_from: participants
    slot_derivations:
      pid:
      label:
        populated_from: name
  EmailRecord:
    populated_from: participants
    slot_derivations:
      pid:
      site:
"#;
    let rows = multi_case().run(spec, &Run::default()).await.unwrap();
    assert_eq!(
        rows,
        vec![
            json!({"pid": "P1", "label": "Ada"}),
            json!({"pid": "P2", "label": "Bo"}),
            json!({"pid": "P3", "label": "Cy"}),
            json!({"pid": "P1", "site": "A"}),
            json!({"pid": "P2", "site": "B"}),
            json!({"pid": "P3", "site": "A"}),
        ]
    );
}

/// `test_extra_data_files_skipped`: tables no block reads are ignored.
#[tokio::test]
async fn extra_data_files_are_skipped() {
    let spec = r#"  Person:
    populated_from: participants
    slot_derivations:
      pid:
"#;
    let case = multi_case();
    case.write_data("Organization.tsv", "id\tname\nO:1\tAcme\n");
    let rows = case.run(spec, &Run::default()).await.unwrap();
    assert_eq!(rows, pids_p());
}

fn pids_p() -> Vec<Value> {
    vec![
        json!({"pid": "P1"}),
        json!({"pid": "P2"}),
        json!({"pid": "P3"}),
    ]
}

/// A block whose table has no file is skipped; the others still run, and the
/// skipped block counts for nothing (oracle).
#[tokio::test]
async fn block_with_a_missing_table_is_skipped() {
    let spec = r#"  Person:
    populated_from: participants
    slot_derivations:
      pid:
  Ghost:
    populated_from: ghosts
    slot_derivations:
      gid:
  Visit:
    populated_from: visits
    slot_derivations:
      vid:
"#;
    let case = multi_case();
    let (rows, stats) = case
        .run_on(&case.data_dir(), spec, &Run::default())
        .await
        .unwrap();
    let mut want = pids_p();
    want.extend((1..=4).map(|i| json!({ "vid": format!("V{i}") })));
    assert_eq!(rows, want);
    assert_eq!((stats.rows_in, stats.rows_out), (7, 7));
}

/// `_find_file` tries `.tsv`, `.csv`, `.yaml`, `.yml`, `.json` in that order,
/// and nothing else (oracle: `.tsv` shadows `.csv`; `notes.txt` is no table).
#[tokio::test]
async fn directory_tables_are_found_by_stem_in_upstream_extension_order() {
    let case = Case::new(MULTI_SOURCE, None);
    case.write_data("participants.csv", "pid,name,site,tags\nP1,CSV,A,t\n");
    case.write_data("participants.tsv", "pid\tname\tsite\ttags\nP1\tTSV\tA\tt\n");
    case.write_data(
        "visits.yaml",
        "- vid: V1\n  pid: P1\n  age: 30\n- vid: V2\n  pid: P2\n  age: 41\n",
    );
    case.write_data(
        "sites.json",
        "[{\"site_code\": \"A\", \"site_name\": \"Alpha\"}]",
    );
    case.write_data("notes.txt", "x");
    let spec = r#"  Person:
    populated_from: participants
    slot_derivations:
      pid:
      name:
  Visit:
    populated_from: visits
    slot_derivations:
      vid:
      age:
  Site:
    populated_from: sites
    slot_derivations:
      site_name:
  Notes:
    populated_from: notes
    slot_derivations:
      x:
"#;
    let rows = case.run(spec, &Run::default()).await.unwrap();
    assert_eq!(
        rows,
        vec![
            json!({"pid": "P1", "name": "TSV"}),
            json!({"vid": "V1", "age": 30}),
            json!({"vid": "V2", "age": 41}),
            json!({"site_name": "Alpha"}),
        ]
    );
}

/// JSONL tables (a Rust extension) and TSV tables mix in one directory, with the
/// output as JSONL: the JSONL block cannot use the raw-line path here, so every
/// block takes the generic one, and the result is the same.
#[tokio::test]
async fn mixed_table_formats_in_one_directory() {
    let case = Case::new(MULTI_SOURCE, None);
    case.write_data("participants.tsv", PARTICIPANTS_MULTI);
    case.write_data(
        "sites.jsonl",
        "{\"site_code\":\"A\",\"site_name\":\"Alpha\"}\n{\"site_code\":\"B\",\"site_name\":\"Beta\"}\n",
    );
    let spec = r#"  Person:
    populated_from: participants
    slot_derivations:
      pid:
  Site:
    populated_from: sites
    slot_derivations:
      site_code:
"#;
    for ordered in [true, false] {
        let run = Run {
            ordered,
            ..Run::default()
        };
        let mut rows = case.run(spec, &run).await.unwrap();
        rows.sort_by_key(|v| v.to_string());
        let mut want = pids_p();
        want.extend([json!({"site_code": "A"}), json!({"site_code": "B"})]);
        want.sort_by_key(|v| v.to_string());
        assert_eq!(rows, want, "ordered={ordered}");
    }
}

/// Both tables JSONL, output JSONL: the raw-line fast path runs every block into
/// one buffer, constants in position.
#[tokio::test]
async fn jsonl_fast_path_runs_every_block() {
    let case = Case::new(MULTI_SOURCE, None);
    case.write_data(
        "participants.jsonl",
        "{\"pid\":\"P1\",\"name\":\"Ada\"}\n{\"pid\":\"P2\",\"name\":\"Bo\"}\n",
    );
    case.write_data(
        "sites.jsonl",
        "{\"site_code\":\"A\",\"site_name\":\"Alpha\"}\n",
    );
    let spec = r#"  Person:
    populated_from: participants
    slot_derivations:
      pid:
  Mid:
    slot_derivations:
      id:
        value: mid
  Site:
    populated_from: sites
    slot_derivations:
      site_name:
"#;
    for workers in [1, 4] {
        let run = Run {
            workers,
            ..Run::default()
        };
        let (rows, stats) = case.run_on(&case.data_dir(), spec, &run).await.unwrap();
        assert_eq!(
            rows,
            vec![
                json!({"pid": "P1"}),
                json!({"pid": "P2"}),
                json!({"id": "mid"}),
                json!({"site_name": "Alpha"}),
            ],
            "workers={workers}"
        );
        assert_eq!((stats.rows_in, stats.rows_out), (3, 4));
    }
}

// ── Single-file input: the file stem / --source-type names the table ────────

/// A single tabular file holds the table named by `--source-type`, else by its
/// stem; only blocks reading that table stream. Constants keep their place
/// (oracle, runs H1–H6).
#[tokio::test]
async fn single_file_streams_only_the_block_for_its_table() {
    let spec = r#"  Pre:
    slot_derivations:
      id:
        value: pre
  Person:
    populated_from: participants
    slot_derivations:
      pid:
  Visit:
    populated_from: visits
    slot_derivations:
      vid:
  Post:
    slot_derivations:
      id:
        value: post
"#;
    let case = multi_case();
    let visits = case.data_dir().join("visits.tsv");
    let want_visits = vec![
        json!({"id": "pre"}),
        json!({"vid": "V1"}),
        json!({"vid": "V2"}),
        json!({"vid": "V3"}),
        json!({"vid": "V4"}),
        json!({"id": "post"}),
    ];
    // By stem.
    let (rows, stats) = case.run_on(&visits, spec, &Run::default()).await.unwrap();
    assert_eq!(rows, want_visits);
    assert_eq!((stats.rows_in, stats.rows_out), (4, 6));
    // By `--source-type`, whatever the file is called.
    let people = case.data_dir().join("people.tsv");
    std::fs::write(&people, VISITS_TSV).unwrap();
    let run = Run {
        source_class: Some("visits".into()),
        ..Run::default()
    };
    let (rows, _) = case.run_on(&people, spec, &run).await.unwrap();
    assert_eq!(rows, want_visits);
    // The same file under another name selects the other block.
    let run = Run {
        source_class: Some("participants".into()),
        ..Run::default()
    };
    let participants = case.data_dir().join("participants.tsv");
    let (rows, _) = case.run_on(&participants, spec, &run).await.unwrap();
    assert_eq!(rows[0], json!({"id": "pre"}));
    assert_eq!(rows[1], json!({"pid": "P1"}));
    assert_eq!(rows.len(), 3 + 2);
}

/// `test_no_data_for_any_class_derivation_fails`: a file no block reads fails the
/// run, saying what was expected and what was found. Same for a wrong
/// `--source-type` (oracle, runs H4 and H6).
#[tokio::test]
async fn single_file_matching_no_block_fails() {
    let spec = r#"  Person:
    populated_from: participants
    slot_derivations:
      pid:
  Visit:
    populated_from: visits
    slot_derivations:
      vid:
"#;
    let case = multi_case();
    let people = case.data_dir().join("people.tsv");
    std::fs::write(&people, VISITS_TSV).unwrap();
    let err = case
        .run_on(&people, spec, &Run::default())
        .await
        .unwrap_err();
    assert!(
        format!("{err:#}").contains(
            "No input data for any class derivation: expected one of \
             ['participants', 'visits'], found ['people']"
        ),
        "{err:#}"
    );
    let run = Run {
        source_class: Some("Nope".into()),
        ..Run::default()
    };
    let err = case.run_on(&people, spec, &run).await.unwrap_err();
    assert!(format!("{err:#}").contains("found ['Nope']"), "{err:#}");
}

// ── Joins and multivalued slots across blocks ───────────────────────────────

/// A block with a join, a block with a multivalued slot and a block that joins
/// the first block's table the other way round, in one run (oracle).
#[tokio::test]
async fn joined_and_multivalued_blocks_share_one_run() {
    let spec = r#"  Person:
    populated_from: participants
    joins:
      sites:
        source_key: site
        lookup_key: site_code
    slot_derivations:
      pid:
      site_name:
        expr: "{sites.site_name}"
      tags:
  Visit:
    populated_from: visits
    joins:
      participants:
        source_key: pid
        lookup_key: pid
    slot_derivations:
      vid:
      who:
        expr: "{participants.name}"
  Site:
    populated_from: sites
    slot_derivations:
      site_code:
      upper:
        expr: "upper({site_name})"
"#;
    for ordered in [true, false] {
        let run = Run {
            ordered,
            ..Run::default()
        };
        let mut rows = multi_case().run(spec, &run).await.unwrap();
        let want = vec![
            json!({"pid": "P1", "site_name": "Alpha", "tags": ["x|y"]}),
            json!({"pid": "P2", "site_name": "Beta", "tags": ["z"]}),
            json!({"pid": "P3", "site_name": "Alpha", "tags": ["q"]}),
            json!({"vid": "V1", "who": "Ada"}),
            json!({"vid": "V2", "who": "Bo"}),
            json!({"vid": "V3", "who": "Ada"}),
            json!({"vid": "V4"}),
            json!({"site_code": "A", "upper": "ALPHA"}),
            json!({"site_code": "B", "upper": "BETA"}),
        ];
        if !ordered {
            for (a, b) in [(0, 3), (3, 7), (7, 9)] {
                rows[a..b].sort_by_key(|v| v.to_string());
            }
        }
        assert_eq!(rows, want, "ordered={ordered}");
    }
}

/// Blocks join a table per block: two blocks may join `sites` on different keys
/// (oracle). A spec-wide lookup index could not hold both.
#[tokio::test]
async fn blocks_may_join_one_table_on_different_keys() {
    let spec = r#"  ByCode:
    populated_from: participants
    joins:
      sites:
        source_key: site
        lookup_key: site_code
    slot_derivations:
      pid:
      site_name:
        expr: "{sites.site_name}"
  ByName:
    populated_from: participants
    joins:
      sites:
        source_key: name
        lookup_key: site_name
    slot_derivations:
      pid:
      code:
        expr: "{sites.site_code}"
"#;
    let case = Case::new(MULTI_SOURCE, None);
    case.write_data(
        "participants.tsv",
        "pid\tname\tsite\ttags\nP1\tAlpha\tA\tx|y\nP2\tBo\tB\tz\nP3\tCy\tA\tq\n",
    );
    case.write_data("sites.tsv", SITES_TSV);
    let rows = case.run(spec, &Run::default()).await.unwrap();
    assert_eq!(
        rows,
        vec![
            json!({"pid": "P1", "site_name": "Alpha"}),
            json!({"pid": "P2", "site_name": "Beta"}),
            json!({"pid": "P3", "site_name": "Alpha"}),
            json!({"pid": "P1", "code": "A"}),
            json!({"pid": "P2"}),
            json!({"pid": "P3"}),
        ]
    );
}

// ── Errors: row index and class derivation, per block ───────────────────────

const SPEC_FAILING_BLOCKS: &str = r#"  Person:
    populated_from: participants
    slot_derivations:
      pid:
      n:
        expr: "int({name})"
  Visit:
    populated_from: visits
    slot_derivations:
      vid:
      n:
        expr: "int({age}) + int({pid})"
"#;

fn failing_case() -> Case {
    let case = Case::new(MULTI_SOURCE, None);
    case.write_data("participants.tsv", "pid\tname\nP1\t5\nP2\tbad\nP3\t7\n");
    case.write_data(
        "visits.tsv",
        "vid\tpid\tage\nV1\t1\t30\nV2\t2\t41\nV3\tP1\t52\nV4\t4\t7\n",
    );
    case
}

/// `--continue-on-error` keeps every block's good rows, and counts the bad ones
/// across blocks (oracle: same output, `2 transformation error(s)`).
#[tokio::test]
async fn continue_on_error_spans_blocks() {
    let case = failing_case();
    let run = Run {
        continue_on_error: true,
        ..Run::default()
    };
    let err = case
        .run_on(&case.data_dir(), SPEC_FAILING_BLOCKS, &run)
        .await
        .unwrap_err();
    assert!(
        format!("{err:#}").contains("2 of 7 rows failed to transform"),
        "{err:#}"
    );
    let text = std::fs::read_to_string(case.tmp.path().join("out.jsonl")).unwrap();
    let rows: Vec<Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(
        rows,
        vec![
            json!({"pid": "P1", "n": 5}),
            json!({"pid": "P3", "n": 7}),
            json!({"vid": "V1", "n": 31}),
            json!({"vid": "V2", "n": 43}),
            json!({"vid": "V4", "n": 11}),
        ]
    );
}

/// Without it the first error ends the run, naming the block's row index (not
/// the position in the whole output) and class derivation (oracle: `row=1`).
#[tokio::test]
async fn first_error_names_the_class_derivation_and_row_index() {
    for ordered in [true, false] {
        let run = Run {
            ordered,
            workers: 1,
            ..Run::default()
        };
        let err = failing_case()
            .run(SPEC_FAILING_BLOCKS, &run)
            .await
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("transform error at row 1; class_derivation=Person"),
            "ordered={ordered}: {msg}"
        );
    }
}

// ── FK specs: each block buffers its own table ──────────────────────────────

/// FK mode (a dotted `expr:` over a class-ranged slot) buffers each block's rows
/// and builds one index per block: two blocks, JSONL out (raw bytes) and
/// non-JSONL out (stream writer), with a constant block between them.
#[tokio::test]
async fn fk_mode_runs_every_block() {
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
    case.write_data(
        "sites.jsonl",
        "{\"site_code\":\"A\",\"site_name\":\"Alpha\"}\n",
    );
    let spec = r#"  Participant:
    populated_from: participants
    slot_derivations:
      tagged:
        expr: pid + ".x"
  Mid:
    slot_derivations:
      id:
        value: mid
  Site:
    populated_from: sites
    slot_derivations:
      named:
        expr: site_name + ".y"
"#;
    let want = vec![
        json!({"tagged": "P1.x"}),
        json!({"tagged": "P2.x"}),
        json!({"id": "mid"}),
        json!({"named": "Alpha.y"}),
    ];
    for workers in [1, 4] {
        let run = Run {
            workers,
            ..Run::default()
        };
        let (rows, stats) = case.run_on(&case.data_dir(), spec, &run).await.unwrap();
        assert_eq!(rows, want, "workers={workers}");
        assert_eq!((stats.rows_in, stats.rows_out), (3, 4));
    }
    // Stream-writer sink: JSON output, parsed by hand.
    let run = Run {
        out_format: Format::Json,
        ..Run::default()
    };
    case.run_on(&case.data_dir(), spec, &run).await.unwrap();
    let text = std::fs::read_to_string(case.tmp.path().join("out.json")).unwrap();
    let parsed: Vec<Value> = serde_json::from_str(&text).unwrap();
    assert_eq!(parsed, want);
}

// ── test_engine_edge_cases.py ───────────────────────────────────────────────

const EDGE_SOURCE: &str = "\
id: https://example.org/engine-test-source
name: engine_test_source
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
      name: {}
      site_code: {}
  sites:
    attributes:
      site_code:
        identifier: true
      site_name: {}
";

const EDGE_TARGET: &str = "\
id: https://example.org/engine-test-target
name: engine_test_target
prefixes:
  linkml: https://w3id.org/linkml/
imports:
  - linkml:types
default_range: string
classes:
  FlatSample:
    attributes:
      sample_id:
        identifier: true
      name: {}
      site_name: {}
  SimpleSample:
    attributes:
      sample_id:
        identifier: true
      name: {}
";

const FLAT_SAMPLE_BLOCK: &str = r#"  FlatSample:
    populated_from: samples
    joins:
      sites:
        join_on: site_code
    slot_derivations:
      sample_id:
        populated_from: sample_id
      name:
        populated_from: name
      site_name:
        expr: "{sites.site_name}"
"#;

const SIMPLE_SAMPLE_BLOCK: &str = r#"  SimpleSample:
    populated_from: samples
    slot_derivations:
      sample_id:
        populated_from: sample_id
      name:
        populated_from: name
"#;

/// `test_mixed_derivations_with_and_without_joins`: one block joins, one does
/// not; both stream, in spec order.
#[tokio::test]
async fn mixed_blocks_with_and_without_joins() {
    let case = Case::new(EDGE_SOURCE, Some(EDGE_TARGET));
    case.write_data(
        "samples.tsv",
        "sample_id\tname\tsite_code\nS001\tAlpha\tSITE_A\n",
    );
    case.write_data(
        "sites.tsv",
        "site_code\tsite_name\nSITE_A\tBoston Medical\n",
    );
    let spec = format!("{FLAT_SAMPLE_BLOCK}{SIMPLE_SAMPLE_BLOCK}");
    let rows = case.run(&spec, &Run::default()).await.unwrap();
    assert_eq!(
        rows,
        vec![
            json!({"sample_id": "S001", "name": "Alpha", "site_name": "Boston Medical"}),
            json!({"sample_id": "S001", "name": "Alpha"}),
        ]
    );
}

/// `test_join_with_empty_secondary_table`: a joined table with headers only
/// resolves to null, and the block still emits its row.
#[tokio::test]
async fn join_with_empty_secondary_table() {
    let case = Case::new(EDGE_SOURCE, Some(EDGE_TARGET));
    case.write_data(
        "samples.tsv",
        "sample_id\tname\tsite_code\nS001\tAlpha\tSITE_A\n",
    );
    case.write_data("sites.tsv", "site_code\tsite_name\n");
    let rows = case.run(FLAT_SAMPLE_BLOCK, &Run::default()).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["sample_id"], "S001");
    assert_eq!(rows[0]["name"], "Alpha");
    assert!(
        rows[0].get("site_name").is_none_or(Value::is_null),
        "{rows:?}"
    );
}

/// `test_engine_no_joins_no_data`: no files, so nothing for any block.
#[tokio::test]
async fn directory_without_data_fails_naming_what_was_expected() {
    let case = Case::new(EDGE_SOURCE, Some(EDGE_TARGET));
    let err = case
        .run(SIMPLE_SAMPLE_BLOCK, &Run::default())
        .await
        .unwrap_err();
    assert!(
        format!("{err:#}").contains("expected one of ['samples'], found []"),
        "{err:#}"
    );
}

// ── Joins that upstream cannot resolve ──────────────────────────────────────

/// `LookupIndex._duckdb_read_expr` reads CSV, TSV and JSON only: a join over a YAML
/// table raises `NotImplementedError` when the block registers its joins, so it fails
/// even with no row to look up (corpus case c80d7ec3435d).
#[tokio::test]
async fn join_over_a_yaml_table_is_not_supported() {
    for samples in [
        "sample_id\tname\tsite_code\nS001\tAlpha\tSITE_A\n",
        "sample_id\tname\tsite_code\n",
    ] {
        let case = Case::new(EDGE_SOURCE, Some(EDGE_TARGET));
        case.write_data("samples.tsv", samples);
        case.write_data("sites.yaml", "- site_code: SITE_A\n  site_name: Boston\n");
        let err = case
            .run(FLAT_SAMPLE_BLOCK, &Run::default())
            .await
            .unwrap_err();
        assert!(
            format!("{err:#}").contains("LookupIndex does not yet support 'yaml' files"),
            "{err:#}"
        );
    }
}

const POPULATED_FROM_JOIN_BLOCK: &str = r#"  FlatSample:
    populated_from: samples
    joins:
      sites:
        join_on: site_code
    slot_derivations:
      sample_id:
        populated_from: sample_id
      site_name:
        populated_from: sites.site_name
"#;

/// `_resolve_joined_row`: a row whose join key has a value, joined to a table with no
/// file, fails (DuckDB: the table does not exist); a row with no key value is a miss.
#[tokio::test]
async fn populated_from_join_without_its_table_fails_only_for_a_key() {
    let case = Case::new(EDGE_SOURCE, Some(EDGE_TARGET));
    case.write_data(
        "samples.tsv",
        "sample_id\tname\tsite_code\nS001\tAlpha\tSITE_A\n",
    );
    let err = case
        .run(POPULATED_FROM_JOIN_BLOCK, &Run::default())
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("sites"), "{err:#}");

    let case = Case::new(EDGE_SOURCE, Some(EDGE_TARGET));
    case.write_data("samples.tsv", "sample_id\tname\tsite_code\nS001\tAlpha\t\n");
    let rows = case
        .run(POPULATED_FROM_JOIN_BLOCK, &Run::default())
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["sample_id"], "S001");
}
