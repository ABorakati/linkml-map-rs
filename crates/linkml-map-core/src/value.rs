//! Dynamic Value type for runtime transformation expression evaluation.
//!
//! This mirrors the Python value model used by `linkml_map.utils.eval_utils`:
//! a nullable dynamic value that can be a scalar, list, or map (object).

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

/// A dynamically-typed value during transformation.
///
/// Objects (dicts) are represented as [`Value::Map`]; an unbound variable
/// resolves to [`Value::Null`] rather than an error (SQL-style semantics).
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// String.
    Str(String),
    /// Ordered list.
    List(Vec<Value>),
    /// Ordered map (object/dict).
    Map(IndexMap<String, Value>),
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

/// Parse a string the way Python's `float()` does for the cases relevant to
/// these expressions: leading/trailing whitespace is stripped, decimals and
/// scientific notation are accepted, an empty string fails.
pub fn parse_python_float(s: &str) -> Option<f64> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return None;
    }
    trimmed.parse::<f64>().ok()
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
                } else {
                    Value::Float(n.as_f64().unwrap_or(f64::NAN))
                }
            }
            serde_json::Value::String(s) => Value::Str(s.clone()),
            serde_json::Value::Array(arr) => Value::List(arr.iter().map(Value::from).collect()),
            serde_json::Value::Object(map) => {
                Value::Map(map.iter().map(|(k, v)| (k.clone(), Value::from(v))).collect())
            }
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
            Value::Str(s) => serde_json::Value::String(s.clone()),
            Value::List(items) => {
                serde_json::Value::Array(items.iter().map(serde_json::Value::from).collect())
            }
            Value::Map(m) => serde_json::Value::Object(
                m.iter().map(|(k, v)| (k.clone(), serde_json::Value::from(v))).collect(),
            ),
        }
    }
}

impl Value {
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
            (Int(a), Float(b)) | (Float(b), Int(a)) => (*a as f64) == *b,
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
            (Str(a), Str(b)) => a == b,
            (List(a), List(b)) => a == b,
            (Map(a), Map(b)) => a == b,
            _ => false,
        }
    }
}
