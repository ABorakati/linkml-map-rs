//! Parquet and DuckDB output (`columnar` feature).
//!
//! Mirror of upstream `ColumnarStreamWriter` / `ParquetStreamWriter` /
//! `DuckDBStreamWriter` (`writers/output_streams.py`, commit `04dc4c8`).  Records
//! are streamed to `<target>.staging.jsonl` exactly as the JSONL writer writes
//! them (null slots stripped), then DuckDB's `read_json_auto` infers a typed
//! column per field and writes the real artifact in `finalize_path`.  Nested
//! objects become `STRUCT` columns and lists of them `STRUCT[]`.
//!
//! Decimals: upstream stages them as strings and then casts those paths to an
//! exact `DECIMAL(p,s)`.  The staging half is ported (a decimal is staged as its
//! fixed-point text, so no digit is rounded through a float); the `DECIMAL` cast is
//! not, so such columns come out as `VARCHAR` rather than `DECIMAL`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use linkml_map_core::value::Value;

use crate::format::Format;
use crate::writers::{StreamWriter, jsonl_line};

fn contains_decimal(value: &Value) -> bool {
    match value {
        Value::Decimal(_) => true,
        Value::List(items) => items.iter().any(contains_decimal),
        Value::Map(map) => map.values().any(contains_decimal),
        Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_) | Value::Str(_) => false,
    }
}

/// Copy of *value* with every decimal replaced by its fixed-point text.
fn decimals_as_strings(value: &Value) -> Value {
    match value {
        Value::Decimal(d) => Value::Str(d.to_fixed_string()),
        Value::List(items) => Value::List(items.iter().map(decimals_as_strings).collect()),
        Value::Map(map) => Value::Map(
            map.iter()
                .map(|(k, v)| (k.clone(), decimals_as_strings(v)))
                .collect(),
        ),
        scalar @ (Value::Null
        | Value::Bool(_)
        | Value::Int(_)
        | Value::Float(_)
        | Value::Str(_)) => scalar.clone(),
    }
}

/// Which artifact the staged JSONL is converted into.
#[derive(Debug, Clone)]
enum Artifact {
    Parquet,
    DuckDb { table_name: Option<String> },
}

/// Writer for [`Format::Parquet`] and [`Format::DuckDb`].
#[derive(Debug)]
pub struct ColumnarStreamWriter {
    artifact: Artifact,
}

impl ColumnarStreamWriter {
    /// A writer for *format*; `table_name` only applies to DuckDB (default: the
    /// output file's stem).
    ///
    /// # Panics
    ///
    /// If *format* is not columnar.
    pub fn new(format: Format, table_name: Option<&str>) -> Self {
        let artifact = match format {
            Format::Parquet => Artifact::Parquet,
            Format::DuckDb => Artifact::DuckDb {
                table_name: table_name.map(str::to_owned),
            },
            other => panic!("{other:?} is not a columnar format"),
        };
        Self { artifact }
    }
}

impl StreamWriter for ColumnarStreamWriter {
    fn write_chunk(&mut self, chunk: &[Value]) -> Result<String> {
        // PARITY: output_streams.py::ColumnarStreamWriter.write_chunk stages
        // `json.dumps(_strip_nulls(obj), ensure_ascii=False)`; an all-null slot is
        // therefore absent from the artifact's schema.
        // `_decimal_as_string`: decimals are staged as strings (see the module docs).
        let mut out = String::new();
        for obj in chunk {
            if contains_decimal(obj) {
                out.push_str(&jsonl_line(&decimals_as_strings(obj)).map_err(anyhow::Error::msg)?);
            } else {
                out.push_str(&jsonl_line(obj).map_err(anyhow::Error::msg)?);
            }
            out.push('\n');
        }
        Ok(out)
    }

    fn staging_path(&self, target: &Path) -> Option<PathBuf> {
        // Same directory, so the conversion reads a local file.
        let mut name = target.file_name().unwrap_or_default().to_owned();
        name.push(".staging.jsonl");
        Some(target.with_file_name(name))
    }

    fn finalize_path(&mut self, target: &Path, staged: Option<&Path>) -> Result<()> {
        let staged = staged.context("columnar writer requires a staging file to convert from")?;
        let result = match &self.artifact {
            Artifact::Parquet => linkml_map_duckdb::columnar::jsonl_to_parquet(staged, target),
            Artifact::DuckDb { table_name } => {
                // `Path.stem` strips only the last suffix, like `file_stem`.
                let table = table_name.clone().unwrap_or_else(|| {
                    target
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default()
                });
                linkml_map_duckdb::columnar::jsonl_to_duckdb(staged, target, &table)
            }
        };
        // PARITY: upstream unlinks the staging file only after a successful
        // conversion and leaves it behind on failure (no `finally`).  Removing it
        // either way is a deliberate divergence with no effect on the artifact.
        let _ = std::fs::remove_file(staged);
        result.with_context(|| format!("writing {}", target.display()))
    }
}

#[cfg(test)]
mod tests {
    //! Ports of upstream `tests/test_writers/test_columnar_output.py`, plus
    //! Rust-specific checks (null stripping, key order, staging cleanup).

    use linkml_map_duckdb::columnar::duckdb::Connection;

    use super::*;
    use crate::writers::{
        MultiStreamWriter, Target, WriteOptions, make_stream_writer, write_all_with,
        write_all_writer,
    };

    fn rows(json: &str) -> Vec<Value> {
        serde_json::from_str(json).unwrap()
    }

    fn nested_rows() -> Vec<Value> {
        rows(
            r#"[
            {"id": "m1", "observation_type": "OBA:VT0001253",
             "value_quantity": {"value_decimal": 155.448, "unit": "cm"}},
            {"id": "m2", "observation_type": "OBA:VT0001253",
             "value_quantity": {"value_decimal": 167.386, "unit": "cm"}}
        ]"#,
        )
    }

    async fn write(rows: Vec<Value>, target: &Path, format: Format, table: Option<&str>) {
        let options = WriteOptions {
            table_name: table.map(str::to_owned),
            ..WriteOptions::default()
        };
        let stream = futures::stream::iter(rows.into_iter().map(Ok::<Value, anyhow::Error>));
        write_all_with(target, format, &options, stream)
            .await
            .unwrap();
    }

    fn column(con: &Connection, sql: &str) -> Vec<String> {
        let mut stmt = con.prepare(sql).unwrap();
        stmt.query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    fn describe(con: &Connection, from: &str) -> Vec<(String, String)> {
        let mut stmt = con
            .prepare(&format!("DESCRIBE SELECT * FROM {from}"))
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    fn count(con: &Connection, from: &str) -> i64 {
        con.query_row(&format!("SELECT count(*) FROM {from}"), [], |r| r.get(0))
            .unwrap()
    }

    // test_extension_maps_to_format
    #[test]
    fn extension_maps_to_format() {
        assert_eq!(Format::from_path("x.parquet").unwrap(), Format::Parquet);
        assert_eq!(Format::from_path("x.duckdb").unwrap(), Format::DuckDb);
        assert_eq!(Format::from_path("x.PARQUET").unwrap(), Format::Parquet);
        assert_eq!(Format::from_path("x.DuckDB").unwrap(), Format::DuckDb);
    }

    // test_parquet_preserves_nesting_as_a_struct_column
    #[tokio::test]
    async fn parquet_preserves_nesting_as_a_struct_column() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("obs.parquet");
        write(nested_rows(), &target, Format::Parquet, None).await;

        let con = Connection::open_in_memory().unwrap();
        let from = format!("read_parquet('{}')", target.display());
        let types = describe(&con, &from);
        let vq = types.iter().find(|(n, _)| n == "value_quantity").unwrap();
        assert_eq!(vq.1, "STRUCT(value_decimal DOUBLE, unit VARCHAR)");
        let got = column(
            &con,
            &format!(
                "SELECT id || '|' || value_quantity.unit || '|' || value_quantity.value_decimal \
                 FROM {from} ORDER BY id"
            ),
        );
        assert_eq!(got, ["m1|cm|155.448", "m2|cm|167.386"]);
    }

    // test_parquet_target_holds_no_staging_file
    #[tokio::test]
    async fn parquet_target_holds_no_staging_file() {
        let dir = tempfile::tempdir().unwrap();
        write(
            nested_rows(),
            &dir.path().join("obs.parquet"),
            Format::Parquet,
            None,
        )
        .await;
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["obs.parquet"]);
    }

    // test_duckdb_writes_a_queryable_table
    #[tokio::test]
    async fn duckdb_writes_a_queryable_table_named_for_the_stem() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("phs000000.duckdb");
        write(nested_rows(), &target, Format::DuckDb, None).await;

        let con = Connection::open(&target).unwrap();
        assert_eq!(column(&con, "SHOW TABLES"), ["phs000000"]);
        let got = column(
            &con,
            "SELECT id || '|' || value_quantity.unit FROM \"phs000000\" ORDER BY id",
        );
        assert_eq!(got, ["m1|cm", "m2|cm"]);
    }

    // test_duckdb_table_name_override
    #[tokio::test]
    async fn duckdb_table_name_override() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("phs000000.duckdb");
        write(
            nested_rows(),
            &target,
            Format::DuckDb,
            Some("measurement_observation"),
        )
        .await;
        let con = Connection::open(&target).unwrap();
        assert_eq!(column(&con, "SHOW TABLES"), ["measurement_observation"]);
    }

    // test_duckdb_accumulates_tables_across_runs
    #[tokio::test]
    async fn duckdb_accumulates_tables_across_runs() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("phs000000.duckdb");
        write(
            nested_rows(),
            &target,
            Format::DuckDb,
            Some("measurement_observation"),
        )
        .await;
        write(
            rows(r#"[{"id": "p1", "sex": "F"}]"#),
            &target,
            Format::DuckDb,
            Some("participant"),
        )
        .await;
        let con = Connection::open(&target).unwrap();
        let mut tables = column(&con, "SHOW TABLES");
        tables.sort();
        assert_eq!(tables, ["measurement_observation", "participant"]);
        assert_eq!(count(&con, "\"measurement_observation\""), 2);
        // A same-named table is replaced, not appended to.
        drop(con);
        write(
            nested_rows()[..1].to_vec(),
            &target,
            Format::DuckDb,
            Some("measurement_observation"),
        )
        .await;
        let con = Connection::open(&target).unwrap();
        assert_eq!(count(&con, "\"measurement_observation\""), 1);
    }

    // test_multivalued_nesting_becomes_a_struct_list
    #[tokio::test]
    async fn multivalued_nesting_becomes_a_struct_list() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("sets.parquet");
        let data = rows(
            r#"[{"id": "s1", "observations": [{"id": "m1", "value": 1.5}, {"id": "m2", "value": 2.5}]}]"#,
        );
        write(data, &target, Format::Parquet, None).await;
        let con = Connection::open_in_memory().unwrap();
        let from = format!("read_parquet('{}')", target.display());
        let types = describe(&con, &from);
        let obs = types.iter().find(|(n, _)| n == "observations").unwrap();
        assert_eq!(obs.1, "STRUCT(id VARCHAR, \"value\" DOUBLE)[]");
        assert_eq!(
            column(&con, &format!("SELECT observations[1].id FROM {from}")),
            ["m1"]
        );
    }

    // test_empty_stream_writes_an_empty_artifact
    #[tokio::test]
    async fn empty_stream_writes_an_empty_artifact() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("empty.parquet");
        write(vec![], &target, Format::Parquet, None).await;
        let con = Connection::open_in_memory().unwrap();
        assert_eq!(
            count(&con, &format!("read_parquet('{}')", target.display())),
            0
        );
    }

    // test_columnar_output_alongside_other_formats
    #[tokio::test]
    async fn columnar_output_alongside_other_formats() {
        let dir = tempfile::tempdir().unwrap();
        let jsonl = dir.path().join("obs.jsonl");
        let parquet = dir.path().join("obs.parquet");
        let outputs = vec![
            (
                make_stream_writer(Format::Jsonl, None).unwrap(),
                Target::Path(jsonl.clone()),
            ),
            (
                make_stream_writer(Format::Parquet, None).unwrap(),
                Target::Path(parquet.clone()),
            ),
        ];
        let stream =
            futures::stream::iter(nested_rows().into_iter().map(Ok::<Value, anyhow::Error>));
        MultiStreamWriter::new(outputs)
            .write_all(1000, stream)
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string(jsonl).unwrap().lines().count(), 2);
        let con = Connection::open_in_memory().unwrap();
        assert_eq!(
            count(&con, &format!("read_parquet('{}')", parquet.display())),
            2
        );
    }

    /// Nulls are stripped before staging, so an all-null slot is absent and the column
    /// order is the record's key order.
    #[tokio::test]
    async fn nulls_are_stripped_and_key_order_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("o.parquet");
        write(
            rows(r#"[{"id": "a", "x": null, "y": 1}]"#),
            &target,
            Format::Parquet,
            None,
        )
        .await;
        let con = Connection::open_in_memory().unwrap();
        let names: Vec<String> = describe(&con, &format!("read_parquet('{}')", target.display()))
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert_eq!(names, ["id", "y"]);
    }

    /// A failed conversion leaves no staging file behind (deliberate divergence).
    #[tokio::test]
    async fn failed_conversion_leaves_no_staging_file() {
        let dir = tempfile::tempdir().unwrap();
        // The staging file is created beside the target, so make conversion fail by
        // pointing the target into a directory that DuckDB cannot write.
        let target = dir.path().join("sub").join("o.parquet");
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let staged = ColumnarStreamWriter::new(Format::Parquet, None)
            .staging_path(&target)
            .unwrap();
        std::fs::write(&staged, "not json at all\n").unwrap();
        let err = ColumnarStreamWriter::new(Format::Parquet, None)
            .finalize_path(&target, Some(&staged))
            .unwrap_err();
        assert!(format!("{err:#}").contains("o.parquet"), "{err:#}");
        assert!(!staged.exists());
    }

    /// Table names with `"` and paths with `'` are quoted, not interpolated.
    #[tokio::test]
    async fn awkward_names_are_quoted() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("it's.duckdb");
        write(nested_rows(), &target, Format::DuckDb, Some("we\"ird")).await;
        let con = Connection::open(&target).unwrap();
        assert_eq!(column(&con, "SHOW TABLES"), ["we\"ird"]);
    }

    /// Columnar formats need a file (upstream: "requires an output file").
    #[tokio::test]
    async fn columnar_to_a_writer_is_rejected() {
        let mut buf: Vec<u8> = Vec::new();
        let err = write_all_writer(&mut buf, Format::Parquet, futures::stream::iter(vec![]))
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "parquet output requires an output file; pass -o/--output FILE"
        );
    }
}

#[cfg(test)]
mod decimal_tests {
    use linkml_map_core::decimal::Decimal;

    use super::*;

    /// Decimals are staged as exact strings, never as float-rounded numbers.
    #[test]
    fn decimals_are_staged_as_strings() {
        let row = Value::Map(
            [
                ("id".to_owned(), Value::Str("a".into())),
                (
                    "v".to_owned(),
                    Value::Decimal(Decimal::parse_python("52.30").unwrap()),
                ),
            ]
            .into_iter()
            .collect(),
        );
        let staged = ColumnarStreamWriter::new(Format::Parquet, None)
            .write_chunk(&[row])
            .unwrap();
        assert_eq!(staged, "{\"id\": \"a\", \"v\": \"52.30\"}\n");
    }
}
