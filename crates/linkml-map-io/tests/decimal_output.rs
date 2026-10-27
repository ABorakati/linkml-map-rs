//! Decimals are written exactly by every text format.
//!
//! Port of upstream `tests/test_writers/test_decimal_output.py` (72c0c28), the
//! text-format half (the Parquet/DuckDB writers do not exist in Rust). Upstream
//! reads each format back with a decimal-aware reader; here the exact literal
//! tokens are asserted on the written text, since reading back through `f64`
//! would hide a rounding bug.
//!
//! Output spellings: JSON/JSONL use `str(Decimal)`, YAML and CSV/TSV cells use
//! `format(d, 'f')`.

use futures::stream;
use linkml_map_core::{decimal::Decimal, value::Value};
use linkml_map_io::{Format, write_all_writer};

fn dec(s: &str) -> Value {
    Value::Decimal(Decimal::parse_python(s).unwrap())
}

fn s(v: &str) -> Value {
    Value::Str(v.into())
}

fn map(pairs: Vec<(&str, Value)>) -> Value {
    Value::Map(pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
}

fn rows() -> Vec<Value> {
    vec![
        map(vec![
            ("id", s("a")),
            ("v", dec("52.30")),
            (
                "q",
                map(vec![
                    ("unit", s("mg")),
                    ("val", dec("0.123456789012345678901")),
                ]),
            ),
            ("vs", Value::List(vec![dec("1"), dec("2.5")])),
        ]),
        map(vec![
            ("id", s("b")),
            ("v", dec("7")),
            ("q", map(vec![("unit", s("g")), ("val", dec("1E-7"))])),
            ("vs", Value::List(vec![dec("-3.25")])),
        ]),
    ]
}

async fn write(format: Format, values: Vec<Value>) -> String {
    let mut buf: Vec<u8> = Vec::new();
    let items = stream::iter(values.into_iter().map(Ok::<Value, anyhow::Error>));
    write_all_writer(&mut buf, format, items).await.unwrap();
    String::from_utf8(buf).unwrap()
}

#[tokio::test]
async fn jsonl_uses_str_decimal() {
    let text = write(Format::Jsonl, rows()).await;
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines,
        [
            r#"{"id":"a","v":52.30,"q":{"unit":"mg","val":0.123456789012345678901},"vs":[1,2.5]}"#,
            r#"{"id":"b","v":7,"q":{"unit":"g","val":1E-7},"vs":[-3.25]}"#,
        ]
    );
    // Each line is still valid JSON.
    for line in lines {
        serde_json::from_str::<serde_json::Value>(line).unwrap();
    }
}

#[tokio::test]
async fn json_uses_str_decimal() {
    let text = write(Format::Json, rows()).await;
    assert!(text.contains("\"v\": 52.30,"), "{text}");
    assert!(text.contains("\"val\": 0.123456789012345678901"), "{text}");
    assert!(text.contains("\"val\": 1E-7"), "{text}");
    assert!(text.contains("-3.25"), "{text}");
    // Numbers, not strings.
    assert!(!text.contains("\"52.30\""), "{text}");
    let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(parsed.as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn yaml_uses_fixed_plain_scalars() {
    let text = write(Format::Yaml, rows()).await;
    assert!(text.contains("v: 52.30\n"), "{text}");
    assert!(text.contains("v: 7\n"), "{text}");
    assert!(text.contains("val: 0.123456789012345678901\n"), "{text}");
    // `format(d, 'f')`: no exponent notation in YAML.
    assert!(text.contains("val: 0.0000001\n"), "{text}");
    assert!(text.contains("- -3.25\n"), "{text}");
    assert!(!text.contains('E'), "{text}");
    // Plain scalars: YAML reads them back as numbers.
    let back: serde_yaml_ng::Value = serde_yaml_ng::from_str(&text).unwrap();
    assert!(
        back[0]["v"].is_number() && back[1]["v"].is_number(),
        "{text}"
    );
}

#[tokio::test]
async fn single_object_yaml_matches_upstream_doctest() {
    // dump_yaml({"v": Decimal("52.30"), "n": Decimal("52")}) -> "n: 52\nv: 52.30\n"
    // (PyYAML sorts keys; the Rust writer keeps insertion order.)
    let text = write(
        Format::Yaml,
        vec![map(vec![("n", dec("52")), ("v", dec("52.30"))])],
    )
    .await;
    assert_eq!(text, "n: 52\nv: 52.30\n");
}

#[tokio::test]
async fn tsv_and_csv_cells() {
    for (format, sep) in [(Format::Tsv, '\t'), (Format::Csv, ',')] {
        let text = write(format, rows()).await;
        let lines: Vec<&str> = text.lines().collect();
        let first: Vec<&str> = lines[1].split(sep).collect();
        // Scalar cell: `format_decimal`.
        assert_eq!(first[1], "52.30", "{text}");
        // Nested containers are JSON cells with `str(Decimal)` literals.
        assert!(lines[1].contains("0.123456789012345678901"), "{text}");
        assert!(lines[1].contains("[1,2.5]"), "{text}");
        assert!(lines[2].contains("1E-7"), "{text}");
        assert!(lines[2].contains("[-3.25]"), "{text}");
        assert!(lines[2].starts_with(&format!("b{sep}7{sep}")), "{text}");
    }
}

#[tokio::test]
async fn csv_scalar_decimal_uses_fixed_spelling() {
    let text = write(
        Format::Csv,
        vec![map(vec![
            ("id", s("a")),
            ("v", dec("1E-7")),
            ("w", dec("1E+3")),
        ])],
    )
    .await;
    assert_eq!(text, "id,v,w\na,0.0000001,1000\n");
}
