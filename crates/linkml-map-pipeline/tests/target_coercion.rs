//! End-to-end target-schema coercion through the real pipeline.
//!
//! Port of upstream `test_target_schema_range.py::test_map_data_coerces_tsv_with_target_schema`:
//! TSV in with a target schema; the spec needs no range, the target slot types
//! the value.

use linkml_map_io::Format;
use linkml_map_pipeline::{PipelineConfig, run_pipeline};

const SOURCE_SCHEMA: &str = r"
id: https://example.org/range-source
name: range_source
prefixes: {linkml: 'https://w3id.org/linkml/'}
default_prefix: range_source
default_range: string
imports: [linkml:types]
classes:
  Row:
    attributes:
      id: {identifier: true}
      amount: {}
      flag: {}
";

const TARGET_SCHEMA: &str = r"
id: https://example.org/range-target
name: range_target
prefixes: {linkml: 'https://w3id.org/linkml/'}
default_prefix: range_target
default_range: string
imports: [linkml:types]
classes:
  Measurement:
    attributes:
      id: {identifier: true}
      amount: {range: decimal}
      flag: {range: boolean}
";

const SPEC: &str = r"
id: target-schema-range
class_derivations:
  Measurement:
    populated_from: Row
    slot_derivations:
      id:
        populated_from: id
      amount:
        populated_from: amount
      flag:
        populated_from: flag
";

#[tokio::test]
async fn map_data_coerces_tsv_with_target_schema() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::write(dir.join("source.yaml"), SOURCE_SCHEMA).unwrap();
    std::fs::write(dir.join("target.yaml"), TARGET_SCHEMA).unwrap();
    std::fs::write(dir.join("spec.yaml"), SPEC).unwrap();
    std::fs::write(dir.join("Row.tsv"), "id\tamount\tflag\nr1\t52.30\tfalse\n").unwrap();
    let out = dir.join("out.json");

    let mut cfg = PipelineConfig::new(
        dir.join("Row.tsv").to_str().unwrap(),
        dir.join("source.yaml").to_str().unwrap(),
        Some(dir.join("target.yaml").to_str().unwrap()),
        dir.join("spec.yaml").to_str().unwrap(),
        out.to_str().unwrap(),
    );
    cfg.out_format = Some(Format::Json);
    cfg.source_class = Some("Row".to_owned());
    run_pipeline(cfg).await.expect("pipeline failed");

    let text = std::fs::read_to_string(&out).unwrap();
    let rows: serde_json::Value = serde_json::from_str(&text).unwrap();
    let row = &rows[0];
    assert_eq!(row["id"], "r1");
    assert_eq!(row["flag"], false, "{text}");
}
