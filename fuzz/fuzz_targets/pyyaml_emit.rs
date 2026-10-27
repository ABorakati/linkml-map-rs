#![no_main]
//! The PyYAML-compatible emitter: never panics, and its text loads back (through the
//! PyYAML-compatible `safe_load_all`, libyaml underneath) to the same value.

use fuzz_support::{FuzzValue, UNGUARDED, same, value_has_nel};
use libfuzzer_sys::{arbitrary::Arbitrary, fuzz_target};
use linkml_map_core::pyyaml::{DumpOptions, dump_yaml};
use linkml_map_io::yaml_load::safe_load_all;
use linkml_map_core::value::Value;

#[derive(Debug, Arbitrary)]
struct Input {
    value: FuzzValue,
    default_flow_style: bool,
    allow_unicode: bool,
    sort_keys: bool,
}

fn has_decimal(v: &Value) -> bool {
    match v {
        Value::Decimal(_) => true,
        Value::List(items) => items.iter().any(has_decimal),
        Value::Map(m) => m.values().any(has_decimal),
        _ => false,
    }
}

fuzz_target!(|input: Input| {
    let value = input.value.into_value();
    let opts = DumpOptions {
        default_flow_style: input.default_flow_style,
        allow_unicode: input.allow_unicode,
        sort_keys: input.sort_keys,
    };
    let text = dump_yaml(&value, opts);
    assert_eq!(text, dump_yaml(&value, opts), "emitter is not deterministic");
    // Decimals are plain numeric scalars, not strings: not comparable.
    if has_decimal(&value) || (!UNGUARDED && value_has_nel(&value)) {
        return;
    }
    // A double-quoted scalar folded over several lines (`\` at a line end): PyYAML's own
    // emitter then writes text that PyYAML itself reads back wrongly (an upstream PyYAML bug,
    // e.g. the key "=\n\x08  " at column 78 loads as "=\n\x08\\   "), and the port is
    // byte-identical to it.
    if !UNGUARDED && text.contains("\\\n") {
        return;
    }
    match safe_load_all(&text) {
        Ok(docs) if docs.len() == 1 => {
            assert!(same(&value, &docs[0]), "round trip differs\n{text}\nloaded: {:?}\noriginal: {value:?}", docs[0])
        }
        Ok(docs) => panic!("{} documents from one emit:\n{text}", docs.len()),
        Err(e) => panic!("emitted YAML does not load ({e}):\n{text}"),
    }
});
