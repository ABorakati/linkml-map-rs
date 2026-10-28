//! Dynamic Value type for runtime transformation expression evaluation.
//!
//! This mirrors the Python value model used by `linkml_map.utils.eval_utils`:
//! a nullable dynamic value that can be a scalar, list, or map (object).

use std::cmp::Ordering;

use indexmap::IndexMap;
use serde::de::{self, Deserializer, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};

mod json;

pub use crate::decimal::Decimal;
use crate::pynum::complex::Complex64;
use crate::pynum::{self, BigInt, IntRef};
pub use crate::temporal::Temporal;
pub use json::{parse_json, parse_json_bytes};

/// A dynamically-typed value during transformation.
///
/// Objects (dicts) are represented as [`Value::Map`]; an unbound variable
/// resolves to [`Value::Null`] rather than an error (SQL-style semantics).
#[derive(Debug, Serialize)]
#[serde(untagged)]
#[derive(Default)]
pub enum Value {
    /// Null / `None`.
    #[default]
    Null,
    /// Boolean. Note: a `bool` is NOT considered numeric for coercion.
    Bool(bool),
    /// Integer.
    Int(i64),
    /// Floating point.
    Float(f64),
    /// Exact decimal (Python `decimal.Decimal`), produced by `range: decimal`
    /// coercion. Never deserialised from data: JSON/YAML numbers stay
    /// `Int`/`Float`, and a decimal serialises generically as its exact text in
    /// a string (the writers emit exact numeric literals themselves).
    #[serde(serialize_with = "serialize_decimal")]
    Decimal(Decimal),
    /// `datetime.date` / `datetime.datetime`, as PyYAML builds them from an
    /// unquoted timestamp scalar. Never deserialised from JSON; serialises as its
    /// `str()` text (see [`crate::temporal`]).
    #[serde(serialize_with = "serialize_temporal")]
    Temporal(Temporal),
    /// String.
    Str(String),
    /// Ordered list.
    List(Vec<Value>),
    /// Ordered map (object/dict).
    Map(IndexMap<String, Value>),
    /// An integer outside `i64` (Python's `int` is unbounded). Only ever built
    /// by [`Value::from_bigint`], which normalises: a value that fits `i64` is
    /// an [`Value::Int`], so the two never overlap. A generic serialisation is
    /// the decimal digits in a string; the writers spell it as a number.
    #[serde(serialize_with = "serialize_bigint")]
    BigInt(Box<BigInt>),
    /// Python's `complex`, from `1j` literals and results such as
    /// `(-8) ** 0.5`. Never read from data. A generic serialisation is its
    /// `str()` text.
    #[serde(serialize_with = "serialize_complex")]
    Complex(Box<Complex64>),
}

// `Value` is moved and cloned on every row: a new variant must not widen it.
const _: () = assert!(std::mem::size_of::<Value>() <= 72);

fn serialize_bigint<S: serde::Serializer>(n: &BigInt, s: S) -> Result<S::Ok, S::Error> {
    s.collect_str(n)
}

fn serialize_complex<S: serde::Serializer>(z: &Complex64, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&pynum::complex::repr(*z))
}

/// `Clone` with the scalar cases inline: every binding read and operand copy of
/// the evaluator clones a `Value`, and the derived implementation (one arm per
/// variant, including the boxed integers and the containers) is too big for the
/// compiler to inline into those loops.
impl Clone for Value {
    #[inline]
    fn clone(&self) -> Self {
        match self {
            Value::Null => Value::Null,
            Value::Bool(b) => Value::Bool(*b),
            Value::Int(i) => Value::Int(*i),
            Value::Float(f) => Value::Float(*f),
            other => other.clone_heap(),
        }
    }
}

impl Value {
    /// The rest of [`Clone::clone`]: variants that own heap data (or are large).
    #[inline(never)]
    fn clone_heap(&self) -> Value {
        match self {
            Value::Null => Value::Null,
            Value::Bool(b) => Value::Bool(*b),
            Value::Int(i) => Value::Int(*i),
            Value::Float(f) => Value::Float(*f),
            Value::Decimal(d) => Value::Decimal(d.clone()),
            Value::Temporal(t) => Value::Temporal(*t),
            Value::Str(s) => Value::Str(s.clone()),
            Value::List(items) => Value::List(items.clone()),
            Value::Map(map) => Value::Map(map.clone()),
            Value::BigInt(n) => Value::BigInt(n.clone()),
            Value::Complex(z) => Value::Complex(z.clone()),
        }
    }
}

fn serialize_temporal<S: serde::Serializer>(t: &Temporal, s: S) -> Result<S::Ok, S::Error> {
    s.collect_str(t)
}

/// Deserialises like the `#[serde(untagged)]` derive did, trying the variants in
/// order `Null`, `Bool`, `Int`, `Float`, `Str`, `List`, `Map`, but straight off the
/// deserializer's own events. The derive buffers every value and re-parses it once
/// per variant, building and formatting an error string for each that fails: more
/// than half the cost of reading a JSONL row. `Decimal` is never read from data.
impl<'de> Deserialize<'de> for Value {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(ValueVisitor)
    }
}

struct ValueVisitor;

/// Pre-allocation cap for sequences and maps, whatever size a format claims.
const MAX_PREALLOCATED: usize = 4096;

impl<'de> Visitor<'de> for ValueVisitor {
    type Value = Value;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("any value: null, bool, number, string, list or map")
    }

    fn visit_unit<E: de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_none<E: de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_bool<E: de::Error>(self, v: bool) -> Result<Value, E> {
        Ok(Value::Bool(v))
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Value, E> {
        Ok(Value::Int(v))
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Value, E> {
        // An integer beyond `i64` is a Python `int` still.
        Ok(match i64::try_from(v) {
            Ok(int) => Value::Int(int),
            Err(_) => Value::BigInt(Box::new(BigInt::from(v))),
        })
    }

    fn visit_i128<E: de::Error>(self, v: i128) -> Result<Value, E> {
        Ok(Value::from_bigint(BigInt::from(v)))
    }

    fn visit_u128<E: de::Error>(self, v: u128) -> Result<Value, E> {
        Ok(Value::from_bigint(BigInt::from(v)))
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Value, E> {
        Ok(Value::Float(v))
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Value, E> {
        Ok(Value::Str(v.to_owned()))
    }

    fn visit_string<E: de::Error>(self, v: String) -> Result<Value, E> {
        Ok(Value::Str(v))
    }

    fn visit_bytes<E: de::Error>(self, v: &[u8]) -> Result<Value, E> {
        std::str::from_utf8(v)
            .map(|text| Value::Str(text.to_owned()))
            .map_err(|_| E::invalid_value(de::Unexpected::Bytes(v), &self))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut items = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(MAX_PREALLOCATED));
        while let Some(item) = seq.next_element()? {
            items.push(item);
        }
        Ok(Value::List(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut entries =
            IndexMap::with_capacity(map.size_hint().unwrap_or(0).min(MAX_PREALLOCATED));
        // A repeated key keeps its first position and takes the last value.
        while let Some((key, value)) = map.next_entry::<String, Value>()? {
            entries.insert(key, value);
        }
        Ok(Value::Map(entries))
    }
}

fn serialize_decimal<S: serde::Serializer>(d: &Decimal, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&d.to_sci_string())
}

impl Value {
    /// Python-style truthiness.
    ///
    /// Null/False/0/0.0/""/[]/{} are falsy; everything else is truthy.
    pub fn is_truthy(&self) -> bool {
        match self {
            Value::Null => false,
            Value::Bool(b) => *b,
            Value::Int(i) => *i != 0,
            Value::Float(f) => *f != 0.0,
            Value::Decimal(d) => !d.is_zero(),
            // Normalised, so never zero.
            Value::BigInt(_) => true,
            Value::Complex(z) => pynum::complex::is_truthy(**z),
            // A `datetime.date` is always truthy (a `datetime.time` is not a case here).
            Value::Temporal(_) => true,
            Value::Str(s) => !s.is_empty(),
            Value::List(items) => !items.is_empty(),
            Value::Map(m) => !m.is_empty(),
        }
    }

    /// Whether the value is `Null`.
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// Numeric coercion matching Python `_try_numeric`:
    ///
    /// - `int`/`float` (but NOT `bool`) → returned as `f64`
    /// - numeric `str` → parsed `f64`
    /// - anything else (including `bool`, empty string, `Null`) → `None`
    pub fn try_numeric(&self) -> Option<f64> {
        match self {
            Value::Bool(_) => None,
            Value::Int(i) => Some(*i as f64),
            Value::Float(f) => Some(*f),
            // PARITY: Python `float(Decimal)` works; arithmetic mixing a
            // Decimal with a float raises TypeError upstream, here it is numeric.
            Value::Decimal(d) => Some(d.to_f64()),
            // The integer itself upstream; its nearest float here (`inf` beyond
            // the float range, where Python's later `float()` would raise).
            Value::BigInt(n) => Some(pynum::big_to_f64(n).unwrap_or(
                if n.sign() == num_bigint::Sign::Minus {
                    f64::NEG_INFINITY
                } else {
                    f64::INFINITY
                },
            )),
            Value::Str(s) => parse_python_float(s),
            _ => None,
        }
    }

    /// Mirror of Python `_is_numeric`: `True` if `float(value)` would succeed
    /// (excluding `bool`).
    pub fn is_numeric(&self) -> bool {
        self.try_numeric().is_some()
    }
}

/// Parse a string the way Python's `float()` does: surrounding ASCII whitespace
/// is stripped, Unicode decimal digits and single underscores between digits are
/// accepted, as are decimals, scientific notation and `inf`/`nan`; an empty
/// string fails. See [`crate::expr::pystr::float_from_str`].
pub fn parse_python_float(s: &str) -> Option<f64> {
    crate::expr::pystr::float_from_str(s)
}

impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::Bool(b)
    }
}
impl From<i64> for Value {
    fn from(i: i64) -> Self {
        Value::Int(i)
    }
}
impl From<f64> for Value {
    fn from(f: f64) -> Self {
        Value::Float(f)
    }
}
impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::Str(s.to_string())
    }
}
impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::Str(s)
    }
}
impl From<Vec<Value>> for Value {
    fn from(v: Vec<Value>) -> Self {
        Value::List(v)
    }
}
impl From<IndexMap<String, Value>> for Value {
    fn from(m: IndexMap<String, Value>) -> Self {
        Value::Map(m)
    }
}

/// The string standing for an integer beyond `u64` inside a `serde_json::Value`
/// tree: its digits between U+0001 and U+0003 (the placeholder the JSON writers
/// replace with a plain number, see `linkml-map-io::serialization`).
fn bigint_sentinel(n: &BigInt) -> String {
    format!("\u{1}{n}\u{3}")
}

/// The integer a [`bigint_sentinel`] string stands for.
fn bigint_from_sentinel(s: &str) -> Option<BigInt> {
    let digits = s.strip_prefix('\u{1}')?.strip_suffix('\u{3}')?;
    let body = digits.strip_prefix('-').unwrap_or(digits);
    if body.is_empty() || !body.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse::<BigInt>().ok()
}

/// Convert a `serde_json::Value` reference into a [`Value`].
/// Non-finite floats (NaN/±Inf) cannot appear in JSON but are mapped to `Value::Float`
/// with the raw `f64` value if somehow constructed.
impl From<&serde_json::Value> for Value {
    fn from(j: &serde_json::Value) -> Self {
        match j {
            serde_json::Value::Null => Value::Null,
            serde_json::Value::Bool(b) => Value::Bool(*b),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Value::Int(i)
                } else if let Some(u) = n.as_u64() {
                    Value::BigInt(Box::new(BigInt::from(u)))
                } else {
                    Value::Float(n.as_f64().unwrap_or(f64::NAN))
                }
            }
            serde_json::Value::String(s) => match bigint_from_sentinel(s) {
                Some(n) => Value::BigInt(Box::new(n)),
                None => Value::Str(s.clone()),
            },
            serde_json::Value::Array(arr) => Value::List(arr.iter().map(Value::from).collect()),
            serde_json::Value::Object(map) => Value::Map(
                map.iter()
                    .map(|(k, v)| (k.clone(), Value::from(v)))
                    .collect(),
            ),
        }
    }
}

/// Convert a [`Value`] reference into a `serde_json::Value`.
/// Non-finite floats (NaN/±Inf) are mapped to `serde_json::Value::Null` because JSON
/// does not support non-finite numbers.
impl From<&Value> for serde_json::Value {
    fn from(v: &Value) -> Self {
        match v {
            Value::Null => serde_json::Value::Null,
            Value::Bool(b) => serde_json::Value::Bool(*b),
            Value::Int(i) => serde_json::Value::Number((*i).into()),
            Value::Float(f) => serde_json::Number::from_f64(*f)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null),
            // Lossless generic form: the exact text as a string. Exact numeric
            // output is the writers' job (see linkml-map-io).
            Value::Decimal(d) => serde_json::Value::String(d.to_sci_string()),
            // A number while it fits `u64`; beyond that `serde_json::Value` has no
            // number to hold it, so the digits travel in a sentinel string that
            // `From<&serde_json::Value>` reads back (and the JSON writers restore).
            Value::BigInt(n) => match u64::try_from(&**n) {
                Ok(u) => serde_json::Value::Number(u.into()),
                Err(_) => serde_json::Value::String(bigint_sentinel(n)),
            },
            Value::Complex(z) => serde_json::Value::String(pynum::complex::repr(**z)),
            // Generic form only; JSON *output* refuses these like upstream does.
            Value::Temporal(t) => serde_json::Value::String(t.to_string()),
            Value::Str(s) => serde_json::Value::String(s.clone()),
            Value::List(items) => {
                serde_json::Value::Array(items.iter().map(serde_json::Value::from).collect())
            }
            Value::Map(m) => serde_json::Value::Object(
                m.iter()
                    .map(|(k, v)| (k.clone(), serde_json::Value::from(v)))
                    .collect(),
            ),
        }
    }
}

/// Exact comparison of an integer with a float; `None` for NaN.
pub(crate) fn cmp_int_float(i: i64, f: f64) -> Option<Ordering> {
    if f.is_nan() {
        return None;
    }
    // 2^63 is exactly representable; every i64 is below it.
    if f >= 9_223_372_036_854_775_808.0 {
        return Some(Ordering::Less);
    }
    if f < -9_223_372_036_854_775_808.0 {
        return Some(Ordering::Greater);
    }
    let floor = f.floor();
    // `floor` is within i64 here.
    let fi = floor as i64;
    Some(i.cmp(&fi).then(if f > floor {
        Ordering::Less
    } else {
        Ordering::Equal
    }))
}

/// `complex == real`: the imaginary part is zero and the real part equals the
/// number (`complex_richcompare`).
fn complex_eq_real(z: &Complex64, other: &Value) -> bool {
    if z.im != 0.0 {
        return false;
    }
    match other {
        Value::Bool(b) => z.re == f64::from(u8::from(*b)),
        Value::Int(i) => cmp_int_float(*i, z.re) == Some(Ordering::Equal),
        Value::BigInt(n) => pynum::big_eq_f64(n, z.re),
        Value::Float(f) => z.re == *f,
        _ => false,
    }
}

impl Value {
    /// The integer a [`Value::Int`] or [`Value::BigInt`] holds (a `bool` is not
    /// one here).
    pub fn as_int_ref(&self) -> Option<IntRef<'_>> {
        match self {
            Value::Int(i) => Some(IntRef::Small(*i)),
            Value::BigInt(n) => Some(IntRef::Big(n)),
            _ => None,
        }
    }

    /// An integer result as a value: [`Value::Int`] when it fits `i64`, else
    /// [`Value::BigInt`]. The only way a `BigInt` is built, so the two variants
    /// never hold the same number.
    pub fn from_bigint(n: BigInt) -> Value {
        match i64::try_from(&n) {
            Ok(i) => Value::Int(i),
            Err(_) => Value::BigInt(Box::new(n)),
        }
    }

    /// A complex number as a value.
    pub fn from_complex(z: Complex64) -> Value {
        Value::Complex(Box::new(z))
    }

    /// Python-native equality between two values (used by `==` / `!=`).
    ///
    /// `None == "x"` is `False`; `None == None` is `True`; numeric types
    /// (`bool`/`int`/`float`) compare by numeric value.
    pub fn py_eq(&self, other: &Value) -> bool {
        use Value::*;
        match (self, other) {
            (Null, Null) => true,
            (Null, _) | (_, Null) => false,
            (Bool(a), Bool(b)) => a == b,
            (Int(a), Int(b)) => a == b,
            (Float(a), Float(b)) => a == b,
            (Bool(a), Int(b)) | (Int(b), Bool(a)) => (*a as i64) == *b,
            (Bool(a), Float(b)) | (Float(b), Bool(a)) => (if *a { 1.0 } else { 0.0 }) == *b,
            (Int(a), Float(b)) | (Float(b), Int(a)) => {
                cmp_int_float(*a, *b) == Some(Ordering::Equal)
            }
            (BigInt(a), BigInt(b)) => a == b,
            (BigInt(a), Float(b)) | (Float(b), BigInt(a)) => pynum::big_eq_f64(a, *b),
            (Complex(a), Complex(b)) => pynum::complex::eq(**a, **b),
            (Complex(z), other) | (other, Complex(z)) => complex_eq_real(z, other),
            (BigInt(a), Decimal(b)) | (Decimal(b), BigInt(a)) => {
                self::Decimal::parse_python(&a.to_string()).is_ok_and(|d| d == *b)
            }
            // Python compares a Decimal with int/bool/Decimal numerically and with
            // a float exactly (`Decimal("0.1") != 0.1`).
            (Decimal(a), Decimal(b)) => a == b,
            (Decimal(a), Int(b)) | (Int(b), Decimal(a)) => *a == self::Decimal::from_i64(*b),
            (Decimal(a), Bool(b)) | (Bool(b), Decimal(a)) => {
                *a == self::Decimal::from_i64(i64::from(*b))
            }
            (Decimal(a), Float(b)) | (Float(b), Decimal(a)) => {
                a.cmp_f64(*b) == Some(Ordering::Equal)
            }
            (Temporal(a), Temporal(b)) => a == b,
            (Str(a), Str(b)) => a == b,
            (List(a), List(b)) => a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.py_eq(y)),
            (Map(a), Map(b)) => {
                a.len() == b.len() && a.iter().all(|(k, v)| b.get(k).is_some_and(|w| v.py_eq(w)))
            }
            _ => false,
        }
    }
}

// Structural equality for tests (distinct from py_eq used by == / !=).
impl PartialEq for Value {
    fn eq(&self, other: &Value) -> bool {
        use Value::*;
        match (self, other) {
            (Null, Null) => true,
            (Bool(a), Bool(b)) => a == b,
            (Int(a), Int(b)) => a == b,
            (Float(a), Float(b)) => a == b,
            (BigInt(a), BigInt(b)) => a == b,
            (Complex(a), Complex(b)) => pynum::complex::eq(**a, **b),
            (Decimal(a), Decimal(b)) => a == b,
            (Temporal(a), Temporal(b)) => a == b,
            (Str(a), Str(b)) => a == b,
            (List(a), List(b)) => a == b,
            (Map(a), Map(b)) => a == b,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `#[serde(untagged)]` derive `Value` used before its hand-written
    /// `Deserialize`: the reference the new one must agree with.
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Untagged {
        Null,
        Bool(bool),
        Int(i64),
        Float(f64),
        Str(String),
        List(Vec<Untagged>),
        Map(IndexMap<String, Untagged>),
    }

    fn convert(v: Untagged) -> Value {
        match v {
            Untagged::Null => Value::Null,
            Untagged::Bool(b) => Value::Bool(b),
            Untagged::Int(i) => Value::Int(i),
            Untagged::Float(f) => Value::Float(f),
            Untagged::Str(s) => Value::Str(s),
            Untagged::List(l) => Value::List(l.into_iter().map(convert).collect()),
            Untagged::Map(m) => Value::Map(m.into_iter().map(|(k, v)| (k, convert(v))).collect()),
        }
    }

    const CORPUS: &[&str] = &[
        "null",
        "true",
        "false",
        "0",
        "-0",
        "42",
        "-42",
        "9223372036854775807",
        "-9223372036854775808",
        // Beyond `u64`: a float from `serde_json` itself (see `parse_json`).
        "18446744073709551616",
        "1.5",
        "-0.0",
        "1e300",
        "1E-7",
        "12345678901234567890.5",
        r#""""#,
        r#""text with \"escapes\" and é and 😀""#,
        "[]",
        "[1, 2.5, \"x\", null, true, [], {}]",
        "{}",
        r#"{"a": 1, "b": {"c": [1, {"d": null}]}, "e": "f"}"#,
        // A repeated key keeps its first position and takes the last value.
        r#"{"a": 1, "b": 2, "a": 3}"#,
        r#"{"": 0, " ": 1}"#,
        // Not valid JSON: both fail.
        "",
        "{",
        "[1,]",
        r#"{"a": }"#,
        "nan",
        "{1: 2}",
    ];

    #[test]
    fn deserialize_matches_the_untagged_derive() {
        for text in CORPUS {
            let new: Result<Value, _> = serde_json::from_str(text);
            let old = serde_json::from_str::<Untagged>(text).map(convert);
            match (new, old) {
                (Ok(new), Ok(old)) => {
                    assert_eq!(format!("{new:?}"), format!("{old:?}"), "from_str {text}");
                    // And through `serde_json::Value`, the other way rows arrive.
                    let json: serde_json::Value = serde_json::from_str(text).unwrap();
                    let via_value: Value = serde_json::from_value(json.clone()).unwrap();
                    let via_value_old = convert(serde_json::from_value::<Untagged>(json).unwrap());
                    assert_eq!(
                        format!("{via_value:?}"),
                        format!("{via_value_old:?}"),
                        "from_value {text}"
                    );
                }
                (Err(_), Err(_)) => {}
                (new, old) => panic!(
                    "{text}: new {:?} vs old {:?}",
                    new.map(|v| format!("{v:?}")).map_err(|e| e.to_string()),
                    old.map(|v| format!("{v:?}")).map_err(|e| e.to_string())
                ),
            }
        }
    }

    #[test]
    fn integers_beyond_i64_are_big_integers() {
        for text in ["9223372036854775808", "18446744073709551615"] {
            let v: Value = serde_json::from_str(text).unwrap();
            assert_eq!(format!("{v:?}"), format!("BigInt({text})"));
            let via_value: Value =
                serde_json::from_value(serde_json::from_str::<serde_json::Value>(text).unwrap())
                    .unwrap();
            assert_eq!(v, via_value);
        }
        // Normalised: a result inside `i64` is an `Int`.
        assert_eq!(Value::from_bigint(BigInt::from(5)), Value::Int(5));
        assert_eq!(
            Value::from_bigint(BigInt::from(i64::MIN)),
            Value::Int(i64::MIN)
        );
    }

    #[test]
    fn map_order_and_nesting_are_kept() {
        let v: Value = serde_json::from_str(r#"{"z": 1, "a": {"y": [1, 2], "b": null}}"#).unwrap();
        let Value::Map(m) = v else { panic!("map") };
        assert_eq!(m.keys().collect::<Vec<_>>(), ["z", "a"]);
        let Value::Map(inner) = &m["a"] else {
            panic!("map")
        };
        assert_eq!(inner.keys().collect::<Vec<_>>(), ["y", "b"]);
    }

    #[test]
    fn a_decimal_is_never_read_from_data() {
        // `Decimal` serialises as its exact text, which reads back as a string.
        let text =
            serde_json::to_string(&Value::Decimal(Decimal::parse_python("1.50").unwrap())).unwrap();
        assert!(matches!(
            serde_json::from_str::<Value>(&text).unwrap(),
            Value::Str(_)
        ));
    }

    #[test]
    fn a_big_integer_survives_a_generic_json_tree() {
        let digits = "1000000000000000000000000000000";
        let big = Value::from_bigint(digits.parse::<BigInt>().unwrap());
        let json = serde_json::Value::from(&big);
        assert!(matches!(json, serde_json::Value::String(_)));
        assert_eq!(Value::from(&json), big);
        let neg = Value::from_bigint(format!("-{digits}").parse::<BigInt>().unwrap());
        assert_eq!(Value::from(&serde_json::Value::from(&neg)), neg);
        // An ordinary string that merely looks similar stays a string.
        for s in [digits, "\u{1}12x\u{3}", "\u{1}\u{3}", "\u{1}-\u{3}"] {
            let j = serde_json::Value::String(s.to_owned());
            assert_eq!(Value::from(&j), Value::Str(s.to_owned()));
        }
    }
}
