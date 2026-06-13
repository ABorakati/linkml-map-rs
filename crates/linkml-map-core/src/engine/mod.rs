//! Object transformer engine — single-object, single-threaded port of
//! `linkml_map.transformer.object_transformer.ObjectTransformer`.
//!
//! # Entry point
//! ```ignore
//! let engine = ObjectTransformer::new(spec, source_schema, target_schema);
//! let result: Value = engine.map_object(source_value, Some("Person"))?;
//! ```
//!
//! # Design notes
//! - Both a source and target `&dyn SchemaProvider` are accepted (may be the
//!   same pointer when schemas are identical, or `None` for the target when not
//!   needed).
//! - The expr evaluator (`eval_expr_with_mapping`) is called for every `expr:`
//!   slot.  Bindings are built from the source object's map keys.
//! - Recursion happens via `map_object` — nested class-ranged slots recurse
//!   into the same engine instance.
//! - Pivot / unit-conversion / join / FK / stringification / offset are **not**
//!   ported in this wave (noted below under NOT PORTED).

use indexmap::IndexMap;

use std::borrow::Cow;
use std::collections::HashMap;

use crate::{
    datamodel::{ClassDerivation, CollectionType, SlotDerivation, TransformationSpecification},
    error::{Error, Result},
    expr::{eval_expr_with_mapping, eval_parsed, parse_expr, Bindings, ExprResult, ParsedExpr},
    schema::{RangeKind, SchemaProvider, SlotDef},
    value::Value,
};

// ── Compiled expr cache ───────────────────────────────────────────────

/// Pre-parsed `expr:` ASTs for an entire [`TransformationSpecification`],
/// built once and reused across every row.
///
/// Each `expr:` string that appears on a class slot derivation or an enum
/// derivation is lexed + parsed a single time into a [`ParsedExpr`]. The engine
/// then evaluates the cached AST per row instead of re-parsing the string,
/// which is the dominant per-row cost on expr-heavy specs.
///
/// `CompiledExprs` is plain data (`Send + Sync`), so a single instance can be
/// shared across worker threads via `Arc`.
#[derive(Debug, Clone, Default)]
pub struct CompiledExprs {
    /// `(class_derivation.name, slot_derivation.name)` → parsed slot `expr:`.
    slot_exprs: HashMap<(String, String), ParsedExpr>,
    /// `enum_derivation map key` → parsed enum-level `expr:`.
    enum_exprs: HashMap<String, ParsedExpr>,
}

impl CompiledExprs {
    /// Parse every `expr:` in `spec` once.
    ///
    /// Returns a parse error if any expression is syntactically invalid; this
    /// surfaces malformed exprs at plan-build time rather than on the first row.
    pub fn build(spec: &TransformationSpecification) -> ExprResult<Self> {
        let mut slot_exprs = HashMap::new();
        if let Some(cds) = &spec.class_derivations {
            for cd in cds {
                if let Some(sds) = &cd.slot_derivations {
                    for (_, sd) in sds {
                        if let Some(expr) = &sd.expr {
                            let parsed = parse_expr(expr)?;
                            slot_exprs.insert((cd.name.clone(), sd.name.clone()), parsed);
                        }
                    }
                }
            }
        }

        let mut enum_exprs = HashMap::new();
        if let Some(eds) = &spec.enum_derivations {
            for (key, ed) in eds {
                if let Some(expr) = &ed.expr {
                    enum_exprs.insert(key.clone(), parse_expr(expr)?);
                }
            }
        }

        Ok(Self {
            slot_exprs,
            enum_exprs,
        })
    }

    fn slot(&self, class: &str, slot: &str) -> Option<&ParsedExpr> {
        self.slot_exprs
            .get(&(class.to_string(), slot.to_string()))
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Single-object transformer.
///
/// Owns the spec; borrows source and target schema providers.
pub struct ObjectTransformer<'s> {
    spec: Cow<'s, TransformationSpecification>,
    source_schema: Option<&'s dyn SchemaProvider>,
    target_schema: Option<&'s dyn SchemaProvider>,
    /// Optional pre-parsed `expr:` ASTs. When present, the per-slot and
    /// per-enum expr paths evaluate the cached AST instead of re-parsing the
    /// `expr:` string on every call. When `None`, the engine falls back to the
    /// string path (`eval_expr_with_mapping`), preserving existing behaviour.
    compiled: Option<&'s CompiledExprs>,
}

impl<'s> ObjectTransformer<'s> {
    /// Create a new transformer.
    ///
    /// `source_schema` is required for range-based coercion and cardinality
    /// decisions.  `target_schema` is optional; when present it is used for
    /// target-slot cardinality decisions.
    pub fn new(
        spec: TransformationSpecification,
        source_schema: Option<&'s dyn SchemaProvider>,
        target_schema: Option<&'s dyn SchemaProvider>,
    ) -> Self {
        Self {
            spec: Cow::Owned(spec),
            source_schema,
            target_schema,
            compiled: None,
        }
    }

    /// Create a transformer that *borrows* the spec instead of owning it.
    ///
    /// This is the hot-path constructor for the row pipeline: it avoids deep-
    /// cloning the whole [`TransformationSpecification`] for every row. The
    /// resulting transformer is otherwise identical to one built with
    /// [`ObjectTransformer::new`].
    pub fn new_borrowed(
        spec: &'s TransformationSpecification,
        source_schema: Option<&'s dyn SchemaProvider>,
        target_schema: Option<&'s dyn SchemaProvider>,
    ) -> Self {
        Self {
            spec: Cow::Borrowed(spec),
            source_schema,
            target_schema,
            compiled: None,
        }
    }

    /// Attach a pre-built [`CompiledExprs`] so the engine evaluates cached
    /// `expr:` ASTs instead of re-parsing strings per row.
    ///
    /// The cache must be built from the same spec this transformer holds
    /// (keyed by class/slot derivation names). When no cache is attached, the
    /// engine transparently falls back to the per-call string parse path, so
    /// callers and tests that use [`ObjectTransformer::new`] are unaffected.
    pub fn with_compiled_exprs(mut self, compiled: &'s CompiledExprs) -> Self {
        self.compiled = Some(compiled);
        self
    }

    /// Transform a single source object.
    ///
    /// `source_type` names the LinkML class of `source_obj`.  When `None` the
    /// engine tries the schema's tree-root class, then falls back to the first
    /// class derivation's name.
    pub fn map_object(&self, source_obj: &Value, source_type: Option<&str>) -> Result<Value> {
        let source_type = self.resolve_source_type(source_type)?;
        self.map_object_with_type(source_obj, &source_type)
    }

    // ── Internal helpers ──────────────────────────────────────────────────────

    /// Core recursive mapper — source type already resolved.
    fn map_object_with_type(&self, source_obj: &Value, source_type: &str) -> Result<Value> {
        // Enum pass-through: if source_type is an enum, transform via enum derivation.
        if let Some(ss) = self.source_schema {
            if ss.all_enum_names().contains(&source_type.to_string()) {
                return self.transform_enum(source_obj, &[source_type.to_string()], source_obj);
            }
        }

        // Scalar pass-through: if it's a known type, return as-is.
        if let Some(ss) = self.source_schema {
            if ss.all_type_names().contains(&source_type.to_string()) {
                return Ok(source_obj.clone());
            }
        }

        // Must be a map.
        let source_map = match source_obj {
            Value::Map(m) => m,
            other => {
                // Graceful degradation — return as-is with a warning path.
                return Ok(other.clone());
            }
        };

        // Find the matching class derivation(s).
        let class_deriv = self.get_class_derivation(source_type)?;

        // Per-slot iteration.
        let empty_map = IndexMap::new();
        let slot_derivations = class_deriv.slot_derivations.as_ref().unwrap_or(&empty_map);

        let mut tgt_attrs: IndexMap<String, Value> = IndexMap::new();

        for (_, slot_deriv) in slot_derivations {
            let slot_name = slot_deriv.name.as_str();
            let v = self
                .derive_slot(slot_deriv, source_map, source_type, &class_deriv)
                .map_err(|e| Error::SlotTransform {
                    class: class_deriv.name.clone(),
                    slot: slot_name.to_string(),
                    cause: e.to_string(),
                })?;
            tgt_attrs.insert(slot_name.to_string(), v);
        }

        Ok(Value::Map(tgt_attrs))
    }

    /// Derive the value for a single slot derivation.
    fn derive_slot(
        &self,
        slot_deriv: &SlotDerivation,
        source_map: &IndexMap<String, Value>,
        source_type: &str,
        class_deriv: &ClassDerivation,
    ) -> Result<Value> {
        let slot_name = slot_deriv.name.as_str();

        // ── Precedence order (mirrors Python map_object) ──────────────────────
        //
        // 1. constant `value:`
        // 2. `expr:`
        // 3. `populated_from:` (direct field copy + value_mappings)
        // 4. `sources:` (first non-null wins)
        // 5. `object_derivations:` (not ported — see NOT PORTED section)
        // 6. implicit same-name copy
        //
        // After obtaining v, apply range coercion + cardinality reshaping.

        let (mut v, source_slot_def) = if let Some(const_val) = &slot_deriv.value {
            // 1. Constant value.
            let v = json_to_value(const_val);
            (v, None)
        } else if let Some(expr) = &slot_deriv.expr {
            // 2. Expression.
            let v = self.eval_expr_for_slot(
                expr,
                source_map,
                slot_name,
                source_type,
                &class_deriv.name,
            )?;
            (v, None)
        } else if let Some(populated_from) = &slot_deriv.populated_from {
            // 3. populated_from — direct field copy.
            let raw = source_map.get(populated_from).cloned().unwrap_or(Value::Null);
            // Apply value_mappings if present and value is non-null.
            let mapped = if let Some(vm) = &slot_deriv.value_mappings {
                if !raw.is_null() {
                    let key = value_to_string_key(&raw);
                    if let Some(kv) = vm.get(&key) {
                        json_to_value(kv.value.as_ref().unwrap_or(&serde_json::Value::Null))
                    } else {
                        Value::Null
                    }
                } else {
                    raw
                }
            } else {
                raw
            };
            // Source slot def for range/cardinality coercion.
            let ssd = self.source_schema.and_then(|ss| {
                ss.induced_slot(populated_from, source_type).ok()
            });
            (mapped, ssd)
        } else if let Some(sources) = &slot_deriv.sources {
            // 4. sources — first non-null wins.
            let mut found_v = Value::Null;
            let mut found_ssd: Option<SlotDef> = None;
            for src_slot in sources {
                let candidate = source_map.get(src_slot).cloned().unwrap_or(Value::Null);
                if !candidate.is_null() {
                    let ssd = self.source_schema.and_then(|ss| {
                        ss.induced_slot(src_slot, source_type).ok()
                    });
                    found_v = candidate;
                    found_ssd = ssd;
                    break;
                }
            }
            (found_v, found_ssd)
        } else if slot_deriv.object_derivations.is_some() {
            // 5. object_derivations — not fully ported (requires target schemaview for
            //    multivalued check on target slot). Return Null for now.
            //    See NOT PORTED section in module docs.
            (Value::Null, None)
        } else {
            // 6. Implicit same-name copy.
            let raw = source_map.get(slot_name).cloned().unwrap_or(Value::Null);
            let ssd = self.source_schema.and_then(|ss| {
                ss.induced_slot(slot_name, source_type).ok()
            });
            (raw, ssd)
        };

        // ── Post-processing: range coercion + cardinality ─────────────────────

        if let Some(ssd) = &source_slot_def {
            if !v.is_null() {
                v = self.map_value_by_range(&v, ssd, slot_deriv.range.as_deref())?;
                v = self.coerce_cardinality(v, slot_deriv, class_deriv, ssd.multivalued)?;
                if let Some(target_range) = slot_deriv.range.as_deref() {
                    v = coerce_datatype(v, target_range);
                }
            }
        }

        Ok(v)
    }

    // ── Expression evaluation ─────────────────────────────────────────────────

    fn eval_expr_for_slot(
        &self,
        expr: &str,
        source_map: &IndexMap<String, Value>,
        slot_name: &str,
        source_type: &str,
        class_name: &str,
    ) -> Result<Value> {
        // Build bindings from all source map keys.
        let mut bindings: Bindings = IndexMap::new();
        bindings.insert("NULL".to_string(), Value::Null);
        for (k, v) in source_map {
            bindings.insert(k.clone(), v.clone());
        }

        // Cached-AST fast path: evaluate the pre-parsed expr when a compiled
        // cache is attached and holds this (class, slot). Falls back to the
        // string parse path otherwise (identical result).
        let result = match self.compiled.and_then(|c| c.slot(class_name, slot_name)) {
            Some(parsed) => eval_parsed(parsed, &bindings),
            None => eval_expr_with_mapping(expr, &bindings),
        };
        result.map_err(|e| Error::ExprEval {
            class: source_type.to_string(),
            slot: slot_name.to_string(),
            cause: e.to_string(),
        })
    }

    // ── Range-based value coercion ────────────────────────────────────────────

    /// Mirror of Python `_map_value_by_range`.
    ///
    /// Recursively maps nested values based on the source slot's range type.
    fn map_value_by_range(
        &self,
        v: &Value,
        source_slot: &SlotDef,
        target_range: Option<&str>,
    ) -> Result<Value> {
        let source_range = &source_slot.range;

        // any_of enum shortcut (range is None/Any but enum alternatives exist).
        if matches!(source_range, RangeKind::None) && !source_slot.any_of_enums.is_empty() {
            let enum_names = source_slot.any_of_enums.clone();
            if source_slot.multivalued {
                if let Value::List(items) = v {
                    let mapped: Result<Vec<Value>> = items
                        .iter()
                        .map(|item| self.transform_enum(item, &enum_names, item))
                        .collect();
                    return Ok(Value::List(mapped?));
                }
            }
            return self.transform_enum(v, &enum_names, v);
        }

        // No range — return as-is unless it's a complex nested value.
        if matches!(source_range, RangeKind::None) {
            return Ok(v.clone());
        }

        match source_range {
            RangeKind::Enum(enum_name) => {
                if source_slot.multivalued {
                    if let Value::List(items) = v {
                        let mapped: Result<Vec<Value>> = items
                            .iter()
                            .map(|item| {
                                self.transform_enum(item, &[enum_name.clone()], item)
                            })
                            .collect();
                        return Ok(Value::List(mapped?));
                    }
                    // Scalar value on a multivalued enum slot: map then wrap in a
                    // one-element list (mirror of Python single→multivalued).
                    let mapped = self.transform_enum(v, &[enum_name.clone()], v)?;
                    return Ok(Value::List(vec![mapped]));
                }
                self.transform_enum(v, &[enum_name.clone()], v)
            }
            RangeKind::Class(class_name) => {
                let target_range_str = target_range.map(|s| s.to_string());
                if source_slot.multivalued {
                    match v {
                        Value::List(items) => {
                            let mapped: Result<Vec<Value>> = items
                                .iter()
                                .map(|item| {
                                    self.map_object_with_type_and_target(
                                        item,
                                        class_name,
                                        target_range_str.as_deref(),
                                    )
                                })
                                .collect();
                            Ok(Value::List(mapped?))
                        }
                        Value::Map(_) => {
                            // Dict of class instances — recurse each value.
                            let mapped: Result<IndexMap<String, Value>> = if let Value::Map(m) = v {
                                m.iter()
                                    .map(|(k, item)| {
                                        let r = self.map_object_with_type_and_target(
                                            item,
                                            class_name,
                                            target_range_str.as_deref(),
                                        )?;
                                        Ok((k.clone(), r))
                                    })
                                    .collect()
                            } else {
                                unreachable!()
                            };
                            Ok(Value::Map(mapped?))
                        }
                        _ => {
                            // Scalar wrapped in list.
                            let inner = self.map_object_with_type_and_target(
                                v,
                                class_name,
                                target_range_str.as_deref(),
                            )?;
                            Ok(Value::List(vec![inner]))
                        }
                    }
                } else {
                    self.map_object_with_type_and_target(
                        v,
                        class_name,
                        target_range_str.as_deref(),
                    )
                }
            }
            RangeKind::Type(_) | RangeKind::None => Ok(v.clone()),
        }
    }

    /// map_object_with_type but also respects an explicit target_range override.
    ///
    /// In Python this is `self.map_object(v, source_class_slot_range, target_range)`.
    fn map_object_with_type_and_target(
        &self,
        v: &Value,
        source_type: &str,
        _target_range: Option<&str>,
    ) -> Result<Value> {
        // For now target_range is only meaningful for scalar type coercions,
        // which are handled by coerce_datatype. The recursive call just needs
        // the source class to find its derivation.
        self.map_object_with_type(v, source_type)
    }

    // ── Cardinality coercion ──────────────────────────────────────────────────

    /// Mirror of Python `_coerce_cardinality`.
    fn coerce_cardinality(
        &self,
        v: Value,
        slot_deriv: &SlotDerivation,
        class_deriv: &ClassDerivation,
        source_multivalued: bool,
    ) -> Result<Value> {
        // Explicit single-valued intent (stringification join, cast SingleValued,
        // or a single-valued target slot) takes precedence over the implicit
        // "source is multivalued" signal, so a multivalued source can still be
        // collapsed (e.g. joined) into one value.
        if self.is_coerce_to_singlevalued(slot_deriv, class_deriv) {
            if let Value::List(items) = v {
                return self.multivalued_to_single(items, slot_deriv);
            }
        } else if self.is_coerce_to_multivalued(slot_deriv, class_deriv, source_multivalued) {
            if !matches!(&v, Value::Null) && !matches!(&v, Value::List(_)) {
                return Ok(self.single_to_multivalued(v, slot_deriv)?);
            }
        }
        Ok(v)
    }

    fn is_coerce_to_multivalued(
        &self,
        sd: &SlotDerivation,
        cd: &ClassDerivation,
        source_multivalued: bool,
    ) -> bool {
        if let Some(cast_as) = &sd.cast_collection_as {
            if matches!(
                cast_as,
                CollectionType::MultiValued
                    | CollectionType::MultiValuedDict
                    | CollectionType::MultiValuedList
            ) {
                return true;
            }
        }
        // stringification reversed = split → list.
        if let Some(s) = &sd.stringification {
            if s.reversed.unwrap_or(false) {
                return true;
            }
        }
        // Target schema says multivalued.
        if let Some(ts) = self.target_schema {
            if let Ok(slot) = ts.induced_slot(&sd.name, &cd.name) {
                if slot.multivalued {
                    return true;
                }
            }
        }
        // Fallback: when the source slot is multivalued and no explicit
        // single-valued directive applied, treat the target as multivalued too
        // (a scalar value is wrapped in a one-element list). Mirrors Python
        // single→multivalued coercion and works without a loaded target schema.
        if source_multivalued {
            return true;
        }
        false
    }

    fn is_coerce_to_singlevalued(&self, sd: &SlotDerivation, cd: &ClassDerivation) -> bool {
        if let Some(cast_as) = &sd.cast_collection_as {
            if matches!(cast_as, CollectionType::SingleValued) {
                return true;
            }
        }
        // stringification not reversed = join → string.
        if let Some(s) = &sd.stringification {
            if !s.reversed.unwrap_or(false) {
                return true;
            }
        }
        // Target schema says single-valued.
        if let Some(ts) = self.target_schema {
            if let Ok(slot) = ts.induced_slot(&sd.name, &cd.name) {
                if !slot.multivalued {
                    return true;
                }
            }
        }
        false
    }

    fn single_to_multivalued(&self, v: Value, sd: &SlotDerivation) -> Result<Value> {
        // If stringification.reversed: split the string.
        if let Some(s) = &sd.stringification {
            if s.reversed.unwrap_or(false) {
                if let Value::Str(ref sv) = v {
                    if let Some(delim) = &s.delimiter {
                        let parts: Vec<Value> = sv
                            .split(delim.as_str())
                            .map(|p| Value::Str(p.to_string()))
                            .collect();
                        let parts = if parts == vec![Value::Str(String::new())] {
                            vec![]
                        } else {
                            parts
                        };
                        return Ok(Value::List(parts));
                    }
                }
            }
        }
        Ok(Value::List(vec![v]))
    }

    fn multivalued_to_single(&self, items: Vec<Value>, sd: &SlotDerivation) -> Result<Value> {
        // Stringification: join with delimiter.
        if let Some(s) = &sd.stringification {
            if let Some(delim) = &s.delimiter {
                let parts: Vec<String> = items.iter().map(|v| value_to_string_key(v)).collect();
                return Ok(Value::Str(parts.join(delim)));
            }
        }
        if items.len() > 1 {
            return Err(Error::Cardinality {
                slot: sd.name.clone(),
                msg: format!("cannot coerce {} values to single-valued", items.len()),
            });
        }
        Ok(items.into_iter().next().unwrap_or(Value::Null))
    }

    // ── Enum transformation ───────────────────────────────────────────────────

    /// Mirror of Python `transform_enum`.
    ///
    /// Iterates `enum_names` in order, tries expr evaluation then PV mappings,
    /// then mirror_source fallback.
    fn transform_enum(
        &self,
        source_value: &Value,
        enum_names: &[String],
        _source_obj: &Value,
    ) -> Result<Value> {
        let enum_derivations = match &self.spec.enum_derivations {
            Some(m) => m,
            None => return Ok(source_value.clone()),
        };

        for enum_name in enum_names {
            // Find a matching enum derivation for this enum name.
            let enum_deriv = enum_derivations.iter().find(|(_, ed)| {
                ed.populated_from.as_deref() == Some(enum_name)
                    || (ed.populated_from.is_none() && ed.name == *enum_name)
            });

            let (ed_key, ed) = match enum_deriv {
                Some((k, e)) => (k.as_str(), e),
                None => continue,
            };

            // Expr evaluation on the enum level (rare).
            if let Some(expr) = &ed.expr {
                let mut bindings: Bindings = IndexMap::new();
                if let Value::Map(m) = _source_obj {
                    for (k, v) in m {
                        bindings.insert(k.clone(), v.clone());
                    }
                }
                bindings.insert("NULL".to_string(), Value::Null);
                // Cached-AST fast path mirrors the slot path; falls back to the
                // string parse when no compiled cache is attached.
                let evaled = match self.compiled.and_then(|c| c.enum_exprs.get(ed_key)) {
                    Some(parsed) => eval_parsed(parsed, &bindings),
                    None => eval_expr_with_mapping(expr, &bindings),
                };
                if let Ok(v) = evaled {
                    if !v.is_null() {
                        return Ok(v);
                    }
                }
            }

            // Permissible value derivations.
            let src_str = value_to_string_key(source_value);
            if let Some(pvds) = &ed.permissible_value_derivations {
                for (_, pvd) in pvds {
                    // populated_from match.
                    if pvd.populated_from.as_deref() == Some(&src_str) {
                        return Ok(Value::Str(pvd.name.clone()));
                    }
                    // sources list match.
                    if let Some(sources) = &pvd.sources {
                        if sources.contains(&src_str) {
                            return Ok(Value::Str(pvd.name.clone()));
                        }
                    }
                }
            }

            // mirror_source fallback.
            if ed.mirror_source.unwrap_or(false) {
                return Ok(Value::Str(src_str));
            }
        }

        Ok(Value::Null)
    }

    // ── Class derivation lookup ───────────────────────────────────────────────

    /// Find the unique ClassDerivation for a given source type.
    ///
    /// Matches on `populated_from == source_type` (explicit mapping) OR
    /// `name == source_type` when `populated_from` is absent (identity mapping).
    /// Then resolves is_a / mixin inheritance by merging ancestor slot_derivations.
    fn get_class_derivation(&self, source_type: &str) -> Result<ClassDerivation> {
        let derivations = match &self.spec.class_derivations {
            Some(v) => v,
            None => {
                return Err(Error::NoClassDerivation {
                    source_type: source_type.to_string(),
                    count: 0,
                })
            }
        };

        let matching: Vec<&ClassDerivation> = derivations
            .iter()
            .filter(|cd| {
                cd.populated_from.as_deref() == Some(source_type)
                    || (cd.populated_from.is_none() && cd.name == source_type)
            })
            .collect();

        if matching.len() != 1 {
            return Err(Error::NoClassDerivation {
                source_type: source_type.to_string(),
                count: matching.len(),
            });
        }

        let cd = matching[0].clone();
        self.resolve_class_derivation_ancestors(cd, derivations)
    }

    /// Merge is_a / mixin ancestor slot_derivations into `cd` (Python:
    /// `_class_derivation_ancestors`).
    ///
    /// Ancestor slots are added only when the descendant does not already
    /// override the same slot name.
    fn resolve_class_derivation_ancestors(
        &self,
        mut cd: ClassDerivation,
        all: &[ClassDerivation],
    ) -> Result<ClassDerivation> {
        let parents: Vec<String> = {
            let mut p = cd.mixins.clone().unwrap_or_default();
            if let Some(is_a) = &cd.is_a {
                p.push(is_a.clone());
            }
            p
        };

        for parent_name in &parents {
            let parent = all
                .iter()
                .find(|d| &d.name == parent_name)
                .cloned()
                .ok_or_else(|| Error::NoClassDerivation {
                    source_type: parent_name.clone(),
                    count: 0,
                })?;
            // Recursively resolve the parent too.
            let parent = self.resolve_class_derivation_ancestors(parent, all)?;
            // Merge: ancestor slots that don't already exist in cd.
            if let Some(ancestor_slots) = parent.slot_derivations {
                let cd_slots = cd.slot_derivations.get_or_insert_with(IndexMap::new);
                for (k, v) in ancestor_slots {
                    cd_slots.entry(k).or_insert(v);
                }
            }
        }

        Ok(cd)
    }

    // ── Source type resolution ────────────────────────────────────────────────

    fn resolve_source_type(&self, source_type: Option<&str>) -> Result<String> {
        if let Some(t) = source_type {
            return Ok(t.to_string());
        }

        // Try tree root from schema.
        if let Some(ss) = self.source_schema {
            if let Some(root) = ss.tree_root_class() {
                return Ok(root);
            }
            // Single class fallback.
            let names = ss.all_class_names();
            if names.len() == 1 {
                return Ok(names.into_iter().next().unwrap());
            }
        }

        // Fall back to first class derivation name.
        if let Some(derivations) = &self.spec.class_derivations {
            if let Some(first) = derivations.first() {
                return Ok(first.name.clone());
            }
        }

        Err(Error::SourceTypeUnresolvable(
            "no source_type provided, no tree root, and spec has no class_derivations".to_string(),
        ))
    }
}

// ── Utility fns ───────────────────────────────────────────────────────────────

/// Convert a `serde_json::Value` (used in SlotDerivation.value) to our `Value`.
fn json_to_value(j: &serde_json::Value) -> Value {
    match j {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Int(i)
            } else if let Some(f) = n.as_f64() {
                Value::Float(f)
            } else {
                Value::Str(n.to_string())
            }
        }
        serde_json::Value::String(s) => Value::Str(s.clone()),
        serde_json::Value::Array(arr) => Value::List(arr.iter().map(json_to_value).collect()),
        serde_json::Value::Object(obj) => {
            let mut m = IndexMap::new();
            for (k, v) in obj {
                m.insert(k.clone(), json_to_value(v));
            }
            Value::Map(m)
        }
    }
}

/// Convert a Value to a string for use as a map key (value_mappings lookups).
fn value_to_string_key(v: &Value) -> String {
    match v {
        Value::Null => "None".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Str(s) => s.clone(),
        Value::List(_) | Value::Map(_) => format!("{v:?}"),
    }
}

/// Mirror of Python `Transformer._coerce_datatype`.
///
/// Recursively converts scalar values to the named target range type.
/// Unknown range names are passed through unchanged.
fn coerce_datatype(v: Value, target_range: &str) -> Value {
    match v {
        Value::List(items) => {
            Value::List(items.into_iter().map(|i| coerce_datatype(i, target_range)).collect())
        }
        Value::Map(m) => {
            Value::Map(m.into_iter().map(|(k, i)| (k, coerce_datatype(i, target_range))).collect())
        }
        scalar => match target_range {
            "integer" | "int" => match &scalar {
                Value::Int(_) => scalar,
                Value::Float(f) => Value::Int(*f as i64),
                Value::Str(s) => s
                    .trim()
                    .parse::<i64>()
                    .map(Value::Int)
                    .unwrap_or(scalar),
                Value::Bool(b) => Value::Int(if *b { 1 } else { 0 }),
                _ => scalar,
            },
            "float" | "double" => match &scalar {
                Value::Float(_) => scalar,
                Value::Int(i) => Value::Float(*i as f64),
                Value::Str(s) => s
                    .trim()
                    .parse::<f64>()
                    .map(Value::Float)
                    .unwrap_or(scalar),
                Value::Bool(b) => Value::Float(if *b { 1.0 } else { 0.0 }),
                _ => scalar,
            },
            "string" | "str" => match &scalar {
                Value::Str(_) => scalar,
                other => Value::Str(value_to_string_key(other)),
            },
            "boolean" | "bool" => match &scalar {
                Value::Bool(_) => scalar,
                Value::Int(i) => Value::Bool(*i != 0),
                Value::Str(s) => match s.to_lowercase().as_str() {
                    "true" | "yes" | "1" => Value::Bool(true),
                    _ => Value::Bool(false),
                },
                _ => scalar,
            },
            _ => scalar,
        },
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        datamodel::{ClassDerivation, KeyVal, PermissibleValueDerivation, EnumDerivation, SlotDerivation, TransformationSpecification},
        schema::{ClassDef, EnumDef, InMemorySchema, InMemorySchemaBuilder, PermissibleValue, RangeKind, SlotDef},
    };
    use indexmap::IndexMap;

    // ── Schema helpers ────────────────────────────────────────────────────────

    fn make_person_schema() -> InMemorySchema {
        InMemorySchemaBuilder::new()
            .add_type("string")
            .add_type("integer")
            .add_type("boolean")
            .add_enum(EnumDef {
                name: "GenderType".into(),
                permissible_values: vec![
                    PermissibleValue { text: "male".into(), description: None, meaning: None },
                    PermissibleValue { text: "female".into(), description: None, meaning: None },
                    PermissibleValue { text: "nonbinary man".into(), description: None, meaning: None },
                ],
            })
            .add_class(ClassDef { name: "Person".into(), tree_root: true, is_a: None, mixins: vec![] })
            .add_slot("Person", SlotDef {
                name: "id".into(), range: RangeKind::Type("string".into()),
                multivalued: false, required: true, identifier: true, key: false,
                unit: None, any_of_enums: vec![],
            })
            .add_slot("Person", SlotDef {
                name: "name".into(), range: RangeKind::Type("string".into()),
                multivalued: false, required: false, identifier: false, key: false,
                unit: None, any_of_enums: vec![],
            })
            .add_slot("Person", SlotDef {
                name: "age_in_years".into(), range: RangeKind::Type("integer".into()),
                multivalued: false, required: false, identifier: false, key: false,
                unit: None, any_of_enums: vec![],
            })
            .add_slot("Person", SlotDef {
                name: "gender".into(), range: RangeKind::Enum("GenderType".into()),
                multivalued: false, required: false, identifier: false, key: false,
                unit: None, any_of_enums: vec![],
            })
            .add_slot("Person", SlotDef {
                name: "aliases".into(), range: RangeKind::Type("string".into()),
                multivalued: true, required: false, identifier: false, key: false,
                unit: None, any_of_enums: vec![],
            })
            .add_class(ClassDef { name: "Address".into(), tree_root: false, is_a: None, mixins: vec![] })
            .add_slot("Address", SlotDef {
                name: "street".into(), range: RangeKind::Type("string".into()),
                multivalued: false, required: false, identifier: false, key: false,
                unit: None, any_of_enums: vec![],
            })
            .add_slot("Address", SlotDef {
                name: "city".into(), range: RangeKind::Type("string".into()),
                multivalued: false, required: false, identifier: false, key: false,
                unit: None, any_of_enums: vec![],
            })
            .add_slot("Person", SlotDef {
                name: "current_address".into(), range: RangeKind::Class("Address".into()),
                multivalued: false, required: false, identifier: false, key: false,
                unit: None, any_of_enums: vec![],
            })
            .add_slot("Person", SlotDef {
                name: "friends".into(), range: RangeKind::Class("Person".into()),
                multivalued: true, required: false, identifier: false, key: false,
                unit: None, any_of_enums: vec![],
            })
            .build()
    }

    /// Minimal spec: identity copy of `id` and `name`.
    fn make_identity_spec() -> TransformationSpecification {
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert("id".into(), SlotDerivation { name: "id".into(), ..Default::default() });
        slots.insert("name".into(), SlotDerivation { name: "name".into(), ..Default::default() });
        TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "Person".into(),
                populated_from: Some("Person".into()),
                slot_derivations: Some(slots),
                ..Default::default()
            }]),
            ..Default::default()
        }
    }

    fn src_person(id: &str, name: &str) -> Value {
        let mut m = IndexMap::new();
        m.insert("id".into(), Value::Str(id.into()));
        m.insert("name".into(), Value::Str(name.into()));
        Value::Map(m)
    }

    // ── Test 1: populated_from copy ───────────────────────────────────────────

    #[test]
    fn test_populated_from_copy() {
        let schema = make_person_schema();
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert("id".into(), SlotDerivation {
            name: "id".into(),
            populated_from: Some("id".into()),
            ..Default::default()
        });
        slots.insert("label".into(), SlotDerivation {
            name: "label".into(),
            populated_from: Some("name".into()),
            ..Default::default()
        });
        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "Agent".into(),
                populated_from: Some("Person".into()),
                slot_derivations: Some(slots),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, Some(&schema), None);
        let src = src_person("P:001", "Alice");
        let result = engine.map_object(&src, Some("Person")).unwrap();

        let m = match result { Value::Map(m) => m, _ => panic!("expected Map") };
        assert_eq!(m["id"], Value::Str("P:001".into()));
        assert_eq!(m["label"], Value::Str("Alice".into()));
        assert!(!m.contains_key("name"), "source key should not appear");
    }

    // ── Test 2: expr-derived slot ─────────────────────────────────────────────

    #[test]
    fn test_expr_derived_slot() {
        let schema = make_person_schema();
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert("age_str".into(), SlotDerivation {
            name: "age_str".into(),
            expr: Some("str(age_in_years) + \" years\"".into()),
            ..Default::default()
        });
        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "Agent".into(),
                populated_from: Some("Person".into()),
                slot_derivations: Some(slots),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, Some(&schema), None);
        let mut m = IndexMap::new();
        m.insert("age_in_years".into(), Value::Int(33));
        let src = Value::Map(m);
        let result = engine.map_object(&src, Some("Person")).unwrap();
        let out = match result { Value::Map(m) => m, _ => panic!("expected Map") };
        assert_eq!(out["age_str"], Value::Str("33 years".into()));
    }

    // ── Test 2b: cached-AST expr path matches the string path ─────────────────

    #[test]
    fn test_compiled_exprs_matches_string_path() {
        let schema = make_person_schema();
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert("age_str".into(), SlotDerivation {
            name: "age_str".into(),
            expr: Some("str(age_in_years) + \" years\"".into()),
            ..Default::default()
        });
        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "Agent".into(),
                populated_from: Some("Person".into()),
                slot_derivations: Some(slots),
                ..Default::default()
            }]),
            ..Default::default()
        };

        // Build the compiled cache once; the engine borrows it.
        let compiled = CompiledExprs::build(&spec).expect("compile exprs");

        for age in [0i64, 33, 100] {
            let mut m = IndexMap::new();
            m.insert("age_in_years".into(), Value::Int(age));
            let src = Value::Map(m);

            // String path (no cache).
            let string_engine = ObjectTransformer::new(spec.clone(), Some(&schema), None);
            let string_out = string_engine.map_object(&src, Some("Person")).unwrap();

            // Cached-AST path.
            let cached_engine = ObjectTransformer::new(spec.clone(), Some(&schema), None)
                .with_compiled_exprs(&compiled);
            let cached_out = cached_engine.map_object(&src, Some("Person")).unwrap();

            assert_eq!(cached_out, string_out, "cached/string mismatch at age={age}");
            if let Value::Map(om) = &cached_out {
                assert_eq!(om["age_str"], Value::Str(format!("{age} years")));
            } else {
                panic!("expected Map");
            }
        }
    }

    // ── Test 3: constant value ────────────────────────────────────────────────

    #[test]
    fn test_constant_value() {
        let schema = make_person_schema();
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert("source".into(), SlotDerivation {
            name: "source".into(),
            value: Some(serde_json::json!("database")),
            ..Default::default()
        });
        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "Person".into(),
                populated_from: Some("Person".into()),
                slot_derivations: Some(slots),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, Some(&schema), None);
        let src = src_person("P:001", "Alice");
        let result = engine.map_object(&src, Some("Person")).unwrap();
        let out = match result { Value::Map(m) => m, _ => panic!("expected Map") };
        assert_eq!(out["source"], Value::Str("database".into()));
    }

    // ── Test 4: value_mappings ────────────────────────────────────────────────

    #[test]
    fn test_value_mappings() {
        let schema = make_person_schema();
        let mut vm: IndexMap<String, KeyVal> = IndexMap::new();
        vm.insert("M".into(), KeyVal { key: "M".into(), value: Some(serde_json::json!("male")) });
        vm.insert("F".into(), KeyVal { key: "F".into(), value: Some(serde_json::json!("female")) });
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert("sex".into(), SlotDerivation {
            name: "sex".into(),
            populated_from: Some("gender".into()),
            value_mappings: Some(vm),
            ..Default::default()
        });
        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "Person".into(),
                populated_from: Some("Person".into()),
                slot_derivations: Some(slots),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, Some(&schema), None);
        let mut m = IndexMap::new();
        m.insert("gender".into(), Value::Str("M".into()));
        let src = Value::Map(m);
        let result = engine.map_object(&src, Some("Person")).unwrap();
        let out = match result { Value::Map(m) => m, _ => panic!("expected Map") };
        assert_eq!(out["sex"], Value::Str("male".into()));
    }

    // ── Test 5: enum PV mapping ───────────────────────────────────────────────

    #[test]
    fn test_enum_pv_mapping() {
        let schema = make_person_schema();
        let mut pvds: IndexMap<String, PermissibleValueDerivation> = IndexMap::new();
        pvds.insert("ACTIVE".into(), PermissibleValueDerivation {
            name: "ACTIVE".into(),
            populated_from: Some("active".into()),
            ..Default::default()
        });
        pvds.insert("INACTIVE".into(), PermissibleValueDerivation {
            name: "INACTIVE".into(),
            populated_from: Some("inactive".into()),
            ..Default::default()
        });
        let mut enum_derivations: IndexMap<String, EnumDerivation> = IndexMap::new();
        enum_derivations.insert("StatusType".into(), EnumDerivation {
            name: "StatusType".into(),
            populated_from: Some("GenderType".into()),
            permissible_value_derivations: Some(pvds),
            ..Default::default()
        });
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert("gender".into(), SlotDerivation {
            name: "gender".into(),
            populated_from: Some("gender".into()),
            ..Default::default()
        });
        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "Person".into(),
                populated_from: Some("Person".into()),
                slot_derivations: Some(slots),
                ..Default::default()
            }]),
            enum_derivations: Some(enum_derivations),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, Some(&schema), None);
        let mut m = IndexMap::new();
        m.insert("gender".into(), Value::Str("active".into()));
        let src = Value::Map(m);
        let result = engine.map_object(&src, Some("Person")).unwrap();
        let out = match result { Value::Map(m) => m, _ => panic!("expected Map") };
        // "active" → "ACTIVE" via PV derivation.
        assert_eq!(out["gender"], Value::Str("ACTIVE".into()));
    }

    // ── Test 6: scalar → list cardinality coercion ────────────────────────────

    #[test]
    fn test_scalar_to_list_coercion() {
        let schema = make_person_schema();
        let target_schema = InMemorySchemaBuilder::new()
            .add_class(ClassDef { name: "Agent".into(), tree_root: true, is_a: None, mixins: vec![] })
            .add_slot("Agent", SlotDef {
                name: "aliases".into(), range: RangeKind::Type("string".into()),
                multivalued: true, required: false, identifier: false, key: false,
                unit: None, any_of_enums: vec![],
            })
            .build();
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert("aliases".into(), SlotDerivation {
            name: "aliases".into(),
            populated_from: Some("name".into()),
            ..Default::default()
        });
        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "Agent".into(),
                populated_from: Some("Person".into()),
                slot_derivations: Some(slots),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, Some(&schema), Some(&target_schema));
        let mut m = IndexMap::new();
        m.insert("name".into(), Value::Str("Alice".into()));
        let src = Value::Map(m);
        let result = engine.map_object(&src, Some("Person")).unwrap();
        let out = match result { Value::Map(m) => m, _ => panic!("expected Map") };
        assert_eq!(out["aliases"], Value::List(vec![Value::Str("Alice".into())]));
    }

    // ── Test 7: list → dict (identifier_slot key) cardinality ─────────────────
    // NOTE: list→keyed-dict via dictionary_key is in _reshape_collection (Python),
    // which is a post-processing step that needs the source SchemaView for
    // identifier_slot lookup. We test the cast_collection_as=SingleValued path
    // (list → scalar) instead, which is purely in coerce_cardinality.

    #[test]
    fn test_list_to_single_coercion() {
        let schema = make_person_schema();
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert("name".into(), SlotDerivation {
            name: "name".into(),
            populated_from: Some("aliases".into()),
            cast_collection_as: Some(CollectionType::SingleValued),
            ..Default::default()
        });
        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "Person".into(),
                populated_from: Some("Person".into()),
                slot_derivations: Some(slots),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, Some(&schema), None);
        let mut m = IndexMap::new();
        m.insert("aliases".into(), Value::List(vec![Value::Str("Al".into())]));
        let src = Value::Map(m);
        let result = engine.map_object(&src, Some("Person")).unwrap();
        let out = match result { Value::Map(m) => m, _ => panic!("expected Map") };
        assert_eq!(out["name"], Value::Str("Al".into()));
    }

    // ── Test 8: nested object recursion ──────────────────────────────────────

    #[test]
    fn test_nested_object_recursion() {
        let schema = make_person_schema();
        // Address passthrough.
        let mut addr_slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        addr_slots.insert("street".into(), SlotDerivation { name: "street".into(), ..Default::default() });
        addr_slots.insert("city".into(), SlotDerivation { name: "city".into(), ..Default::default() });
        // Person with current_address.
        let mut person_slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        person_slots.insert("id".into(), SlotDerivation { name: "id".into(), ..Default::default() });
        person_slots.insert("current_address".into(), SlotDerivation {
            name: "current_address".into(),
            populated_from: Some("current_address".into()),
            ..Default::default()
        });
        let spec = TransformationSpecification {
            class_derivations: Some(vec![
                ClassDerivation {
                    name: "Person".into(),
                    populated_from: Some("Person".into()),
                    slot_derivations: Some(person_slots),
                    ..Default::default()
                },
                ClassDerivation {
                    name: "Address".into(),
                    populated_from: Some("Address".into()),
                    slot_derivations: Some(addr_slots),
                    ..Default::default()
                },
            ]),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, Some(&schema), None);

        let mut addr = IndexMap::new();
        addr.insert("street".into(), Value::Str("1 Oak St".into()));
        addr.insert("city".into(), Value::Str("Oaktown".into()));

        let mut person = IndexMap::new();
        person.insert("id".into(), Value::Str("P:001".into()));
        person.insert("current_address".into(), Value::Map(addr));

        let result = engine.map_object(&Value::Map(person), Some("Person")).unwrap();
        let out = match result { Value::Map(m) => m, _ => panic!("expected Map") };
        let addr_out = match &out["current_address"] { Value::Map(m) => m, _ => panic!("expected nested Map") };
        assert_eq!(addr_out["street"], Value::Str("1 Oak St".into()));
        assert_eq!(addr_out["city"], Value::Str("Oaktown".into()));
    }

    // ── Test 9: multivalued nested list recursion ─────────────────────────────

    #[test]
    fn test_multivalued_nested_list() {
        let schema = make_person_schema();
        let mut person_slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        person_slots.insert("id".into(), SlotDerivation { name: "id".into(), ..Default::default() });
        person_slots.insert("friends".into(), SlotDerivation {
            name: "friends".into(),
            populated_from: Some("friends".into()),
            ..Default::default()
        });
        // Friend class (same as Person for recursion).
        let mut friend_slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        friend_slots.insert("id".into(), SlotDerivation { name: "id".into(), ..Default::default() });
        let spec = TransformationSpecification {
            class_derivations: Some(vec![
                ClassDerivation {
                    name: "Person".into(),
                    populated_from: Some("Person".into()),
                    slot_derivations: Some(person_slots),
                    ..Default::default()
                },
            ]),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, Some(&schema), None);

        let mut f1 = IndexMap::new(); f1.insert("id".into(), Value::Str("P:002".into()));
        let mut f2 = IndexMap::new(); f2.insert("id".into(), Value::Str("P:003".into()));

        let mut person = IndexMap::new();
        person.insert("id".into(), Value::Str("P:001".into()));
        person.insert("friends".into(), Value::List(vec![
            Value::Map(f1), Value::Map(f2),
        ]));

        let result = engine.map_object(&Value::Map(person), Some("Person")).unwrap();
        let out = match result { Value::Map(m) => m, _ => panic!("expected Map") };
        if let Value::List(friends) = &out["friends"] {
            assert_eq!(friends.len(), 2);
            if let Value::Map(f) = &friends[0] {
                assert_eq!(f["id"], Value::Str("P:002".into()));
            } else { panic!("expected Map in list"); }
        } else { panic!("expected List"); }
    }

    // ── Test 10: implicit same-name copy (no populated_from) ─────────────────

    #[test]
    fn test_implicit_copy() {
        let schema = make_person_schema();
        let spec = make_identity_spec();
        let engine = ObjectTransformer::new(spec, Some(&schema), None);
        let src = src_person("P:042", "Bob");
        let result = engine.map_object(&src, None).unwrap(); // None → tree_root
        let out = match result { Value::Map(m) => m, _ => panic!("expected Map") };
        assert_eq!(out["id"], Value::Str("P:042".into()));
        assert_eq!(out["name"], Value::Str("Bob".into()));
    }

    // ── Test 11: NULL expr sets slot to Null ──────────────────────────────────

    #[test]
    fn test_null_expr() {
        let schema = make_person_schema();
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert("name".into(), SlotDerivation {
            name: "name".into(),
            expr: Some("NULL".into()),
            ..Default::default()
        });
        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "Person".into(),
                populated_from: Some("Person".into()),
                slot_derivations: Some(slots),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, Some(&schema), None);
        let src = src_person("P:001", "Alice");
        let result = engine.map_object(&src, Some("Person")).unwrap();
        let out = match result { Value::Map(m) => m, _ => panic!("expected Map") };
        assert_eq!(out["name"], Value::Null);
    }

    // ── Test 12: sources (first non-null wins) ────────────────────────────────

    #[test]
    fn test_sources_first_wins() {
        let schema = make_person_schema();
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert("display_name".into(), SlotDerivation {
            name: "display_name".into(),
            sources: Some(vec!["name".into(), "id".into()]),
            ..Default::default()
        });
        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "Person".into(),
                populated_from: Some("Person".into()),
                slot_derivations: Some(slots),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, Some(&schema), None);
        // name is present → should win.
        let src = src_person("P:001", "Charlie");
        let result = engine.map_object(&src, Some("Person")).unwrap();
        let out = match result { Value::Map(m) => m, _ => panic!() };
        assert_eq!(out["display_name"], Value::Str("Charlie".into()));

        // name is absent → id wins.
        let mut m2 = IndexMap::new();
        m2.insert("id".into(), Value::Str("P:999".into()));
        let result2 = engine.map_object(&Value::Map(m2), Some("Person")).unwrap();
        let out2 = match result2 { Value::Map(m) => m, _ => panic!() };
        assert_eq!(out2["display_name"], Value::Str("P:999".into()));
    }

    // ── Test 13: enum mirror_source ───────────────────────────────────────────

    #[test]
    fn test_enum_mirror_source() {
        let schema = make_person_schema();
        let mut enum_derivations: IndexMap<String, EnumDerivation> = IndexMap::new();
        enum_derivations.insert("GenderType".into(), EnumDerivation {
            name: "GenderType".into(),
            populated_from: Some("GenderType".into()),
            mirror_source: Some(true),
            ..Default::default()
        });
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert("gender".into(), SlotDerivation {
            name: "gender".into(),
            populated_from: Some("gender".into()),
            ..Default::default()
        });
        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "Person".into(),
                populated_from: Some("Person".into()),
                slot_derivations: Some(slots),
                ..Default::default()
            }]),
            enum_derivations: Some(enum_derivations),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, Some(&schema), None);
        let mut m = IndexMap::new();
        m.insert("gender".into(), Value::Str("nonbinary man".into()));
        let result = engine.map_object(&Value::Map(m), Some("Person")).unwrap();
        let out = match result { Value::Map(m) => m, _ => panic!() };
        assert_eq!(out["gender"], Value::Str("nonbinary man".into()));
    }

    // ── Test 14: coerce_datatype ──────────────────────────────────────────────

    #[test]
    fn test_coerce_datatype_integer() {
        assert_eq!(coerce_datatype(Value::Str("42".into()), "integer"), Value::Int(42));
        assert_eq!(coerce_datatype(Value::Float(3.7), "integer"), Value::Int(3));
        assert_eq!(coerce_datatype(Value::Int(5), "integer"), Value::Int(5));
    }

    #[test]
    fn test_coerce_datatype_float() {
        assert_eq!(coerce_datatype(Value::Str("3.14".into()), "float"), Value::Float(3.14));
        assert_eq!(coerce_datatype(Value::Int(2), "float"), Value::Float(2.0));
    }

    #[test]
    fn test_coerce_datatype_string() {
        assert_eq!(coerce_datatype(Value::Int(99), "string"), Value::Str("99".into()));
    }

    #[test]
    fn test_coerce_datatype_bool() {
        assert_eq!(coerce_datatype(Value::Str("true".into()), "boolean"), Value::Bool(true));
        assert_eq!(coerce_datatype(Value::Int(0), "boolean"), Value::Bool(false));
    }

    // ── Test 15: json_to_value round-trip ─────────────────────────────────────

    #[test]
    fn test_json_to_value() {
        assert_eq!(json_to_value(&serde_json::json!(null)), Value::Null);
        assert_eq!(json_to_value(&serde_json::json!(true)), Value::Bool(true));
        assert_eq!(json_to_value(&serde_json::json!(42)), Value::Int(42));
        assert_eq!(json_to_value(&serde_json::json!(3.14)), Value::Float(3.14));
        assert_eq!(json_to_value(&serde_json::json!("hello")), Value::Str("hello".into()));
        assert_eq!(json_to_value(&serde_json::json!([1, 2])), Value::List(vec![Value::Int(1), Value::Int(2)]));
    }

    // ── Test 16: no class derivation → error ─────────────────────────────────

    #[test]
    fn test_no_class_derivation_error() {
        let schema = make_person_schema();
        let spec = make_identity_spec();
        let engine = ObjectTransformer::new(spec, Some(&schema), None);
        let src = src_person("P:001", "Alice");
        let err = engine.map_object(&src, Some("Organization")).unwrap_err();
        assert!(matches!(err, Error::NoClassDerivation { .. }));
    }

    // ── Test 17: is_a ancestor slot inheritance ───────────────────────────────

    #[test]
    fn test_is_a_ancestor_slots() {
        let schema = make_person_schema();
        // Entity has id slot. Agent is_a Entity and adds label.
        let mut entity_slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        entity_slots.insert("id".into(), SlotDerivation { name: "id".into(), ..Default::default() });
        let mut agent_slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        agent_slots.insert("label".into(), SlotDerivation {
            name: "label".into(),
            populated_from: Some("name".into()),
            ..Default::default()
        });
        let spec = TransformationSpecification {
            class_derivations: Some(vec![
                ClassDerivation {
                    name: "Entity".into(),
                    slot_derivations: Some(entity_slots),
                    ..Default::default()
                },
                ClassDerivation {
                    name: "Agent".into(),
                    is_a: Some("Entity".into()),
                    populated_from: Some("Person".into()),
                    slot_derivations: Some(agent_slots),
                    ..Default::default()
                },
            ]),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, Some(&schema), None);
        let src = src_person("P:001", "Dave");
        let result = engine.map_object(&src, Some("Person")).unwrap();
        let out = match result { Value::Map(m) => m, _ => panic!() };
        // Inherited from Entity.
        assert_eq!(out["id"], Value::Str("P:001".into()));
        // Own slot.
        assert_eq!(out["label"], Value::Str("Dave".into()));
    }

    // ── Test 18: string split to list (stringification.reversed) ─────────────

    #[test]
    fn test_string_split_to_list() {
        let schema = make_person_schema();
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert("aliases".into(), SlotDerivation {
            name: "aliases".into(),
            populated_from: Some("name".into()),
            stringification: Some(crate::datamodel::StringificationConfiguration {
                delimiter: Some("|".into()),
                reversed: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        });
        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "Person".into(),
                populated_from: Some("Person".into()),
                slot_derivations: Some(slots),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, Some(&schema), None);
        let mut m = IndexMap::new();
        m.insert("name".into(), Value::Str("Alice|Bob|Carol".into()));
        let result = engine.map_object(&Value::Map(m), Some("Person")).unwrap();
        let out = match result { Value::Map(m) => m, _ => panic!() };
        assert_eq!(out["aliases"], Value::List(vec![
            Value::Str("Alice".into()), Value::Str("Bob".into()), Value::Str("Carol".into()),
        ]));
    }

    // ── Test 19: list join to string (stringification delimiter) ─────────────

    #[test]
    fn test_list_join_to_string() {
        let schema = make_person_schema();
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert("name".into(), SlotDerivation {
            name: "name".into(),
            populated_from: Some("aliases".into()),
            stringification: Some(crate::datamodel::StringificationConfiguration {
                delimiter: Some(", ".into()),
                reversed: Some(false),
                ..Default::default()
            }),
            ..Default::default()
        });
        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "Person".into(),
                populated_from: Some("Person".into()),
                slot_derivations: Some(slots),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, Some(&schema), None);
        let mut m = IndexMap::new();
        m.insert("aliases".into(), Value::List(vec![
            Value::Str("Alice".into()), Value::Str("Smith".into()),
        ]));
        let result = engine.map_object(&Value::Map(m), Some("Person")).unwrap();
        let out = match result { Value::Map(m) => m, _ => panic!() };
        assert_eq!(out["name"], Value::Str("Alice, Smith".into()));
    }
}
