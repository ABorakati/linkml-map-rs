//! Pivot (MELT / UNMELT) operations for EAV-shaped data.
//!
//! Port of `linkml_map.transformer.pivot` (upstream #304). `perform_pivot_operation`
//! (a class-level `pivot_operation`) and `perform_melt` (also used by a slot-level
//! `pivot_operation`) are the entry points the engine calls; the rest are the
//! internal steps, named after their upstream counterparts.

use indexmap::IndexMap;

use crate::{
    datamodel::{PivotDirectionType, PivotOperation},
    error::{Error, Result},
    expr::eval::{py_repr, py_str},
    schema::SchemaProvider,
    value::Value,
};

type DictObj = IndexMap<String, Value>;

/// `pivot_op.<slot> or <default>`: an empty string is as good as unset.
fn slot_or<'a>(slot: &'a str, default: &'a str) -> &'a str {
    if slot.is_empty() { default } else { slot }
}

/// Names of the induced slots of `class`, as `target_sv.class_induced_slots(class)`
/// raises `ValueError: No such class` for an unknown class.
fn class_induced_slot_names(target_sv: &dyn SchemaProvider, class: &str) -> Result<Vec<String>> {
    target_sv
        .induced_slots(class)
        .map(|slots| slots.into_iter().map(|s| s.name).collect())
        .map_err(|_| Error::Python(format!("ValueError: No such class: \"{class}\"")))
}

/// Perform a pivot (MELT or UNMELT) operation.
///
/// Mirrors `pivot.perform_pivot_operation`. The result of an UNMELT is one wide
/// object, that of a MELT a list of EAV records.
pub(crate) fn perform_pivot_operation(
    pivot_op: &PivotOperation,
    source_obj: &DictObj,
    target_sv: Option<&dyn SchemaProvider>,
) -> Result<Value> {
    match pivot_op.direction {
        PivotDirectionType::Unmelt => perform_unmelt(pivot_op, source_obj, target_sv),
        PivotDirectionType::Melt => perform_melt(pivot_op, source_obj, target_sv),
    }
}

/// Mirror of `pivot._perform_unmelt`: EAV/long format to wide format, for a
/// single EAV record or a slot holding a collection of them.
fn perform_unmelt(
    pivot_op: &PivotOperation,
    source_obj: &DictObj,
    target_sv: Option<&dyn SchemaProvider>,
) -> Result<Value> {
    let variable_slot = slot_or(&pivot_op.variable_slot, "variable");
    let value_slot = slot_or(&pivot_op.value_slot, "value");

    // The source object itself is an EAV record.
    if source_obj.contains_key(variable_slot) && source_obj.contains_key(value_slot) {
        return unmelt_single_record(pivot_op, source_obj, target_sv);
    }

    // Otherwise look for a multivalued slot holding EAV records.
    for slot_value in source_obj.values() {
        if let Value::List(items) = slot_value
            && let Some(Value::Map(first_item)) = items.first()
            && first_item.contains_key(variable_slot)
        {
            return unmelt_collection(pivot_op, items);
        }
    }

    // Fallback: treat source_obj as a single EAV record.
    unmelt_single_record(pivot_op, source_obj, target_sv)
}

/// `template.format(variable=variable, unit=unit)` for the `slot_name_template`
/// (`"{variable}"` by default); `unit` is `""` when the record has none.
fn target_slot_name(
    pivot_op: &PivotOperation,
    variable: &Value,
    unit: Option<&Value>,
) -> Result<String> {
    let template = slot_or(&pivot_op.slot_name_template, "{variable}");
    let unit = unit.map_or_else(String::new, py_str);
    py_format(template, &py_str(variable), &unit)
}

/// The `variable` / `unit` of one EAV record: the unit only when the pivot
/// names a `unit_slot` and the record has it.
fn unit_of<'a>(pivot_op: &PivotOperation, record: &'a DictObj) -> Option<&'a Value> {
    pivot_op
        .unit_slot
        .as_deref()
        .filter(|u| !u.is_empty())
        .and_then(|u| record.get(u))
}

/// Mirror of `pivot._unmelt_single_record`:
/// `{att: 'len', val: 1.0, unit: 'm'}` becomes `{len_m: 1.0}`.
fn unmelt_single_record(
    pivot_op: &PivotOperation,
    record: &DictObj,
    target_sv: Option<&dyn SchemaProvider>,
) -> Result<Value> {
    let variable_slot = slot_or(&pivot_op.variable_slot, "variable");
    let value_slot = slot_or(&pivot_op.value_slot, "value");
    let mut result = DictObj::new();

    // Copy the ID slots (non-pivoted attributes).
    for id_slot in pivot_op.id_slots.iter().flatten() {
        if let Some(v) = record.get(id_slot) {
            result.insert(id_slot.clone(), v.clone());
        }
    }

    let variable = record.get(variable_slot).filter(|v| !v.is_null());
    let value = record.get(value_slot).cloned().unwrap_or(Value::Null);
    let Some(variable) = variable else {
        log::warn!("No variable found in slot '{variable_slot}'");
        return Ok(Value::Map(result));
    };

    let name = target_slot_name(pivot_op, variable, unit_of(pivot_op, record))?;

    // Validate against the target schema if unmelt_to_class is specified.
    if let (Some(class), Some(target_sv)) = (&pivot_op.unmelt_to_class, target_sv) {
        let mut valid_slots = class_induced_slot_names(target_sv, class)?;
        if let Some(only) = &pivot_op.unmelt_to_slots {
            valid_slots.retain(|s| only.contains(s));
        }
        if !valid_slots.contains(&name) {
            log::warn!("Generated slot name '{name}' not in valid slots for class '{class}'");
        }
    }

    result.insert(name, value);
    Ok(Value::Map(result))
}

/// Mirror of `pivot._unmelt_collection`:
/// `[{att: 'height', val: 1.8}, {att: 'weight', val: 75.0}]` becomes
/// `{height: 1.8, weight: 75.0}`.
fn unmelt_collection(pivot_op: &PivotOperation, records: &[Value]) -> Result<Value> {
    let variable_slot = slot_or(&pivot_op.variable_slot, "variable");
    let value_slot = slot_or(&pivot_op.value_slot, "value");
    let mut result = DictObj::new();

    for record in records {
        // `record.get(...)` on a non-dict record is an AttributeError.
        let Value::Map(record) = record else {
            return Err(attribute_error_get(record));
        };
        let Some(variable) = record.get(variable_slot).filter(|v| !v.is_null()) else {
            continue;
        };
        let value = record.get(value_slot).cloned().unwrap_or(Value::Null);
        let target_slot = target_slot_name(pivot_op, variable, unit_of(pivot_op, record))?;
        if result.contains_key(&target_slot) {
            log::warn!(
                "Duplicate variable '{}' in unmelt; last value wins",
                py_str(variable)
            );
        }
        result.insert(target_slot, value);
    }

    // Copy the ID slots from the first record (assumed equal across records).
    if let Some(first) = records.first() {
        let Value::Map(first) = first else {
            return Err(attribute_error_get(first));
        };
        for id_slot in pivot_op.id_slots.iter().flatten() {
            if let Some(v) = first.get(id_slot) {
                result.insert(id_slot.clone(), v.clone());
            }
        }
    }
    Ok(Value::Map(result))
}

fn attribute_error_get(v: &Value) -> Error {
    Error::Python(format!(
        "AttributeError: '{}' object has no attribute 'get'",
        match v {
            Value::Null => "NoneType",
            Value::Bool(_) => "bool",
            Value::Int(_) | Value::BigInt(_) => "int",
            Value::Float(_) => "float",
            Value::Complex(_) => "complex",
            Value::Decimal(_) => "Decimal",
            Value::Temporal(t) => t.type_name(),
            Value::Str(_) => "str",
            Value::List(_) => "list",
            Value::Map(_) => "dict",
        }
    ))
}

/// Mirror of `pivot.perform_melt`: wide format to EAV/long format.
///
/// `{height: 1.8, weight: 75.0}` becomes
/// `[{variable: 'height', value: 1.8}, {variable: 'weight', value: 75.0}]`.
/// A slot-level `pivot_operation` always melts, whatever its direction.
pub(crate) fn perform_melt(
    pivot_op: &PivotOperation,
    source_obj: &DictObj,
    target_sv: Option<&dyn SchemaProvider>,
) -> Result<Value> {
    let variable_slot = slot_or(&pivot_op.variable_slot, "variable");
    let value_slot = slot_or(&pivot_op.value_slot, "value");

    // Determine which slots to melt.
    let slots_to_melt: Vec<String> = match (&pivot_op.source_slots, &pivot_op.unmelt_to_class) {
        (Some(source_slots), _) if !source_slots.is_empty() => source_slots.clone(),
        // Infer from the target class.
        (_, Some(class)) if !class.is_empty() && target_sv.is_some() => {
            class_induced_slot_names(target_sv.expect("checked above"), class)?
        }
        // Melt all non-ID slots.
        _ => {
            let id_slots = pivot_op.id_slots.as_deref().unwrap_or_default();
            source_obj
                .keys()
                .filter(|k| !id_slots.contains(k))
                .cloned()
                .collect()
        }
    };

    // Base record: the ID slots, copied into every melted record.
    let mut base_record = DictObj::new();
    for id_slot in pivot_op.id_slots.iter().flatten() {
        if let Some(v) = source_obj.get(id_slot) {
            base_record.insert(id_slot.clone(), v.clone());
        }
    }

    // One record per melted slot.
    let mut results = Vec::new();
    for slot_name in &slots_to_melt {
        if let Some(v) = source_obj.get(slot_name).filter(|v| !v.is_null()) {
            let mut record = base_record.clone();
            record.insert(variable_slot.to_owned(), Value::Str(slot_name.clone()));
            record.insert(value_slot.to_owned(), v.clone());
            results.push(Value::Map(record));
        }
    }
    Ok(Value::List(results))
}

/// The subset of Python's `str.format` a `slot_name_template` can use with the
/// two string fields `variable` and `unit`: literal `{{` / `}}`, `{variable}`,
/// `{unit}`, a `!s` / `!r` conversion and a string format spec
/// (`[[fill]align][width][.precision]`).
///
/// Anything else is the exception Python raises: `KeyError` for another field
/// name, `IndexError` for an automatic or numbered one, `ValueError` for a
/// malformed template.
fn py_format(template: &str, variable: &str, unit: &str) -> Result<String> {
    let err = |kind: &str, msg: &str| Error::Python(format!("{kind}: {msg}"));
    let mut out = String::with_capacity(template.len() + variable.len());
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                out.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                out.push('}');
            }
            '}' => return Err(err("ValueError", "Single '}' encountered in format string")),
            '{' => {
                // Collect the replacement field up to its closing brace.
                let mut field = String::new();
                let mut closed = false;
                for f in chars.by_ref() {
                    if f == '}' {
                        closed = true;
                        break;
                    }
                    field.push(f);
                }
                if !closed {
                    return Err(err("ValueError", "expected '}' before end of string"));
                }
                out.push_str(&format_field(&field, variable, unit)?);
            }
            other => out.push(other),
        }
    }
    Ok(out)
}

/// One replacement field of [`py_format`] (the text between the braces).
fn format_field(field: &str, variable: &str, unit: &str) -> Result<String> {
    let err = |kind: &str, msg: String| Error::Python(format!("{kind}: {msg}"));
    // field_name [! conversion] [: format_spec]
    let (head, spec) = match field.split_once(':') {
        Some((h, s)) => (h, s),
        None => (field, ""),
    };
    let (name, conversion) = match head.split_once('!') {
        Some((n, c)) => (n, Some(c)),
        None => (head, None),
    };
    let value = match name {
        "variable" => variable,
        "unit" => unit,
        "" => {
            return Err(err(
                "IndexError",
                "Replacement index 0 out of range for positional args tuple".into(),
            ));
        }
        n if n.chars().all(|c| c.is_ascii_digit()) => {
            return Err(err(
                "IndexError",
                format!("Replacement index {n} out of range for positional args tuple"),
            ));
        }
        n => return Err(err("KeyError", py_repr(&Value::Str(n.to_owned())))),
    };
    let text = match conversion {
        None | Some("s") => value.to_owned(),
        Some("r") => crate::pyfmt::py_repr_str(value),
        Some("a") => crate::pyfmt::py_repr_str(value),
        Some(c) => {
            return Err(err(
                "ValueError",
                format!("Unknown conversion specifier {c}"),
            ));
        }
    };
    apply_str_spec(&text, spec)
}

/// Python's string format spec: `[[fill]align][width][.precision][s]`.
fn apply_str_spec(text: &str, spec: &str) -> Result<String> {
    if spec.is_empty() {
        return Ok(text.to_owned());
    }
    let bad = || {
        Error::Python(format!(
            "ValueError: Invalid format specifier '{spec}' for object of type 'str'"
        ))
    };
    let chars: Vec<char> = spec.chars().collect();
    let is_align = |c: char| matches!(c, '<' | '>' | '^');
    let (fill, align, mut i) = match chars.as_slice() {
        [f, a, ..] if is_align(*a) => (*f, Some(*a), 2),
        [a, ..] if is_align(*a) => (' ', Some(*a), 1),
        _ => (' ', None, 0),
    };
    let width_start = i;
    while chars.get(i).is_some_and(char::is_ascii_digit) {
        i += 1;
    }
    let width: usize = chars[width_start..i]
        .iter()
        .collect::<String>()
        .parse()
        .unwrap_or(0);
    let mut precision = None;
    if chars.get(i) == Some(&'.') {
        let p_start = i + 1;
        i = p_start;
        while chars.get(i).is_some_and(char::is_ascii_digit) {
            i += 1;
        }
        if i == p_start {
            return Err(Error::Python(
                "ValueError: Format specifier missing precision".into(),
            ));
        }
        precision = chars[p_start..i]
            .iter()
            .collect::<String>()
            .parse::<usize>()
            .ok();
    }
    if chars.get(i) == Some(&'s') {
        i += 1;
    }
    if i != chars.len() {
        return Err(bad());
    }
    let mut s: String = match precision {
        Some(p) => text.chars().take(p).collect(),
        None => text.to_owned(),
    };
    let len = s.chars().count();
    if len < width {
        let pad = width - len;
        let (left, right) = match align.unwrap_or('<') {
            '>' => (pad, 0),
            '^' => (pad / 2, pad - pad / 2),
            _ => (0, pad),
        };
        s = format!(
            "{}{s}{}",
            fill.to_string().repeat(left),
            fill.to_string().repeat(right)
        );
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, Value)]) -> DictObj {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect()
    }

    fn op(direction: PivotDirectionType) -> PivotOperation {
        serde_json::from_value(serde_json::json!({
            "direction": match direction {
                PivotDirectionType::Melt => "MELT",
                PivotDirectionType::Unmelt => "UNMELT",
            }
        }))
        .unwrap()
    }

    // upstream test_pivot_operation.py::TestMeltBasic::test_melt_basic
    #[test]
    fn melt_basic_and_id_slots() {
        let mut p = op(PivotDirectionType::Melt);
        p.source_slots = Some(vec!["height".into(), "weight".into()]);
        p.id_slots = Some(vec!["id".into()]);
        let src = map(&[
            ("id", Value::Str("p1".into())),
            ("height", Value::Float(1.8)),
            ("weight", Value::Float(75.0)),
        ]);
        let Value::List(rows) = perform_melt(&p, &src, None).unwrap() else {
            panic!("melt returns a list")
        };
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0],
            Value::Map(map(&[
                ("id", Value::Str("p1".into())),
                ("variable", Value::Str("height".into())),
                ("value", Value::Float(1.8)),
            ]))
        );
    }

    // upstream test_pivot_operation.py::TestEdgeCases::test_melt_null_values_skipped
    #[test]
    fn melt_skips_nulls() {
        let mut p = op(PivotDirectionType::Melt);
        p.source_slots = Some(vec!["height".into(), "weight".into()]);
        let src = map(&[("height", Value::Float(1.8)), ("weight", Value::Null)]);
        let Value::List(rows) = perform_melt(&p, &src, None).unwrap() else {
            panic!()
        };
        assert_eq!(rows.len(), 1);
    }

    // upstream test_pivot_operation.py::TestEdgeCases::test_unmelt_missing_variable
    // — the single-record fallback copies no id slot and warns.
    #[test]
    fn unmelt_missing_variable_is_empty() {
        let mut p = op(PivotDirectionType::Unmelt);
        p.variable_slot = "att".into();
        p.value_slot = "val".into();
        let src = map(&[("val", Value::Float(1.8))]);
        assert_eq!(
            perform_pivot_operation(&p, &src, None).unwrap(),
            Value::Map(DictObj::new())
        );
    }

    // upstream test_pivot_operation.py::TestEdgeCases::test_unmelt_missing_value
    #[test]
    fn unmelt_missing_value_is_null() {
        let mut p = op(PivotDirectionType::Unmelt);
        p.variable_slot = "att".into();
        p.value_slot = "val".into();
        let src = map(&[("att", Value::Str("height".into()))]);
        assert_eq!(
            perform_pivot_operation(&p, &src, None).unwrap(),
            Value::Map(map(&[("height", Value::Null)]))
        );
    }

    #[test]
    fn unmelt_collection_copies_ids_from_first_record() {
        let mut p = op(PivotDirectionType::Unmelt);
        p.id_slots = Some(vec!["id".into()]);
        let rec = |id: &str, var: &str, val: f64| {
            Value::Map(map(&[
                ("id", Value::Str(id.into())),
                ("variable", Value::Str(var.into())),
                ("value", Value::Float(val)),
            ]))
        };
        let src = map(&[(
            "obs",
            Value::List(vec![rec("a", "h", 1.0), rec("b", "w", 2.0)]),
        )]);
        assert_eq!(
            perform_pivot_operation(&p, &src, None).unwrap(),
            Value::Map(map(&[
                ("h", Value::Float(1.0)),
                ("w", Value::Float(2.0)),
                ("id", Value::Str("a".into())),
            ]))
        );
    }

    #[test]
    fn slot_name_template_follows_str_format() {
        assert_eq!(py_format("{variable}_{unit}", "len", "m").unwrap(), "len_m");
        assert_eq!(
            py_format("{{x}}{variable!s:>5}", "ab", "").unwrap(),
            "{x}   ab"
        );
        assert!(
            matches!(py_format("{nope}", "a", ""), Err(Error::Python(m)) if m.starts_with("KeyError"))
        );
        assert!(
            matches!(py_format("{}", "a", ""), Err(Error::Python(m)) if m.starts_with("IndexError"))
        );
        assert!(
            matches!(py_format("}", "a", ""), Err(Error::Python(m)) if m.starts_with("ValueError"))
        );
    }
}
