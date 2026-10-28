//! `invert`: upstream `cli.py::invert`.
//!
//! Loads and merges the `-T` specification files, builds a `SchemaView` over the source
//! schema, inverts the specification with `TransformationSpecificationInverter` and
//! writes the inverse as YAML (`dump_output(inverted_spec, "yaml", output)`).

use std::path::Path;

use std::collections::BTreeSet;

use serde_json::Value;

use linkml_map_core::{
    datamodel::{TransformationSpecification, normalise_spec_json},
    error::Error,
    inference::{SchemaView, TransformationSpecificationInverter, dump_inverted_spec},
    validate::expr_refs::extract_expr_attribute_references,
};

use crate::validate_spec::load_and_merge_specs;

/// What `linkml-map invert` was asked to do.
#[derive(Debug, Clone)]
pub struct InvertOptions {
    /// `-T/--transformer-specification` (repeatable; files or directories).
    pub spec_files: Vec<String>,
    /// The source schema (upstream's positional `SCHEMA`).
    pub schema: String,
    /// `-o/--output`; `None` writes to stdout.
    pub output: Option<String>,
    /// `--strict/--no-strict` (default strict).
    pub strict: bool,
}

/// Run `invert`, returning the text upstream would print, or the message of the Python
/// exception upstream would die with (`Type: message`; the CLI exits with status 1).
pub fn run_invert(opts: &InvertOptions) -> Result<String, String> {
    log::info!(
        "Inverting {:?} using {} as source",
        opts.spec_files,
        opts.schema
    );
    // `_load_specs`: `load_and_merge_specs`, `normalize_spec`, `TransformationSpecification(**obj)`.
    let paths: Vec<&str> = opts.spec_files.iter().map(String::as_str).collect();
    let merged = load_and_merge_specs(&paths)
        .map_err(|e| format!("linkml_map.utils.spec_merge.SpecMergeError: {e}"))?;
    reject_null_sections(&merged)?;
    let mut obj = serde_json::Value::Object(merged);
    normalise_spec_json(&mut obj);
    let spec: TransformationSpecification = serde_json::from_value(obj.clone())
        .map_err(|e| format!("pydantic_core.ValidationError: {e}"))?;

    let source_schemaview =
        SchemaView::from_path(Path::new(&opts.schema)).map_err(|e| format!("Exception: {e}"))?;
    preflight_source_classes(&obj, &source_schemaview)?;
    let inverter = if opts.strict {
        TransformationSpecificationInverter::new(&source_schemaview)
    } else {
        TransformationSpecificationInverter::non_strict(&source_schemaview)
    }
    .with_explicit_nulls(&obj);
    let inverted = inverter.invert(&spec).map_err(|e| match e {
        Error::NonInvertible(msg) => {
            format!("linkml_map.inference.inverter.NonInvertibleSpecificationError: {msg}")
        }
        other => other.to_string(),
    })?;
    Ok(dump_inverted_spec(&inverted))
}

/// Upstream's `normalize_spec` does not survive a `null` where it expects a collection:
/// `class_derivations: null` fails pydantic validation (`class_derivations.0`), and a class
/// derivation's `slot_derivations: null` makes the shape pass call `.items()` on `None`.
// PARITY: spec_normalizer.py `normalize_transform_spec` / pydantic. `normalise_spec_json`
// (shared with every command) is lenient here, so `invert` reports the failure itself.
fn reject_null_sections(spec: &serde_json::Map<String, serde_json::Value>) -> Result<(), String> {
    use serde_json::Value;
    let class_derivations = spec.get("class_derivations");
    if matches!(class_derivations, Some(Value::Null)) {
        return Err(
            "pydantic_core._pydantic_core.ValidationError: 1 validation error for \
                    TransformationSpecification\nclass_derivations.0\n  Input should be a valid \
                    dictionary or instance of ClassDerivation"
                .to_string(),
        );
    }
    let entries: Vec<&Value> = match class_derivations {
        Some(Value::Array(items)) => items.iter().collect(),
        Some(Value::Object(map)) => map.values().collect(),
        _ => Vec::new(),
    };
    for entry in entries {
        if entry.get("slot_derivations").is_some_and(Value::is_null) {
            return Err("AttributeError: 'NoneType' object has no attribute 'items'".to_string());
        }
    }
    Ok(())
}

// ── `_pre_flight_validate` ────────────────────────────────────────────────────

/// `spec_scan.iter_derivation_dicts`: the dicts of a derivations section (list or dict form).
fn iter_derivation_dicts(raw: Option<&Value>) -> Vec<Value> {
    match raw {
        Some(Value::Array(items)) => items.iter().filter(|i| i.is_object()).cloned().collect(),
        Some(Value::Object(map)) => map
            .iter()
            .map(|(name, body)| {
                let mut d = body.as_object().cloned().unwrap_or_default();
                d.entry("name")
                    .or_insert_with(|| Value::String(name.clone()));
                Value::Object(d)
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// The part of upstream's CLI pre-flight check that can *raise*.
///
/// `cli.py::invert` calls `_pre_flight_validate(tr, source_schema=schema)`, which is meant to
/// be informational (findings go to stderr) but runs `validate_spec_semantics` against the
/// source schema, and that calls `source_sv.class_induced_slots(cls)` on every source class
/// the spec reads. A class whose slots do not resolve (a `slot_usage`-less reference to an
/// undefined slot, an `is_a` that names no class, ...) therefore ends the run with a
/// `ValueError` before any inversion happens, even when the inverter would never look at that
/// class. This replays those `class_induced_slots` calls, in `validator.py`'s order, and
/// reports the first failure.
// PARITY: validator.py `_validate_class_derivation`, `_check_join_keys`, `_build_joined_class_map`.
// The target schema (auto-detected from the spec's `target_schema` when that names an existing
// path relative to the working directory) is not loaded here, so its `class_induced_slots`
// failures are not reproduced.
fn preflight_source_classes(spec: &Value, sv: &SchemaView) -> Result<(), String> {
    let all: BTreeSet<String> = sv
        .all_classes()
        .map_err(|e| format!("ValueError: {}", e.0))?
        .into_keys()
        .collect();
    for cd in iter_derivation_dicts(spec.get("class_derivations")) {
        preflight_class_derivation(&cd, None, sv, &all)?;
    }
    Ok(())
}

fn induced_slot_names(sv: &SchemaView, class: &str) -> Result<BTreeSet<String>, String> {
    let slots = sv
        .class_induced_slots(class)
        .map_err(|e| format!("ValueError: {}", e.0))?;
    Ok(slots.iter().filter_map(|s| s.get_str("name")).collect())
}

fn str_field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
}

fn preflight_class_derivation(
    cd: &Value,
    parent: Option<&Value>,
    sv: &SchemaView,
    all: &BTreeSet<String>,
) -> Result<(), String> {
    let cd_name = cd.get("name").and_then(Value::as_str).unwrap_or("?");
    // Source: populated_from class should exist; without one the runtime falls back to
    // the parent's effective source (nested) or the identity case (top level).
    let source_class = cd.get("populated_from").and_then(Value::as_str);
    let mut resolved = source_class.filter(|c| all.contains(*c));
    if source_class.is_none() {
        let fallback = match parent {
            Some(p) => {
                str_field(p, "populated_from").or_else(|| p.get("name").and_then(Value::as_str))
            }
            None => Some(cd_name),
        };
        resolved = fallback.filter(|c| !c.is_empty() && all.contains(*c));
    }
    if let Some(class) = resolved {
        induced_slot_names(sv, class)?;
    }
    // `_check_join_keys`
    if let Some(joins) = cd.get("joins").and_then(Value::as_object) {
        for (alias, spec) in joins {
            let get = |k: &str| spec.as_object().and_then(|_| str_field(spec, k));
            if get("join_on").is_none()
                && !(get("source_key").is_some() && get("lookup_key").is_some())
            {
                continue;
            }
            let joined = get("class_named").unwrap_or(alias);
            if let Some(primary) = resolved
                && all.contains(joined)
            {
                induced_slot_names(sv, primary)?;
                induced_slot_names(sv, joined)?;
            }
        }
    }
    // `_build_joined_class_map`
    let sds = iter_derivation_dicts(cd.get("slot_derivations"));
    let mut joined: BTreeSet<String> = BTreeSet::new();
    if let Some(joins) = cd.get("joins").and_then(Value::as_object) {
        for (alias, spec) in joins {
            let class = str_field(spec, "class_named").unwrap_or(alias);
            if all.contains(class) {
                induced_slot_names(sv, class)?;
            }
            joined.insert(alias.clone());
        }
    }
    let parent_source = str_field(cd, "populated_from").unwrap_or(cd_name);
    if !parent_source.is_empty() {
        for sd in &sds {
            for nested in iter_derivation_dicts(sd.get("class_derivations")) {
                if let Some(ns) = str_field(&nested, "populated_from")
                    && ns != parent_source
                    && !joined.contains(ns)
                {
                    if all.contains(ns) {
                        induced_slot_names(sv, ns)?;
                    }
                    joined.insert(ns.to_string());
                }
            }
        }
        let source_slots = if all.contains(parent_source) {
            induced_slot_names(sv, parent_source)?
        } else {
            BTreeSet::new()
        };
        for sd in &sds {
            let mut bases: BTreeSet<String> = str_field(sd, "expr")
                .map(|e| extract_expr_attribute_references(e).into_keys().collect())
                .unwrap_or_default();
            if let Some(pf) = str_field(sd, "populated_from")
                && let Some((base, _)) = pf.split_once('.')
            {
                bases.insert(base.to_string());
            }
            for base in &bases {
                if !source_slots.contains(base)
                    && !joined.contains(base)
                    && base != parent_source
                    && all.contains(base)
                {
                    induced_slot_names(sv, base)?;
                    joined.insert(base.clone());
                }
            }
        }
    }
    for sd in &sds {
        for nested in iter_derivation_dicts(sd.get("class_derivations")) {
            preflight_class_derivation(&nested, Some(cd), sv, all)?;
        }
    }
    Ok(())
}
