//! The deprecated-field scan of a raw spec: Python `linkml_map.spec_scan`.
//!
//! Upstream runs it in the SCAN phase of `spec_normalizer.normalize_spec`, on the
//! canonically shaped dict before the MIGRATE phase rewrites the deprecated fields.
//! Rust's typed datamodel has no place for most of them (see
//! [`crate::datamodel::normalise_spec_json`], which migrates or drops them while
//! loading), so the scan reads the raw JSON-shaped value instead.

use serde_json::{Map, Value};

use super::{Severity, ValidationMessage};

/// Python truthiness of a JSON-shaped value.
pub(crate) fn py_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
        Value::Number(n) => n.as_f64() != Some(0.0),
    }
}

/// `spec_scan.iter_derivation_dicts`, for a section that may still hold compact-key
/// list items (`- Name: {...}`), which upstream expands before the scan: a derivations
/// section (mapping or list) as a list of owned bodies, each with its `name` set.
fn iter_derivation_dicts(raw: Option<&Value>) -> Vec<Map<String, Value>> {
    match raw {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| {
                let Value::Object(d) = item else { return None };
                // `_expand_compact_keys`: a sole key other than `name` whose value is a
                // mapping (or null) is the derivation's name.
                if d.len() == 1
                    && let Some((key, body)) = d.iter().next()
                    && key != "name"
                    && matches!(body, Value::Object(_) | Value::Null)
                {
                    let mut expanded = match body {
                        Value::Object(o) => o.clone(),
                        _ => Map::new(),
                    };
                    expanded
                        .entry("name")
                        .or_insert_with(|| Value::String(key.clone()));
                    return Some(expanded);
                }
                Some(d.clone())
            })
            .collect(),
        Some(Value::Object(by_name)) => by_name
            .iter()
            .map(|(name, body)| {
                let mut d = match body {
                    Value::Object(o) => o.clone(),
                    _ => Map::new(),
                };
                d.entry("name")
                    .or_insert_with(|| Value::String(name.clone()));
                d
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// A derivation's name as Python's f-string renders it (`cd.get("name", "<unnamed>")`).
fn name_of(d: &Map<String, Value>) -> String {
    match d.get("name") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => "<unnamed>".to_owned(),
        Some(other) => other.to_string(),
    }
}

fn truthy_field(d: &Map<String, Value>, key: &str) -> bool {
    d.get(key).is_some_and(py_truthy)
}

/// `(count, first five names, "(and N more)" suffix)` as upstream previews them.
fn preview(names: &[String]) -> (String, String) {
    let shown = names.iter().take(5).cloned().collect::<Vec<_>>().join(", ");
    let suffix = if names.len() > 5 {
        format!(" (and {} more)", names.len() - 5)
    } else {
        String::new()
    };
    (shown, suffix)
}

/// Scan a raw spec for deprecated-field usage and ambiguous combinations.
///
/// Mirrors `spec_scan.check_deprecated_fields`: `sources` (class, slot, enum and
/// permissible-value derivations), `derived_from` and `object_derivations` on slot
/// derivations, bare-string `source_schema` / `target_schema` (warnings, category
/// `deprecated`), and the two ambiguous combinations (`populated_from` plus `sources`
/// on a permissible value; `object_derivations` plus `class_derivations` on a slot),
/// which are errors. The `sources` findings are collapsed to one message per
/// derivation type.
pub fn check_deprecated_fields(data: &Value) -> Vec<ValidationMessage> {
    let mut messages = Vec::new();
    let mut sources_counts: [(&str, Vec<String>); 4] = [
        ("ClassDerivation", Vec::new()),
        ("SlotDerivation", Vec::new()),
        ("EnumDerivation", Vec::new()),
        ("PermissibleValueDerivation", Vec::new()),
    ];
    let mut derived_from_names = Vec::new();
    let mut object_derivation_names = Vec::new();

    let root = data.as_object();
    let section = |key: &str| root.and_then(|r| r.get(key));

    for cd in iter_derivation_dicts(section("class_derivations")) {
        let cd_name = name_of(&cd);
        if truthy_field(&cd, "sources") {
            sources_counts[0].1.push(cd_name.clone());
        }
        for sd in iter_derivation_dicts(cd.get("slot_derivations")) {
            let sd_name = name_of(&sd);
            if truthy_field(&sd, "sources") {
                sources_counts[1].1.push(sd_name.clone());
            }
            if truthy_field(&sd, "derived_from") {
                derived_from_names.push(sd_name.clone());
            }
            if truthy_field(&sd, "object_derivations") {
                object_derivation_names.push(sd_name.clone());
                if truthy_field(&sd, "class_derivations") {
                    messages.push(ValidationMessage::new(
                        Severity::Error,
                        format!("$.class_derivations[{cd_name}].slot_derivations[{sd_name}]"),
                        format!(
                            "SlotDerivation '{sd_name}' sets both 'object_derivations' \
                             and 'class_derivations'. Remove 'object_derivations' and \
                             use 'class_derivations' only."
                        ),
                    ));
                }
            }
        }
    }

    for ed in iter_derivation_dicts(section("enum_derivations")) {
        let ed_name = name_of(&ed);
        if truthy_field(&ed, "sources") {
            sources_counts[2].1.push(ed_name.clone());
        }
        for pvd in iter_derivation_dicts(ed.get("permissible_value_derivations")) {
            let pvd_name = name_of(&pvd);
            if truthy_field(&pvd, "sources") {
                sources_counts[3].1.push(pvd_name.clone());
                if truthy_field(&pvd, "populated_from") {
                    messages.push(ValidationMessage::new(
                        Severity::Error,
                        format!(
                            "$.enum_derivations[{ed_name}].permissible_value_derivations[{pvd_name}]"
                        ),
                        format!(
                            "PermissibleValueDerivation '{pvd_name}' sets both \
                             'populated_from' and 'sources'. These are alternative \
                             spellings of the same field; set only 'populated_from' \
                             (which now accepts a list)."
                        ),
                    ));
                }
            }
        }
    }

    for (deriv_type, names) in &sources_counts {
        if !names.is_empty() {
            let (shown, suffix) = preview(names);
            messages.push(
                ValidationMessage::new(
                    Severity::Warning,
                    format!("$.{deriv_type}"),
                    format!(
                        "{} {deriv_type}(s) use 'sources', which is deprecated: \
                         {shown}{suffix}. Use 'populated_from' instead. \
                         'sources' will be removed in a future version.",
                        names.len()
                    ),
                )
                .with_category("deprecated"),
            );
        }
    }

    if !object_derivation_names.is_empty() {
        let (shown, suffix) = preview(&object_derivation_names);
        messages.push(
            ValidationMessage::new(
                Severity::Warning,
                "$.SlotDerivation",
                format!(
                    "{} SlotDerivation(s) use 'object_derivations', \
                     which is deprecated and flattened into 'class_derivations' at load time: \
                     {shown}{suffix}. Use list-based 'class_derivations' instead. \
                     'object_derivations' will be removed in a future version. \
                     See https://github.com/linkml/linkml-map/issues/112",
                    object_derivation_names.len()
                ),
            )
            .with_category("deprecated"),
        );
    }

    if !derived_from_names.is_empty() {
        let (shown, suffix) = preview(&derived_from_names);
        messages.push(
            ValidationMessage::new(
                Severity::Warning,
                "$.SlotDerivation",
                format!(
                    "{} SlotDerivation(s) use 'derived_from', \
                     which is deprecated and ignored by the runtime: \
                     {shown}{suffix}. This field can be removed \u{2014} source slot \
                     dependencies are derivable from 'expr'. 'derived_from' will \
                     be removed in a future version.",
                    derived_from_names.len()
                ),
            )
            .with_category("deprecated"),
        );
    }

    for schema_field in ["source_schema", "target_schema"] {
        if section(schema_field).is_some_and(Value::is_string) {
            messages.push(
                ValidationMessage::new(
                    Severity::Warning,
                    format!("$.{schema_field}"),
                    format!(
                        "'{schema_field}' is set to a bare string, which is deprecated. \
                         Use the SchemaReference object form '{schema_field}: {{name: ...}}'. \
                         The string form will be removed in a future version."
                    ),
                )
                .with_category("deprecated"),
            );
        }
    }

    messages
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// `test_spec_scan.py`-style: collapsed `sources`, per-entry ambiguity errors.
    #[test]
    fn scan_collapses_sources_and_flags_conflicts() {
        let spec = json!({
            "source_schema": "s.yaml",
            "class_derivations": {
                "A": {"sources": ["x"], "slot_derivations": {
                    "s1": {"sources": ["a"], "derived_from": ["b"]},
                    "s2": {"object_derivations": [{}], "class_derivations": [{"name": "N"}]},
                }},
            },
            "enum_derivations": {"E": {"permissible_value_derivations": {
                "P": {"sources": ["q"], "populated_from": "r"},
            }}},
        });
        let msgs = check_deprecated_fields(&spec);
        let texts: Vec<String> = msgs.iter().map(ToString::to_string).collect();
        assert!(texts.iter().any(|t| t.starts_with(
            "$.class_derivations[A].slot_derivations[s2]: [error] SlotDerivation 's2' sets both"
        )));
        assert!(texts.iter().any(|t| t.starts_with(
            "$.enum_derivations[E].permissible_value_derivations[P]: [error] PermissibleValueDerivation 'P' sets both"
        )));
        assert!(
            texts
                .iter()
                .any(|t| t.contains("1 ClassDerivation(s) use 'sources'"))
        );
        assert!(
            texts
                .iter()
                .any(|t| t.contains("1 SlotDerivation(s) use 'sources'"))
        );
        assert!(
            texts
                .iter()
                .any(|t| t.contains("1 SlotDerivation(s) use 'object_derivations'"))
        );
        assert!(
            texts
                .iter()
                .any(|t| t.contains("1 SlotDerivation(s) use 'derived_from'"))
        );
        assert!(
            texts
                .iter()
                .any(|t| t.starts_with("$.source_schema: [warning]"))
        );
        assert_eq!(
            msgs.iter()
                .filter(|m| m.category.as_deref() == Some("deprecated"))
                .count(),
            6
        );
    }

    #[test]
    fn scan_expands_compact_keys_and_previews_five() {
        let spec = json!({"class_derivations": [
            {"A": {"sources": ["x"]}}, {"B": {"sources": ["x"]}}, {"C": {"sources": ["x"]}},
            {"D": {"sources": ["x"]}}, {"E": {"sources": ["x"]}}, {"F": {"sources": ["x"]}},
        ]});
        let msgs = check_deprecated_fields(&spec);
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].message.contains("A, B, C, D, E (and 1 more)"));
    }

    #[test]
    fn clean_spec_has_no_findings() {
        assert!(check_deprecated_fields(&json!({"class_derivations": {"A": {}}})).is_empty());
        assert!(check_deprecated_fields(&json!(null)).is_empty());
    }
}
