//! Property tests for the PyYAML-compatible emitter (`linkml_map_core::pyyaml`).
//!
//! Whatever `Value` is emitted, the emitter must not panic, and for values with no
//! YAML-1.1 ambiguity the text must parse back (with `serde_yaml_ng`, a YAML 1.2 core
//! schema parser) to the same value: the emitter quotes every string that a 1.1 resolver
//! would read as another type, which a 1.2 parser then also reads as a string.

mod common;

use common::*;
use linkml_map_core::pyyaml::{DumpOptions, dump_yaml};
use linkml_map_core::value::Value;
use proptest::prelude::*;

fn shown<T: std::fmt::Debug>(r: &T) -> String {
    format!("{r:?}")
}

fn arb_opts() -> impl Strategy<Value = DumpOptions> {
    (any::<bool>(), any::<bool>(), any::<bool>()).prop_map(
        |(default_flow_style, allow_unicode, sort_keys)| DumpOptions {
            default_flow_style,
            allow_unicode,
            sort_keys,
        },
    )
}

/// A value of the shapes whose text the parser reads back identically: no decimals
/// (those are plain numeric scalars, not strings) and finite-or-special floats.
fn arb_round_trip_value() -> BoxedStrategy<Value> {
    let leaf = prop_oneof![
        2 => Just(Value::Null),
        2 => any::<bool>().prop_map(Value::Bool),
        3 => any::<i64>().prop_map(Value::Int),
        3 => prop_oneof![
            any::<f64>(),
            Just(f64::INFINITY),
            Just(f64::NEG_INFINITY),
            Just(f64::NAN),
            Just(1e20),
            Just(1e-7),
            Just(100000.0),
            Just(-0.0),
        ].prop_map(Value::Float),
        6 => arb_tricky_string().prop_map(Value::Str),
    ];
    leaf.prop_recursive(4, 40, 5, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..5).prop_map(Value::List),
            prop::collection::vec((arb_tricky_string(), inner), 0..5)
                .prop_map(|pairs| Value::Map(pairs.into_iter().collect())),
        ]
    })
    .boxed()
}

/// Round-trip equality that treats NaN as equal to itself and ignores key order
/// (`IndexMap` equality already does).
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Float(x), Value::Float(y)) => {
            x.to_bits() == y.to_bits() || (x.is_nan() && y.is_nan()) || x == y
        }
        (Value::List(x), Value::List(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q))
        }
        (Value::Map(x), Value::Map(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w)))
        }
        _ => a == b,
    }
}

proptest! {
    #![proptest_config(config(256))]

    /// Emitting never panics, for any value (decimals and NaN included) and options.
    #[test]
    fn emitter_never_panics(v in arb_value(), opts in arb_opts()) {
        let _ = dump_yaml(&v, opts);
    }

    /// Emitting is deterministic.
    #[test]
    fn emitter_is_deterministic(v in arb_value(), opts in arb_opts()) {
        prop_assert_eq!(dump_yaml(&v, opts), dump_yaml(&v, opts));
    }

    /// Emitted text parses back to the same value (single document, any options).
    #[test]
    fn emitted_yaml_round_trips(v in arb_round_trip_value(), opts in arb_opts()) {
        let text = dump_yaml(&v, opts);
        // A double-quoted scalar folded over lines (`\` at a line end) is not read back
        // correctly even by PyYAML (upstream emitter bug); the port is byte-identical to it.
        prop_assume!(!text.contains("\\\n"));
        let parsed: Result<Value, _> = serde_yaml_ng::from_str(&text);
        match parsed {
            Ok(back) => prop_assert!(same(&v, &back), "text:\n{text}\nparsed: {}", shown(&back)),
            Err(e) => prop_assert!(false, "does not parse ({e}):\n{text}"),
        }
    }

    /// Block output has no tab indentation and ends with a newline (or `...` marker).
    #[test]
    fn emitted_text_is_well_formed(v in arb_round_trip_value()) {
        let text = dump_yaml(&v, DumpOptions::default());
        prop_assert!(text.ends_with('\n'), "no trailing newline: {text:?}");
        prop_assert!(!text.lines().any(|l| l.starts_with('\t')), "tab-indented line in {text:?}");
    }
}
