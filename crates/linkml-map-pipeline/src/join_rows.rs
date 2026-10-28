//! The primary rows of upstream's set-based join engine.
//!
//! Port of the value half of `transformer/join_engine.py::transform_block_via_join`.
//! A block with `joins:` over CSV, TSV or JSON tables does not read its primary
//! table through the loader: DuckDB reads it (every CSV/TSV column as text, every
//! JSON column cast to `VARCHAR`) and `_parse_numeric` is then run over *every*
//! cell of every row, whatever the schema says about the column:
//!
//! * a CSV/TSV cell `"007"` reads as `7`, `"1e3"` as `1000.0` (the loader would
//!   keep text for a column that is not numeric in the schema);
//! * a JSON column has the one type DuckDB infers for all its values (`BOOLEAN`,
//!   `BIGINT`, `DOUBLE`, `VARCHAR`, a `LIST` or `STRUCT`, or `JSON` for a mix of
//!   types), and its cells are the `VARCHAR` casts of that: a boolean is the text
//!   `true`, an integer in a column that also holds a float is `3.0`, a string in
//!   a mixed column keeps its JSON quotes (`"x"`), a list is `[a, 'b,c']`;
//! * a key missing from some JSON rows is a `NULL` cell.
//!
//! Not modelled: DuckDB's date and timestamp detection for string columns (a
//! timestamp reads back as `2020-01-01 10:00:00`), the `HUGEINT`/`DOUBLE` split
//! of integers beyond 64 bits (they print as their digits), and a file DuckDB
//! reads in more than one sample.

use linkml_map_core::{pyfmt::py_float_repr, value::Value};
use linkml_map_io::parse_numeric;

/// A CSV/TSV row read as text: `_parse_numeric` over every cell.
pub(crate) fn parse_text_row(row: Value) -> Value {
    match row {
        Value::Map(cells) => Value::Map(
            cells
                .into_iter()
                .map(|(k, v)| match v {
                    Value::Str(text) => (k, parse_numeric(&text)),
                    other => (k, other),
                })
                .collect(),
        ),
        other => other,
    }
}

/// The type DuckDB's `read_json_auto` gives a column (or a nested value).
#[derive(Debug, Clone, PartialEq)]
enum Ty {
    Null,
    Bool,
    Int,
    Double,
    Str,
    List(Box<Ty>),
    Struct(Vec<(String, Ty)>),
    /// The `JSON` type: values of different types, kept as raw JSON text.
    Json,
}

impl Ty {
    fn of(v: &Value) -> Self {
        match v {
            Value::Null => Self::Null,
            Value::Bool(_) => Self::Bool,
            Value::Int(_) | Value::BigInt(_) | Value::Decimal(_) => Self::Int,
            // PARITY (approximate): DuckDB infers `HUGEINT` for an integer beyond 64
            // bits (and `DOUBLE` beyond 128); both print as the digits here.
            Value::Complex(_) => Self::Str,
            Value::Float(_) => Self::Double,
            Value::Str(_) | Value::Temporal(_) => Self::Str,
            Value::List(items) => Self::List(Box::new(
                items.iter().map(Self::of).fold(Self::Null, Self::merge),
            )),
            Value::Map(m) => {
                Self::Struct(m.iter().map(|(k, v)| (k.clone(), Self::of(v))).collect())
            }
        }
    }

    /// DuckDB's unification of two inferred types: `NULL` yields to anything, an
    /// integer and a float make a `DOUBLE`, lists and structs unify member-wise
    /// (a struct takes the union of the fields), any other mix is `JSON`.
    fn merge(self, other: Self) -> Self {
        match (self, other) {
            (Self::Null, t) | (t, Self::Null) => t,
            (Self::Int, Self::Double) | (Self::Double, Self::Int) => Self::Double,
            (Self::List(a), Self::List(b)) => Self::List(Box::new(a.merge(*b))),
            (Self::Struct(mut a), Self::Struct(b)) => {
                for (k, t) in b {
                    match a.iter_mut().find(|(seen, _)| *seen == k) {
                        Some((_, seen)) => *seen = std::mem::replace(seen, Self::Null).merge(t),
                        None => a.push((k, t)),
                    }
                }
                Self::Struct(a)
            }
            (a, b) if a == b => a,
            _ => Self::Json,
        }
    }
}

/// `CAST(v AS VARCHAR)` for a value of column type `ty`; `nested` inside a list
/// or struct, where text that could be mistaken for syntax is quoted.
fn render(v: &Value, ty: &Ty, nested: bool) -> String {
    match (v, ty) {
        (Value::Null, _) => "NULL".to_owned(),
        (_, Ty::Json) => serde_json::to_string(v).unwrap_or_default(),
        (Value::Bool(b), _) => b.to_string(),
        (Value::Int(i), Ty::Double) => py_float_repr(*i as f64),
        (Value::Int(i), _) => i.to_string(),
        (Value::BigInt(n), Ty::Double) => py_float_repr(
            linkml_map_core::pynum::big_to_f64(n).unwrap_or(f64::INFINITY),
        ),
        (Value::BigInt(n), _) => n.to_string(),
        (Value::Complex(z), _) => linkml_map_core::pynum::complex::repr(**z),
        (Value::Decimal(d), _) => d.to_sci_string(),
        (Value::Float(f), _) => py_float_repr(*f),
        (Value::Str(s), _) if nested => quote_nested(s),
        (Value::Str(s), _) => s.clone(),
        (Value::Temporal(t), _) => t.to_string(),
        (Value::List(items), Ty::List(elem)) => {
            let parts: Vec<String> = items.iter().map(|i| render(i, elem, true)).collect();
            format!("[{}]", parts.join(", "))
        }
        (Value::Map(m), Ty::Struct(fields)) => {
            let parts: Vec<String> = fields
                .iter()
                .map(|(k, t)| {
                    let cell = m
                        .get(k)
                        .map_or_else(|| "NULL".to_owned(), |v| render(v, t, true));
                    format!("{}: {cell}", quote_always(k))
                })
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
        // A value whose type was unified away (cannot happen once `Ty::of` has seen it).
        (other, _) => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// DuckDB quotes a string inside a list or struct when it is empty, has leading
/// or trailing whitespace, reads as `NULL`, or holds any of `{}[](),:='"`.
fn quote_nested(s: &str) -> String {
    let needs = s.is_empty()
        || s.starts_with(char::is_whitespace)
        || s.ends_with(char::is_whitespace)
        || s.eq_ignore_ascii_case("null")
        || s.contains(['{', '}', '[', ']', '(', ')', ',', ':', '=', '\'', '"']);
    if needs { quote_always(s) } else { s.to_owned() }
}

fn quote_always(s: &str) -> String {
    format!("'{}'", s.replace('\'', "\\'"))
}

/// The rows of a JSON table as upstream's join engine sees them: every column
/// (the keys in order of first appearance) cast to text by its inferred type and
/// run through `_parse_numeric`; an absent key is `NULL`.
pub(crate) fn json_rows(rows: Vec<Value>) -> Vec<Value> {
    let mut columns: Vec<(String, Ty)> = Vec::new();
    for row in &rows {
        let Value::Map(cells) = row else { continue };
        for (k, v) in cells {
            let ty = Ty::of(v);
            match columns.iter_mut().find(|(seen, _)| seen == k) {
                Some((_, seen)) => *seen = std::mem::replace(seen, Ty::Null).merge(ty),
                None => columns.push((k.clone(), ty)),
            }
        }
    }
    rows.into_iter()
        .map(|row| {
            let Value::Map(cells) = row else { return row };
            Value::Map(
                columns
                    .iter()
                    .map(|(k, ty)| {
                        let cell = match cells.get(k) {
                            None | Some(Value::Null) => Value::Null,
                            Some(v) => parse_numeric(&render(v, ty, false)),
                        };
                        (k.clone(), cell)
                    })
                    .collect(),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(json: &str) -> Vec<Value> {
        match serde_json::from_str::<Value>(json).unwrap() {
            Value::List(items) => json_rows(items),
            other => panic!("not a list: {other:?}"),
        }
    }

    fn col(rows: &[Value], k: &str) -> Vec<Value> {
        rows.iter()
            .map(|r| match r {
                Value::Map(m) => m.get(k).cloned().unwrap(),
                _ => unreachable!(),
            })
            .collect()
    }

    fn s(t: &str) -> Value {
        Value::Str(t.to_owned())
    }

    #[test]
    fn text_cells_are_parsed_numerically() {
        let row =
            parse_text_row(serde_json::from_str(r#"{"a": "007", "b": "x", "c": "1e3"}"#).unwrap());
        assert_eq!(
            row,
            serde_json::from_str::<Value>(r#"{"a": 7, "b": "x", "c": 1000.0}"#).unwrap()
        );
    }

    // Each case below was checked against DuckDB 1.5 (`CAST(columns(*) AS VARCHAR)
    // FROM read_json_auto(...)`) followed by upstream's `_parse_numeric`.

    #[test]
    fn booleans_become_text() {
        let r = rows(r#"[{"a": true}, {"a": false}]"#);
        assert_eq!(col(&r, "a"), [s("true"), s("false")]);
    }

    #[test]
    fn a_mixed_column_keeps_json_text() {
        let r = rows(r#"[{"a": true}, {"a": "x"}, {"a": 5}]"#);
        assert_eq!(col(&r, "a"), [s("true"), s("\"x\""), Value::Int(5)]);
    }

    #[test]
    fn integers_in_a_float_column_are_floats() {
        let r = rows(r#"[{"a": 1}, {"a": 2.5}]"#);
        assert_eq!(col(&r, "a"), [Value::Float(1.0), Value::Float(2.5)]);
    }

    #[test]
    fn strings_are_parsed_numerically() {
        let r = rows(r#"[{"a": "007"}, {"a": "x"}]"#);
        assert_eq!(col(&r, "a"), [Value::Int(7), s("x")]);
    }

    #[test]
    fn absent_keys_are_null() {
        let r = rows(r#"[{"a": "x"}, {"b": "y"}]"#);
        assert_eq!(col(&r, "a"), [s("x"), Value::Null]);
        assert_eq!(col(&r, "b"), [Value::Null, s("y")]);
    }

    #[test]
    fn nested_values_are_duckdb_text() {
        let r =
            rows(r#"[{"a": ["x", "y,z", ""], "s": {"k": "v", "n": 1}}, {"a": [], "s": {"j": 2}}]"#);
        assert_eq!(col(&r, "a"), [s("[x, 'y,z', '']"), s("[]")]);
        assert_eq!(
            col(&r, "s"),
            [
                s("{'k': v, 'n': 1, 'j': NULL}"),
                s("{'k': NULL, 'n': NULL, 'j': 2}")
            ]
        );
    }
}
