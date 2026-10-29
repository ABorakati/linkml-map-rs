//! `GetValueNumerical` (`extension/json/json_functions/json_transform.cpp`): what DuckDB's `read_json` does
//! with a JSON value that is not of the type inferred for its column.
//!
//! Inference reads the first 20480 records, so a later record can hold something else: an integer in a
//! `DOUBLE` column, a boolean in a `BIGINT` column, a string in either.  DuckDB casts it (as `TRY_CAST` with
//! `strict_cast`), and raises when the cast fails.  The rules below were measured against DuckDB 1.5.6, one
//! value type at a time; the string grammars are the ones DuckDB's own parsers accept.

use linkml_map_core::value::Value;

/// C's `isspace`.
fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// The 128-bit integer a JSON integer is, when it fits (`BigInt`s past `u64` are raw numbers, which DuckDB
/// casts from their text).
fn integer_of(v: &Value) -> Option<i128> {
    match v {
        Value::Int(i) => Some(i128::from(*i)),
        Value::BigInt(n) => n.to_string().parse().ok(),
        _ => None,
    }
}

/// A cast to `BOOLEAN`: numbers are `!= 0`, strings are `true`/`t`/`yes` and `false`/`f`/`no` in any case.
pub(super) fn to_bool(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        Value::Int(i) => Some(*i != 0),
        // an integer in the `u64` range is a number, beyond it raw text, which is not a boolean
        Value::BigInt(_) => integer_of(v)
            .filter(|i| (i128::from(i64::MAX) + 1..=i128::from(u64::MAX)).contains(i))
            .map(|_| true),
        Value::Float(f) if f.is_finite() => Some(*f != 0.0),
        Value::Str(s) => match s.to_ascii_lowercase().as_str() {
            "true" | "t" | "yes" => Some(true),
            "false" | "f" | "no" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

/// A cast to `BIGINT` (`wide = false`) or `HUGEINT`: the integer, as the digits it prints as.
pub(super) fn to_integer(v: &Value, wide: bool) -> Option<String> {
    let limit = if wide {
        i128::MAX
    } else {
        i128::from(i64::MAX)
    };
    let floor = if wide {
        i128::MIN
    } else {
        i128::from(i64::MIN)
    };
    let n: i128 = match v {
        Value::Bool(b) => i128::from(*b),
        Value::Int(_) | Value::BigInt(_) => integer_of(v)?,
        // rounded half to even; raw (beyond `f64`) numbers are text, which is not an integer
        Value::Float(f) if f.is_finite() => {
            let r = f.round_ties_even();
            #[allow(clippy::cast_precision_loss)]
            let (lo, hi) = (floor as f64, limit as f64);
            if r < lo || r >= hi {
                return None;
            }
            #[allow(clippy::cast_possible_truncation)]
            {
                r as i128
            }
        }
        Value::Str(s) => integer_from_text(s, wide)?,
        _ => return None,
    };
    (floor..=limit).contains(&n).then(|| n.to_string())
}

/// DuckDB's string to integer: spaces around, then `0` or an integer without leading zeros, or `0x`/`0b`
/// digits.
fn integer_from_text(s: &str, wide: bool) -> Option<i128> {
    let t = s.trim_matches(|c: char| c.is_ascii() && is_space(c as u8));
    let (negative, digits) = match t.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, t),
    };
    // hexadecimal and binary literals are read for `BIGINT` only
    let value = if let Some(hex) = digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
        .filter(|_| !wide)
    {
        i128::from_str_radix(hex, 16)
            .ok()
            .filter(|_| !hex.starts_with(['+', '-']))?
    } else if let Some(bits) = digits
        .strip_prefix("0b")
        .or_else(|| digits.strip_prefix("0B"))
        .filter(|_| !wide)
    {
        i128::from_str_radix(bits, 2)
            .ok()
            .filter(|_| !bits.starts_with(['+', '-']))?
    } else {
        let plain = digits == "0"
            || (digits.starts_with(|c: char| ('1'..='9').contains(&c))
                && digits.bytes().all(|b| b.is_ascii_digit()));
        if !plain {
            return None;
        }
        digits.parse::<i128>().ok()?
    };
    Some(if negative { -value } else { value })
}

/// A cast to `DOUBLE`.
pub(super) fn to_double(v: &Value) -> Option<f64> {
    match v {
        Value::Bool(b) => Some(f64::from(u8::from(*b))),
        Value::Int(i) => Some(*i as f64),
        Value::BigInt(n) => Some(linkml_map_core::pynum::big_to_f64(n).unwrap_or(
            if n.to_string().starts_with('-') {
                f64::NEG_INFINITY
            } else {
                f64::INFINITY
            },
        )),
        Value::Float(f) => Some(*f),
        Value::Str(s) => double_from_text(s),
        _ => None,
    }
}

/// DuckDB's string to double: leading spaces, an optional `-`, then `inf`, `infinity`, `nan` or a decimal
/// with at least one digit (`.5`, `5.`, `1e3`); no `+`, no trailing space.
fn double_from_text(s: &str) -> Option<f64> {
    let t = s.trim_start_matches(|c: char| c.is_ascii() && is_space(c as u8));
    let body = t.strip_prefix('-').unwrap_or(t);
    let lower = body.to_ascii_lowercase();
    if matches!(lower.as_str(), "inf" | "infinity" | "nan") {
        return t.parse().ok();
    }
    let (mantissa, exponent) = match body.find(['e', 'E']) {
        Some(at) => (&body[..at], Some(&body[at + 1..])),
        None => (body, None),
    };
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits = |x: &str| x.bytes().all(|b| b.is_ascii_digit());
    if !digits(whole) || !digits(fraction) || (whole.is_empty() && fraction.is_empty()) {
        return None;
    }
    if let Some(e) = exponent {
        let e = e.strip_prefix(['+', '-']).unwrap_or(e);
        if e.is_empty() || !digits(e) {
            return None;
        }
    }
    t.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(t: &str) -> Value {
        Value::Str(t.to_owned())
    }

    #[test]
    fn integers_round_half_to_even_and_parse_strings() {
        let narrow = |v: Value| to_integer(&v, false);
        assert_eq!(narrow(Value::Float(2.5)).as_deref(), Some("2"));
        assert_eq!(narrow(Value::Float(-1.5)).as_deref(), Some("-2"));
        assert_eq!(narrow(Value::Float(1e300)), None);
        assert_eq!(narrow(s(" 5")).as_deref(), Some("5"));
        assert_eq!(narrow(s("05")), None);
        assert_eq!(narrow(s("0x10")).as_deref(), Some("16"));
        assert_eq!(narrow(Value::Bool(true)).as_deref(), Some("1"));
        assert_eq!(
            to_integer(&Value::Int(i64::MAX), false).as_deref(),
            Some("9223372036854775807")
        );
    }

    #[test]
    fn doubles_and_booleans_from_text() {
        assert_eq!(to_double(&s(".5")), Some(0.5));
        assert_eq!(to_double(&s("5 ")), None);
        assert_eq!(to_double(&s("+1.5")), None);
        assert_eq!(to_double(&s("-inf")), Some(f64::NEG_INFINITY));
        assert_eq!(to_bool(&s("No")), Some(false));
        assert_eq!(to_bool(&s("1")), None);
        assert_eq!(to_bool(&Value::Int(0)), Some(false));
    }
}
