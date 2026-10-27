//! Exact decimal serialisation for the text writers.
//!
//! Mirrors upstream `linkml_map.utils.serialization`: a `decimal.Decimal` is
//! written as an exact numeric literal, never rounded through a float.
//!
//! * JSON / JSONL (`dumps_json`, simplejson with `use_decimal`): `str(Decimal)`
//!   — [`Decimal::to_sci_string`] (`1E-7`, `52.30`).
//! * YAML (`dump_yaml`) and CSV/TSV cells (`format_decimal`): `format(d, 'f')`
//!   — [`Decimal::to_fixed_string`] (`0.0000001`, `52.30`).
//!
//! `serde_json` (without `arbitrary_precision`) and `serde_yaml_ng` cannot emit
//! a number literal with arbitrary digits. The writers therefore convert a
//! [`Value`] to a `serde_json::Value` tree as before, where a decimal is a
//! *placeholder string* carrying both spellings, serialise the tree, and then
//! replace each serialised placeholder with the plain literal. Keeping the
//! decimal inside an ordinary tree means any tree-level processing a writer
//! does (null stripping, flattening) composes with it unchanged.
//!
//! The placeholder is wrapped in control characters (`U+0001` ... `U+0003`)
//! that never occur in transformed data in practice; a string that really
//! starts with `U+0001` is left alone unless it also matches the full shape.

use std::borrow::Cow;

use linkml_map_core::decimal::Decimal;

const START: char = '\u{1}';
const SEP: char = '\u{2}';
const END: char = '\u{3}';

/// The placeholder string standing for `d` inside a `serde_json::Value` tree.
pub fn decimal_placeholder(d: &Decimal) -> String {
    format!(
        "{START}{}{SEP}{}{END}",
        d.to_sci_string(),
        d.to_fixed_string()
    )
}

/// Which spelling a restore pass writes.
#[derive(Clone, Copy)]
enum Spelling {
    /// `str(Decimal)`.
    Sci,
    /// `format(d, 'f')`.
    Fixed,
}

/// How a serialiser spells the placeholder's three control characters.
struct Escapes {
    start: &'static str,
    sep: &'static str,
    end: &'static str,
}

/// Parse the scalar body `sci SEP fixed END` (in serialised spelling) into its
/// two decimal spellings.
fn parse_placeholder<'a>(body: &'a str, esc: &Escapes) -> Option<(&'a str, &'a str)> {
    let inner = body.strip_suffix(esc.end)?;
    let (sci, fixed) = inner.split_once(esc.sep)?;
    let numeric = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'+' | b'E'))
    };
    (numeric(sci) && numeric(fixed)).then_some((sci, fixed))
}

/// Replace every double-quoted placeholder scalar in `text` with the chosen
/// spelling of its decimal.
fn restore<'t>(text: &'t str, esc: &Escapes, spelling: Spelling) -> Cow<'t, str> {
    // Fast path: nothing was a decimal.
    if !text.contains(esc.start) {
        return Cow::Borrowed(text);
    }
    let opener = format!("\"{}", esc.start);
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(&opener) {
        out.push_str(&rest[..at]);
        let after = &rest[at + opener.len()..];
        let parsed = after
            .find('"')
            .and_then(|close| parse_placeholder(&after[..close], esc).map(|p| (p, close)));
        match parsed {
            Some(((sci, fixed), close)) => {
                out.push_str(match spelling {
                    Spelling::Sci => sci,
                    Spelling::Fixed => fixed,
                });
                rest = &after[close + 1..];
            }
            None => {
                // Not one of ours: keep the quote and carry on after it.
                out.push('"');
                rest = &rest[at + 1..];
            }
        }
    }
    out.push_str(rest);
    Cow::Owned(out)
}

/// Replace the placeholders in serialised JSON with `str(Decimal)` literals.
pub fn restore_json_decimals(json: &str) -> Cow<'_, str> {
    // serde_json escapes control characters as `\u0001`.
    const ESC: Escapes = Escapes {
        start: "\\u0001",
        sep: "\\u0002",
        end: "\\u0003",
    };
    restore(json, &ESC, Spelling::Sci)
}

/// Replace the placeholders in serialised YAML with `format(d, 'f')` plain scalars.
pub fn restore_yaml_decimals(yaml: &str) -> Cow<'_, str> {
    // serde_yaml_ng writes a control-character string double-quoted, escaped
    // as `\x01`.
    const ESC: Escapes = Escapes {
        start: "\\x01",
        sep: "\\x02",
        end: "\\x03",
    };
    restore(yaml, &ESC, Spelling::Fixed)
}

/// Serialise a `serde_json::Value` tree (as produced by
/// [`crate::writers::value_to_json`]) compactly, writing decimals as exact
/// `str(Decimal)` literals. For callers that serialise JSON lines themselves.
pub fn json_to_string(tree: &serde_json::Value) -> serde_json::Result<String> {
    let text = serde_json::to_string(tree)?;
    Ok(restore_json_decimals(&text).into_owned())
}

/// `format_decimal`: fixed-point rendering used for CSV/TSV cells.
pub fn format_decimal(d: &Decimal) -> String {
    d.to_fixed_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(s: &str) -> Decimal {
        Decimal::parse_python(s).unwrap()
    }

    fn tree() -> serde_json::Value {
        serde_json::json!({
            "id": "a",
            "v": decimal_placeholder(&dec("52.30")),
            "n": decimal_placeholder(&dec("52")),
            "q": {"unit": "mg", "val": decimal_placeholder(&dec("0.123456789012345678901"))},
            "vs": [decimal_placeholder(&dec("1E-7")), decimal_placeholder(&dec("-3.25"))],
            "s": "\u{1}not a placeholder",
        })
    }

    #[test]
    fn json_uses_str_decimal() {
        let text = serde_json::to_string(&tree()).unwrap();
        let out = restore_json_decimals(&text);
        assert_eq!(
            out,
            "{\"id\":\"a\",\"v\":52.30,\"n\":52,\"q\":{\"unit\":\"mg\",\"val\":0.123456789012345678901},\
             \"vs\":[1E-7,-3.25],\"s\":\"\\u0001not a placeholder\"}"
        );
        // The result is still valid JSON.
        serde_json::from_str::<serde_json::Value>(&out).unwrap();
    }

    #[test]
    fn pretty_json_uses_str_decimal() {
        let text = serde_json::to_string_pretty(&tree()).unwrap();
        let out = restore_json_decimals(&text);
        assert!(out.contains("\"v\": 52.30,"), "{out}");
        assert!(out.contains("1E-7,"), "{out}");
        assert!(!out.contains("\\u0002"), "{out}");
    }

    #[test]
    fn yaml_uses_fixed_decimal_plain_scalars() {
        let text = serde_yaml_ng::to_string(&tree()).unwrap();
        let out = restore_yaml_decimals(&text);
        assert!(out.contains("v: 52.30\n"), "{out}");
        assert!(out.contains("n: 52\n"), "{out}");
        assert!(out.contains("val: 0.123456789012345678901\n"), "{out}");
        assert!(out.contains("- 0.0000001\n"), "{out}");
        assert!(out.contains("- -3.25\n"), "{out}");
        // A plain scalar, so YAML reads them back as numbers.
        let back: serde_yaml_ng::Value = serde_yaml_ng::from_str(&out).unwrap();
        assert!(back["v"].is_number(), "{out}");
    }

    #[test]
    fn text_without_decimals_is_untouched() {
        let text = "{\"a\":1}";
        assert!(matches!(restore_json_decimals(text), Cow::Borrowed(_)));
        assert!(matches!(restore_yaml_decimals("a: 1\n"), Cow::Borrowed(_)));
    }

    #[test]
    fn csv_cells_use_fixed() {
        assert_eq!(format_decimal(&dec("1E-7")), "0.0000001");
        assert_eq!(format_decimal(&dec("52.30")), "52.30");
    }
}
