//! Python-compatible text rendering helpers for the output writers.
//!
//! Upstream's writers (`linkml_map/writers/output_streams.py`,
//! `linkml_map/cli/cli.py::dump_output`) lean on Python behaviour that has no
//! direct Rust equivalent: `simplejson.dumps` spacing, `repr(float)`, `str(value)`,
//! `flatten_dict.flatten`, `_strip_nulls` and `_strip_empty`.  This module
//! reproduces just enough of each to keep the CLI output byte-identical.
//!
//! Every function that inspects a [`Value`] matches it exhaustively, so adding a
//! variant is a compile error pointing at each rendering site.

use std::fmt::Write as _;

use indexmap::IndexMap;
use linkml_map_core::value::Value;

// ─── Floats ──────────────────────────────────────────────────────────────────

/// Render a finite float the way Python's `repr(float)` does.
///
/// Shortest round-trip digits; positional notation while the decimal exponent
/// lies in `-4 < decpt <= 16`, otherwise `d.ddde+XX` with at least two exponent
/// digits (`1e+16`, `1e-05`).  Non-finite values use Python's `str()` spelling
/// (`nan`, `inf`, `-inf`).
///
/// ```
/// # use linkml_map_io::pycompat::py_float_repr;
/// assert_eq!(py_float_repr(1.0), "1.0");
/// assert_eq!(py_float_repr(1e16), "1e+16");
/// assert_eq!(py_float_repr(0.00001), "1e-05");
/// assert_eq!(py_float_repr(172.5), "172.5");
/// ```
pub fn py_float_repr(f: f64) -> String {
    // One implementation for the whole workspace: the expression evaluator's.
    linkml_map_core::pyfmt::py_float_repr(f)
}

// ─── JSON (simplejson.dumps) ─────────────────────────────────────────────────

/// Render *value* as `simplejson.dumps(value, ensure_ascii=False, allow_nan=True)`.
///
/// * `indent: None` gives Python's default compact form with `", "` and `": "`
///   separators; `Some(n)` gives the pretty form with `n`-space indentation.
/// * `skip_null` applies `_strip_nulls` while writing: `None`-valued object
///   members are dropped at every depth, `None` list elements are kept.
///
/// ```
/// # use linkml_map_io::pycompat::dumps_json;
/// # use linkml_map_core::value::Value;
/// let v: Value = serde_json::from_str(r#"{"a": 1, "b": null, "c": [1, null]}"#).unwrap();
/// assert_eq!(dumps_json(&v, None, true).unwrap(), r#"{"a": 1, "c": [1, null]}"#);
/// ```
///
/// # Errors
/// simplejson's `TypeError` message: a `set` is not JSON serialisable, and a
/// dict key must be a string, number, boolean or `None`. (A tuple is written
/// as an array and a dict's keys as text, as simplejson does.)
pub fn dumps_json(value: &Value, indent: Option<usize>, skip_null: bool) -> Result<String, String> {
    let mut out = String::new();
    write_json(&mut out, value, indent, 0, skip_null)?;
    Ok(out)
}

fn write_json(
    out: &mut String,
    value: &Value,
    indent: Option<usize>,
    level: usize,
    skip_null: bool,
) -> Result<(), String> {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Int(i) => {
            let _ = write!(out, "{i}");
        }
        Value::Float(f) => out.push_str(&json_float(*f)),
        // simplejson(use_decimal=True) writes `str(Decimal)` as a numeric literal.
        Value::Decimal(d) => out.push_str(&d.to_sci_string()),
        // PARITY (lenient): simplejson raises `TypeError: Object of type date is
        // not JSON serializable` for the `datetime.date` PyYAML builds from an
        // unquoted timestamp, aborting the run part-way through the output.
        // The `str()` text is written instead.
        Value::Temporal(t) => write_json_str(out, &t.to_string()),
        Value::Str(s) => write_json_str(out, s),
        Value::List(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return Ok(());
            }
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                write_item_separator(out, i == 0, indent, level + 1);
                write_json(out, item, indent, level + 1, skip_null)?;
            }
            write_close(out, ']', indent, level);
        }
        // simplejson's `tuple_as_array`: a tuple is an array. `_strip_nulls` does
        // not look inside one, so nothing below it is stripped.
        Value::Tuple(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return Ok(());
            }
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                write_item_separator(out, i == 0, indent, level + 1);
                write_json(out, item, indent, level + 1, false)?;
            }
            write_close(out, ']', indent, level);
        }
        Value::Map(map) => {
            let mut members = map
                .iter()
                .filter(|(_, v)| !(skip_null && v.is_null()))
                .peekable();
            if members.peek().is_none() {
                out.push_str("{}");
                return Ok(());
            }
            out.push('{');
            for (i, (key, member)) in members.enumerate() {
                write_item_separator(out, i == 0, indent, level + 1);
                write_json_str(out, key);
                out.push_str(": ");
                write_json(out, member, indent, level + 1, skip_null)?;
            }
            write_close(out, '}', indent, level);
        }
        // A dict whose keys are not all strings: int, float, bool and None keys
        // are written as text, any other key is simplejson's `TypeError`.
        Value::Dict(dict) => {
            let mut members = dict
                .iter()
                .filter(|(_, v)| !(skip_null && v.is_null()))
                .peekable();
            if members.peek().is_none() {
                out.push_str("{}");
                return Ok(());
            }
            out.push('{');
            for (i, (key, member)) in members.enumerate() {
                write_item_separator(out, i == 0, indent, level + 1);
                write_json_str(out, &linkml_map_core::value::simplejson_key(key)?);
                out.push_str(": ");
                write_json(out, member, indent, level + 1, skip_null)?;
            }
            write_close(out, '}', indent, level);
        }
        Value::Set(_) => return Err("Object of type set is not JSON serializable".to_owned()),
    }
    Ok(())
}

fn write_item_separator(out: &mut String, first: bool, indent: Option<usize>, level: usize) {
    match indent {
        Some(width) => {
            if !first {
                out.push(',');
            }
            out.push('\n');
            out.extend(std::iter::repeat_n(' ', width * level));
        }
        None => {
            if !first {
                out.push_str(", ");
            }
        }
    }
}

fn write_close(out: &mut String, close: char, indent: Option<usize>, level: usize) {
    if let Some(width) = indent {
        out.push('\n');
        out.extend(std::iter::repeat_n(' ', width * level));
    }
    out.push(close);
}

fn write_json_str(out: &mut String, s: &str) {
    // serde_json escapes exactly what `json.dumps(ensure_ascii=False)` does:
    // `"`, `\`, and control characters (`\b \f \n \r \t`, otherwise `\u00XX`).
    match serde_json::to_string(s) {
        Ok(quoted) => out.push_str(&quoted),
        Err(_) => {
            let _ = write!(out, "{s:?}");
        }
    }
}

/// `float.__repr__`, with the `NaN` / `Infinity` spellings `json` uses.
fn json_float(f: f64) -> String {
    if f.is_nan() {
        "NaN".to_owned()
    } else if f.is_infinite() {
        if f < 0.0 { "-Infinity" } else { "Infinity" }.to_owned()
    } else {
        py_float_repr(f)
    }
}

// ─── str() / repr() ──────────────────────────────────────────────────────────

/// Python `str(value)` for a scalar (a tuple or set too), `dumps_json(value)`
/// for a list or dict.
///
/// This is `TabularStreamWriter._escape_value` minus the quoting: booleans
/// render as `True` / `False` and `None` as the empty string.
///
/// # Errors
/// The `TypeError` of `dumps_json` for a list or dict holding a set.
pub fn tabular_cell(value: &Value) -> Result<String, String> {
    Ok(match value {
        Value::Null => String::new(),
        Value::Bool(b) => py_bool(*b).to_owned(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => py_float_repr(*f),
        // `format_decimal`: fixed-point, every digit kept.
        Value::Decimal(d) => d.to_fixed_string(),
        // `str(date)`.
        Value::Temporal(t) => t.to_string(),
        Value::Str(s) => s.clone(),
        Value::List(_) | Value::Map(_) | Value::Dict(_) => dumps_json(value, None, false)?,
        // `str(tuple)` / `str(set)`: not a list or dict, so not JSON.
        Value::Tuple(_) | Value::Set(_) => py_repr(value),
    })
}

/// Python `str(value)`: scalars as printed, containers as their `repr`.
pub fn py_str(value: &Value) -> String {
    match value {
        Value::Str(s) => s.clone(),
        Value::Decimal(d) => d.to_sci_string(),
        Value::Temporal(t) => t.to_string(),
        other => py_repr(other),
    }
}

/// Python `repr(value)`.
pub fn py_repr(value: &Value) -> String {
    match value {
        Value::Null => "None".to_owned(),
        Value::Bool(b) => py_bool(*b).to_owned(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => py_float_repr(*f),
        Value::Decimal(d) => format!("Decimal('{}')", d.to_sci_string()),
        Value::Temporal(t) => t.repr(),
        Value::Str(s) => py_str_repr(s),
        Value::List(items) => {
            let inner: Vec<String> = items.iter().map(py_repr).collect();
            format!("[{}]", inner.join(", "))
        }
        Value::Map(map) => {
            let inner: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{}: {}", py_str_repr(k), py_repr(v)))
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
        Value::Tuple(items) => {
            let inner: Vec<String> = items.iter().map(py_repr).collect();
            match inner.as_slice() {
                [only] => format!("({only},)"),
                _ => format!("({})", inner.join(", ")),
            }
        }
        Value::Set(set) => {
            if set.is_empty() {
                "set()".to_owned()
            } else {
                let inner: Vec<String> = set.iter().map(py_repr).collect();
                format!("{{{}}}", inner.join(", "))
            }
        }
        Value::Dict(dict) => {
            let inner: Vec<String> = dict
                .iter()
                .map(|(k, v)| format!("{}: {}", py_repr(k), py_repr(v)))
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
    }
}

fn py_bool(b: bool) -> &'static str {
    if b { "True" } else { "False" }
}

/// Python `repr(str)` (see [`linkml_map_core::pyfmt::py_repr_str`]).
fn py_str_repr(s: &str) -> String {
    linkml_map_core::pyfmt::py_repr_str(s)
}

// ─── Null / empty stripping ──────────────────────────────────────────────────

/// Mirror of `output_streams._strip_nulls`: drop `None`-valued dict keys,
/// recursively; `None` elements in lists are kept.
pub fn strip_nulls(value: &Value) -> Value {
    match value {
        Value::Map(map) => Value::Map(
            map.iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| (k.clone(), strip_nulls(v)))
                .collect(),
        ),
        Value::List(items) => Value::List(items.iter().map(strip_nulls).collect()),
        // `_strip_nulls` recurses into dicts (of any key type) and lists only: a
        // tuple or set is returned as it is.
        Value::Dict(dict) => Value::Dict(Box::new(
            dict.iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| (k.clone(), strip_nulls(v)))
                .collect(),
        )),
        scalar @ (Value::Null
        | Value::Bool(_)
        | Value::Int(_)
        | Value::Float(_)
        | Value::Decimal(_)
        | Value::Temporal(_)
        | Value::Str(_)
        | Value::Tuple(_)
        | Value::Set(_)) => scalar.clone(),
    }
}

/// Mirror of `cli._strip_empty`: recursively drop `None`, empty lists and empty
/// dicts, keeping the outer container.
pub fn strip_empty(value: &Value) -> Value {
    match value {
        Value::Map(map) => Value::Map(
            map.iter()
                .map(|(k, v)| (k.clone(), strip_empty(v)))
                .filter(|(_, v)| !is_empty(v))
                .collect(),
        ),
        Value::List(items) => Value::List(
            items
                .iter()
                .map(strip_empty)
                .filter(|v| !is_empty(v))
                .collect(),
        ),
        Value::Dict(dict) => Value::Dict(Box::new(
            dict.iter()
                .map(|(k, v)| (k.clone(), strip_empty(v)))
                .filter(|(_, v)| !is_empty(v))
                .collect(),
        )),
        scalar @ (Value::Null
        | Value::Bool(_)
        | Value::Int(_)
        | Value::Float(_)
        | Value::Decimal(_)
        | Value::Temporal(_)
        | Value::Str(_)
        | Value::Tuple(_)
        | Value::Set(_)) => scalar.clone(),
    }
}

/// Mirror of `cli._is_empty`: `None` or an empty list or dict.
fn is_empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::List(items) => items.is_empty(),
        Value::Map(map) => map.is_empty(),
        Value::Dict(dict) => dict.is_empty(),
        Value::Bool(_)
        | Value::Int(_)
        | Value::Float(_)
        | Value::Decimal(_)
        | Value::Temporal(_)
        | Value::Str(_)
        | Value::Tuple(_)
        | Value::Set(_) => false,
    }
}

/// Recursively sort mapping keys, as PyYAML's `sort_keys=True` does.
pub fn sort_keys(value: &Value) -> Value {
    match value {
        Value::Map(map) => {
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            Value::Map(
                entries
                    .into_iter()
                    .map(|(k, v)| (k.clone(), sort_keys(v)))
                    .collect(),
            )
        }
        Value::List(items) => Value::List(items.iter().map(sort_keys).collect()),
        scalar @ (Value::Null
        | Value::Bool(_)
        | Value::Int(_)
        | Value::Float(_)
        | Value::Decimal(_)
        | Value::Temporal(_)
        | Value::Str(_)
        | Value::Tuple(_)
        | Value::Set(_)
        | Value::Dict(_)) => scalar.clone(),
    }
}

// ─── flatten_dict.flatten ────────────────────────────────────────────────────

/// Mirror of `flatten_dict.flatten(obj, reducer=make_reducer(sep))`.
///
/// Nested mappings are flattened into `parent<sep>child` keys; lists and scalars
/// are leaves; an empty mapping contributes no key at all.  Returns `None` when
/// *value* is not a mapping (`flatten` raises `ValueError` for those).
pub fn flatten<'a>(value: &'a Value, sep: &str) -> Option<IndexMap<String, &'a Value>> {
    let Value::Map(map) = value else {
        return None;
    };
    let mut flat = IndexMap::new();
    flatten_into(map, None, sep, &mut flat);
    Some(flat)
}

fn flatten_into<'a>(
    map: &'a IndexMap<String, Value>,
    parent: Option<&str>,
    sep: &str,
    flat: &mut IndexMap<String, &'a Value>,
) {
    for (key, value) in map {
        let flat_key = match parent {
            Some(p) => format!("{p}{sep}{key}"),
            None => key.clone(),
        };
        flatten_value(value, flat_key, sep, flat);
    }
}

/// One member of a flattened mapping: a mapping recurses, anything else is a leaf.
fn flatten_value<'a>(
    value: &'a Value,
    flat_key: String,
    sep: &str,
    flat: &mut IndexMap<String, &'a Value>,
) {
    match value {
        Value::Map(child) => flatten_into(child, Some(&flat_key), sep, flat),
        // `flatten_dict` recurses into any mapping, joining the keys as `str()`.
        Value::Dict(child) => {
            for (k, v) in child.iter() {
                flatten_value(v, format!("{flat_key}{sep}{}", py_str(k)), sep, flat);
            }
        }
        leaf @ (Value::Null
        | Value::Bool(_)
        | Value::Int(_)
        | Value::Float(_)
        | Value::Decimal(_)
        | Value::Temporal(_)
        | Value::Str(_)
        | Value::List(_)
        | Value::Tuple(_)
        | Value::Set(_)) => {
            flat.insert(flat_key, leaf);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(json: &str) -> Value {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn float_repr_matches_python() {
        for (f, want) in [
            (0.0, "0.0"),
            (-0.0, "-0.0"),
            (1.0, "1.0"),
            (172.5, "172.5"),
            (0.1, "0.1"),
            (1e15, "1000000000000000.0"),
            (1e16, "1e+16"),
            (1.5e16, "1.5e+16"),
            (123456789012345680.0, "1.2345678901234568e+17"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (1.5e-7, "1.5e-07"),
            (-2.5e-10, "-2.5e-10"),
            (1e100, "1e+100"),
        ] {
            assert_eq!(py_float_repr(f), want, "{f:?}");
        }
    }

    #[test]
    fn dumps_json_spacing_matches_simplejson() {
        let val = v(r#"{"a": [1, 2.0, {"b": "x"}], "c": {}, "d": []}"#);
        assert_eq!(
            dumps_json(&val, None, false).unwrap(),
            r#"{"a": [1, 2.0, {"b": "x"}], "c": {}, "d": []}"#
        );
        assert_eq!(
            dumps_json(&val, Some(2), false).unwrap(),
            "{\n  \"a\": [\n    1,\n    2.0,\n    {\n      \"b\": \"x\"\n    }\n  ],\n  \"c\": {},\n  \"d\": []\n}"
        );
    }

    #[test]
    fn dumps_json_keeps_non_ascii_and_escapes_controls() {
        let val = Value::Str("caf\u{e9}\n\u{1}".to_owned());
        assert_eq!(
            dumps_json(&val, None, false).unwrap(),
            "\"caf\u{e9}\\n\\u0001\""
        );
    }

    #[test]
    fn dumps_json_non_finite_floats() {
        assert_eq!(
            dumps_json(&Value::Float(f64::NAN), None, false).unwrap(),
            "NaN"
        );
        assert_eq!(
            dumps_json(&Value::Float(f64::INFINITY), None, false).unwrap(),
            "Infinity"
        );
        assert_eq!(
            dumps_json(&Value::Float(f64::NEG_INFINITY), None, false).unwrap(),
            "-Infinity"
        );
    }

    /// Upstream `test_writers_drop_null_slots` row.
    #[test]
    fn strip_nulls_matches_upstream_contract() {
        let row = v(
            r#"{"id": "p1", "n": null, "q": {"unit": "mg", "value": null}, "tags": ["a", null]}"#,
        );
        let want = v(r#"{"id": "p1", "q": {"unit": "mg"}, "tags": ["a", null]}"#);
        assert_eq!(strip_nulls(&row), want);
        // dumps_json(skip_null).unwrap() and strip_nulls agree.
        assert_eq!(
            dumps_json(&row, None, true).unwrap(),
            dumps_json(&want, None, false).unwrap()
        );
        // A dict emptied by stripping stays as `{}`.
        assert_eq!(strip_nulls(&v(r#"{"a": {"b": null}}"#)), v(r#"{"a": {}}"#));
    }

    /// Upstream `_strip_empty` doctest.
    #[test]
    fn strip_empty_matches_upstream_doctest() {
        let got = strip_empty(&v(r#"{"a": [], "b": {"c": null}, "d": [{}, 1]}"#));
        assert_eq!(got, v(r#"{"d": [1]}"#));
        // The outer container is kept even when everything inside is dropped.
        assert_eq!(strip_empty(&v(r#"{"a": null}"#)), v("{}"));
    }

    #[test]
    fn sort_keys_is_recursive() {
        let got = sort_keys(&v(r#"{"b": 1, "a": {"z": 1, "y": [{"q": 1, "p": 2}]}}"#));
        assert_eq!(
            dumps_json(&got, None, false).unwrap(),
            r#"{"a": {"y": [{"p": 2, "q": 1}], "z": 1}, "b": 1}"#
        );
    }

    #[test]
    fn flatten_matches_flatten_dict() {
        let val =
            v(r#"{"id": 1, "q": {"unit": "mg", "inner": {"x": 1}, "none": {}}, "tags": [1, 2]}"#);
        let flat = flatten(&val, "__").unwrap();
        let keys: Vec<&str> = flat.keys().map(String::as_str).collect();
        assert_eq!(keys, ["id", "q__unit", "q__inner__x", "tags"]);
        assert!(flatten(&Value::Int(1), "__").is_none());
    }

    #[test]
    fn tabular_cell_follows_str() {
        assert_eq!(tabular_cell(&Value::Bool(true)).unwrap(), "True");
        assert_eq!(tabular_cell(&Value::Float(2.0)).unwrap(), "2.0");
        assert_eq!(tabular_cell(&Value::Null).unwrap(), "");
        assert_eq!(tabular_cell(&v(r#"[1, "a"]"#)).unwrap(), r#"[1, "a"]"#);
    }

    #[test]
    fn py_repr_follows_python() {
        assert_eq!(
            py_str(&v(r#"["a", 1, null, true, {"k": 1.5}]"#)),
            "['a', 1, None, True, {'k': 1.5}]"
        );
        assert_eq!(py_str_repr("it's"), "\"it's\"");
        assert_eq!(py_str_repr("a'b\"c"), "'a\\'b\"c'");
    }
}
