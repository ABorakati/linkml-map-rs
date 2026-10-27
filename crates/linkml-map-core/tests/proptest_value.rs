//! Property tests for the value model: serde round trips, Python-style equality,
//! `Decimal` text semantics and the canonical form behind content-id synthesis.

mod common;

use common::*;
use indexmap::IndexMap;
use linkml_map_core::decimal::{Decimal, DecimalError};
use linkml_map_core::engine::content_id::{ContentIds, canonical_json};
use linkml_map_core::value::Value;
use proptest::prelude::*;
use uuid::Uuid;

/// `Debug` text: equality that treats `NaN == NaN`.
fn shown<T: std::fmt::Debug>(r: &T) -> String {
    format!("{r:?}")
}

// ── Decimal ──────────────────────────────────────────────────────────────────

/// Valid decimal literals in Python's spelling, exponents kept modest so that
/// the fixed-point rendering stays small.
fn arb_decimal_text() -> impl Strategy<Value = String> {
    (
        prop::sample::select(vec!["", "-", "+"]),
        "[0-9]{1,30}",
        proptest::option::of("[0-9]{0,15}"),
        proptest::option::of((
            prop::sample::select(vec!["e", "E"]),
            prop::sample::select(vec!["", "-", "+"]),
            0u32..400,
        )),
        prop::sample::select(vec!["", " ", "\t", "\n "]),
    )
        .prop_map(|(sign, int, frac, exp, pad)| {
            let mut s = format!("{pad}{sign}{int}");
            if let Some(frac) = frac {
                s.push('.');
                s.push_str(&frac);
            }
            if let Some((e, esign, ex)) = exp {
                s.push_str(&format!("{e}{esign}{ex}"));
            }
            s.push_str(pad);
            s
        })
}

proptest! {
    #![proptest_config(config(256))]

    /// Parsing never panics, whatever the text.
    #[test]
    fn decimal_parse_never_panics(text in prop_oneof![any::<String>(), "\\PC{0,30}", "[0-9eE+_. -]{0,24}", "(nan|snan|inf|infinity|-inf|NaN123)"]) {
        let _ = Decimal::parse_python(&text);
    }

    /// parse -> str -> parse is the identity, on the value and on both spellings.
    #[test]
    fn decimal_text_round_trip(text in arb_decimal_text()) {
        let d = Decimal::parse_python(&text).expect("generated text is a valid literal");
        let sci = d.to_sci_string();
        let again = Decimal::parse_python(&sci).expect("str(Decimal) parses");
        prop_assert_eq!(&d, &again);
        prop_assert_eq!(&sci, &again.to_sci_string(), "to_sci_string is not a fixed point");
        prop_assert_eq!(d.to_fixed_string(), again.to_fixed_string());
        // `Display` is `str(Decimal)`.
        prop_assert_eq!(d.to_string(), sci);
        // The fixed-point spelling parses back to the same value and digits.
        let fixed = Decimal::parse_python(&d.to_fixed_string()).expect("fixed spelling parses");
        prop_assert_eq!(&d, &fixed);
    }

    /// Equality is numeric, ordering is total and consistent with it, `normalize` keeps the value
    /// (up to the 28-digit context rounding) and is idempotent.
    #[test]
    fn decimal_order_and_normalise(a in arb_decimal_text(), b in arb_decimal_text()) {
        let (a, b) = (Decimal::parse_python(&a).unwrap(), Decimal::parse_python(&b).unwrap());
        prop_assert_eq!(a.cmp(&b), b.cmp(&a).reverse());
        prop_assert_eq!(a == b, a.cmp(&b) == std::cmp::Ordering::Equal);
        prop_assert_eq!(a.cmp(&a), std::cmp::Ordering::Equal);
        let n = a.normalize().expect("exponents here are within the context's Emax");
        let again = n.normalize().expect("a normalised decimal normalises");
        prop_assert_eq!(again.to_sci_string(), n.to_sci_string());
        if a.to_fixed_string().trim_start_matches('-').trim_start_matches('0').replace('.', "").trim_end_matches('0').len() <= 28 {
            prop_assert_eq!(&a, &n, "normalize changed a value of at most 28 digits");
        }
        // Order agrees with the f64 image whenever the floats differ clearly.
        let (fa, fb) = (a.to_f64(), b.to_f64());
        if fa.is_finite() && fb.is_finite() && fa < fb {
            prop_assert!(a <= b);
        }
    }

    /// Integers, and the digits of a float's repr, convert exactly.
    #[test]
    fn decimal_from_numbers(i in any::<i64>(), f in any::<f64>().prop_filter("finite", |f| f.is_finite())) {
        prop_assert_eq!(Decimal::from_i64(i).to_fixed_string(), i.to_string());
        let d = Decimal::from_f64_repr(f).expect("finite float");
        prop_assert_eq!(d.to_f64(), f, "Decimal(str(f)) lost the float");
        prop_assert_eq!(d.cmp_f64(f).is_some(), true);
    }

    /// Operations on extreme exponents stay panic-free (no allocation of the full expansion).
    #[test]
    fn decimal_extreme_exponents_do_not_panic(
        digits in "[0-9]{1,40}",
        exp in prop_oneof![
            // Stay 100 clear of i32::MAX: folding trailing zeros into the exponent
            // overflows beyond that (see `decimal_exponent_overflow_is_not_a_panic`).
            Just(i64::from(i32::MAX) - 100), Just(i64::from(i32::MIN)),
            (i64::from(i32::MAX) - 150)..=(i64::from(i32::MAX) - 100),
            (i64::from(i32::MIN))..=(i64::from(i32::MIN) + 50),
            any::<i32>().prop_map(|e| i64::from(e).min(i64::from(i32::MAX) - 100)),
        ],
        other in arb_decimal_text(),
    ) {
        if let Ok(d) = Decimal::parse_python(&format!("{digits}e{exp}")) {
            let other = Decimal::parse_python(&other).unwrap();
            let _ = d.to_sci_string();
            let _ = d.cmp(&other);
            let _ = d == other;
            let _ = d.normalize();
            let _ = d.to_f64();
            let _ = d.is_zero();
            let _ = d.cmp_f64(1.5);
        }
    }
}

/// `normalize()` (used by content-id canonicalisation) and the numeric ordering both
/// fold trailing coefficient zeros into the exponent with an unchecked `i32` addition.
#[test]
fn decimal_exponent_overflow_is_not_a_panic() {
    let d = Decimal::parse_python("10e2147483647").unwrap();
    let _ = d.normalize();
    let _ = d.cmp(&Decimal::from_i64(1));
}

#[test]
fn decimal_non_finite_literals_are_reported_as_such() {
    for text in ["nan", "NaN", "-snan", "inf", "-Infinity", "NaN42"] {
        assert_eq!(
            Decimal::parse_python(text),
            Err(DecimalError::NotFinite),
            "{text}"
        );
    }
    for text in ["", " ", "e5", ".", "1e", "1__0", "_1", "1_", "0x10", "1,5"] {
        assert_eq!(
            Decimal::parse_python(text),
            Err(DecimalError::Invalid),
            "{text:?}"
        );
    }
}

// ── Value serde ──────────────────────────────────────────────────────────────

/// `serde_json` without its `float_roundtrip` feature parses a float with more than
/// 15 significant digits to a neighbouring `f64` about one time in ten; CPython's
/// `json.loads` (and Rust's own `str::parse`) always round correctly. Every JSON/JSONL
/// data value and every `Value` deserialised from JSON text goes through this parser.
#[test]
fn json_float_parsing_is_correctly_rounded() {
    for text in [
        "1.8728745502104746e160",
        "0.30000000000000004",
        "2.2250738585072014e-308",
    ] {
        let expected: f64 = text.parse().unwrap();
        let v: Value = serde_json::from_str(text).unwrap();
        assert_eq!(v, Value::Float(expected), "{text}");
    }
}

proptest! {
    #![proptest_config(config(256))]

    /// JSON text -> `Value` -> JSON text is lossless for finite values (floats limited
    /// to few digits; see `json_float_parsing_is_correctly_rounded` for the rest).
    #[test]
    fn value_json_text_round_trip(v in arb_value_with(true, false, false).prop_map(|v| short_floats(&v))) {
        let text = serde_json::to_string(&v).unwrap();
        let back: Value = serde_json::from_str(&text).unwrap();
        prop_assert_eq!(&back, &v);
        prop_assert_eq!(serde_json::to_string(&back).unwrap(), text);
    }

    /// `Value` -> `serde_json::Value` -> `Value` is lossless for finite values.
    #[test]
    fn value_json_tree_round_trip(v in arb_value_with(true, false, false)) {
        let tree = serde_json::Value::from(&v);
        prop_assert_eq!(Value::from(&tree), v.clone());
        // And the tree path agrees with direct serialisation.
        prop_assert_eq!(serde_json::to_value(&v).unwrap(), tree);
    }

    /// YAML text -> `Value` is lossless for finite values too (the loaders' path).
    #[test]
    fn value_yaml_round_trip(v in arb_value_with(true, false, false)) {
        let text = serde_yaml_ng::to_string(&v).unwrap();
        let back: Value = serde_yaml_ng::from_str(&text).unwrap();
        prop_assert_eq!(back, v);
    }

    /// A decimal serialises as its exact `str()` text; non-finite floats become null
    /// in the generic tree (JSON has no NaN).
    #[test]
    fn value_special_serialisation(d in arb_decimal_text(), f in prop_oneof![Just(f64::NAN), Just(f64::INFINITY), Just(f64::NEG_INFINITY)]) {
        let d = Decimal::parse_python(&d).unwrap();
        prop_assert_eq!(
            serde_json::to_value(Value::Decimal(d.clone())).unwrap(),
            serde_json::Value::String(d.to_sci_string())
        );
        prop_assert_eq!(serde_json::Value::from(&Value::Float(f)), serde_json::Value::Null);
    }

    /// Python equality is symmetric; and reflexive except where NaN is involved.
    #[test]
    fn py_eq_is_symmetric_and_reflexive(a in arb_value(), b in arb_value()) {
        prop_assert_eq!(a.py_eq(&b), b.py_eq(&a));
        if !shown(&a).contains("NaN") {
            prop_assert!(a.py_eq(&a.clone()), "py_eq is not reflexive for {a:?}");
        }
    }

    /// Structural equality implies Python equality (when no NaN is involved).
    #[test]
    fn structural_eq_implies_py_eq(a in arb_value()) {
        let b = a.clone();
        if a == b {
            prop_assert!(a.py_eq(&b));
        }
    }

    /// Truthiness and numeric coercion never panic and agree with each other.
    #[test]
    fn truthiness_and_numeric_coercion(v in arb_value()) {
        let _ = v.is_truthy();
        prop_assert_eq!(v.is_numeric(), v.try_numeric().is_some());
        prop_assert_eq!(v.is_null(), matches!(v, Value::Null));
    }
}

// ── Canonical form / content ids ─────────────────────────────────────────────

/// Rebuild `v` with every map's keys in reverse order and every list reversed.
fn reorder(v: &Value) -> Value {
    match v {
        Value::Map(m) => Value::Map(
            m.iter()
                .rev()
                .map(|(k, v)| (k.clone(), reorder(v)))
                .collect::<IndexMap<_, _>>(),
        ),
        Value::List(items) => Value::List(items.iter().rev().map(reorder).collect()),
        other => other.clone(),
    }
}

proptest! {
    #![proptest_config(config(256))]

    /// `canonical_json` is a pure function and never panics.
    #[test]
    fn canonical_json_is_pure(v in arb_value()) {
        prop_assert_eq!(canonical_json(&v), canonical_json(&v.clone()));
    }

    /// Upstream sorts map keys and (unordered) list items, so neither order matters.
    #[test]
    fn canonical_json_ignores_key_and_list_order(v in arb_value()) {
        // An unsupported value (a temporal) is an error whose text names the first offender,
        // so under reordering only success vs failure and the Ok text are comparable.
        let (a, b) = (canonical_json(&v), canonical_json(&reorder(&v)));
        prop_assert_eq!(a.is_ok(), b.is_ok());
        if a.is_ok() {
            prop_assert_eq!(a, b);
        }
    }

    /// Null entries, empty lists and empty maps are dropped from objects.
    #[test]
    fn canonical_json_drops_absent_entries(
        m in prop::collection::vec(("[a-z]{1,4}", arb_value()), 0..4),
        extra in prop::collection::vec(("[A-Z]{1,4}", prop_oneof![Just(Value::Null), Just(Value::List(vec![])), Just(Value::Map(IndexMap::new()))]), 0..4),
    ) {
        let base: IndexMap<String, Value> = m.into_iter().collect();
        let mut padded = base.clone();
        padded.extend(extra);
        prop_assert_eq!(
            canonical_json(&Value::Map(base)),
            canonical_json(&Value::Map(padded))
        );
    }

    /// Ints and floats that are numerically equal share one encoding.
    #[test]
    fn canonical_json_unifies_numbers(i in -999_999_999_999_999i64..=999_999_999_999_999) {
        prop_assert_eq!(
            canonical_json(&Value::Int(i)),
            canonical_json(&Value::Float(i as f64))
        );
    }

    /// The minted id is a version-5 UUID, a pure function of the record (not of key order).
    #[test]
    fn content_id_is_pure(
        pairs in prop::collection::vec((prop::sample::select(vec!["name", "x", "y", "value", "tags", "kind", "address", "friends", "other"]).prop_map(str::to_string), arb_value()), 0..6),
        class in prop::sample::select(vec!["Person", "Row", "Address", "Unknown"]),
    ) {
        let schema = tiny_schema();
        let ids = ContentIds::build(&schema, ["Person", "Row", "Address"]);
        let record: IndexMap<String, Value> = pairs.into_iter().collect();
        let reversed: IndexMap<String, Value> =
            record.iter().rev().map(|(k, v)| (k.clone(), v.clone())).collect();
        let first = ids.content_id(&record, class);
        prop_assert_eq!(&first, &ids.content_id(&record, class));
        // Key order is irrelevant (upstream sorts keys).
        prop_assert_eq!(first.is_ok(), ids.content_id(&reversed, class).is_ok());
        if first.is_ok() {
            prop_assert_eq!(&first, &ids.content_id(&reversed, class));
        }
        if let Ok(id) = first {
            let uuid = Uuid::parse_str(&id).expect("a UUID string");
            prop_assert_eq!(uuid.get_version_num(), 5);
            prop_assert_eq!(uuid.to_string(), id);
        }
        // A rebuilt synthesiser mints the same id (no hidden state).
        let rebuilt = ContentIds::build(&tiny_schema(), ["Person", "Row", "Address"]);
        prop_assert_eq!(ids.content_id(&record, class), rebuilt.content_id(&record, class));
    }

    /// Different target classes never share an id for the same record.
    #[test]
    fn content_id_depends_on_the_class(
        pairs in prop::collection::vec((prop::sample::select(vec!["name", "x", "y"]).prop_map(str::to_string), arb_scalar(true, false)), 1..4),
    ) {
        let schema = tiny_schema();
        let ids = ContentIds::build(&schema, ["Person", "Row"]);
        let record: IndexMap<String, Value> = pairs.into_iter().collect();
        if let (Ok(a), Ok(b)) = (ids.content_id(&record, "Person"), ids.content_id(&record, "Row")) {
            prop_assert_ne!(a, b);
        }
    }
}

/// `parse_python` rejects an exponent outside `i32` (documented), but `to_sci_string` spells
/// the *adjusted* exponent, which exceeds `i32` for a value parsed with an exponent near
/// `i32::MAX`: the value cannot be read back from its own `str()`. Python's `Decimal` has
/// no such gap (`Emax` is 999999999999999999).
#[test]
fn decimal_sci_string_of_extreme_exponent_parses_back() {
    let d = Decimal::parse_python("10e2147483647").unwrap();
    Decimal::parse_python(&d.to_sci_string()).expect("str(Decimal) parses");
}

/// Upstream `_canonical` raises `TypeError("Cannot synthesize an identifier from a value of
/// type {type(value).__name__}: {value!r}")`; for a `datetime.date` that is `type date:
/// datetime.date(2000, 1, 1)`. The port prints `type unsupported: Temporal(Date(..))`.
#[test]
fn content_id_error_for_a_date_matches_upstream_text() {
    use linkml_map_core::value::Temporal;
    let date = Temporal::from_yaml_timestamp("2000-01-01").unwrap();
    let err = canonical_json(&Value::Temporal(date)).unwrap_err();
    assert_eq!(
        err,
        "Cannot synthesize an identifier from a value of type date: datetime.date(2000, 1, 1)"
    );
}
