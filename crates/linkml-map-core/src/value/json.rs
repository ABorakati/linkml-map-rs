//! JSON text to [`Value`], as Python's `json.loads` reads it.
//!
//! `serde_json` (without its `arbitrary_precision` feature, which would slow
//! every number) turns an integer beyond `u64` into a float, where Python's
//! `int` is exact. [`parse_json`] therefore hands the text to `serde_json` as
//! before and only when it holds an integer literal of 19 or more digits (outside
//! a string) reads it with a small parser of its own, which keeps such integers
//! as [`Value::BigInt`]. Everything else about the two readers is the same.

use indexmap::IndexMap;
use serde::de::Error as _;

use super::Value;
use crate::pynum;

/// Parse `text` as one JSON document.
///
/// # Errors
/// The `serde_json` error for malformed text; for an integer of more than 4300
/// digits, the `ValueError` message `json.loads` raises.
pub fn parse_json(text: &str) -> Result<Value, serde_json::Error> {
    if !may_hold_big_integer(text.as_bytes()) {
        return serde_json::from_str(text);
    }
    match Reader::new(text).document() {
        Ok(Some(value)) => Ok(value),
        Ok(None) => serde_json::from_str(text),
        Err(message) => Err(serde_json::Error::custom(message)),
    }
}

/// [`parse_json`] for bytes (which must be UTF-8).
///
/// # Errors
/// As [`parse_json`], and for bytes that are not UTF-8.
pub fn parse_json_bytes(bytes: &[u8]) -> Result<Value, serde_json::Error> {
    if !may_hold_big_integer(bytes) {
        return serde_json::from_slice(bytes);
    }
    match std::str::from_utf8(bytes) {
        Ok(text) => parse_json(text),
        Err(_) => serde_json::from_slice(bytes),
    }
}

/// Whether `bytes` may hold an integer literal beyond `i64`: first the cheap
/// test (a run of 19 or more ASCII digits anywhere), and only then the exact
/// one, which skips strings and fractions and exponents.
fn may_hold_big_integer(bytes: &[u8]) -> bool {
    let mut run = 0u32;
    for &b in bytes {
        if b.is_ascii_digit() {
            run += 1;
            if run >= 19 {
                return has_big_integer_literal(bytes);
            }
        } else {
            run = 0;
        }
    }
    false
}

/// Whether `bytes` has, outside a string, an integer literal (no fraction, no
/// exponent) of 19 or more digits.
fn has_big_integer_literal(bytes: &[u8]) -> bool {
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
                i += 1;
            }
            b'-' | b'0'..=b'9' => {
                let start = i;
                while i < bytes.len()
                    && matches!(bytes[i], b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-')
                {
                    i += 1;
                }
                let token = &bytes[start..i];
                let digits = token.iter().filter(|b| b.is_ascii_digit()).count();
                let integral = token.iter().all(|b| b.is_ascii_digit() || *b == b'-');
                if integral && digits >= 19 {
                    return true;
                }
            }
            _ => i += 1,
        }
    }
    false
}

struct Reader<'a> {
    text: &'a str,
    bytes: &'a [u8],
    at: usize,
    depth: usize,
    /// Whether an integer beyond `i64` was read (otherwise `serde_json` would
    /// have read the same value, and its errors are the better ones).
    saw_big: bool,
}

/// `serde_json`'s recursion limit.
const MAX_DEPTH: usize = 128;

impl<'a> Reader<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            text,
            bytes: text.as_bytes(),
            at: 0,
            depth: 0,
            saw_big: false,
        }
    }

    /// `Ok(None)` when the text holds no integer beyond `i64` after all.
    fn document(mut self) -> Result<Option<Value>, String> {
        self.skip_ws();
        let value = self.value()?;
        self.skip_ws();
        if self.at != self.bytes.len() {
            return Err(format!("trailing characters at byte {}", self.at));
        }
        Ok(self.saw_big.then_some(value))
    }

    fn skip_ws(&mut self) {
        while matches!(self.bytes.get(self.at), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }

    fn eat(&mut self, literal: &str) -> bool {
        if self.text[self.at..].starts_with(literal) {
            self.at += literal.len();
            true
        } else {
            false
        }
    }

    fn value(&mut self) -> Result<Value, String> {
        match self.bytes.get(self.at) {
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'"') => self.string().map(Value::Str),
            Some(b't') if self.eat("true") => Ok(Value::Bool(true)),
            Some(b'f') if self.eat("false") => Ok(Value::Bool(false)),
            Some(b'n') if self.eat("null") => Ok(Value::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => Err(format!("expected a value at byte {}", self.at)),
        }
    }

    fn nested<T>(&mut self, f: impl FnOnce(&mut Self) -> Result<T, String>) -> Result<T, String> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err("recursion limit exceeded".to_owned());
        }
        let out = f(self);
        self.depth -= 1;
        out
    }

    fn array(&mut self) -> Result<Value, String> {
        self.nested(|r| {
            r.at += 1;
            let mut items = Vec::new();
            r.skip_ws();
            if r.bytes.get(r.at) == Some(&b']') {
                r.at += 1;
                return Ok(Value::List(items));
            }
            loop {
                r.skip_ws();
                items.push(r.value()?);
                r.skip_ws();
                match r.bytes.get(r.at) {
                    Some(b',') => r.at += 1,
                    Some(b']') => {
                        r.at += 1;
                        return Ok(Value::List(items));
                    }
                    _ => return Err(format!("expected `,` or `]` at byte {}", r.at)),
                }
            }
        })
    }

    fn object(&mut self) -> Result<Value, String> {
        self.nested(|r| {
            r.at += 1;
            let mut entries = IndexMap::new();
            r.skip_ws();
            if r.bytes.get(r.at) == Some(&b'}') {
                r.at += 1;
                return Ok(Value::Map(entries));
            }
            loop {
                r.skip_ws();
                if r.bytes.get(r.at) != Some(&b'"') {
                    return Err(format!("key must be a string at byte {}", r.at));
                }
                let key = r.string()?;
                r.skip_ws();
                if r.bytes.get(r.at) != Some(&b':') {
                    return Err(format!("expected `:` at byte {}", r.at));
                }
                r.at += 1;
                r.skip_ws();
                // A repeated key keeps its first position and takes the last value.
                entries.insert(key, r.value()?);
                r.skip_ws();
                match r.bytes.get(r.at) {
                    Some(b',') => r.at += 1,
                    Some(b'}') => {
                        r.at += 1;
                        return Ok(Value::Map(entries));
                    }
                    _ => return Err(format!("expected `,` or `}}` at byte {}", r.at)),
                }
            }
        })
    }

    /// A string literal, unescaped by `serde_json`.
    fn string(&mut self) -> Result<String, String> {
        let start = self.at;
        self.at += 1;
        loop {
            match self.bytes.get(self.at) {
                Some(b'\\') => self.at += 2,
                Some(b'"') => break,
                Some(_) => self.at += 1,
                None => return Err("unterminated string".to_owned()),
            }
        }
        self.at += 1;
        let literal = self.text.get(start..self.at).ok_or("invalid string")?;
        serde_json::from_str(literal).map_err(|e| e.to_string())
    }

    fn number(&mut self) -> Result<Value, String> {
        let start = self.at;
        let digits = |r: &mut Self| {
            let from = r.at;
            while r.bytes.get(r.at).is_some_and(u8::is_ascii_digit) {
                r.at += 1;
            }
            r.at - from
        };
        if self.bytes.get(self.at) == Some(&b'-') {
            self.at += 1;
        }
        let int_start = self.at;
        match digits(self) {
            0 => return Err(format!("invalid number at byte {start}")),
            n if n > 1 && self.bytes[int_start] == b'0' => {
                return Err(format!("invalid number at byte {start}"));
            }
            _ => {}
        }
        let int_end = self.at;
        let mut is_float = false;
        if self.bytes.get(self.at) == Some(&b'.') {
            self.at += 1;
            if digits(self) == 0 {
                return Err(format!("invalid number at byte {start}"));
            }
            is_float = true;
        }
        if matches!(self.bytes.get(self.at), Some(b'e' | b'E')) {
            self.at += 1;
            if matches!(self.bytes.get(self.at), Some(b'+' | b'-')) {
                self.at += 1;
            }
            if digits(self) == 0 {
                return Err(format!("invalid number at byte {start}"));
            }
            is_float = true;
        }
        let literal = &self.text[start..self.at];
        if is_float {
            return literal
                .parse::<f64>()
                .map(Value::Float)
                .map_err(|e| e.to_string());
        }
        if let Ok(i) = literal.parse::<i64>() {
            return Ok(Value::Int(i));
        }
        self.saw_big = true;
        let negative = literal.starts_with('-');
        let magnitude =
            pynum::parse_digits(&self.text[int_start..int_end], 10).map_err(|e| e.to_string())?;
        Ok(Value::from_bigint(if negative {
            -magnitude
        } else {
            magnitude
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn big_integers_are_exact() {
        let v = parse_json(r#"{"a": 123456789012345678901234567890, "b": [-9223372036854775809, 1.5e3, "12345678901234567890123"], "c": 18446744073709551615}"#).unwrap();
        let Value::Map(m) = v else { panic!("map") };
        assert_eq!(
            format!("{:?}", m["a"]),
            "BigInt(123456789012345678901234567890)"
        );
        let Value::List(l) = &m["b"] else {
            panic!("list")
        };
        assert_eq!(format!("{:?}", l[0]), "BigInt(-9223372036854775809)");
        assert_eq!(l[1], Value::Float(1500.0));
        assert_eq!(l[2], Value::Str("12345678901234567890123".into()));
        assert_eq!(format!("{:?}", m["c"]), "BigInt(18446744073709551615)");
    }

    #[test]
    fn text_without_a_big_integer_reads_as_serde_json_does() {
        for text in [
            r#"{"a": "12345678901234567890123", "b": 0.12345678901234567890}"#,
            "[1, 2, 3]",
            "1e999999999999999999999",
        ] {
            let ours = parse_json(text).map(|v| format!("{v:?}"));
            let theirs = serde_json::from_str::<Value>(text).map(|v| format!("{v:?}"));
            assert_eq!(ours.is_ok(), theirs.is_ok(), "{text}");
            if let (Ok(a), Ok(b)) = (ours, theirs) {
                assert_eq!(a, b, "{text}");
            }
        }
    }

    #[test]
    fn long_digit_strings_and_floats_take_the_serde_path() {
        for text in [
            r#"{"id": "1234567890123456789012", "n": 5}"#,
            r#"{"a": "x\"1234567890123456789012", "b": 0.1234567890123456789012}"#,
            "[1.2345678901234567890123e5, 12345678901234567890.5]",
        ] {
            assert!(!has_big_integer_literal(text.as_bytes()), "{text}");
            assert!(parse_json(text).is_ok(), "{text}");
        }
        assert!(has_big_integer_literal(b"[1, 12345678901234567890]"));
        assert!(has_big_integer_literal(
            b"{\"a\": \"x\", \"b\": -12345678901234567890}"
        ));
    }

    #[test]
    fn the_digit_limit_is_python_s() {
        let ok = format!("[{}]", "9".repeat(4300));
        assert!(matches!(parse_json(&ok), Ok(Value::List(_))));
        let too_long = format!("[{}]", "9".repeat(4301));
        let err = parse_json(&too_long).unwrap_err().to_string();
        assert!(err.contains("Exceeds the limit (4300 digits)"), "{err}");
    }

    #[test]
    fn malformed_text_is_an_error() {
        for text in [
            "[123456789012345678901234567890,]",
            "{\"a\": 123456789012345678901234567890",
            "01234567890123456789012",
            "123456789012345678901234567890 1",
        ] {
            assert!(parse_json(text).is_err(), "{text}");
        }
    }
}
