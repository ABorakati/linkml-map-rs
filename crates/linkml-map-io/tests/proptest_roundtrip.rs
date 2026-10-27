//! Property tests for the writers and loaders: round trips where a format is
//! lossless, and "never emits null" for the null-stripping writers.

#[path = "../../linkml-map-core/tests/common/mod.rs"]
mod common;

use common::*;
use indexmap::IndexMap;
use linkml_map_core::value::Value;
use linkml_map_io::pycompat::{sort_keys, strip_nulls, tabular_cell};
use linkml_map_io::pyyaml::{DumpOptions, dump_yaml};
use linkml_map_io::yaml_load::safe_load_all;
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

    /// YAML writer (one document per record, keys sorted) -> loader. The loader has PyYAML
    /// `safe_load` semantics, so no string is ambiguous: `yes`, `1e3` and `017` all come back
    /// as the strings that were written.
    #[test]
    fn yaml_round_trip(records in arb_records(arb_lossless())) {
        // An empty YAML stream loads as one null record (like `yaml.safe_load("")`).
        prop_assume!(!records.is_empty());
        let dir = tempfile::tempdir().unwrap();
        let (text, loaded) = write_and_load(dir.path(), "yaml", Format::Yaml, records.clone());
        let expected: Vec<Value> = records.iter().map(|r| sort_keys(&strip_nulls(r))).collect();
        let loaded = loaded.unwrap_or_else(|e| panic!("does not load ({e:#}):\n{text}"));
        prop_assert_eq!(&loaded, &expected, "text:\n{}", text);
        prop_assert!(no_null_map_values(&Value::List(loaded)), "null in {}", text);
    }

    /// The PyYAML-compatible emitter and the PyYAML-compatible loader are inverse on
    /// everything PyYAML itself round-trips: any string (including `yes`, `1e3`, `017`,
    /// `2001-12-14`), ints, floats (NaN and infinities too), booleans, null, dates and
    /// datetimes, nested containers, any option combination.
    #[test]
    fn emit_then_safe_load_is_the_identity(
        v in arb_yaml_value(),
        default_flow_style in any::<bool>(),
        allow_unicode in any::<bool>(),
        sort_keys in any::<bool>(),
    ) {
        let text = dump_yaml(&v, DumpOptions { default_flow_style, allow_unicode, sort_keys });
        // A double-quoted scalar folded over lines (`\` at a line end) is read back wrongly by
        // PyYAML itself (upstream emitter bug); the port is byte-identical to it.
        prop_assume!(!text.contains("\\\n"));
        let docs = safe_load_all(&text).unwrap_or_else(|e| panic!("does not load ({e}):\n{text}"));
        prop_assert_eq!(docs.len(), 1, "text:\n{}", text);
        prop_assert!(same(&v, &docs[0]), "text:\n{}\nloaded: {:?}", text, docs[0]);
    }

    /// `safe_load_all` (libyaml underneath, the only `unsafe` in the crate) returns `Ok` or
    /// `Err` for arbitrary text, never panics; and what loads re-emits and re-loads equal.
    #[test]
    fn safe_load_never_panics_and_is_stable(
        text in prop_oneof![
            any::<String>(),
            "\\PC{0,200}",
            "[a-z0-9:,\\-\\[\\]{}!&*|>'\" #?%@`\\n\\t\\\\]{0,120}",
            arb_yaml_value().prop_map(|v| dump_yaml(&v, DumpOptions::default())),
        ],
    ) {
        let Ok(docs) = safe_load_all(&text) else { return Ok(()) };
        for doc in &docs {
            if contains_decimal(doc) { continue; }
            let again = dump_yaml(doc, DumpOptions::default());
            if again.contains("\\\n") { continue; }
            let reloaded = safe_load_all(&again)
                .unwrap_or_else(|e| panic!("re-emitted text does not load ({e}):\n{again}"));
            prop_assert_eq!(reloaded.len(), 1);
            prop_assert!(same(doc, &reloaded[0]), "doc {:?}\nre-emitted:\n{}\nreloaded {:?}", doc, again, reloaded[0]);
        }
    }

    /// TSV and CSV: flat records of words round-trip. The loader mirrors upstream's
    /// `_DelimitedFileLoader._rows`, so a cell equal to `""` is dropped (its key is absent)
    /// and a row whose cells are all empty is dropped; cells holding the separator, quotes
    /// and newlines survive through quoting.
    #[test]
    fn delimited_round_trip(
        keys in prop::collection::btree_set("[a-z_]{1,6}", 2..5),
        rows in prop::collection::vec(prop::collection::vec(
            prop_oneof![
                3 => "[a-zA-Z][^\\r]{0,11}",
                1 => prop::sample::select(vec!["a\tb", "a,b", "say \"hi\"", "two\nlines", "\"", ",", "\t", "x\ty,z", ""]).prop_map(str::to_string),
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
        // The loader's view: empty cells dropped, all-empty rows dropped.
        let expected: Vec<Value> = rows
            .iter()
            .filter(|cells| keys.iter().zip(*cells).any(|(_, c)| !c.is_empty()))
            .map(|cells| {
                Value::Map(
                    keys.iter()
                        .zip(cells)
                        .filter(|(_, c)| !c.is_empty())
                        .map(|(k, c)| (k.clone(), Value::Str(c.clone())))
                        .collect::<IndexMap<_, _>>(),
                )
            })
            .collect();
        let (format, ext) = if csv { (Format::Csv, "csv") } else { (Format::Tsv, "tsv") };
        let dir = tempfile::tempdir().unwrap();
        let (text, loaded) = write_and_load(dir.path(), ext, format, records);
        prop_assert_eq!(loaded.unwrap(), expected, "text:\n{}", text);
    }

    /// Scalars of every type written to TSV come back as what Python's `csv` + `_parse_numeric`
    /// make of their `str()` text: numbers for numeric text, strings otherwise, nothing for `""`.
    #[test]
    fn tsv_scalars_come_back_as_their_text(
        cells in prop::collection::vec(arb_scalar(false, false), 2..5),
    ) {
        // A bare carriage return is not representable in a cell.
        prop_assume!(!cells.iter().any(|v| matches!(v, Value::Str(s) if s.contains('\r'))));
        let record = Value::Map(
            cells.iter().enumerate().map(|(i, v)| (format!("c{i}"), v.clone())).collect::<IndexMap<_, _>>(),
        );
        let dir = tempfile::tempdir().unwrap();
        let (text, loaded) = write_and_load(dir.path(), "tsv", Format::Tsv, vec![record]);
        let loaded = loaded.unwrap();
        // `skipinitialspace=True`: an unquoted cell of spaces reads back as `""` (dropped).
        let effectively_empty = |t: &str| {
            t.trim_start_matches(' ').is_empty() && !t.contains(['\t', '"', '\n'])
        };
        if cells.iter().all(|v| effectively_empty(&tabular_cell(v))) {
            prop_assert!(loaded.is_empty(), "an all-empty row is dropped; text:\n{}", text);
            return Ok(());
        }
        let Some(Value::Map(row)) = loaded.first() else {
            return Err(TestCaseError::fail(format!("no row loaded; text:\n{text}")));
        };
        for (i, v) in cells.iter().enumerate() {
            let got = row.get(&format!("c{i}"));
            let want_text = tabular_cell(v);
            if effectively_empty(&want_text) {
                prop_assert!(got.is_none(), "empty cell c{i} should be absent, got {got:?}");
                continue;
            }
            match (v, got) {
                (Value::Int(a), Some(Value::Int(b))) => prop_assert_eq!(a, b),
                (Value::Float(a), Some(b)) if a.is_finite() => {
                    prop_assert_eq!(Some(*a), b.try_numeric(), "float c{} text {:?}", i, want_text)
                }
                (Value::Bool(_), Some(b)) => prop_assert_eq!(b, &Value::Str(want_text)),
                // Text that looks numeric becomes a number; anything else stays text.
                (Value::Str(s), Some(Value::Str(t))) => {
                    // Quoted cells keep leading spaces; unquoted ones lose them.
                    let want = if s.contains(['\t', '"', '\n']) { s.as_str() } else { s.trim_start_matches(' ') };
                    prop_assert_eq!(want, t.as_str())
                }
                (_, Some(_)) => {}
                (_, None) => prop_assert!(false, "cell c{i} ({v:?}) lost; text:\n{text}"),
            }
        }
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

fn contains_decimal(v: &Value) -> bool {
    match v {
        Value::Decimal(_) => true,
        Value::List(items) => items.iter().any(contains_decimal),
        Value::Map(m) => m.values().any(contains_decimal),
        _ => false,
    }
}

/// Equality that treats NaN as equal to itself and ignores map key order.
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Float(x), Value::Float(y)) => x == y || (x.is_nan() && y.is_nan()),
        (Value::List(x), Value::List(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q))
        }
        (Value::Map(x), Value::Map(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w)))
        }
        _ => a == b,
    }
}

/// Values YAML can carry exactly (no decimals): awkward strings, special floats, temporals.
fn arb_yaml_value() -> BoxedStrategy<Value> {
    let leaf = prop_oneof![
        2 => Just(Value::Null),
        2 => any::<bool>().prop_map(Value::Bool),
        3 => any::<i64>().prop_map(Value::Int),
        3 => prop_oneof![any::<f64>(), Just(f64::INFINITY), Just(f64::NEG_INFINITY), Just(f64::NAN), Just(1e20), Just(1e-7), Just(-0.0)].prop_map(Value::Float),
        2 => arb_temporal().prop_map(Value::Temporal),
        6 => arb_pyyaml_string().prop_map(Value::Str),
    ];
    leaf.prop_recursive(4, 40, 5, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..5).prop_map(Value::List),
            prop::collection::vec((arb_pyyaml_string(), inner), 0..5)
                .prop_map(|pairs| Value::Map(pairs.into_iter().collect())),
        ]
    })
    .boxed()
}

/// Any awkward string except those PyYAML itself cannot round-trip: U+0085 (NEL, a YAML 1.1
/// line break it writes inside a quoted scalar and reads back as a space).
fn arb_pyyaml_string() -> impl Strategy<Value = String> {
    arb_any_tricky_string().prop_filter("NEL", |s| !s.contains('\u{85}'))
}
