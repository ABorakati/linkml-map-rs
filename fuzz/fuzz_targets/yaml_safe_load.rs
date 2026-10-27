#![no_main]
//! `yaml_load::safe_load_all`: PyYAML `safe_load` semantics over libyaml's event parser
//! (`libyaml.rs`, the only `unsafe` in the workspace). Run under the default AddressSanitizer
//! of `cargo fuzz`. Whatever loads must also re-emit and re-load to the same value.

use fuzz_support::{same, value_has_nel};
use libfuzzer_sys::fuzz_target;
use linkml_map_core::pyyaml::{DumpOptions, dump_yaml};
use linkml_map_core::value::Value;
use linkml_map_io::yaml_load::safe_load_all;

fn has_decimal(v: &Value) -> bool {
    match v {
        Value::Decimal(_) => true,
        Value::List(items) => items.iter().any(has_decimal),
        Value::Map(m) => m.values().any(has_decimal),
        _ => false,
    }
}

fuzz_target!(|data: &[u8]| {
    // libyaml takes UTF-8 text; invalid UTF-8 is the caller's error before it gets here.
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let Ok(docs) = safe_load_all(text) else {
        return;
    };
    for doc in docs {
        if has_decimal(&doc) || value_has_nel(&doc) {
            continue;
        }
        let again = dump_yaml(&doc, DumpOptions::default());
        // Folded double-quoted scalars are an upstream PyYAML round-trip bug (see pyyaml_emit).
        if again.contains("\\\n") {
            continue;
        }
        let reloaded = safe_load_all(&again).unwrap_or_else(|e| panic!("re-emitted text does not load ({e}):\n{again}"));
        assert_eq!(reloaded.len(), 1);
        assert!(same(&doc, &reloaded[0]), "doc {doc:?}\nre-emitted:\n{again}\nreloaded {:?}", reloaded[0]);
    }
});
