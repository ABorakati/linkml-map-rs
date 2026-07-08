//! Semantic validation of a transformation spec against source/target schemas.
//!
//! Native port of the *semantic* layer of Python `linkml_map.validator`
//! (commit `5a42c2af67`) — `validate_spec_semantics` and its helpers
//! (`_validate_class_derivation`, `_build_joined_class_map`,
//! `_check_cross_table_join`, `_check_class_inheritance_refs`,
//! `_validate_slot_derivation`, `_validate_enum_derivation`), plus the two
//! expression-reference extractors in [`expr_refs`].
//!
//! # What this checks
//!
//! Given a spec and the source and/or target [`SchemaProvider`]s, it
//! cross-references every derivation to catch mistakes that would otherwise
//! surface as silent nulls (a typo'd `populated_from`, an unresolvable `expr`
//! reference) or a runtime error at transform time:
//!
//! - target class / slot / enum names exist in the target schema;
//! - `populated_from` class / slot / enum names exist in the source schema;
//! - bare-name and `{alias.field}` expression references resolve to a source
//!   slot or a joined class's slot;
//! - a nested cross-table `populated_from` can be joined (explicit `joins:`,
//!   or an inferable implicit join key — reusing
//!   [`crate::normalize::join_utils`]);
//! - `is_a` / `mixins` refs resolve to a spec-internal class derivation or a
//!   target-schema class;
//! - every required target slot has a derivation.
//!
//! # Divergences from upstream (dict → typed struct)
//!
//! Upstream works off a *normalized dict*; this port works off the already
//! deserialized [`TransformationSpecification`] (serde has done the structural
//! validation the dict form needed). Consequently:
//!
//! - The JSON-Schema structural layer and the deprecated-field scan are **not**
//!   ported (serde rejects unknown/mis-typed fields at deserialize time, and
//!   the deprecated `sources`/`derived_from`/`object_derivations` fields do not
//!   exist in this crate's datamodel).
//! - `class_derivations` is a `Vec`, and `slot_derivations` / nested
//!   `class_derivations` / `enum_derivations` are `IndexMap`s whose `name`
//!   field is injected from the map key at deserialize time — so this port
//!   reads `.name` directly instead of upstream's `_iter_derivation_dicts`.
//!
//! This is an **opt-in pre-flight check** — it is not wired into the transform
//! path and does not affect engine output.

pub mod expr_refs;

use std::collections::BTreeSet;
use std::fmt;

use crate::datamodel::{ClassDerivation, SlotDerivation, TransformationSpecification};
use crate::normalize::join_utils::{find_common_columns, pick_join_key};
use crate::schema::SchemaProvider;

use expr_refs::{extract_expr_attribute_references, extract_expr_slot_references};

// ── ValidationMessage ─────────────────────────────────────────────────────────

/// Severity of a [`ValidationMessage`]. Mirrors the Python
/// `Literal["error", "warning", "info"]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Info,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "info",
        })
    }
}

/// A single validation finding with severity and location context.
///
/// Mirrors the Python `ValidationMessage` dataclass. `category` is an optional
/// tag downstream consumers can group/filter on. The [`fmt::Display`] impl
/// reproduces Python `__str__`: `"{path}: [{severity}] {message}"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationMessage {
    pub severity: Severity,
    pub path: String,
    pub message: String,
    pub category: Option<String>,
}

impl ValidationMessage {
    fn new(severity: Severity, path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity,
            path: path.into(),
            message: message.into(),
            category: None,
        }
    }
}

impl fmt::Display for ValidationMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: [{}] {}", self.path, self.severity, self.message)
    }
}

// ── entry point ───────────────────────────────────────────────────────────────

/// Shared, precomputed context threaded through the recursive validation.
///
/// Mirrors the keyword arguments upstream precomputes once and threads through
/// `_validate_class_derivation` (`derivation_pool`, `source_all_classes`,
/// `target_all_classes`) so nested derivations don't rebuild them.
struct Ctx<'a> {
    source: Option<&'a dyn SchemaProvider>,
    target: Option<&'a dyn SchemaProvider>,
    strict: bool,
    /// Names of all top-level class derivations (the `is_a`/`mixins` pool).
    pool: BTreeSet<String>,
    source_classes: BTreeSet<String>,
    target_classes: BTreeSet<String>,
}

/// Validate a spec's references against the source and/or target schemas.
///
/// Native port of Python `validator.validate_spec_semantics`. Pass whichever
/// schemas are available; with neither, no semantic messages are produced.
/// When `strict` is true, unresolved expression references are reported as
/// [`Severity::Error`] instead of [`Severity::Warning`].
pub fn validate_spec_semantics(
    spec: &TransformationSpecification,
    source_schema: Option<&dyn SchemaProvider>,
    target_schema: Option<&dyn SchemaProvider>,
    strict: bool,
) -> Vec<ValidationMessage> {
    let mut messages = Vec::new();

    if source_schema.is_none() && target_schema.is_none() {
        return messages;
    }

    let ctx = Ctx {
        source: source_schema,
        target: target_schema,
        strict,
        pool: collect_class_derivation_pool(spec),
        source_classes: source_schema
            .map(class_name_set)
            .unwrap_or_default(),
        target_classes: target_schema
            .map(class_name_set)
            .unwrap_or_default(),
    };

    for cd in spec.class_derivations.iter().flatten() {
        validate_class_derivation(&ctx, cd, None, "", &mut messages);
    }

    for ed in spec.enum_derivations.iter().flatten().map(|(_, v)| v) {
        validate_enum_derivation(&ctx, ed, &mut messages);
    }

    messages
}

/// Names of all top-level class derivations. Mirrors Python
/// `_collect_class_derivation_pool` — only the top-level list, matching the
/// runtime's `_find_class_derivation_by_name`.
fn collect_class_derivation_pool(spec: &TransformationSpecification) -> BTreeSet<String> {
    spec.class_derivations
        .iter()
        .flatten()
        .map(|cd| cd.name.clone())
        .collect()
}

// ── helpers over the schema provider ────────────────────────────────────────

/// All class names of a schema as a set.
fn class_name_set(sv: &dyn SchemaProvider) -> BTreeSet<String> {
    sv.all_class_names().into_iter().collect()
}

/// The induced (inheritance-resolved) slot names of `class_name`, or empty on a
/// lookup error. Mirrors `{s.name for s in sv.class_induced_slots(class_name)}`.
fn induced_slot_names(sv: &dyn SchemaProvider, class_name: &str) -> BTreeSet<String> {
    sv.induced_slots(class_name)
        .map(|slots| slots.into_iter().map(|s| s.name).collect())
        .unwrap_or_default()
}

/// Slot derivations of a class derivation, in declaration order.
fn slot_derivations(cd: &ClassDerivation) -> Vec<&SlotDerivation> {
    cd.slot_derivations
        .iter()
        .flatten()
        .map(|(_, v)| v)
        .collect()
}

/// Nested class derivations declared under a slot derivation, in order.
fn nested_class_derivations(sd: &SlotDerivation) -> impl Iterator<Item = &ClassDerivation> {
    sd.class_derivations.iter().flatten().map(|(_, v)| v)
}

/// The effective source class of a class derivation: `populated_from`, falling
/// back to its own name (the identity case the runtime uses when
/// `populated_from` is omitted).
fn effective_source(cd: &ClassDerivation) -> Option<&str> {
    cd.populated_from
        .as_deref()
        .or(Some(cd.name.as_str()))
        .filter(|s| !s.is_empty())
}

// ── class derivation ────────────────────────────────────────────────────────

/// Validate one class derivation, recursing into nested class derivations.
/// Mirrors Python `_validate_class_derivation`.
fn validate_class_derivation(
    ctx: &Ctx,
    cd: &ClassDerivation,
    parent_cd: Option<&ClassDerivation>,
    parent_path: &str,
    messages: &mut Vec<ValidationMessage>,
) {
    let cd_name = cd.name.as_str();
    let cd_path = if parent_path.is_empty() {
        format!("class_derivations[{cd_name}]")
    } else {
        format!("{parent_path}.class_derivations[{cd_name}]")
    };

    // Cross-table check for nested CDs (upstream #211).
    if let Some(parent) = parent_cd {
        check_cross_table_join(ctx, cd, parent, &cd_path, messages);
    }

    // is_a / mixins resolution (upstream #219).
    check_class_inheritance_refs(ctx, cd, &cd_path, messages);

    // Target: class name should exist.
    let mut target_class_slots: Option<BTreeSet<String>> = None;
    if let Some(target) = ctx.target {
        if !ctx.target_classes.contains(cd_name) {
            messages.push(ValidationMessage::new(
                Severity::Error,
                &cd_path,
                format!("Target class '{cd_name}' not found in target schema"),
            ));
        } else {
            target_class_slots = Some(induced_slot_names(target, cd_name));
        }
    }

    // Source: populated_from class should exist.
    let mut source_class_slots: Option<BTreeSet<String>> = None;
    if let (Some(source), Some(source_class)) = (ctx.source, cd.populated_from.as_deref()) {
        if !ctx.source_classes.contains(source_class) {
            messages.push(ValidationMessage::new(
                Severity::Error,
                &cd_path,
                format!(
                    "Source class '{source_class}' (populated_from) not found in source schema"
                ),
            ));
        } else {
            source_class_slots = Some(induced_slot_names(source, source_class));
        }
    }

    // Fallback when source_sv is provided but no populated_from: nested CDs
    // inherit the parent's effective source; top-level CDs use identity
    // (cd_name). Mirrors the runtime's _derive_nested_objects behaviour.
    if let Some(source) = ctx.source
        && cd.populated_from.is_none()
        && source_class_slots.is_none()
    {
        let fallback = match parent_cd {
            Some(parent) => parent.populated_from.as_deref().unwrap_or(parent.name.as_str()),
            None => cd_name,
        };
        if !fallback.is_empty() && ctx.source_classes.contains(fallback) {
            source_class_slots = Some(induced_slot_names(source, fallback));
        }
    }

    let sds = slot_derivations(cd);

    // alias -> (joined class, slot set) for cross-table expression checks.
    let joined_class_slots = build_joined_class_map(ctx, cd, &sds);

    for sd in &sds {
        validate_slot_derivation(
            ctx,
            sd,
            cd_name,
            &cd_path,
            source_class_slots.as_ref(),
            target_class_slots.as_ref(),
            &joined_class_slots,
            messages,
        );

        let sd_path = format!("{cd_path}.slot_derivations[{}]", sd.name);
        for nested in nested_class_derivations(sd) {
            validate_class_derivation(ctx, nested, Some(cd), &sd_path, messages);
        }
    }

    // Warning: required target slots with no derivation.
    if let (Some(target), Some(_)) = (ctx.target, target_class_slots.as_ref()) {
        let derived: BTreeSet<&str> = sds.iter().map(|sd| sd.name.as_str()).collect();
        if let Ok(slots) = target.induced_slots(cd_name) {
            for slot in slots {
                if slot.required && !derived.contains(slot.name.as_str()) {
                    messages.push(ValidationMessage::new(
                        Severity::Warning,
                        &cd_path,
                        format!("Required target slot '{}' has no derivation", slot.name),
                    ));
                }
            }
        }
    }
}

/// Resolved joined class for an alias: `(class_name, Some(slot_set))`, or
/// `(class_name, None)` when the class can't be resolved in the source schema.
type JoinedClass = (String, Option<BTreeSet<String>>);

/// Map alias -> resolved joined class + slot set, combining explicit `joins:`
/// aliases and the implicit nested-CD `populated_from` targets the runtime's
/// normalization will synthesize joins for. Mirrors Python
/// `_build_joined_class_map`.
fn build_joined_class_map(
    ctx: &Ctx,
    cd: &ClassDerivation,
    sds: &[&SlotDerivation],
) -> std::collections::BTreeMap<String, JoinedClass> {
    let mut result: std::collections::BTreeMap<String, JoinedClass> =
        std::collections::BTreeMap::new();

    for (alias, spec) in cd.joins.iter().flatten() {
        let joined_class = spec.class_named.clone().unwrap_or_else(|| alias.clone());
        let entry = resolve_joined_class(ctx, joined_class);
        result.insert(alias.clone(), entry);
    }

    // Identity case: populated_from omitted -> the CD's own name is the source.
    if ctx.source.is_some()
        && let Some(parent_source) = effective_source(cd)
    {
        for sd in sds {
            for nested in nested_class_derivations(sd) {
                if let Some(nested_source) = nested.populated_from.as_deref()
                    && nested_source != parent_source
                    && !result.contains_key(nested_source)
                {
                    let entry = resolve_joined_class(ctx, nested_source.to_string());
                    result.insert(nested_source.to_string(), entry);
                }
            }
        }
    }

    result
}

/// Resolve a joined class name to `(name, Some(slots))` when it exists in the
/// source schema, else `(name, None)`.
fn resolve_joined_class(ctx: &Ctx, joined_class: String) -> JoinedClass {
    match ctx.source {
        Some(source) if ctx.source_classes.contains(&joined_class) => {
            let slots = induced_slot_names(source, &joined_class);
            (joined_class, Some(slots))
        }
        _ => (joined_class, None),
    }
}

/// Diagnose a nested CD referencing a different source table than its parent.
/// Mirrors Python `_check_cross_table_join`.
fn check_cross_table_join(
    ctx: &Ctx,
    nested_cd: &ClassDerivation,
    parent_cd: &ClassDerivation,
    nested_path: &str,
    messages: &mut Vec<ValidationMessage>,
) {
    let Some(nested_source) = nested_cd.populated_from.as_deref() else {
        return;
    };
    let Some(parent_source) = effective_source(parent_cd) else {
        return;
    };
    if nested_source == parent_source {
        return;
    }

    // Explicit join present: verify it carries enough keys to resolve a row.
    if let Some(join_spec) = parent_cd
        .joins
        .as_ref()
        .and_then(|j| j.get(nested_source))
    {
        let join_on = join_spec.join_on.as_deref();
        let source_key = join_spec.source_key.as_deref();
        let lookup_key = join_spec.lookup_key.as_deref();
        if join_on.is_none() && !(source_key.is_some() && lookup_key.is_some()) {
            messages.push(ValidationMessage::new(
                Severity::Warning,
                nested_path,
                format!(
                    "Join spec for '{nested_source}' is missing keys: must specify 'join_on' \
                     or both 'source_key' and 'lookup_key'. Runtime will raise ValueError."
                ),
            ));
        } else if ctx.source.is_some()
            && ctx.source_classes.contains(parent_source)
            && ctx.source_classes.contains(nested_source)
        {
            // Keys declared — verify they exist on the respective source classes.
            let source = ctx.source.unwrap();
            let parent_slots = induced_slot_names(source, parent_source);
            let nested_slots = induced_slot_names(source, nested_source);
            let key_checks: Vec<(&str, &BTreeSet<String>, &str, &str)> = if let Some(k) = join_on {
                vec![
                    (k, &parent_slots, parent_source, "join_on"),
                    (k, &nested_slots, nested_source, "join_on"),
                ]
            } else {
                vec![
                    (source_key.unwrap(), &parent_slots, parent_source, "source_key"),
                    (lookup_key.unwrap(), &nested_slots, nested_source, "lookup_key"),
                ]
            };
            for (key_value, slot_set, class_name, key_label) in key_checks {
                if slot_set.contains(key_value) {
                    continue;
                }
                messages.push(ValidationMessage::new(
                    Severity::Warning,
                    nested_path,
                    format!(
                        "Join spec for '{nested_source}': '{key_label}={key_value}' is not a \
                         slot on source class '{class_name}'. Runtime will silently resolve \
                         cross-table values to null."
                    ),
                ));
            }
        }
        return;
    }

    let Some(source) = ctx.source else {
        return;
    };
    if !ctx.source_classes.contains(parent_source) || !ctx.source_classes.contains(nested_source) {
        // Missing-class errors are emitted elsewhere; can't predict joinability.
        return;
    }

    if let Some(key) = pick_join_key(source, parent_source, nested_source) {
        messages.push(ValidationMessage::new(
            Severity::Info,
            nested_path,
            format!(
                "Nested 'populated_from={nested_source}' differs from parent \
                 'populated_from={parent_source}'. No explicit join entry for \
                 '{nested_source}'; implicit join will be synthesized on column '{key}'. \
                 Consider declaring the join explicitly."
            ),
        ));
        return;
    }

    let common = find_common_columns(source, parent_source, nested_source);
    let reason = if common.is_empty() {
        format!("no columns are shared between '{parent_source}' and '{nested_source}'")
    } else {
        // BTreeSet iterates sorted (matches Python `sorted`).
        let candidates = common
            .iter()
            .map(|c| format!("'{c}'"))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "multiple candidate join columns are shared between '{parent_source}' and \
             '{nested_source}' ({candidates}); cannot pick automatically"
        )
    };
    messages.push(ValidationMessage::new(
        Severity::Warning,
        nested_path,
        format!(
            "Nested 'populated_from={nested_source}' differs from parent \
             'populated_from={parent_source}', but no implicit join can be synthesized: \
             {reason}. Add an explicit join entry for '{nested_source}' — cross-table values \
             will otherwise resolve to null."
        ),
    ));
}

/// Resolve `is_a` / `mixins` string references. Mirrors Python
/// `_check_class_inheritance_refs`: each must resolve to a spec-internal class
/// derivation or a target-schema class, and this is only checked when a target
/// schema is available (ambiguous otherwise).
fn check_class_inheritance_refs(
    ctx: &Ctx,
    cd: &ClassDerivation,
    cd_path: &str,
    messages: &mut Vec<ValidationMessage>,
) {
    if ctx.target.is_none() {
        return;
    }

    let mut parents: Vec<(&str, &str)> = Vec::new();
    if let Some(is_a) = cd.is_a.as_deref() {
        parents.push(("is_a", is_a));
    }
    for m in cd.mixins.iter().flatten() {
        parents.push(("mixins", m));
    }

    for (field_label, parent_name) in parents {
        if ctx.pool.contains(parent_name) || ctx.target_classes.contains(parent_name) {
            continue;
        }
        messages.push(ValidationMessage::new(
            Severity::Error,
            cd_path,
            format!(
                "'{field_label}: {parent_name}' does not resolve to a class_derivation in this \
                 spec or a class in the target schema"
            ),
        ));
    }
}

// ── slot derivation ─────────────────────────────────────────────────────────

/// Validate one slot derivation. Mirrors Python `_validate_slot_derivation`.
#[allow(clippy::too_many_arguments)]
fn validate_slot_derivation(
    ctx: &Ctx,
    sd: &SlotDerivation,
    parent_class_name: &str,
    parent_path: &str,
    source_class_slots: Option<&BTreeSet<String>>,
    target_class_slots: Option<&BTreeSet<String>>,
    joined_class_slots: &std::collections::BTreeMap<String, JoinedClass>,
    messages: &mut Vec<ValidationMessage>,
) {
    let sd_name = sd.name.as_str();
    let sd_path = format!("{parent_path}.slot_derivations[{sd_name}]");

    // Target: slot name should be valid on the target class.
    if let Some(target_slots) = target_class_slots
        && !target_slots.contains(sd_name)
    {
        messages.push(ValidationMessage::new(
            Severity::Error,
            &sd_path,
            format!("Slot '{sd_name}' not found on target class '{parent_class_name}'"),
        ));
    }

    // Source: populated_from slot should be valid on the source class.
    if let (Some(source_slots), Some(populated_from)) =
        (source_class_slots, sd.populated_from.as_deref())
        && !source_slots.contains(populated_from)
    {
        messages.push(ValidationMessage::new(
            Severity::Error,
            &sd_path,
            format!("Source slot '{populated_from}' (populated_from) not found on source class"),
        ));
    }

    let (Some(expr), Some(source_slots)) = (sd.expr.as_deref(), source_class_slots) else {
        return;
    };

    let expr_severity = if ctx.strict {
        Severity::Error
    } else {
        Severity::Warning
    };

    // Bare-name refs — join aliases are a "base", not a source slot.
    let mut refs = extract_expr_slot_references(expr);
    for alias in joined_class_slots.keys() {
        refs.remove(alias);
    }
    for r in &refs {
        if !source_slots.contains(r) {
            messages.push(ValidationMessage::new(
                expr_severity,
                &sd_path,
                format!("Expression references '{r}' which is not a slot on the source class"),
            ));
        }
    }

    // Cross-table attribute refs — {alias.field} against the joined class.
    for (base, attrs) in extract_expr_attribute_references(expr) {
        let Some((joined_class, joined_slots)) = joined_class_slots.get(&base) else {
            continue;
        };
        let Some(joined_slots) = joined_slots else {
            continue;
        };
        let class_descriptor = if *joined_class == base {
            format!("joined class '{joined_class}'")
        } else {
            format!("joined class '{joined_class}' (alias '{base}')")
        };
        for attr in &attrs {
            if !joined_slots.contains(attr) {
                messages.push(ValidationMessage::new(
                    expr_severity,
                    &sd_path,
                    format!(
                        "Expression references '{base}.{attr}' but '{attr}' is not a slot on \
                         {class_descriptor}"
                    ),
                ));
            }
        }
    }
}

// ── enum derivation ─────────────────────────────────────────────────────────

/// Validate one enum derivation. Mirrors Python `_validate_enum_derivation`.
fn validate_enum_derivation(
    ctx: &Ctx,
    ed: &crate::datamodel::EnumDerivation,
    messages: &mut Vec<ValidationMessage>,
) {
    let ed_name = ed.name.as_str();
    let ed_path = format!("enum_derivations[{ed_name}]");

    if let Some(target) = ctx.target {
        let target_enums: BTreeSet<String> = target.all_enum_names().into_iter().collect();
        if !target_enums.contains(ed_name) {
            messages.push(ValidationMessage::new(
                Severity::Error,
                &ed_path,
                format!("Target enum '{ed_name}' not found in target schema"),
            ));
        }
    }

    if let (Some(source), Some(populated_from)) = (ctx.source, ed.populated_from.as_deref()) {
        let source_enums: BTreeSet<String> = source.all_enum_names().into_iter().collect();
        if !source_enums.contains(populated_from) {
            messages.push(ValidationMessage::new(
                Severity::Error,
                &ed_path,
                format!("Source enum '{populated_from}' (populated_from) not found in source schema"),
            ));
        }
    }
}

#[cfg(test)]
mod tests;
