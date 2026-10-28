//! Invert a transformation specification.
//!
//! Upstream: `src/linkml_map/inference/inverter.py` (`TransformationSpecificationInverter`)
//! and `cli.py::invert`, which writes the result with `dump_output(spec, "yaml")`;
//! [`dump_inverted_spec`] reproduces that dump.
//!
//! Each method mirrors the Python method of the same name, line by line, including
//! the points where upstream raises something other than
//! `NonInvertibleSpecificationError` (an `AttributeError` for a slot derivation with a
//! `range` but no resolvable source slot, `ValueError`s from `SchemaView`): those
//! surface as [`Error::Python`], which the CLI turns into a non-zero exit as Python's
//! uncaught exception does.

use indexmap::IndexMap;

use super::{
    metamodel::{MetamodelError, Node},
    schema_view::SchemaView,
};
use crate::{
    datamodel::{
        ClassDerivation, CollectionType, EnumDerivation, PermissibleValueDerivation,
        SlotDerivation, TransformationSpecification, UnitConversionConfiguration,
    },
    error::{Error, Result},
    pyfmt::py_repr_str,
    value::Value,
};

/// `ValueError` (the type `SchemaView` raises) from a metamodel lookup failure.
fn value_error(e: MetamodelError) -> Error {
    Error::Python(format!("ValueError: {}", e.0))
}

fn none_has_no_attribute(attribute: &str) -> Error {
    Error::Python(format!(
        "AttributeError: 'NoneType' object has no attribute '{attribute}'"
    ))
}

fn non_invertible(msg: String) -> Error {
    Error::NonInvertible(msg)
}

/// `bool(s)` for an optional string: set and non-empty.
fn truthy(s: &Option<String>) -> Option<&str> {
    s.as_deref().filter(|s| !s.is_empty())
}

/// Invert a transformation specification.
///
/// Mirrors Python `TransformationSpecificationInverter`. The forward transformation's
/// **source** schema is supplied here; it becomes the *target* of the inverse (the
/// inverse maps back into it), which is why range / identifier / unit metadata for the
/// inverse is looked up against this schema.
pub struct TransformationSpecificationInverter<'a> {
    /// `source_schemaview`.
    source_schemaview: &'a SchemaView,
    /// In strict mode (the default) an expression that is not a bare slot name is an
    /// error; otherwise its slot derivation is dropped.
    strict: bool,
    /// Collections the specification set to an explicit `null` (see
    /// [`Self::with_explicit_nulls`]).
    nulls: ExplicitNulls,
}

/// The `null` collections upstream's inverter trips over.
#[derive(Debug, Default)]
struct ExplicitNulls {
    /// `enum_derivations: null`.
    enum_derivations: bool,
    /// Enum derivations (by key) with `permissible_value_derivations: null`.
    permissible_value_derivations: std::collections::HashSet<String>,
}

impl<'a> TransformationSpecificationInverter<'a> {
    /// `TransformationSpecificationInverter(source_schemaview=sv)` (`strict=True`).
    pub fn new(source_schemaview: &'a SchemaView) -> Self {
        Self {
            source_schemaview,
            strict: true,
            nulls: ExplicitNulls::default(),
        }
    }

    /// `TransformationSpecificationInverter(source_schemaview=sv, strict=False)`.
    pub fn non_strict(source_schemaview: &'a SchemaView) -> Self {
        Self {
            source_schemaview,
            strict: false,
            nulls: ExplicitNulls::default(),
        }
    }

    /// Tell the inverter which collections the (normalised) specification JSON set to an
    /// explicit `null`.
    ///
    /// The typed [`TransformationSpecification`] cannot tell `null` from absent, but pydantic
    /// keeps `None` where it would default to `{}`, and `invert` then iterates it with
    /// `.values()`: upstream dies with an `AttributeError` for `enum_derivations: null` and
    /// for an enum derivation's `permissible_value_derivations: null`.
    pub fn with_explicit_nulls(mut self, spec_json: &serde_json::Value) -> Self {
        let null_at =
            |v: &serde_json::Value, key: &str| v.get(key).is_some_and(serde_json::Value::is_null);
        self.nulls.enum_derivations = null_at(spec_json, "enum_derivations");
        if let Some(eds) = spec_json
            .get("enum_derivations")
            .and_then(|e| e.as_object())
        {
            self.nulls.permissible_value_derivations = eds
                .iter()
                .filter(|(_, ed)| null_at(ed, "permissible_value_derivations"))
                .map(|(name, _)| name.clone())
                .collect();
        }
        self
    }

    /// `invert`.
    pub fn invert(
        &self,
        spec: &TransformationSpecification,
    ) -> Result<TransformationSpecification> {
        log::info!("Inverting specification");
        let mut inverted_spec = TransformationSpecification::default();
        let mut class_derivations = Vec::new();
        for cd in spec.class_derivations.iter().flatten() {
            class_derivations.push(self.invert_class_derivation(cd, spec)?);
        }
        inverted_spec.class_derivations = Some(class_derivations);
        if self.nulls.enum_derivations {
            return Err(none_has_no_attribute("values"));
        }
        let mut enum_derivations = IndexMap::new();
        for (key, ed) in spec.enum_derivations.iter().flatten() {
            if self.nulls.permissible_value_derivations.contains(key) {
                return Err(none_has_no_attribute("values"));
            }
            let inverted_ed = self.invert_enum_derivation(ed, spec)?;
            enum_derivations.insert(inverted_ed.name.clone(), inverted_ed);
        }
        inverted_spec.enum_derivations = Some(enum_derivations);
        Ok(inverted_spec)
    }

    /// `invert_class_derivation`.
    fn invert_class_derivation(
        &self,
        cd: &ClassDerivation,
        spec: &TransformationSpecification,
    ) -> Result<ClassDerivation> {
        let mut inverted_cd = ClassDerivation {
            name: truthy(&cd.populated_from).unwrap_or(&cd.name).to_string(),
            populated_from: Some(cd.name.clone()),
            ..Default::default()
        };
        let mut slot_derivations = IndexMap::new();
        for sd in cd.slot_derivations.iter().flat_map(IndexMap::values) {
            if sd.hide == Some(true) {
                // Hidden slots have no target counterpart, so they have nothing
                // to invert from.
                continue;
            }
            let inverted_sd = self.invert_slot_derivation(sd, cd, spec)?;
            if let Some(inverted_sd) = inverted_sd {
                slot_derivations.insert(inverted_sd.name.clone(), inverted_sd);
            }
            // PARITY: inverter.py `invert_class_derivation` has an
            // `elif self.strict: raise NonInvertibleSpecificationError("Cannot invert slot
            // derivation: ...")` branch, but `invert_slot_derivation` returns `None`
            // only when `not self.strict`, so it can never run. Not reproduced.
        }
        inverted_cd.slot_derivations = Some(slot_derivations);
        Ok(inverted_cd)
    }

    /// `invert_enum_derivation`.
    fn invert_enum_derivation(
        &self,
        ed: &EnumDerivation,
        _spec: &TransformationSpecification,
    ) -> Result<EnumDerivation> {
        let mut inverted_ed = EnumDerivation {
            name: truthy(&ed.populated_from).unwrap_or(&ed.name).to_string(),
            populated_from: Some(ed.name.clone()),
            ..Default::default()
        };
        // PARITY: inverter.py tests `inverted_ed.expr` (the freshly built inverse, whose
        // `expr` is always `None`) rather than `ed.expr`, so the "TODO: invert enum
        // derivation with expression" error can never fire. An enum derivation with an
        // `expr` is silently inverted from its permissible value derivations.
        let mut pvds = IndexMap::new();
        for pv_deriv in ed
            .permissible_value_derivations
            .iter()
            .flat_map(IndexMap::values)
        {
            let sources: Vec<String> = match &pv_deriv.populated_from {
                Some(s) if !s.is_empty() => s.clone(),
                _ => vec![pv_deriv.name.clone()],
            };
            if sources.len() > 1 {
                let listed = format!(
                    "[{}]",
                    sources
                        .iter()
                        .map(|s| py_repr_str(s))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                return Err(non_invertible(format!(
                    "Cannot invert PermissibleValueDerivation '{}': populated_from has multiple \
                     source values {listed}, which is not a one-to-one mapping.",
                    pv_deriv.name
                )));
            }
            let inverted_name = sources[0].clone();
            let inverted_pv_deriv = PermissibleValueDerivation {
                name: inverted_name,
                populated_from: Some(vec![pv_deriv.name.clone()]),
                ..Default::default()
            };
            pvds.insert(inverted_pv_deriv.name.clone(), inverted_pv_deriv);
        }
        inverted_ed.permissible_value_derivations = Some(pvds);
        Ok(inverted_ed)
    }

    /// `invert_slot_derivation`.
    fn invert_slot_derivation(
        &self,
        sd: &SlotDerivation,
        cd: &ClassDerivation,
        _spec: &TransformationSpecification,
    ) -> Result<Option<SlotDerivation>> {
        let sv = self.source_schemaview;
        let mut populated_from = sd.populated_from.clone();
        if let Some(expr) = truthy(&sd.expr) {
            if is_word_string(expr) {
                populated_from = Some(expr.to_string());
            } else {
                if !self.strict {
                    return Ok(None);
                }
                // TODO (upstream): add logic for reversible expressions
                return Err(non_invertible(format!(
                    "Cannot invert expression {expr} in slot derivation: {}",
                    sd.name
                )));
            }
        }

        let populated_from = match truthy(&populated_from) {
            Some(p) => p.to_string(),
            // use defaults. TODO (upstream): decide on semantics of defaults
            None => sd.name.clone(),
        };
        let mut inverted_sd = SlotDerivation {
            name: populated_from,
            populated_from: Some(sd.name.clone()),
            ..Default::default()
        };
        let source_cls_name = cd.populated_from.as_deref();
        let source_class_known = match source_cls_name {
            None => true,
            Some(c) => sv.all_classes().map_err(value_error)?.contains_key(c),
        };
        let source_slot = match truthy(&sd.populated_from) {
            Some(pf) if source_class_known => {
                Some(sv.induced_slot(pf, source_cls_name).map_err(value_error)?)
            }
            _ => None,
        };
        if truthy(&sd.range).is_some() {
            // PARITY: `source_slot` is `None` when the derivation has no `populated_from`
            // (or its source class is unknown); upstream then dies with an AttributeError.
            let Some(source_slot) = &source_slot else {
                return Err(none_has_no_attribute("range"));
            };
            let range = source_slot.get("range");
            inverted_sd.range = range.as_str().map(str::to_string);
            if let Some(range) = self.range_class(&range)? {
                let id_slot = sv.get_identifier_slot(&range, true).map_err(value_error)?;
                if let Some(id_slot) = id_slot {
                    inverted_sd.dictionary_key = id_slot.get_str("name");
                }
            }
        }
        if let Some(source_slot) = &source_slot
            && source_slot.get("multivalued").is_truthy()
        {
            if source_slot.get("inlined_as_list").is_truthy() {
                inverted_sd.cast_collection_as = Some(CollectionType::MultiValuedList);
            } else if source_slot.get("inlined").is_truthy()
                && let Some(range) = self.range_class(&source_slot.get("range"))?
            {
                let id_slot = sv.get_identifier_slot(&range, true).map_err(value_error)?;
                if let Some(id_slot) = id_slot {
                    inverted_sd.cast_collection_as = Some(CollectionType::MultiValuedDict);
                    inverted_sd.dictionary_key = id_slot.get_str("name");
                }
            }
        }
        if let Some(uc) = &sd.unit_conversion {
            // PARITY: unlike the lookups above this one is not guarded, so a missing
            // `populated_from` or an unknown source class raises a ValueError.
            let Some(pf) = &sd.populated_from else {
                // `induced_slot(None, class)`: the class is looked up first.
                let unknown_class = match source_cls_name.filter(|c| !c.is_empty()) {
                    Some(c) => !sv.all_classes().map_err(value_error)?.contains_key(c),
                    None => false,
                };
                return Err(Error::Python(if unknown_class {
                    format!(
                        "ValueError: No such class: \"{}\"",
                        source_cls_name.unwrap_or_default()
                    )
                } else {
                    format!(
                        "ValueError: No such slot None as an attribute of {} ancestors or as a slot \
                         definition in the schema",
                        source_cls_name.unwrap_or("None")
                    )
                }));
            };
            let source_slot = sv.induced_slot(pf, source_cls_name).map_err(value_error)?;
            let mut target_unit = None;
            let mut target_unit_scheme = None;
            if let Node::Obj(unit) = source_slot.get("unit") {
                // PARITY: only `ucum_code` and `symbol` are consulted (not `iec61360code`,
                // `abbreviation` or `descriptive_name`), in that order.
                for p in ["ucum_code", "symbol"] {
                    if let Some(code) = unit.borrow().get_str(p) {
                        target_unit = Some(code);
                        target_unit_scheme = Some(p.to_string());
                        break;
                    }
                }
            }
            let mut inverted_uc = UnitConversionConfiguration {
                target_unit,
                target_unit_scheme,
                ..Default::default()
            };
            if let Some(s) = truthy(&uc.source_unit_slot) {
                inverted_uc.target_unit_slot = Some(s.to_string());
            }
            if let Some(s) = truthy(&uc.source_magnitude_slot) {
                inverted_uc.target_magnitude_slot = Some(s.to_string());
            }
            if let Some(s) = truthy(&uc.target_unit) {
                inverted_uc.source_unit = Some(s.to_string());
            }
            inverted_sd.unit_conversion = Some(inverted_uc);
        }
        if let Some(stringification) = &sd.stringification {
            let mut inverted = stringification.clone();
            inverted.reversed = Some(!inverted.reversed.unwrap_or(false));
            inverted_sd.stringification = Some(inverted);
        }
        Ok(Some(inverted_sd))
    }

    /// `source_slot.range in self.source_schemaview.all_classes()`: the range when it
    /// names a class.
    fn range_class(&self, range: &Node) -> Result<Option<String>> {
        let Some(name) = range.as_str() else {
            return Ok(None);
        };
        let classes = self.source_schemaview.all_classes().map_err(value_error)?;
        Ok(classes.contains_key(name).then(|| name.to_string()))
    }
}

/// `re.match(r"^\w+$", s)`: `\w` is `[alphanumeric or _]` and `$` also matches before a
/// single trailing newline.
fn is_word_string(s: &str) -> bool {
    let body = s.strip_suffix('\n').unwrap_or(s);
    !body.is_empty() && body.chars().all(is_py_word_char)
}

/// Python's `\w` for `str` patterns: `str.isalnum()` or `_`.
///
/// PARITY gap: Rust's `char::is_alphanumeric` (also used for `str.isalnum` elsewhere in this
/// port) accepts a few combining marks that Python's `isalpha` rejects.
fn is_py_word_char(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

// ── Dumping ──────────────────────────────────────────────────────────────────

fn put(map: &mut IndexMap<String, Value>, key: &str, value: Option<Value>) {
    if let Some(v) = value {
        map.insert(key.to_string(), v);
    }
}

fn s(v: &Option<String>) -> Option<Value> {
    v.as_ref().map(|s| Value::Str(s.clone()))
}

/// A `dict`/`list` is dropped by `remove_empty_items` when empty.
fn non_empty_map(m: IndexMap<String, Value>) -> Option<Value> {
    (!m.is_empty()).then_some(Value::Map(m))
}

fn enum_text<T: serde::Serialize>(v: &T) -> Option<Value> {
    match serde_json::to_value(v) {
        Ok(serde_json::Value::String(s)) => Some(Value::Str(s)),
        _ => None,
    }
}

fn dump_unit_conversion(uc: &UnitConversionConfiguration) -> Option<Value> {
    let mut m = IndexMap::new();
    put(&mut m, "target_unit", s(&uc.target_unit));
    put(&mut m, "target_unit_scheme", s(&uc.target_unit_scheme));
    put(&mut m, "source_unit", s(&uc.source_unit));
    put(&mut m, "source_unit_scheme", s(&uc.source_unit_scheme));
    put(&mut m, "source_unit_slot", s(&uc.source_unit_slot));
    put(
        &mut m,
        "source_magnitude_slot",
        s(&uc.source_magnitude_slot),
    );
    put(&mut m, "target_unit_slot", s(&uc.target_unit_slot));
    put(
        &mut m,
        "target_magnitude_slot",
        s(&uc.target_magnitude_slot),
    );
    put(
        &mut m,
        "none_if_non_numeric",
        uc.none_if_non_numeric.map(Value::Bool),
    );
    non_empty_map(m)
}

fn dump_slot_derivation(sd: &SlotDerivation) -> Option<Value> {
    let mut m = IndexMap::new();
    put(&mut m, "name", Some(Value::Str(sd.name.clone())));
    put(&mut m, "populated_from", s(&sd.populated_from));
    put(&mut m, "range", s(&sd.range));
    put(
        &mut m,
        "unit_conversion",
        sd.unit_conversion.as_ref().and_then(dump_unit_conversion),
    );
    put(
        &mut m,
        "cast_collection_as",
        sd.cast_collection_as.as_ref().and_then(enum_text),
    );
    put(&mut m, "dictionary_key", s(&sd.dictionary_key));
    if let Some(st) = &sd.stringification {
        let mut sm = IndexMap::new();
        put(&mut sm, "delimiter", s(&st.delimiter));
        put(&mut sm, "reversed", st.reversed.map(Value::Bool));
        put(
            &mut sm,
            "over_slots",
            st.over_slots
                .as_ref()
                .filter(|l| !l.is_empty())
                .map(|l| Value::List(l.iter().map(|s| Value::Str(s.clone())).collect())),
        );
        put(&mut sm, "syntax", st.syntax.as_ref().and_then(enum_text));
        put(&mut m, "stringification", non_empty_map(sm));
    }
    non_empty_map(m)
}

fn dump_class_derivation(cd: &ClassDerivation) -> Value {
    let mut m = IndexMap::new();
    put(&mut m, "name", Some(Value::Str(cd.name.clone())));
    put(&mut m, "populated_from", s(&cd.populated_from));
    let sds: IndexMap<String, Value> = cd
        .slot_derivations
        .iter()
        .flatten()
        .filter_map(|(k, sd)| dump_slot_derivation(sd).map(|v| (k.clone(), v)))
        .collect();
    put(&mut m, "slot_derivations", non_empty_map(sds));
    Value::Map(m)
}

fn dump_enum_derivation(ed: &EnumDerivation) -> Value {
    let mut m = IndexMap::new();
    put(&mut m, "name", Some(Value::Str(ed.name.clone())));
    put(&mut m, "populated_from", s(&ed.populated_from));
    let pvds: IndexMap<String, Value> = ed
        .permissible_value_derivations
        .iter()
        .flatten()
        .map(|(k, pv)| {
            let mut pm = IndexMap::new();
            put(&mut pm, "name", Some(Value::Str(pv.name.clone())));
            put(
                &mut pm,
                "populated_from",
                pv.populated_from
                    .as_ref()
                    .filter(|l| !l.is_empty())
                    .map(|l| Value::List(l.iter().map(|s| Value::Str(s.clone())).collect())),
            );
            (k.clone(), Value::Map(pm))
        })
        .collect();
    put(&mut m, "permissible_value_derivations", non_empty_map(pvds));
    Value::Map(m)
}

/// The YAML `linkml-map invert` prints: `yaml_dumper.dumps(inverted_spec)`, i.e.
/// `yaml.dump(remove_empty_items(spec.model_dump()), Dumper=SafeDumper, sort_keys=False,
/// allow_unicode=True)`.
///
/// `model_dump` lists every field in declaration order and `remove_empty_items` drops
/// `None` and empty containers (but keeps `False` and `""`). The inverter only ever sets
/// `class_derivations` / `enum_derivations` and the fields below, so the dump is written
/// out for exactly those, in the pydantic model's field order.
pub fn dump_inverted_spec(spec: &TransformationSpecification) -> String {
    let mut m = IndexMap::new();
    let cds: Vec<Value> = spec
        .class_derivations
        .iter()
        .flatten()
        .map(dump_class_derivation)
        .collect();
    put(
        &mut m,
        "class_derivations",
        (!cds.is_empty()).then_some(Value::List(cds)),
    );
    let eds: IndexMap<String, Value> = spec
        .enum_derivations
        .iter()
        .flatten()
        .map(|(k, ed)| (k.clone(), dump_enum_derivation(ed)))
        .collect();
    put(&mut m, "enum_derivations", non_empty_map(eds));
    crate::pyyaml::dump_yaml(
        &Value::Map(m),
        crate::pyyaml::DumpOptions {
            sort_keys: false,
            ..Default::default()
        },
    )
}
