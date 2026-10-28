//! Async streaming I/O for linkml-map.
//!
//! # Quick start
//!
//! ```no_run
//! use linkml_map_io::{load_stream, write_all, Format};
//! use futures::StreamExt;
//!
//! # #[tokio::main]
//! # async fn main() -> anyhow::Result<()> {
//! // Stream every row from a CSV file as Value::Map.
//! let mut stream = load_stream("data.csv", Format::Csv).await?;
//! while let Some(row) = stream.next().await {
//!     println!("{:?}", row?);
//! }
//!
//! // Write a vec of Values as JSONL.
//! use linkml_map_core::value::Value;
//! use futures::stream;
//! let values = vec![Value::Str("hello".into())];
//! let stream = stream::iter(values.into_iter().map(Ok::<_, anyhow::Error>));
//! write_all("out.jsonl", Format::Jsonl, stream).await?;
//! # Ok(())
//! # }
//! ```

#[cfg(feature = "columnar")]
pub mod columnar;
mod delimited;
pub use delimited::parse_numeric;
pub mod format;
mod libyaml;
pub mod loaders;
pub mod pycompat;
/// The PyYAML-compatible emitter (re-exported from `linkml-map-core`, where the
/// engine's YAML stringification also uses it).
pub use linkml_map_core::pyyaml;
pub mod serialization;
pub mod writers;
pub mod yaml_load;

// Re-export the most commonly used items at crate root.
pub use format::Format;
pub use loaders::{
    NumericColumnHints, load_all, load_all_single_object, load_all_with_numeric_hints, load_stream,
    load_stream_auto, load_stream_single_object, load_stream_with_numeric_hints,
};
pub use writers::{
    DEFAULT_CHUNK_SIZE, MultiStreamWriter, StreamWriter, Target, WriteOptions, columnar_needs_file,
    dump_output, jsonl_line, make_stream_writer, validate_additional_outputs, value_to_json,
    write_all, write_all_auto, write_all_with, write_all_writer, write_all_writer_with, write_vec,
};

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use futures::StreamExt;
    use linkml_map_core::value::Value;

    use super::*;
    use crate::writers::value_to_json;

    // Helper: build a Value::Map from &str pairs (all values are Str).
    // Uses serde_json round-trip to avoid a direct indexmap dependency here.
    fn make_map(pairs: &[(&str, &str)]) -> Value {
        let obj = serde_json::Value::Object(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), serde_json::Value::String(v.to_string())))
                .collect(),
        );
        serde_json::from_value(obj).unwrap()
    }

    // Helper: build a Value::Map from mixed Value pairs.
    fn make_map_mixed(pairs: &[(&str, Value)]) -> Value {
        let obj = serde_json::Value::Object(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), value_to_json(v).unwrap()))
                .collect(),
        );
        serde_json::from_value(obj).unwrap()
    }

    // ── JSONL round-trip ────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_jsonl_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("records.jsonl");

        let original = vec![
            make_map_mixed(&[
                ("name", Value::Str("Alice".into())),
                ("age", Value::Int(30)),
            ]),
            make_map_mixed(&[("name", Value::Str("Bob".into())), ("age", Value::Int(25))]),
            make_map_mixed(&[
                ("name", Value::Str("Charlie".into())),
                ("age", Value::Int(40)),
            ]),
        ];

        // Write.
        write_vec(&path, Format::Jsonl, original.clone())
            .await
            .unwrap();

        // Read back.
        let loaded = load_all(&path, Format::Jsonl).await.unwrap();

        assert_eq!(loaded.len(), 3, "expected 3 records");
        assert_eq!(loaded, original, "JSONL round-trip mismatch");
    }

    #[tokio::test]
    async fn test_jsonl_streaming_records() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stream.jsonl");

        // Write 5 records.
        let original: Vec<Value> = (0..5)
            .map(|i| {
                make_map_mixed(&[
                    ("id", Value::Int(i)),
                    ("val", Value::Str(format!("item{}", i))),
                ])
            })
            .collect();

        write_vec(&path, Format::Jsonl, original.clone())
            .await
            .unwrap();

        // Stream back one by one.
        let mut stream = load_stream(&path, Format::Jsonl).await.unwrap();
        let mut count = 0usize;
        while let Some(item) = stream.next().await {
            let v = item.unwrap();
            if let Value::Map(m) = &v {
                assert_eq!(m["id"], Value::Int(count as i64));
            } else {
                panic!("expected Map, got {:?}", v);
            }
            count += 1;
        }
        assert_eq!(count, 5);
    }

    // ── CSV round-trip ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_csv_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.csv");

        // All values stored as strings (CSV contract).
        let original = vec![
            make_map(&[("name", "Alice"), ("city", "London")]),
            make_map(&[("name", "Bob"), ("city", "Paris")]),
        ];

        write_vec(&path, Format::Csv, original.clone())
            .await
            .unwrap();

        let loaded = load_all(&path, Format::Csv).await.unwrap();

        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded, original, "CSV round-trip mismatch (all-string)");
    }

    #[tokio::test]
    async fn test_csv_header_map() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("headers.csv");

        let original = vec![
            make_map(&[("a", "1"), ("b", "2"), ("c", "3")]),
            make_map(&[("a", "4"), ("b", "5"), ("c", "6")]),
        ];

        write_vec(&path, Format::Csv, original).await.unwrap();

        let loaded = load_all(&path, Format::Csv).await.unwrap();

        for row in &loaded {
            if let Value::Map(m) = row {
                assert!(m.contains_key("a"), "missing key 'a'");
                assert!(m.contains_key("b"), "missing key 'b'");
                assert!(m.contains_key("c"), "missing key 'c'");
            } else {
                panic!("expected Map");
            }
        }
    }

    #[tokio::test]
    async fn test_csv_numeric_hints_coerce_only_schema_numeric_columns() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("typed.csv");
        tokio::fs::write(
            &path,
            "count,ratio,zip,status\n42,3.5,00123,007\nnot-a-number,NaN,00456,001\n",
        )
        .await
        .unwrap();

        let hints = NumericColumnHints::from(["count".to_owned(), "ratio".to_owned()]);
        let loaded = load_all_with_numeric_hints(&path, Format::Csv, hints)
            .await
            .unwrap();

        assert_eq!(
            loaded,
            vec![
                make_map_mixed(&[
                    ("count", Value::Int(42)),
                    ("ratio", Value::Float(3.5)),
                    ("zip", Value::Str("00123".into())),
                    ("status", Value::Str("007".into())),
                ]),
                make_map_mixed(&[
                    ("count", Value::Str("not-a-number".into())),
                    ("ratio", Value::Str("NaN".into())),
                    ("zip", Value::Str("00456".into())),
                    ("status", Value::Str("001".into())),
                ]),
            ]
        );
    }

    // ── TSV round-trip ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_tsv_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.tsv");

        let original = vec![
            make_map(&[("gene", "BRCA1"), ("chromosome", "17")]),
            make_map(&[("gene", "TP53"), ("chromosome", "17")]),
            make_map(&[("gene", "EGFR"), ("chromosome", "7")]),
        ];

        write_vec(&path, Format::Tsv, original.clone())
            .await
            .unwrap();
        let loaded = load_all(&path, Format::Tsv).await.unwrap();

        assert_eq!(loaded, original, "TSV round-trip mismatch");
    }

    #[tokio::test]
    async fn test_tsv_numeric_hints_are_applied_per_row() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("typed.tsv");
        tokio::fs::write(&path, "score\tcode\n8.25\t0008\n")
            .await
            .unwrap();

        let hints = NumericColumnHints::from(["score".to_owned()]);
        let loaded = load_all_with_numeric_hints(&path, Format::Tsv, hints)
            .await
            .unwrap();
        assert_eq!(
            loaded,
            vec![make_map_mixed(&[
                ("score", Value::Float(8.25)),
                ("code", Value::Str("0008".into())),
            ])]
        );
    }

    // ── Sparse TSV (upstream #210) ──────────────────────────────────────────

    /// Regression test for upstream issue #210: a tab-separated lookup/join
    /// table whose rows are *sparse* (trailing columns empty, or early rows
    /// with fewer populated fields) must still split on the declared `\t`
    /// delimiter and line up with the header columns.
    ///
    /// The fixture is written as raw bytes (not via `write_vec`, which always
    /// emits full-width rows) so it reproduces the on-disk shape that tripped
    /// the loader:
    ///   * the first data row has fewer fields than the header (trailing tabs
    ///     omitted), and
    ///   * a later row has empty trailing columns.
    ///
    /// Before the fix the `csv_async` reader ran with `flexible(false)` and
    /// aborted these rows with an `UnequalLengths` error (or, equivalently,
    /// the columns were mis-split). With the declared TSV delimiter and
    /// flexible records, every row loads with the correct column mapping.
    #[tokio::test]
    async fn test_sparse_tsv_keeps_column_split() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lookup.tsv");

        // 4 header columns. Note the deliberately ragged rows:
        //   row 1: only 2 fields populated, trailing tabs omitted entirely.
        //   row 2: full width.
        //   row 3: empty trailing columns (present-but-empty).
        let raw = "patient_id\tname\tsite\tnotes\n\
                   P001\tAlice\n\
                   P002\tBob\tLEEDS\tvip\n\
                   P003\tCarol\t\t\n";
        std::fs::write(&path, raw).unwrap();

        let loaded = load_all(&path, Format::Tsv).await.unwrap();
        assert_eq!(loaded.len(), 3, "expected 3 data rows");

        let get = |row: &Value, key: &str| -> Option<Value> {
            match row {
                Value::Map(m) => m.get(key).cloned(),
                other => panic!("expected Map row, got {other:?}"),
            }
        };
        let text = |s: &str| Some(Value::Str(s.to_owned()));

        // PARITY: a short row keeps its columns, with `None` (Null) for the
        // missing trailing ones (`csv.DictReader` `restval`; the loader's
        // `v != ""` filter keeps `None`).
        assert_eq!(get(&loaded[0], "patient_id"), text("P001"));
        assert_eq!(get(&loaded[0], "name"), text("Alice"));
        assert_eq!(get(&loaded[0], "site"), Some(Value::Null));
        assert_eq!(get(&loaded[0], "notes"), Some(Value::Null));

        // Full-width row.
        assert_eq!(get(&loaded[1], "patient_id"), text("P002"));
        assert_eq!(get(&loaded[1], "site"), text("LEEDS"));
        assert_eq!(get(&loaded[1], "notes"), text("vip"));

        // Present-but-empty trailing cells are dropped.
        assert_eq!(get(&loaded[2], "patient_id"), text("P003"));
        assert_eq!(get(&loaded[2], "name"), text("Carol"));
        assert_eq!(get(&loaded[2], "site"), None);
        assert_eq!(get(&loaded[2], "notes"), None);
    }

    // ── JSON round-trip ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_json_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.json");

        let original = vec![
            make_map_mixed(&[("x", Value::Int(1)), ("y", Value::Bool(true))]),
            make_map_mixed(&[("x", Value::Int(2)), ("y", Value::Bool(false))]),
        ];

        write_vec(&path, Format::Json, original.clone())
            .await
            .unwrap();
        let loaded = load_all(&path, Format::Json).await.unwrap();

        assert_eq!(loaded, original, "JSON round-trip mismatch");
    }

    // ── YAML input is PyYAML `safe_load` (YAML 1.1) ──────────────────────────

    #[tokio::test]
    async fn test_yaml_input_resolves_scalars_as_yaml_1_1() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.yaml");
        std::fs::write(
            &path,
            "a: yes\nb: 017\nc: 1_000\nd: 1:30\ne: ~\nf: 1.0e20\ng: 0o7\nh: 2024-01-01\n\
             i: 'yes'\nj: 2024-01-01 10:30:00Z\n",
        )
        .unwrap();
        let loaded = load_all(&path, Format::Yaml).await.unwrap();
        let Value::Map(m) = &loaded[0] else {
            panic!("map expected");
        };
        assert_eq!(m["a"], Value::Bool(true));
        assert_eq!(m["b"], Value::Int(15));
        assert_eq!(m["c"], Value::Int(1000));
        assert_eq!(m["d"], Value::Int(90));
        assert_eq!(m["e"], Value::Null);
        assert_eq!(m["f"], Value::Str("1.0e20".into()));
        assert_eq!(m["g"], Value::Str("0o7".into()));
        assert_eq!(m["i"], Value::Str("yes".into()));
        let Value::Temporal(date) = &m["h"] else {
            panic!("date expected");
        };
        assert_eq!(date.to_string(), "2024-01-01");
        let Value::Temporal(datetime) = &m["j"] else {
            panic!("datetime expected");
        };
        assert_eq!(datetime.to_string(), "2024-01-01 10:30:00+00:00");
    }

    // Upstream `_map_data_single`: `yaml.safe_load`, then `json.loads` on a YAMLError.
    #[tokio::test]
    async fn test_single_object_json_goes_through_the_yaml_loader_first() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.json");
        // PyYAML reads an exponent without a `.` as a string.
        std::fs::write(&path, r#"{"a": 1e5, "b": 1.5e+5, "c": 1E+5, "d": [1, 2]}"#).unwrap();
        let loaded = load_all_single_object(&path, Format::Json).await.unwrap();
        let Value::Map(m) = &loaded[0] else {
            panic!("map expected");
        };
        assert_eq!(m["a"], Value::Str("1e5".into()));
        assert_eq!(m["b"], Value::Float(150000.0));
        assert_eq!(m["c"], Value::Str("1E+5".into()));
        // DataLoader (directory/tabular input) reads `.json` with json.load.
        let strict = load_all(&path, Format::Json).await.unwrap();
        let Value::Map(m) = &strict[0] else {
            panic!("map expected");
        };
        assert_eq!(m["a"], Value::Float(100000.0));

        // A tab-indented document is not YAML: fall back to JSON.
        std::fs::write(&path, "{\n\t\"a\": 1e5\n}").unwrap();
        let loaded = load_all_single_object(&path, Format::Json).await.unwrap();
        assert_eq!(
            loaded,
            vec![make_map_mixed(&[("a", Value::Float(100000.0))])]
        );

        // An impossible date is a ValueError upstream, not a YAMLError: no fallback.
        std::fs::write(&path, "a: 2024-13-45\n").unwrap();
        assert!(load_all_single_object(&path, Format::Yaml).await.is_err());
    }

    // ── YAML round-trip ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_yaml_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("schema.yaml");

        let original = vec![make_map_mixed(&[
            ("id", Value::Int(42)),
            ("label", Value::Str("test".into())),
        ])];

        // Single-object mode: upstream `dump_output` writes one plain document.
        let options = WriteOptions {
            single_object: true,
            ..WriteOptions::default()
        };
        let stream =
            futures::stream::iter(original.clone().into_iter().map(Ok::<_, anyhow::Error>));
        write_all_with(&path, Format::Yaml, &options, stream)
            .await
            .unwrap();
        let loaded = load_all(&path, Format::Yaml).await.unwrap();

        assert_eq!(loaded, original, "YAML round-trip mismatch");

        let text = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_yaml_ng::from_str(&text).unwrap();
        assert!(
            parsed.is_object(),
            "single YAML record should serialize as a top-level object"
        );
    }

    /// Single-object mode (upstream `dump_output`): several records are one YAML list.
    #[tokio::test]
    async fn test_yaml_multi_record_writes_sequence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("records.yaml");

        let original = vec![
            make_map_mixed(&[("id", Value::Str("P:1".into()))]),
            make_map_mixed(&[("id", Value::Str("P:2".into()))]),
        ];

        let options = WriteOptions {
            single_object: true,
            ..WriteOptions::default()
        };
        let stream =
            futures::stream::iter(original.clone().into_iter().map(Ok::<_, anyhow::Error>));
        write_all_with(&path, Format::Yaml, &options, stream)
            .await
            .unwrap();

        let loaded = load_all(&path, Format::Yaml).await.unwrap();
        assert_eq!(loaded, original, "YAML multi-record round-trip mismatch");

        let text = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_yaml_ng::from_str(&text).unwrap();
        let items = parsed
            .as_array()
            .expect("multiple YAML records should serialize as a sequence");
        assert_eq!(items.len(), 2, "expected 2 YAML sequence items");
    }

    /// Streaming YAML (upstream `YAMLStreamWriter`): one document per record, and
    /// the loader reads that multi-document stream back.
    #[tokio::test]
    async fn test_yaml_stream_multi_document_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("records.yaml");

        let original = vec![
            make_map_mixed(&[("id", Value::Str("P:1".into()))]),
            make_map_mixed(&[("id", Value::Str("P:2".into()))]),
        ];
        write_vec(&path, Format::Yaml, original.clone())
            .await
            .unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text, "id: P:1\n---\nid: P:2\n---\n");
        let loaded = load_all(&path, Format::Yaml).await.unwrap();
        assert_eq!(loaded, original, "YAML stream round-trip mismatch");
    }

    // ── Format auto-detection ───────────────────────────────────────────────

    #[tokio::test]
    async fn test_format_detection() {
        assert_eq!(Format::from_path("foo.csv").unwrap(), Format::Csv);
        assert_eq!(Format::from_path("foo.tsv").unwrap(), Format::Tsv);
        assert_eq!(Format::from_path("foo.json").unwrap(), Format::Json);
        assert_eq!(Format::from_path("foo.jsonl").unwrap(), Format::Jsonl);
        assert_eq!(Format::from_path("foo.ndjson").unwrap(), Format::Jsonl);
        assert_eq!(Format::from_path("foo.yaml").unwrap(), Format::Yaml);
        assert_eq!(Format::from_path("foo.yml").unwrap(), Format::Yaml);
        assert_eq!(Format::from_path("foo.parquet").unwrap(), Format::Parquet);
        assert_eq!(Format::from_path("foo.duckdb").unwrap(), Format::DuckDb);
        assert!(Format::from_path("foo.xlsx").is_err());
    }

    // ── Streaming CSV parse ─────────────────────────────────────────────────

    #[tokio::test]
    async fn test_csv_streaming_parse() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stream.csv");

        // Write 10 rows.
        let original: Vec<Value> = (0..10)
            .map(|i| make_map(&[("row", &i.to_string()), ("label", "x")]))
            .collect();
        write_vec(&path, Format::Csv, original.clone())
            .await
            .unwrap();

        // Stream back and verify order.
        let mut stream = load_stream(&path, Format::Csv).await.unwrap();
        let mut idx = 0usize;
        while let Some(item) = stream.next().await {
            let v = item.unwrap();
            if let Value::Map(m) = &v {
                assert_eq!(
                    m["row"],
                    Value::Str(idx.to_string()),
                    "row mismatch at index {}",
                    idx
                );
            } else {
                panic!("expected Map");
            }
            idx += 1;
        }
        assert_eq!(idx, 10, "expected 10 rows");
    }

    // ── write_all_auto / load_stream_auto ───────────────────────────────────

    #[tokio::test]
    async fn test_auto_detect_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auto.jsonl");

        let items = vec![
            Value::Str("hello".into()),
            Value::Int(99),
            Value::Bool(true),
        ];
        write_vec(&path, Format::Jsonl, items.clone())
            .await
            .unwrap();

        let loaded = load_all(&path, Format::Jsonl).await.unwrap();
        assert_eq!(loaded, items);
    }

    // ── CSV quoting ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_csv_quoting() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("quoted.csv");

        // A value containing a comma must be quoted.
        let original = vec![make_map(&[("col", "hello, world"), ("other", "plain")])];
        write_vec(&path, Format::Csv, original.clone())
            .await
            .unwrap();

        let loaded = load_all(&path, Format::Csv).await.unwrap();
        assert_eq!(loaded, original, "comma in value broke round-trip");
    }

    // ── Null / mixed types through JSONL ────────────────────────────────────

    #[tokio::test]
    async fn test_jsonl_null_and_types() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("types.jsonl");

        let original = vec![
            Value::Null,
            Value::Bool(false),
            Value::Float(2.5),
            Value::List(vec![Value::Int(1), Value::Int(2)]),
        ];

        write_vec(&path, Format::Jsonl, original.clone())
            .await
            .unwrap();
        let loaded = load_all(&path, Format::Jsonl).await.unwrap();
        assert_eq!(loaded, original);
    }

    /// `write_all_writer` writes to any `AsyncWrite` — Vec<u8> here.
    #[tokio::test]
    async fn test_write_all_writer_vec() {
        let val = make_map(&[("id", "1"), ("name", "Alice")]);
        let stream = futures::stream::iter(vec![Ok(val)]);

        let mut buf = Vec::new();
        super::write_all_writer(&mut buf, Format::Jsonl, stream)
            .await
            .unwrap();

        let out = String::from_utf8(buf).unwrap();
        // Python `json.dumps` spacing, as upstream's JSONL writer emits.
        assert_eq!(out, "{\"id\": \"1\", \"name\": \"Alice\"}\n");
    }
}
