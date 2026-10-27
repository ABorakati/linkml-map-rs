//! The scalar half of PyYAML's `SafeLoader` (`yaml/constructor.py`).
//!
//! `yaml.safe_load` decides what a scalar *is* in two steps. The resolver
//! (`yaml/resolver.py`, already ported for the emitter as
//! [`resolve_implicit`]) gives an untagged **plain** scalar one of the YAML 1.1
//! tags by regular expression; quoted and block scalars are always `!!str`.
//! The constructor then builds the Python value for the tag:
//! `construct_yaml_null`, `construct_yaml_bool`, `construct_yaml_int`,
//! `construct_yaml_float`, `construct_yaml_timestamp` and `construct_yaml_str`.
//!
//! This module ports that second step so the loaders in `linkml-map-io` can
//! read YAML exactly as upstream does: `yes`/`on` are booleans, `017` is the
//! octal 15, `1_000` is 1000, `1:30` is 90, `~` is null, `2024-01-01` is a
//! `datetime.date`, but `1e3`, `0o7` and `1.0e20` (no sign on the exponent) are
//! plain strings.
//!
//! The parser lives in `linkml-map-io`; it hands each scalar here with its tag
//! (if any) and whether it was plain.

use super::{Tag, resolve_implicit};
use crate::decimal::Decimal;
use crate::temporal::Temporal;
use crate::value::Value;

/// Why a document could not be loaded, split the way upstream's `except` clause
/// splits it (`cli.py::_map_data_single` falls back to `json.loads` only on
/// `yaml.YAMLError`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadError {
    /// A `yaml.YAMLError` (reader, scanner, parser, composer or constructor error).
    Yaml(String),
    /// A plain Python exception raised while building a value (`ValueError` for
    /// an impossible date or `!!int abc`, `KeyError` for `!!bool maybe`); it is
    /// *not* a `YAMLError`, so it propagates out of `yaml.safe_load`.
    Value(String),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Yaml(m) | LoadError::Value(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for LoadError {}

const TAG_PREFIX: &str = "tag:yaml.org,2002:";

/// What a scalar used as a *mapping key* means to `flatten_mapping`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyRole {
    /// `<<` (`tag:yaml.org,2002:merge`): its value is merged into the mapping.
    Merge,
    /// An ordinary key.
    Plain,
}

/// The tag a scalar ends up with (`Composer.compose_scalar_node`): an explicit
/// tag other than `!`, else, when the implicit resolvers apply, the one that
/// matches the text, else `str`.
///
/// `implicit` is the parser's `implicit[0]` (libyaml's `plain_implicit`): true
/// for a plain untagged scalar *and* for a scalar tagged with the non-specific
/// `!`, whatever its quoting (`! '1'` is the integer 1).
fn effective_tag(tag: Option<&str>, text: &str, implicit: bool) -> String {
    match tag {
        Some(t) if t != "!" => t.to_owned(),
        _ if implicit => {
            let name = match resolve_implicit(text) {
                Tag::Str => "str",
                Tag::Int => "int",
                Tag::Float => "float",
                Tag::Bool => "bool",
                Tag::Null => "null",
                Tag::Merge => "merge",
                Tag::Timestamp => "timestamp",
                Tag::Value => "value",
                Tag::Yaml => "yaml",
            };
            format!("{TAG_PREFIX}{name}")
        }
        _ => format!("{TAG_PREFIX}str"),
    }
}

/// How `flatten_mapping` treats a scalar key.
pub fn key_role(tag: Option<&str>, text: &str, implicit: bool) -> KeyRole {
    if effective_tag(tag, text, implicit) == format!("{TAG_PREFIX}merge") {
        KeyRole::Merge
    } else {
        KeyRole::Plain
    }
}

/// Build the value of a scalar node (`SafeConstructor` dispatch on the tag).
///
/// * `tag`: the explicit tag as written, expanded (`!!int` is
///   `tag:yaml.org,2002:int`), or `None`; `!` counts as no tag.
/// * `implicit`: the implicit resolvers apply, i.e. the scalar is plain and
///   untagged or is tagged `!` (libyaml's `plain_implicit`).
///
/// # Errors
/// [`LoadError::Yaml`] for a tag `SafeLoader` has no scalar constructor for
/// (`construct_undefined`); [`LoadError::Value`] for text the constructor
/// cannot convert.
pub fn construct_scalar(tag: Option<&str>, text: &str, implicit: bool) -> Result<Value, LoadError> {
    let tag = effective_tag(tag, text, implicit);
    let Some(name) = tag.strip_prefix(TAG_PREFIX) else {
        return Err(undefined(&tag));
    };
    match name {
        "null" => Ok(Value::Null),
        "bool" => construct_yaml_bool(text),
        "int" => construct_yaml_int(text),
        "float" => construct_yaml_float(text),
        "timestamp" => Temporal::from_yaml_timestamp(text)
            .map(Value::Temporal)
            .map_err(LoadError::Value),
        "str" => Ok(Value::Str(text.to_owned())),
        // `merge`, `value` and `yaml` have no constructor of their own, and the
        // collection tags cannot apply to a scalar.
        _ => Err(undefined(&tag)),
    }
}

fn undefined(tag: &str) -> LoadError {
    LoadError::Yaml(format!(
        "could not determine a constructor for the tag '{tag}'"
    ))
}

/// `construct_yaml_bool`: `bool_values[value.lower()]`.
fn construct_yaml_bool(text: &str) -> Result<Value, LoadError> {
    match text.to_lowercase().as_str() {
        "yes" | "true" | "on" => Ok(Value::Bool(true)),
        "no" | "false" | "off" => Ok(Value::Bool(false)),
        _ => Err(LoadError::Value(format!("KeyError: {text:?}"))),
    }
}

/// An integer built in `i128`, narrowed to [`Value::Int`] or, beyond `i64`
/// (Python's `int` is unbounded), an exact [`Value::Decimal`].
fn int_value(n: i128) -> Value {
    match i64::try_from(n) {
        Ok(i) => Value::Int(i),
        Err(_) => Decimal::parse_python(&n.to_string())
            .map_or(Value::Str(n.to_string()), |d| Value::Decimal(d)),
    }
}

/// Python's `int(text, base)` for the digits the constructor passes it.
fn parse_radix(digits: &str, base: u32, original: &str) -> Result<i128, LoadError> {
    // Python's `int(text, base)` also accepts surrounding whitespace.
    i128::from_str_radix(digits.trim(), base).map_err(|_| {
        LoadError::Value(format!(
            "ValueError: invalid literal for int() with base {base}: {original:?}"
        ))
    })
}

/// `construct_yaml_int`.
fn construct_yaml_int(text: &str) -> Result<Value, LoadError> {
    let value = text.replace('_', "");
    let (sign, value) = match value.as_bytes().first() {
        Some(b'-') => (-1i128, &value[1..]),
        Some(b'+') => (1, &value[1..]),
        _ => (1, value.as_str()),
    };
    let n = if value == "0" {
        0
    } else if let Some(rest) = value.strip_prefix("0b") {
        sign * parse_radix(rest, 2, text)?
    } else if let Some(rest) = value.strip_prefix("0x") {
        sign * parse_radix(rest, 16, text)?
    } else if value.starts_with('0') {
        sign * parse_radix(value, 8, text)?
    } else if value.contains(':') {
        // Sexagesimal: base-60 digits, most significant first.
        let mut total = 0i128;
        for part in value.split(':') {
            total = total * 60 + parse_radix(part, 10, text)?;
        }
        sign * total
    } else {
        sign * parse_radix(value, 10, text)?
    };
    Ok(int_value(n))
}

/// Python `float(text)` for the spellings the float resolver admits (and the
/// `inf`/`nan` ones an explicit `!!float` tag lets through).
fn py_float(text: &str) -> Result<f64, LoadError> {
    text.parse::<f64>().map_err(|_| {
        LoadError::Value(format!(
            "ValueError: could not convert string to float: {text:?}"
        ))
    })
}

/// `construct_yaml_float`.
fn construct_yaml_float(text: &str) -> Result<Value, LoadError> {
    let value = text.replace('_', "").to_lowercase();
    let (sign, value) = match value.as_bytes().first() {
        Some(b'-') => (-1.0, &value[1..]),
        Some(b'+') => (1.0, &value[1..]),
        _ => (1.0, value.as_str()),
    };
    let f = if value == ".inf" {
        sign * f64::INFINITY
    } else if value == ".nan" {
        f64::NAN
    } else if value.contains(':') {
        let mut total = 0.0;
        for part in value.split(':') {
            total = total * 60.0 + py_float(part)?;
        }
        sign * total
    } else {
        sign * py_float(value)?
    };
    Ok(Value::Float(f))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(text: &str) -> Value {
        construct_scalar(None, text, true).unwrap()
    }

    // Expected values from `yaml.safe_load` under PyYAML 6.0.3.
    #[test]
    fn plain_scalars_resolve_as_yaml_1_1() {
        assert_eq!(plain("yes"), Value::Bool(true));
        assert_eq!(plain("Off"), Value::Bool(false));
        assert_eq!(plain("~"), Value::Null);
        assert_eq!(plain(""), Value::Null);
        assert_eq!(plain("017"), Value::Int(15));
        assert_eq!(plain("0x1F"), Value::Int(31));
        assert_eq!(plain("-0b101"), Value::Int(-5));
        assert_eq!(plain("1_000"), Value::Int(1000));
        assert_eq!(plain("1:30"), Value::Int(90));
        assert_eq!(plain("-1:30:15"), Value::Int(-5415));
        // The resolver's `[0-9_]*` lets underscores repeat and trail.
        assert_eq!(plain("1__0"), Value::Int(10));
        assert_eq!(plain("1_"), Value::Int(1));
        assert_eq!(plain("0_7"), Value::Int(7));
        assert_eq!(plain("-0"), Value::Int(0));
        assert_eq!(plain("1."), Value::Float(1.0));
        assert_eq!(plain("1:2:3.5"), Value::Float(3723.5));
        assert_eq!(plain("1.5"), Value::Float(1.5));
        assert_eq!(plain("1_0.5"), Value::Float(10.5));
        assert_eq!(plain("1:30.5"), Value::Float(90.5));
        assert_eq!(plain("1.0e+20"), Value::Float(1e20));
        assert_eq!(plain(".inf"), Value::Float(f64::INFINITY));
        assert_eq!(plain("-.INF"), Value::Float(f64::NEG_INFINITY));
        assert!(matches!(plain(".nan"), Value::Float(f) if f.is_nan()));
        // Not numbers in YAML 1.1:
        for text in [
            "1.0e20",
            "1e3",
            "0o7",
            "1E5",
            "+.5",
            "-.5",
            ".5e3",
            "inf",
            "nan",
            "NaN",
            "Yes please",
            "08",
            "0x",
            "_1",
            "0b2",
            "0b",
            "1:60",
            ".iNf",
        ] {
            assert_eq!(plain(text), Value::Str(text.to_owned()), "{text:?}");
        }
        assert!(matches!(plain("2024-01-01"), Value::Temporal(_)));
        assert_eq!(
            plain("2024-01-01 junk"),
            Value::Str("2024-01-01 junk".into())
        );
    }

    #[test]
    fn quoted_scalars_stay_strings_and_tags_override() {
        let c = |tag: Option<&str>, text: &str, plain: bool| construct_scalar(tag, text, plain);
        assert_eq!(c(None, "yes", false), Ok(Value::Str("yes".into())));
        assert_eq!(
            c(Some("tag:yaml.org,2002:int"), "42", false),
            Ok(Value::Int(42))
        );
        assert_eq!(
            c(Some("tag:yaml.org,2002:str"), "42", true),
            Ok(Value::Str("42".into()))
        );
        // The non-specific tag `!` applies the implicit resolvers to any style.
        assert_eq!(c(Some("!"), "42", true), Ok(Value::Int(42)));
        assert_eq!(c(Some("!"), "42", false), Ok(Value::Str("42".into())));
        assert!(matches!(
            c(Some("tag:yaml.org,2002:int"), "abc", true),
            Err(LoadError::Value(_))
        ));
        assert!(matches!(
            c(Some("!custom"), "x", true),
            Err(LoadError::Yaml(_))
        ));
        // A bad date is a ValueError, not a YAMLError.
        assert!(matches!(
            construct_scalar(None, "2024-13-45", true),
            Err(LoadError::Value(_))
        ));
    }

    #[test]
    fn integers_beyond_i64_are_exact_decimals() {
        let Value::Decimal(d) = plain("123456789012345678901234567890") else {
            panic!("decimal expected");
        };
        assert_eq!(d.to_sci_string(), "123456789012345678901234567890");
        assert_eq!(plain("-9223372036854775808"), Value::Int(i64::MIN));
    }

    #[test]
    fn merge_keys() {
        assert_eq!(key_role(None, "<<", true), KeyRole::Merge);
        assert_eq!(key_role(None, "<<", false), KeyRole::Plain);
        assert_eq!(key_role(None, "a", true), KeyRole::Plain);
    }
}
