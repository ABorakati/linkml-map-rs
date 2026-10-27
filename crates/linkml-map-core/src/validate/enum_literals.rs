//! Enum-literal checks of the semantic validator (upstream `7665ee3`, #360).
//!
//! Ports `validator._check_enum_literals`: a slot derivation's `value` and
//! `value_mappings` targets are literals known before any data is read, so a
//! spelling the runtime cannot resolve to a permissible value of the target
//! enum is reported here instead of surfacing in the output. Reuses the
//! runtime's [`crate::enum_index::EnumIndex`], so the two agree on what
//! resolves.

use super::{Ctx, Severity, ValidationMessage};
use crate::datamodel::SlotDerivation;
use crate::enum_index::enum_index;
use crate::expr::eval::py_str;
use crate::schema::SchemaProvider;
use crate::value::Value;

/// Check that a slot's `value` and `value_mappings` targets name values of its
/// target enum.
///
/// Only called for a slot present on the target class (the Python guard
/// `sd_name in target_class_slots`). Silent for a non-enum range, an enum with
/// no closed list of permissible values, and a spec `range` that is not an
/// enum (the spec range wins). Messages carry no category; severity is
/// `warning`, or `error` under `strict`.
pub(super) fn check_enum_literals(
    ctx: &Ctx,
    sd: &SlotDerivation,
    class_name: &str,
    target: &dyn SchemaProvider,
    sd_path: &str,
    messages: &mut Vec<ValidationMessage>,
) {
    let slot_range = match sd.range.as_deref().filter(|r| !r.is_empty()) {
        Some(range) => Some(range.to_owned()),
        // Python `induced_slot(...).range` falls back to the schema's `default_range`.
        None => target
            .induced_slot(&sd.name, class_name)
            .ok()
            .and_then(|s| {
                s.range
                    .name()
                    .map(str::to_owned)
                    .or_else(|| target.default_range())
            }),
    };
    let Some(slot_range) = slot_range else {
        return;
    };
    if !ctx.target_enums.contains(&slot_range) {
        return;
    }
    let Some(index) = enum_index(target, &slot_range) else {
        return;
    };

    // `value` first, then `value_mappings` in declaration order. A null is
    // skipped; `false`, `0` and `""` are checked (`is not None`).
    let mut literals: Vec<(String, &serde_json::Value)> = Vec::new();
    if let Some(value) = sd.value.as_ref().filter(|v| !v.is_null()) {
        literals.push(("value".to_owned(), value));
    }
    for (_, mapping) in sd.value_mappings.iter().flatten() {
        if let Some(value) = mapping.value.as_ref().filter(|v| !v.is_null()) {
            literals.push((format!("value_mappings[{}]", mapping.key), value));
        }
    }

    let severity = if ctx.strict {
        Severity::Error
    } else {
        Severity::Warning
    };
    for (field, literal) in literals {
        // Python `str(literal)`: `False` is 'False', `1.5` is '1.5'.
        let text = py_str(&Value::from(literal));
        if let Err(problem) = index.resolve(&text, target) {
            messages.push(ValidationMessage::new(
                severity,
                sd_path,
                format!("{field}: {problem}"),
            ));
        }
    }
}
