//! Property tests for the writers and loaders: round trips where a format is
//! lossless, and "never emits null" for the null-stripping writers.

#[path = "../../linkml-map-core/tests/common/mod.rs"]
mod common;

use common::*;
use indexmap::IndexMap;
use linkml_map_core::value::Value;
use linkml_map_io::pycompat::{sort_keys, strip_nulls, tabular_cell};
use linkml_map_io::{Format, dump_output, load_all, write_vec};
use proptest::prelude::*;
use std::path::Path;

fn block_on<T>(fut: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(fut)
}

/// Write `records` to a fresh file with `ext` and return the file's text and what the
/// loader makes of it.
fn write_and_load(
    dir: &Path,
    ext: &str,
    format: Format,
    records: Vec<Value>,
) -> (String, anyhow::Result<Vec<Value>>) {
    let path = dir.join(format!("out.{ext}"));
    block_on(async {
        write_vec(&path, format, records).await.expect("write");
        let text = std::fs::read_to_string(&path).expect("read back");
        (text, load_all(&path, format).await)
    })
}

/// Finite values, no decimals, floats that survive the JSON parser.
fn arb_lossless() -> BoxedStrategy<Value> {
    arb_value_with(true, false, false)
        .prop_map(|v| short_floats(&v))
        .boxed()
}

/// True when no map in `v` has a null value (nulls inside lists are kept by upstream).
fn no_null_map_values(v: &Value) -> bool {
    match v {
        Value::Map(m) => m.values().all(|x| !x.is_null() && no_null_map_values(x)),
        Value::List(items) => items.iter().all(no_null_map_values),
        _ => true,
    }
}

proptest! {
    #![proptest_config(config(64))]

    /// JSONL writer -> loader gives back the records with null map values stripped.
    #[test]
    fn jsonl_round_trip(records in arb_records(arb_lossless())) {
        let dir = tempfile::tempdir().unwrap();
        let (text, loaded) = write_and_load(dir.path(), "jsonl", Format::Jsonl, records.clone());
        prop_assert_eq!(text.lines().count(), records.len());
        let expected: Vec<Value> = records.iter().map(strip_nulls).collect();
        prop_assert_eq!(loaded.unwrap(), expected);
        // The writer never emits a null map value.
        for line in text.lines() {
            let parsed: Value = serde_json::from_str(line).unwrap();
            prop_assert!(no_null_map_values(&parsed), "null in {line}");
        }
    }

    /// JSON-array writer -> loader.
    #[test]
    fn json_round_trip(records in arb_records(arb_lossless())) {
        let dir = tempfile::tempdir().unwrap();
        let (text, loaded) = write_and_load(dir.path(), "json", Format::Json, records.clone());
        let expected: Vec<Value> = records.iter().map(strip_nulls).collect();
        // An empty array loads as zero records, a lone record as that record.
        prop_assert_eq!(loaded.unwrap(), expected);
        let parsed: Value = serde_json::from_str(&text).unwrap();
        prop_assert!(no_null_map_values(&parsed), "null in {text}");
    }

    /// YAML writer (one document per record, keys sorted) -> loader.
    #[test]
    fn yaml_round_trip(
        records in arb_records(
            arb_lossless()
                .prop_filter("1.1/1.2 ambiguity", |v| !has_ambiguous_string(v))
                .boxed()
        ),
    ) {
        // An empty YAML stream loads as one null record (like `yaml.safe_load("")`).
        prop_assume!(!records.is_empty());
        let dir = tempfile::tempdir().unwrap();
        let (text, loaded) = write_and_load(dir.path(), "yaml", Format::Yaml, records.clone());
        let expected: Vec<Value> = records.iter().map(|r| sort_keys(&strip_nulls(r))).collect();
        let loaded = loaded.unwrap_or_else(|e| panic!("does not load ({e:#}):\n{text}"));
        // A single record loads as one item and several as several: same as `expected`.
        prop_assert_eq!(&loaded, &expected, "text:\n{}", text);
        prop_assert!(no_null_map_values(&Value::List(loaded)), "null in {text}");
    }

    /// TSV and CSV: flat string-ish records round-trip as strings (what the loader yields
    /// without schema hints), including cells holding the separator, quotes and newlines.
    #[test]
    fn delimited_round_trip(
        keys in prop::collection::btree_set("[a-z_]{1,6}", 2..5),
        rows in prop::collection::vec(prop::collection::vec(
            prop_oneof![
                3 => "[^\\r]{0,12}",
                1 => prop::sample::select(vec!["a\tb", "a,b", "say \"hi\"", "two\nlines", " ", "", "\"", ",", "\t", "x\ty,z"]).prop_map(str::to_string),
            ],
            5), 0..6),
        csv in any::<bool>(),
    ) {
        let keys: Vec<String> = keys.into_iter().collect();
        let records: Vec<Value> = rows
            .iter()
            .map(|cells| {
                Value::Map(
                    keys.iter()
                        .zip(cells)
                        .map(|(k, c)| (k.clone(), Value::Str(c.clone())))
                        .collect::<IndexMap<_, _>>(),
                )
            })
            .collect();
        let (format, ext) = if csv { (Format::Csv, "csv") } else { (Format::Tsv, "tsv") };
        let dir = tempfile::tempdir().unwrap();
        let (text, loaded) = write_and_load(dir.path(), ext, format, records.clone());
        prop_assert_eq!(loaded.unwrap(), records, "text:\n{}", text);
    }

    /// Scalars of every type written to TSV come back as the text `str()` gives them.
    #[test]
    fn tsv_scalars_come_back_as_text(
        cells in prop::collection::vec(arb_scalar(false, false), 2..5),
    ) {
        let record = Value::Map(
            cells.iter().enumerate().map(|(i, v)| (format!("c{i}"), v.clone())).collect::<IndexMap<_, _>>(),
        );
        let expected = Value::Map(
            cells.iter().enumerate()
                .map(|(i, v)| (format!("c{i}"), Value::Str(tabular_cell(v))))
                .collect::<IndexMap<_, _>>(),
        );
        // Cells with a bare carriage return or a lone-empty first column are not representable.
        prop_assume!(!cells.iter().any(|v| matches!(v, Value::Str(s) if s.contains('\r'))));
        let dir = tempfile::tempdir().unwrap();
        let (text, loaded) = write_and_load(dir.path(), "tsv", Format::Tsv, vec![record]);
        prop_assert_eq!(loaded.unwrap(), vec![expected], "text:\n{}", text);
    }

    /// The writers never panic on arbitrary values (including NaN, decimals and nested
    /// containers); a record that cannot be written is an `Err`.
    #[test]
    fn writers_never_panic(v in arb_value(), single in any::<bool>()) {
        for format in [Format::Json, Format::Jsonl, Format::Yaml, Format::Tsv, Format::Csv] {
            let _ = dump_output(&v, format);
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(format!("o.{}", format.extension()));
            let values = if single { vec![v.clone()] } else { vec![v.clone(), v.clone()] };
            let _ = block_on(write_vec(&path, format, values));
        }
    }

    /// Loaders return `Err` or records for arbitrary bytes, never panic.
    #[test]
    fn loaders_never_panic(
        bytes in prop_oneof![
            prop::collection::vec(any::<u8>(), 0..200),
            "\\PC{0,200}".prop_map(String::into_bytes),
            "[a-z0-9:,\\t\" \\n{}\\[\\]-]{0,120}".prop_map(String::into_bytes),
        ],
    ) {
        let dir = tempfile::tempdir().unwrap();
        for format in [Format::Json, Format::Jsonl, Format::Yaml, Format::Tsv, Format::Csv] {
            let path = dir.path().join(format!("in.{}", format.extension()));
            std::fs::write(&path, &bytes).unwrap();
            let _ = block_on(load_all(&path, format));
        }
    }

    /// `dump_output` (the single-object path) never emits a null map value in JSON/YAML.
    #[test]
    fn dump_output_has_no_null_map_values(v in arb_lossless()) {
        let json = dump_output(&v, Format::Json).unwrap();
        let parsed: Value = serde_json::from_str(&json).unwrap();
        prop_assert!(no_null_map_values(&parsed), "null in {json}");
    }
}

fn has_ambiguous_string(v: &Value) -> bool {
    match v {
        Value::Str(s) => ambiguous_between_1_1_and_1_2(s),
        Value::List(items) => items.iter().any(has_ambiguous_string),
        Value::Map(m) => m.values().any(has_ambiguous_string),
        _ => false,
    }
}
