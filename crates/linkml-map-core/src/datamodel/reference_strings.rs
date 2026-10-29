//! The shape of a multivalued string field (`sources`) once upstream's
//! `ReferenceValidator` has normalised it, as `spec_normalizer._shape_normalize` does
//! before pydantic builds the model.
//!
//! `ReferenceValidator` wraps a bare scalar into a one-element list, turns a mapping into
//! the list of its values and stringifies every non-string scalar with Python's `str()`
//! (`1` -> `'1'`, `True` -> `'True'`, `1.5` -> `'1.5'`). It leaves a null where it finds
//! one, and pydantic then refuses it (`Input should be a valid string`), so `sources:`
//! with no value, `sources: [null]` and a null among the values all fail to load. A list
//! inside the list is `NotImplementedError("List of Lists")`.

use serde::{Deserialize, Deserializer, de::Error};

use crate::{expr::eval::py_repr, pyfmt::py_float_repr, value::Value};

/// `Option<Vec<String>>` of a multivalued string field, in the shape upstream accepts.
///
/// Meant for `#[serde(default, deserialize_with = ...)]`: an absent field is `None`, an
/// explicit null is an error.
pub(super) fn deserialize_option<'de, D>(deserializer: D) -> Result<Option<Vec<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde_json::Value as J;
    let items = match Option::<J>::deserialize(deserializer)? {
        None | Some(J::Null) => return Err(D::Error::custom(NOT_A_STRING)),
        Some(J::Array(items)) if items.iter().any(J::is_array) => {
            return Err(D::Error::custom(format!(
                "NotImplementedError: List of Lists: {}",
                py_repr(&Value::from(&J::Array(items)))
            )));
        }
        Some(J::Array(items)) => items,
        Some(J::Object(map)) => map.into_iter().map(|(_, v)| v).collect(),
        Some(scalar) => vec![scalar],
    };
    items
        .iter()
        .map(|item| stringify(item).map_err(D::Error::custom))
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

const NOT_A_STRING: &str = "sources: Input should be a valid string [type=string_type, input_value=None, input_type=NoneType]";

/// Python `str()` of one item, as `ReferenceValidator` writes it.
fn stringify(item: &serde_json::Value) -> Result<String, String> {
    use serde_json::Value as J;
    Ok(match item {
        J::Null => return Err(NOT_A_STRING.to_owned()),
        J::String(s) => s.clone(),
        J::Bool(b) => if *b { "True" } else { "False" }.to_owned(),
        J::Number(n) if n.is_f64() => py_float_repr(n.as_f64().unwrap_or_default()),
        J::Number(n) => n.to_string(),
        // A list among a mapping's values: no upstream reference; spelt like its `str()`.
        J::Array(_) | J::Object(_) => py_repr(&Value::from(item)),
    })
}
