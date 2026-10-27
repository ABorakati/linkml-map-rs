//! Differential test of `linkml_map_core::pyyaml` against PyYAML.
//!
//! `tests/pyyaml/pyyaml_cases.json` holds values together with the text that
//! upstream's `dump_yaml` (PyYAML's pure-Python emitter, upstream's
//! `_DecimalDumper`) produced for them under each option set; regenerate it
//! with `python3 tests/pyyaml/generate.py`. Every case must match byte for
//! byte.

use linkml_map_core::decimal::Decimal;
use linkml_map_core::pyyaml::{DumpOptions, dump_yaml};
use linkml_map_core::value::Value;
use serde_json::Value as Json;

fn decode(json: &Json) -> Value {
    match json {
        Json::Null => Value::Null,
        Json::Bool(b) => Value::Bool(*b),
        Json::Number(n) => match n.as_i64() {
            Some(i) => Value::Int(i),
            None => Value::Float(n.as_f64().expect("finite float")),
        },
        Json::String(s) => Value::Str(s.clone()),
        Json::Array(items) => Value::List(items.iter().map(decode).collect()),
        Json::Object(map) => {
            if let Some(Json::String(kind)) = map.get("$float") {
                return Value::Float(match kind.as_str() {
                    "inf" => f64::INFINITY,
                    "-inf" => f64::NEG_INFINITY,
                    "nan" => f64::NAN,
                    other => panic!("unknown $float {other}"),
                });
            }
            if let Some(Json::String(text)) = map.get("$decimal") {
                return Value::Decimal(Decimal::parse_python(text).expect("decimal"));
            }
            Value::Map(map.iter().map(|(k, v)| (k.clone(), decode(v))).collect())
        }
    }
}

#[test]
fn matches_pyyaml_on_the_whole_corpus() {
    // `PYYAML_CORPUS=<file>` swaps in a larger corpus from `generate.py`.
    let text = match std::env::var_os("PYYAML_CORPUS") {
        Some(path) => std::fs::read_to_string(path).unwrap(),
        None => include_str!("pyyaml/pyyaml_cases.json").to_owned(),
    };
    let corpus: Json = serde_json::from_str(&text).unwrap();
    let cases = corpus["cases"].as_array().unwrap();
    assert!(cases.len() > 1000);
    let mut failures = Vec::new();
    for case in cases {
        let opts = &case["opts"];
        let opts = DumpOptions {
            default_flow_style: opts["flow"].as_bool().unwrap(),
            allow_unicode: opts["unicode"].as_bool().unwrap(),
            sort_keys: opts["sort"].as_bool().unwrap(),
        };
        let value = decode(&case["value"]);
        let want = case["expected"].as_str().unwrap();
        let got = dump_yaml(&value, opts);
        if got != want {
            failures.push(format!(
                "opts {opts:?}\nvalue {:?}\nwant {want:?}\n got {got:?}\n",
                case["value"]
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} cases differ from PyYAML; first few:\n{}",
        failures.len(),
        cases.len(),
        failures
            .iter()
            .take(8)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}
