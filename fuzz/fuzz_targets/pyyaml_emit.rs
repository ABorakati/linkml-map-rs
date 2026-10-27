#![no_main]
//! The PyYAML-compatible emitter: never panics, and for values with no YAML 1.1/1.2
//! ambiguity the text parses back to the same value.

use fuzz_support::{FuzzValue, UNGUARDED, same, value_has_ambiguous_string};
use libfuzzer_sys::{arbitrary::Arbitrary, fuzz_target};
use linkml_map_core::pyyaml::{DumpOptions, dump_yaml};
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
    if has_decimal(&value) || value_has_ambiguous_string(&value) {
        return;
    }
    // A double-quoted scalar folded over several lines (`\` at a line end): PyYAML's own
    // emitter then writes text that PyYAML itself reads back wrongly (an upstream PyYAML bug,
    // e.g. the key "=\n\x08  " at column 78 loads as "=\n\x08\\   "), and the port is
    // byte-identical to it.
    if !UNGUARDED && text.contains("\\\n") {
        return;
    }
    match serde_yaml_ng::from_str::<Value>(&text) {
        Ok(back) => assert!(same(&value, &back), "round trip differs\n{text}\nparsed: {back:?}\noriginal: {value:?}"),
        Err(e) => panic!("emitted YAML does not parse ({e}):\n{text}"),
    }
});
