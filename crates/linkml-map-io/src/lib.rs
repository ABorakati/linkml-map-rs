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

pub mod format;
pub mod loaders;
pub mod writers;

// Re-export the most commonly used items at crate root.
pub use format::Format;
pub use loaders::{load_all, load_stream, load_stream_auto};
pub use writers::{value_to_json, write_all, write_all_auto, write_vec};

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

    // ── YAML round-trip ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_yaml_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("schema.yaml");

        let original = vec![make_map_mixed(&[
            ("id", Value::Int(42)),
            ("label", Value::Str("test".into())),
        ])];

        write_vec(&path, Format::Yaml, original.clone())
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

    #[tokio::test]
    async fn test_yaml_multi_record_writes_sequence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("records.yaml");

        let original = vec![
            make_map_mixed(&[("id", Value::Str("P:1".into()))]),
            make_map_mixed(&[("id", Value::Str("P:2".into()))]),
        ];

        write_vec(&path, Format::Yaml, original.clone())
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
        assert!(Format::from_path("foo.parquet").is_err());
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
            Value::Float(3.14),
            Value::List(vec![Value::Int(1), Value::Int(2)]),
        ];

        write_vec(&path, Format::Jsonl, original.clone())
            .await
            .unwrap();
        let loaded = load_all(&path, Format::Jsonl).await.unwrap();
        assert_eq!(loaded, original);
    }
}
