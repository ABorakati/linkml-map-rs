//! Inference utilities over a [`TransformationSpecification`].
//!
//! Currently this provides [`TransformationSpecificationInverter`], a faithful
//! port of Python `linkml_map.inference.inverter.TransformationSpecificationInverter`:
//! given a forward spec (source → target), it derives the inverse spec
//! (target → source) so a mapped object can be round-tripped back.
mod schema_mapper;

use indexmap::IndexMap;

use crate::{
    datamodel::{
        ClassDerivation, CollectionType, EnumDerivation, PermissibleValueDerivation,
        SlotDerivation, TransformationSpecification, UnitConversionConfiguration,
    },
    error::{Error, Result},
    schema::{RangeKind, SchemaProvider, SlotDef, UnitSystem},
};

pub use schema_mapper::SchemaMapper;

/// Invert a transformation specification.
///
/// Mirrors Python `TransformationSpecificationInverter`. The forward
/// transformation's **source** schema is supplied here; it becomes the
/// *target* of the inverse (the inverse maps back into it), which is why range /
/// identifier / unit metadata for the inverse is looked up against this schema.
pub struct TransformationSpecificationInverter<'a> {
    source_schema: &'a dyn SchemaProvider,
    /// In strict mode a slot that cannot be inverted is an error; otherwise it
    /// is silently dropped (Python `strict`, default `true`).
    strict: bool,
}

impl<'a> TransformationSpecificationInverter<'a> {
    /// Strict inverter (errors on non-invertible slots).
    pub fn new(source_schema: &'a dyn SchemaProvider) -> Self {
        Self {
            source_schema,
            strict: true,
        }
    }

    /// Non-strict inverter (drops non-invertible slots instead of erroring).
    pub fn non_strict(source_schema: &'a dyn SchemaProvider) -> Self {
        Self {
            source_schema,
            strict: false,
        }
    }

    /// Invert `spec`, producing the reverse transformation.
    pub fn invert(
        &self,
        spec: &TransformationSpecification,
    ) -> Result<TransformationSpecification> {
        let mut inverted = TransformationSpecification::default();

        let mut inv_cds = Vec::new();
        for cd in spec.class_derivations.iter().flatten() {
            inv_cds.push(self.invert_class_derivation(cd)?);
        }
        inverted.class_derivations = Some(inv_cds);

        if let Some(eds) = &spec.enum_derivations {
            let mut map = IndexMap::new();
            for ed in eds.values() {
                let inv = self.invert_enum_derivation(ed)?;
                map.insert(inv.name.clone(), inv);
            }
            inverted.enum_derivations = Some(map);
        }

        Ok(inverted)
    }

    fn invert_class_derivation(&self, cd: &ClassDerivation) -> Result<ClassDerivation> {
        let name = cd.populated_from.clone().unwrap_or_else(|| cd.name.clone());
        let mut inv = ClassDerivation {
            name,
            populated_from: Some(cd.name.clone()),
            ..Default::default()
        };

        let mut sds = IndexMap::new();
        for sd in cd.slot_derivations.iter().flatten().map(|(_, v)| v) {
            // Hidden slots have no target counterpart → nothing to invert from.
            if sd.hide == Some(true) {
                continue;
            }
            match self.invert_slot_derivation(sd, cd)? {
                Some(inv_sd) => {
                    sds.insert(inv_sd.name.clone(), inv_sd);
                }
                None if self.strict => {
                    return Err(Error::NonInvertible(format!(
                        "cannot invert slot derivation '{}'",
                        sd.name
                    )));
                }
                None => {}
            }
        }
        inv.slot_derivations = Some(sds);
        Ok(inv)
    }

    fn invert_enum_derivation(&self, ed: &EnumDerivation) -> Result<EnumDerivation> {
        let name = ed.populated_from.clone().unwrap_or_else(|| ed.name.clone());
        let mut inv = EnumDerivation {
            name,
            populated_from: Some(ed.name.clone()),
            ..Default::default()
        };

        let mut pvds = IndexMap::new();
        for pv in ed
            .permissible_value_derivations
            .iter()
            .flatten()
            .map(|(_, v)| v)
        {
            // Forward maps source PV(s) -> this target PV; the inverse swaps
            // them. List-form `populated_from` (many sources -> one target) is
            // not uniquely invertible — take the first source as the inverted
            // name (pragmatic, mirrors the single-source common case).
            let pname = pv
                .populated_from
                .as_ref()
                .and_then(|v| v.first())
                .cloned()
                .unwrap_or_else(|| pv.name.clone());
            pvds.insert(
                pname.clone(),
                PermissibleValueDerivation {
                    name: pname,
                    populated_from: Some(vec![pv.name.clone()]),
                    ..Default::default()
                },
            );
        }
        inv.permissible_value_derivations = Some(pvds);
        Ok(inv)
    }

    fn invert_slot_derivation(
        &self,
        sd: &SlotDerivation,
        cd: &ClassDerivation,
    ) -> Result<Option<SlotDerivation>> {
        // Determine the inverse slot name. An `expr` that is a bare identifier
        // (`^\w+$`) is reversible (it just reads one source slot); anything more
        // complex is not.
        let mut populated_from = sd.populated_from.clone();
        if let Some(expr) = &sd.expr {
            if is_bare_identifier(expr) {
                populated_from = Some(expr.clone());
            } else if !self.strict {
                return Ok(None);
            } else {
                return Err(Error::NonInvertible(format!(
                    "cannot invert expression '{}' in slot derivation '{}'",
                    expr, sd.name
                )));
            }
        }
        let populated_from = populated_from.unwrap_or_else(|| sd.name.clone());

        let mut inv = SlotDerivation {
            name: populated_from,
            populated_from: Some(sd.name.clone()),
            ..Default::default()
        };

        // Look up the forward source slot (becomes the inverse *target*), so the
        // inverse can carry range / identifier / collection metadata.
        let source_cls_name = cd.populated_from.clone();
        let source_slot =
            self.lookup_source_slot(source_cls_name.as_deref(), sd.populated_from.as_deref());

        if sd.range.is_some() {
            if let Some(ss) = &source_slot {
                inv.range = range_name(&ss.range);
                if let RangeKind::Class(rc) = &ss.range {
                    if let Ok(Some(id_slot)) = self.source_schema.identifier_slot(rc) {
                        inv.dictionary_key = Some(id_slot.name);
                    }
                }
            }
        }

        if let Some(ss) = &source_slot {
            if ss.multivalued {
                if ss.inlined_as_list {
                    inv.cast_collection_as = Some(CollectionType::MultiValuedList);
                } else if ss.inlined {
                    if let RangeKind::Class(rc) = &ss.range {
                        if let Ok(Some(id_slot)) = self.source_schema.identifier_slot(rc) {
                            inv.cast_collection_as = Some(CollectionType::MultiValuedDict);
                            inv.dictionary_key = Some(id_slot.name);
                        }
                    }
                }
            }
        }

        if let Some(uc) = &sd.unit_conversion {
            // Re-resolve the source slot's declared unit → the inverse's target unit.
            let ss =
                self.lookup_source_slot(source_cls_name.as_deref(), sd.populated_from.as_deref());
            let (target_unit, target_unit_scheme) = match ss.as_ref().and_then(|s| s.unit.as_ref())
            {
                Some(u) => (
                    Some(u.code.clone()),
                    Some(unit_scheme_name(u.system).to_string()),
                ),
                None => (None, None),
            };
            let mut iuc = UnitConversionConfiguration {
                target_unit,
                target_unit_scheme,
                ..Default::default()
            };
            // A structured {magnitude, unit} *source* becomes a structured target.
            if let Some(s) = &uc.source_unit_slot {
                iuc.target_unit_slot = Some(s.clone());
            }
            if let Some(s) = &uc.source_magnitude_slot {
                iuc.target_magnitude_slot = Some(s.clone());
            }
            // The forward target unit becomes the inverse's source unit.
            if let Some(t) = &uc.target_unit {
                iuc.source_unit = Some(t.clone());
            }
            inv.unit_conversion = Some(iuc);
        }

        if let Some(strf) = &sd.stringification {
            let mut s = strf.clone();
            // Join ⇄ split: invert the direction.
            s.reversed = Some(!s.reversed.unwrap_or(false));
            inv.stringification = Some(s);
        }

        Ok(Some(inv))
    }

    /// Mirror of `induced_slot(slot, class)` guarded by Python's
    /// `class is None or class in all_classes`. Returns `None` when the class is
    /// unknown or the slot can't be resolved.
    fn lookup_source_slot(&self, class: Option<&str>, slot: Option<&str>) -> Option<SlotDef> {
        let slot = slot?;
        match class {
            Some(c) => {
                if self.source_schema.all_class_names().iter().any(|x| x == c) {
                    self.source_schema.induced_slot(slot, c).ok()
                } else {
                    None
                }
            }
            None => None,
        }
    }
}

/// True for a string matching `^\w+$` (a single bare identifier).
fn is_bare_identifier(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// The range *name* string for a [`RangeKind`], as Python stores `slot.range`.
fn range_name(range: &RangeKind) -> Option<String> {
    match range {
        RangeKind::Class(n) | RangeKind::Type(n) | RangeKind::Enum(n) => Some(n.clone()),
        RangeKind::None => None,
    }
}

/// The LinkML unit metaslot name a [`UnitSystem`] was read from.
fn unit_scheme_name(system: UnitSystem) -> &'static str {
    match system {
        UnitSystem::Ucum => "ucum_code",
        UnitSystem::Iec61360 => "iec61360code",
        UnitSystem::Other => "symbol",
    }
}

#[cfg(test)]
mod tests;
