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
//! - FK join, stringification, `unit_conversion:`, `class_derivations:`,
//!   `offset:`, `aggregation_operation:` and `pivot_operation:` (melt/unmelt)
//!   ARE ported (`unit_conversion` via the dependency-free [`units`] table).
//!   Aggregation has no Python `ObjectTransformer` reference — its semantics are
//!   defined in [`apply_aggregation`]. Spec→Python/SQL compilation and
//!   JSON-schema target validation are intentionally out of scope.

pub mod lookup_index;
pub mod object_index;
pub mod units;
pub use lookup_index::{LookupIndex, LookupIndexRef};
pub use object_index::ObjectIndex;

use indexmap::IndexMap;

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

use crate::{
    datamodel::{
        AggregationOperation, AggregationType, AliasedClass, ClassDerivation, CollectionType,
        InvalidValueHandlingStrategy, Offset, PivotDirectionType, PivotOperation,
        SerializationSyntaxType, SlotDerivation, TransformationSpecification,
    },
    error::{Error, Result},
    expr::{
        eval_expr_with_mapping, eval_expr_with_mapping_strict, eval_parsed, eval_parsed_strict,
        parse_expr, Bindings, ExprResult, ParsedExpr,
    },
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
        self.slot_exprs.get(&(class.to_string(), slot.to_string()))
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
    /// Optional cross-table lookup index for join resolution.
    ///
    /// When set, `populated_from: "table.field"` and `{table.field}` expr
    /// bindings resolve through this index.  Multi-row aggregation
    /// (`aggregation_operation` + join) uses `lookup_rows` to collect all
    /// matching rows before reducing.  Attach via [`with_lookup_index`].
    lookup_index: Option<Arc<LookupIndex>>,
    /// When true, `expr:` evaluation errors on unbound names / unresolved
    /// `slot()` references instead of silently yielding null (#232). Default
    /// false (lax). Set via [`with_strict_exprs`].
    strict: bool,
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
            lookup_index: None,
            strict: false,
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
            lookup_index: None,
            strict: false,
        }
    }

    /// Attach a pre-built [`LookupIndex`] for cross-table join resolution.
    ///
    /// Register secondary tables on the index before attaching it; the engine
    /// then resolves `populated_from: "alias.field"` and `expr: "{alias.field}"`
    /// references and supports multi-row aggregation over joined rows.
    pub fn with_lookup_index(mut self, index: Arc<LookupIndex>) -> Self {
        self.lookup_index = Some(index);
        self
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

    /// Enable strict expression evaluation: unbound names and unresolved
    /// `slot()` references error instead of yielding null (#232). Default lax.
    pub fn with_strict_exprs(mut self, strict: bool) -> Self {
        self.strict = strict;
        self
    }

    /// Transform a single source object.
    ///
    /// `source_type` names the LinkML class of `source_obj`.  When `None` the
    /// engine tries the schema's tree-root class, then falls back to the first
    /// class derivation's name.
    pub fn map_object(&self, source_obj: &Value, source_type: Option<&str>) -> Result<Value> {
        let source_type = self.resolve_source_type(source_type)?;
        self.map_object_with_type(source_obj, &source_type, None)
    }

    /// Transform a single source object with a pre-built [`ObjectIndex`].
    ///
    /// This is the FK-pipeline entry point: the caller builds the index ONCE
    /// (from the full buffered dataset), wraps it in `Arc`, and hands a
    /// reference into every parallel worker. Each worker calls this method
    /// instead of [`map_object`], paying zero per-row index-rebuild cost.
    ///
    /// When `index.is_empty()`, the call degrades to [`map_object`].
    pub fn map_object_with_index(
        &self,
        source_obj: &Value,
        source_type: Option<&str>,
        index: &ObjectIndex,
    ) -> Result<Value> {
        let source_type = self.resolve_source_type(source_type)?;
        let idx_ref = if index.is_empty() { None } else { Some(index) };
        self.map_object_with_type(source_obj, &source_type, idx_ref)
    }

    /// Transform a whole source **container**, resolving foreign-key references.
    ///
    /// This is the FK-aware entry point. Unlike [`ObjectTransformer::map_object`]
    /// (single object / row-streamable), `map_container` first scans the entire
    /// `container` to build an [`ObjectIndex`] keyed by identifier, then maps the
    /// container with that index available to every nested `expr:` so an FK
    /// scalar (e.g. `subject: "X:1"`) dereferences to its referenced object
    /// (`{id: "X:1", name: "x1"}`) before attribute access (`subject.id`).
    ///
    /// When the spec does **not** dereference any FK-ranged slot, this is exactly
    /// equivalent to [`ObjectTransformer::map_object`]: the index is built but
    /// never consulted, so the result is identical and the (cheap) scan is the
    /// only overhead.
    ///
    /// # Note on streaming
    /// FK resolution needs the *whole* dataset (the referenced collection may
    /// appear after the referencing rows), so FK specs are **not** row-
    /// streamable. The concurrent pipeline keeps the streaming `map_object` path
    /// for non-FK specs; wiring FK into the pipeline (build the index once, share
    /// it across workers) is a follow-up.
    pub fn map_container(&self, container: &Value, source_type: Option<&str>) -> Result<Value> {
        let source_type = self.resolve_source_type(source_type)?;
        let index = ObjectIndex::build(container, Some(&source_type), self.source_schema);
        let idx_ref = if index.is_empty() { None } else { Some(&index) };
        self.map_object_with_type(container, &source_type, idx_ref)
    }

    // ── Internal helpers ──────────────────────────────────────────────────────

    /// Core recursive mapper — source type already resolved.
    ///
    /// `index` carries the optional FK [`ObjectIndex`] down through nested
    /// class-ranged recursion so deep `expr:` accesses can still dereference
    /// foreign keys.
    fn map_object_with_type(
        &self,
        source_obj: &Value,
        source_type: &str,
        index: Option<&ObjectIndex>,
    ) -> Result<Value> {
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

        // Class-level pivot (melt/unmelt) replaces per-slot derivation entirely.
        // Mirrors Python `ObjectTransformer._perform_pivot_operation`.
        if let Some(pivot) = &class_deriv.pivot_operation {
            return self.perform_pivot(pivot, source_map);
        }

        // Per-slot iteration.
        let empty_map = IndexMap::new();
        let slot_derivations = class_deriv.slot_derivations.as_ref().unwrap_or(&empty_map);

        let mut tgt_attrs: IndexMap<String, Value> = IndexMap::new();

        for (_, slot_deriv) in slot_derivations {
            let slot_name = slot_deriv.name.as_str();
            let v = self
                .derive_slot(
                    slot_deriv,
                    source_map,
                    source_type,
                    &class_deriv,
                    index,
                    Some(&tgt_attrs),
                )
                .map_err(|e| Error::SlotTransform {
                    class: class_deriv.name.clone(),
                    slot: slot_name.to_string(),
                    cause: e.to_string(),
                })?;
            tgt_attrs.insert(slot_name.to_string(), v);
        }
        // Remove hidden slots from output (they exist only for slot() references).
        // PARITY: object_transformer.py::map_object (a19eb095) — only `hide`
        // pops a key; a nested join miss is handled upstream in
        // `_derive_nested_objects` (slot retained as null/[]), not here.
        for (_, slot_deriv) in slot_derivations {
            if slot_deriv.hide.unwrap_or(false) {
                tgt_attrs.shift_remove(slot_deriv.name.as_str());
            }
        }

        Ok(Value::Map(tgt_attrs))
    }

    /// Like [`map_object_with_type`] but takes an explicit [`ClassDerivation`]
    /// instead of looking it up from the spec.  Used by `derive_nested_objects`
    /// where the derivation comes directly from the `object_derivations` field.
    fn map_object_internal(
        &self,
        source_map: &IndexMap<String, Value>,
        source_type: &str,
        class_deriv: &ClassDerivation,
        index: Option<&ObjectIndex>,
    ) -> Result<Value> {
        let empty_map = IndexMap::new();
        let slot_derivations = class_deriv.slot_derivations.as_ref().unwrap_or(&empty_map);

        let mut tgt_attrs: IndexMap<String, Value> = IndexMap::new();
        for (_, slot_deriv) in slot_derivations {
            let slot_name = slot_deriv.name.as_str();
            let v = self
                .derive_slot(
                    slot_deriv,
                    source_map,
                    source_type,
                    class_deriv,
                    index,
                    Some(&tgt_attrs),
                )
                .map_err(|e| Error::SlotTransform {
                    class: class_deriv.name.clone(),
                    slot: slot_name.to_string(),
                    cause: e.to_string(),
                })?;
            tgt_attrs.insert(slot_name.to_string(), v);
        }
        // PARITY: object_transformer.py::map_object (a19eb095) — only `hide`
        // removes a key. Nested join misses are collapsed to null/[] in
        // `derive_nested_objects`, so the key stays present here.
        for (_, slot_deriv) in slot_derivations {
            if slot_deriv.hide.unwrap_or(false) {
                tgt_attrs.shift_remove(slot_deriv.name.as_str());
            }
        }
        Ok(Value::Map(tgt_attrs))
    }

    // ── Pivot (melt / unmelt) ─────────────────────────────────────────────────

    /// Dispatch a class-level `pivot_operation`. Mirrors Python
    /// `ObjectTransformer._perform_pivot_operation`.
    fn perform_pivot(
        &self,
        pivot: &PivotOperation,
        source_map: &IndexMap<String, Value>,
    ) -> Result<Value> {
        match pivot.direction {
            PivotDirectionType::Melt => self.perform_melt(pivot, source_map),
            PivotDirectionType::Unmelt => self.perform_unmelt(pivot, source_map),
        }
    }

    /// Wide → EAV/long. `{height: 1.8, weight: 75}` → `[{variable: height,
    /// value: 1.8}, {variable: weight, value: 75}]`. Mirrors `_perform_melt`.
    fn perform_melt(
        &self,
        pivot: &PivotOperation,
        source_map: &IndexMap<String, Value>,
    ) -> Result<Value> {
        let id_slots: Vec<String> = pivot.id_slots.clone().unwrap_or_default();

        // Which slots to melt: explicit source_slots, else inferred from the
        // unmelt_to_class' target slots, else every non-ID slot.
        let slots_to_melt: Vec<String> = if let Some(ss) = &pivot.source_slots {
            ss.clone()
        } else if let (Some(cls), Some(ts)) = (&pivot.unmelt_to_class, self.target_schema) {
            ts.induced_slots(cls)
                .map(|v| v.into_iter().map(|s| s.name).collect())
                .unwrap_or_default()
        } else {
            source_map
                .keys()
                .filter(|k| !id_slots.contains(k))
                .cloned()
                .collect()
        };

        // Base record carries the ID slots into every melted record.
        let mut base = IndexMap::new();
        for id in &id_slots {
            if let Some(v) = source_map.get(id) {
                base.insert(id.clone(), v.clone());
            }
        }

        let mut results = Vec::new();
        for sname in &slots_to_melt {
            if let Some(v) = source_map.get(sname) {
                if !v.is_null() {
                    let mut rec = base.clone();
                    rec.insert(pivot.variable_slot.clone(), Value::Str(sname.clone()));
                    rec.insert(pivot.value_slot.clone(), v.clone());
                    results.push(Value::Map(rec));
                }
            }
        }
        Ok(Value::List(results))
    }

    /// EAV/long → wide. A single EAV record, or a slot holding a list of EAV
    /// records, is collapsed into one wide object. Mirrors `_perform_unmelt`.
    fn perform_unmelt(
        &self,
        pivot: &PivotOperation,
        source_map: &IndexMap<String, Value>,
    ) -> Result<Value> {
        // The source object is itself one EAV record.
        if source_map.contains_key(&pivot.variable_slot)
            && source_map.contains_key(&pivot.value_slot)
        {
            return Ok(unmelt_single_record(pivot, source_map));
        }
        // Otherwise find a multivalued slot holding EAV records.
        for v in source_map.values() {
            if let Value::List(items) = v {
                if let Some(Value::Map(first)) = items.first() {
                    if first.contains_key(&pivot.variable_slot) {
                        return Ok(unmelt_collection(pivot, items));
                    }
                }
            }
        }
        // Nothing to unmelt → pass through.
        Ok(Value::Map(source_map.clone()))
    }

    /// Derive the value for a single slot derivation.
    fn derive_slot(
        &self,
        slot_deriv: &SlotDerivation,
        source_map: &IndexMap<String, Value>,
        source_type: &str,
        class_deriv: &ClassDerivation,
        index: Option<&ObjectIndex>,
        target_attrs: Option<&IndexMap<String, Value>>,
    ) -> Result<Value> {
        let slot_name = slot_deriv.name.as_str();

        // ── Precedence order (mirrors Python map_object) ──────────────────────
        //
        // 1. constant `value:`
        // 2. `expr:`
        // 3. `populated_from:` (direct field copy + value_mappings)
        // 4. `class_derivations:` (nested object derivation — v0.6.0)
        // 5. implicit same-name copy
        //
        // After obtaining v, apply range coercion + cardinality reshaping.

        let (mut v, source_slot_def) = if let Some(const_val) = &slot_deriv.value {
            // 1. Constant value.
            let v = Value::from(const_val);
            (v, None)
        } else if slot_deriv.unit_conversion.is_some() {
            // 1b. unit_conversion — dimensional magnitude conversion.
            //     Mirrors Python `ObjectTransformer._perform_unit_conversion`.
            //     Returns the converted scalar (or a {magnitude, unit} map when
            //     target_magnitude_slot is set); leaves the value unchanged when
            //     the conversion is impossible (unknown / cross-dimension units).
            let v = self.perform_unit_conversion(slot_deriv, source_map, source_type)?;
            (v, None)
        } else if let Some(agg) = &slot_deriv.aggregation_operation {
            // 1c. aggregation_operation — reduce a multivalued source to a scalar
            //     (or a collection, for List/Set/Array).
            //
            // Join path (#188): when `populated_from` is "alias.field" and a
            // LookupIndex is attached, gather ALL matching rows from the join
            // table, pluck `field` from each, and aggregate over the resulting
            // list.  Mirrors Python `_apply_aggregation_over_join`.
            //
            // Otherwise read the source list from `populated_from` (else same-
            // name slot) and aggregate as before.
            let src_key = slot_deriv.populated_from.as_deref().unwrap_or(slot_name);
            let raw = resolve_aggregation_source(
                src_key,
                source_map,
                self.lookup_index.as_deref(),
                class_deriv,
            );
            let v = apply_aggregation(agg, raw, slot_name)?;
            (v, None)
        } else if let Some(expr) = &slot_deriv.expr {
            // 2. Expression.
            let v = self.eval_expr_for_slot(
                expr,
                source_map,
                slot_name,
                source_type,
                class_deriv,
                index,
                target_attrs,
            )?;
            (v, None)
        } else if let Some(populated_from) = &slot_deriv.populated_from {
            // 3. populated_from — direct field copy.
            // `_source_class` is a virtual slot that resolves to the source class
            // name itself, enabling value_mappings keyed by source class (#193).
            // `"alias.field"` resolves via the LookupIndex join (#188).
            let raw = resolve_populated_from_raw(
                populated_from,
                source_type,
                source_map,
                self.lookup_index.as_deref(),
                class_deriv,
                self.source_schema,
                slot_deriv,
            )?;
            // Apply mappings if present and value is non-null.
            let mapped = self.apply_value_mappings(
                raw,
                slot_deriv,
                source_map,
                slot_name,
                source_type,
                class_deriv,
                index,
                target_attrs,
            )?;
            // Source slot def for range/cardinality coercion.
            // _source_class is virtual — no real schema slot to look up.
            let ssd = if populated_from == "_source_class" {
                None
            } else {
                self.source_schema
                    .and_then(|ss| ss.induced_slot(populated_from, source_type).ok())
            };
            (mapped, ssd)
        } else if slot_deriv.class_derivations.is_some() {
            // 4. class_derivations — nested target objects (v0.6.0; replaces the
            //    removed `object_derivations`). Recurse into each nested
            //    ClassDerivation, then decide multivalued/scalar from the TARGET
            //    slot (mirrors Python `_derive_nested_objects` which calls
            //    `target_schemaview.induced_slot`).
            let v = self.derive_nested_objects(
                slot_deriv,
                source_map,
                source_type,
                class_deriv,
                index,
            )?;
            (v, None)
        } else {
            // 5. Implicit same-name copy.
            let raw = source_map.get(slot_name).cloned().unwrap_or(Value::Null);
            let ssd = self
                .source_schema
                .and_then(|ss| ss.induced_slot(slot_name, source_type).ok());
            (raw, ssd)
        };

        // ── missing_values: map sentinel codes (e.g. -9, 999, "NA") to null ───
        //    Mirrors Python `SlotDerivation.missing_values`. Applied to the
        //    resolved value across all derivation branches, before coercion.
        v = apply_missing_values(v, slot_deriv);

        // ── Post-processing: range coercion + cardinality ─────────────────────

        v = self.apply_range_coercion(v, slot_deriv, class_deriv, source_slot_def.as_ref(), index)?;

        // ── URI / CURIE coercion (applied after other coercions) ──────────────
        //
        // When the slot derivation declares `range: uri` or `range: uriorcurie`,
        // expand any CURIE value to a full URI using the source schema's prefix
        // map.  When `range: curie`, compress an absolute URI to a CURIE.
        // This mirrors Python `ObjectTransformer._coerce_uri` behaviour.
        if !v.is_null() {
            if let Some(target_range) = slot_deriv.range.as_deref() {
                v = self.coerce_uri_curie(v, target_range);
            }
        }

        // ── Offset (longitudinal baseline ± offset_value * offset_field) ──────
        //    Mirrors Python `ObjectTransformer._apply_offset`.
        if let Some(off) = &slot_deriv.offset {
            if !v.is_null() {
                v = apply_offset(off, v, source_map);
            }
        }

        Ok(v)
    }

    /// Apply `value_mappings` / `expression_mappings` to `raw` (arm 3 second half).
    ///
    /// Returns `raw` unchanged when neither mapping table is present.  When both
    /// are absent the mapping block is a no-op, matching Python behaviour.
    fn apply_value_mappings(
        &self,
        raw: Value,
        slot_deriv: &SlotDerivation,
        source_map: &IndexMap<String, Value>,
        slot_name: &str,
        source_type: &str,
        class_deriv: &ClassDerivation,
        index: Option<&ObjectIndex>,
        target_attrs: Option<&IndexMap<String, Value>>,
    ) -> Result<Value> {
        if slot_deriv.value_mappings.is_none() && slot_deriv.expression_mappings.is_none() {
            return Ok(raw);
        }
        if raw.is_null() {
            return Ok(raw);
        }
        let key = value_to_string_key(&raw);
        if let Some(kv) = slot_deriv
            .value_mappings
            .as_ref()
            .and_then(|vm| vm.get(&key))
        {
            Ok(Value::from(kv.value.as_ref().unwrap_or(&serde_json::Value::Null)))
        } else if let Some(kv) = slot_deriv
            .expression_mappings
            .as_ref()
            .and_then(|em| em.get(&key))
        {
            let expr = kv.value.as_ref().and_then(|v| v.as_str()).unwrap_or("None");
            self.eval_expr_for_slot(
                expr,
                source_map,
                slot_name,
                source_type,
                class_deriv,
                index,
                target_attrs,
            )
        } else {
            Ok(Value::Null)
        }
    }

    /// Apply range coercion post-processing when a source slot def is present.
    ///
    /// Runs `map_value_by_range` → `coerce_cardinality` → `coerce_datatype` →
    /// `reshape_collection` in that order.  Mirrors Python post-derivation
    /// coercion.  Returns `v` unchanged when `source_slot_def` is `None`, or
    /// when `v` is null or the slot is hidden.
    fn apply_range_coercion(
        &self,
        mut v: Value,
        slot_deriv: &SlotDerivation,
        class_deriv: &ClassDerivation,
        source_slot_def: Option<&SlotDef>,
        index: Option<&ObjectIndex>,
    ) -> Result<Value> {
        if let Some(ssd) = source_slot_def {
            if !v.is_null() && !slot_deriv.hide.unwrap_or(false) {
                v = self.map_value_by_range(&v, ssd, slot_deriv.range.as_deref(), index)?;
                v = self.coerce_cardinality(v, slot_deriv, class_deriv, ssd.multivalued)?;
                if let Some(target_range) = slot_deriv.range.as_deref() {
                    v = coerce_datatype(v, target_range);
                }
                v = self.reshape_collection(v, slot_deriv, ssd)?;
            }
        }
        Ok(v)
    }

    /// Perform a `unit_conversion:` slot derivation.
    ///
    /// Mirrors Python `ObjectTransformer._perform_unit_conversion`:
    ///
    /// - The magnitude is read from `populated_from` (scalar input) or, when
    ///   `source_unit_slot` is set, from a structured `{magnitude, unit}` map
    ///   via `source_magnitude_slot` / `source_unit_slot`.
    /// - The source unit is taken from (in priority order) the structured
    ///   value's unit slot, then the spec's `source_unit`, then the schema
    ///   slot's `unit` annotation. A mismatch between an explicit spec unit
    ///   and the schema unit is an error, matching Python.
    /// - The magnitude is converted to `target_unit` via
    ///   [`units::convert_checked`].
    /// - When `target_magnitude_slot` is set the result is a
    ///   `{target_magnitude_slot: value, target_unit_slot: unit}` map;
    ///   otherwise the bare converted scalar is returned.
    ///
    /// When a unit is unknown to the table or the conversion is dimensionally
    /// incompatible (incl. molar↔mass, which needs a molecular weight not present
    /// in the token), an [`Error::UnitConversion`] is raised — matching Python's
    /// `UndefinedUnitError` / `DimensionalityError`. A non-numeric magnitude
    /// returns `Null` when `none_if_non_numeric` is set, otherwise the original
    /// value is returned unchanged.
    fn perform_unit_conversion(
        &self,
        slot_deriv: &SlotDerivation,
        source_map: &IndexMap<String, Value>,
        source_type: &str,
    ) -> Result<Value> {
        let uc = slot_deriv
            .unit_conversion
            .as_ref()
            .expect("caller guarantees unit_conversion is Some");

        // Locate the raw source value. Prefer populated_from, else same-name.
        let src_key = slot_deriv
            .populated_from
            .as_deref()
            .unwrap_or(slot_deriv.name.as_str());
        let curr_v = match source_map.get(src_key) {
            Some(v) if !v.is_null() => v,
            _ => return Ok(Value::Null),
        };

        // Schema-declared source unit (the SlotDef carries the resolved unit
        // code plus the metaslot scheme — ucum_code / symbol / etc. — if any).
        let schema_unit_ref: Option<crate::schema::UnitRef> = self.source_schema.and_then(|ss| {
            ss.induced_slot(src_key, source_type)
                .ok()
                .and_then(|s| s.unit.clone())
        });
        // The unit system used for conversion follows the source slot's
        // metaslot (Python derives it the same way); units supplied only via the
        // spec or a structured value carry no metaslot, so default to `Other`
        // (a plain pint registry).
        let unit_system = schema_unit_ref
            .as_ref()
            .map(|u| u.system)
            .unwrap_or(crate::schema::UnitSystem::Other);
        let schema_unit: Option<String> = schema_unit_ref.as_ref().map(|u| u.code.clone());
        let spec_unit = uc.source_unit.clone();

        if let (Some(su), Some(pu)) = (&schema_unit, &spec_unit) {
            if su != pu {
                return Err(Error::SlotTransform {
                    class: source_type.to_string(),
                    slot: slot_deriv.name.clone(),
                    cause: format!(
                        "mismatch in source units for slot '{src_key}': schema unit \
                         '{su}' vs. transformation spec '{pu}'"
                    ),
                });
            }
        }
        // Resolved 'from' unit from schema or spec (may still be overridden by
        // a structured value's unit slot below).
        let mut from_unit: Option<String> = schema_unit.clone().or_else(|| spec_unit.clone());

        // Extract magnitude (scalar vs structured {value, unit}).
        let magnitude_val: &Value = if let Some(unit_slot) = &uc.source_unit_slot {
            // Structured input.
            let map = match curr_v {
                Value::Map(m) => m,
                _ => {
                    return Err(Error::SlotTransform {
                        class: source_type.to_string(),
                        slot: slot_deriv.name.clone(),
                        cause: format!(
                            "source_unit_slot set but value for '{src_key}' is not a map"
                        ),
                    })
                }
            };
            match map.get(unit_slot) {
                Some(Value::Str(u)) if !u.is_empty() => {
                    if let Some(fu) = &from_unit {
                        if u != fu {
                            return Err(Error::SlotTransform {
                                class: source_type.to_string(),
                                slot: slot_deriv.name.clone(),
                                cause: format!(
                                    "value unit '{u}' does not match expected '{fu}' \
                                     for slot '{src_key}'"
                                ),
                            });
                        }
                    }
                    from_unit = Some(u.clone());
                }
                _ => {
                    return Err(Error::SlotTransform {
                        class: source_type.to_string(),
                        slot: slot_deriv.name.clone(),
                        cause: format!("missing unit in structured value for slot '{src_key}'"),
                    })
                }
            }
            let mag_slot = uc.source_magnitude_slot.as_deref().unwrap_or("value");
            match map.get(mag_slot) {
                Some(m) => m,
                None => {
                    return Err(Error::SlotTransform {
                        class: source_type.to_string(),
                        slot: slot_deriv.name.clone(),
                        cause: format!(
                            "missing magnitude in structured value for slot '{src_key}'"
                        ),
                    })
                }
            }
        } else {
            curr_v
        };

        // Coerce magnitude to f64.
        let magnitude = match magnitude_val.try_numeric() {
            Some(m) => m,
            None => {
                if uc.none_if_non_numeric.unwrap_or(false) {
                    return Ok(Value::Null);
                }
                // No numeric magnitude and not told to null it: leave unchanged.
                return Ok(curr_v.clone());
            }
        };

        // Determine target unit (defaults to the source unit → identity).
        let to_unit = uc.target_unit.clone().or_else(|| from_unit.clone());

        let (result, out_unit) = match (&from_unit, &to_unit) {
            (Some(fu), Some(tu)) => match units::convert_checked_ex(
                magnitude,
                fu,
                tu,
                unit_system,
                uc.molecular_weight,
                uc.valence,
            ) {
                Ok(r) => (r, tu.clone()),
                // Unknown unit / incompatible dimensions (incl. molar↔mass):
                // raise, mirroring Python's UndefinedUnitError / DimensionalityError.
                Err(units::ConvError::Undefined(u)) => {
                    return Err(Error::UnitConversion {
                        slot: slot_deriv.name.clone(),
                        msg: format!("undefined unit '{u}'"),
                    })
                }
                Err(units::ConvError::Dimensionality(f, t)) => {
                    return Err(Error::UnitConversion {
                        slot: slot_deriv.name.clone(),
                        msg: format!("cannot convert '{f}' to '{t}': incompatible dimensions"),
                    })
                }
            },
            // No units resolvable at all: pass the magnitude through.
            _ => (magnitude, from_unit.clone().unwrap_or_default()),
        };

        let result_val = float_to_value(result);

        if let Some(tgt_mag_slot) = &uc.target_magnitude_slot {
            let mut out = IndexMap::new();
            out.insert(tgt_mag_slot.clone(), result_val);
            if let Some(tgt_unit_slot) = &uc.target_unit_slot {
                out.insert(tgt_unit_slot.clone(), Value::Str(out_unit));
            }
            Ok(Value::Map(out))
        } else {
            Ok(result_val)
        }
    }

    /// Apply URI expansion or CURIE compression to a value based on the
    /// target range name.
    ///
    /// - `"uri"` / `"uriorcurie"` → expand CURIEs to full URIs (already-
    ///   absolute URIs pass through unchanged).
    /// - `"curie"` → compress full URIs to CURIEs (already-CURIE strings
    ///   pass through unchanged).
    ///
    /// Lists are mapped element-wise.  Non-string scalars and maps are
    /// returned unchanged.  When the schema provider cannot perform the
    /// conversion (unknown prefix / no prefix map), the value is returned
    /// unchanged (safe no-op).
    fn coerce_uri_curie(&self, v: Value, target_range: &str) -> Value {
        match target_range {
            "uri" | "uriorcurie" => self.apply_to_strings(v, |s| {
                if let Some(ss) = self.source_schema {
                    ss.expand_curie(s).unwrap_or_else(|| s.to_owned())
                } else {
                    s.to_owned()
                }
            }),
            "curie" => self.apply_to_strings(v, |s| {
                if let Some(ss) = self.source_schema {
                    ss.compress_uri(s).unwrap_or_else(|| s.to_owned())
                } else {
                    s.to_owned()
                }
            }),
            _ => v,
        }
    }

    /// Map a string-transform function over a [`Value`], applying it to every
    /// string leaf while leaving non-string scalars and maps unchanged.
    fn apply_to_strings<F>(&self, v: Value, f: F) -> Value
    where
        F: Fn(&str) -> String + Copy,
    {
        match v {
            Value::Str(s) => Value::Str(f(&s)),
            Value::List(items) => Value::List(
                items
                    .into_iter()
                    .map(|i| self.apply_to_strings(i, f))
                    .collect(),
            ),
            other => other,
        }
    }

    // ── Expression evaluation ─────────────────────────────────────────────────

    fn eval_expr_for_slot(
        &self,
        expr: &str,
        source_map: &IndexMap<String, Value>,
        slot_name: &str,
        source_type: &str,
        class_deriv: &ClassDerivation,
        index: Option<&ObjectIndex>,
        target_attrs: Option<&IndexMap<String, Value>>,
    ) -> Result<Value> {
        let class_name = class_deriv.name.as_str();
        // Build bindings from all source map keys.
        let mut bindings: Bindings = IndexMap::new();
        bindings.insert("NULL".to_string(), Value::Null);
        // _source_class is the source class name, available in expr: bindings (#193).
        bindings.insert(
            "_source_class".to_string(),
            Value::Str(source_type.to_string()),
        );
        for (k, v) in source_map {
            // FK dereference: when this source slot holds a foreign key (a
            // scalar identifier whose range is a class), replace the binding
            // with the *referenced object* from the index so attribute access
            // (`subject.id` / `subject.name`) resolves against the dereferenced
            // entity rather than failing on a bare string. Distributes over a
            // multivalued FK (list of ids → list of resolved objects).
            let bound = match index {
                Some(idx) if self.is_fk_slot(k, source_type, v, idx) => {
                    self.deref_fk_value(k, source_type, v, idx)
                }
                _ => v.clone(),
            };
            bindings.insert(k.clone(), bound);
        }

        // Join bindings (#188): for each declared join alias, look up the
        // matching row from the LookupIndex and bind the whole row map as
        // `alias → Value::Map(row)`.  This lets expr: "{alias.field}" resolve
        // via normal dot-access on the bound map without any parser changes.
        if let (Some(li), Some(joins)) = (self.lookup_index.as_deref(), class_deriv.joins.as_ref())
        {
            for (alias, ac) in joins {
                // Python: source_key or join_on; skip binding when key spec absent.
                let Some(sk) = join_source_key(ac) else {
                    continue;
                };
                if let Some(key_val) = source_map.get(sk) {
                    let key_str = match key_val {
                        Value::Str(s) => s.clone(),
                        Value::Int(i) => i.to_string(),
                        _ => continue,
                    };
                    let table = ac.class_named.as_deref().unwrap_or(alias.as_str());
                    if let Some(row) = li.lookup_row(table, &key_str) {
                        bindings.insert(alias.clone(), Value::Map(row.clone()));
                    }
                }
            }
        }

        // Bind the whole source object as `src`, mirroring the asteval
        // `usersyms={"src": ctxt_obj, ...}` contract used by linkml-map's
        // multi-statement (`expr:`) blocks. Multi-statement specs read
        // `src.<slot>`; single-expression specs bind slot names directly and
        // do not need `src`, but binding it is harmless (it only shadows a
        // literal `src` source slot, which the schemas do not define).
        let src_obj: IndexMap<String, Value> = bindings
            .iter()
            .filter(|(k, _)| k.as_str() != "NULL")
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        bindings.insert("src".to_string(), Value::Map(src_obj));
        if let Some(attrs) = target_attrs {
            bindings.insert("__slot_values".to_string(), Value::Map(attrs.clone()));
        }

        // Cached-AST fast path: evaluate the pre-parsed expr when a compiled
        // cache is attached and holds this (class, slot). Falls back to the
        // string parse path otherwise (identical result).
        let result = match (
            self.compiled.and_then(|c| c.slot(class_name, slot_name)),
            self.strict,
        ) {
            (Some(parsed), false) => eval_parsed(parsed, &bindings),
            (Some(parsed), true) => eval_parsed_strict(parsed, &bindings),
            (None, false) => eval_expr_with_mapping(expr, &bindings),
            (None, true) => eval_expr_with_mapping_strict(expr, &bindings),
        };
        result.map_err(|e| Error::ExprEval {
            class: source_type.to_string(),
            slot: slot_name.to_string(),
            cause: e.to_string(),
        })
    }

    /// Decide whether `(slot_name, value)` on `source_type` is a foreign-key
    /// reference that should be dereferenced through the [`ObjectIndex`].
    ///
    /// Schema-first: when the source schema knows this slot and its range is a
    /// **class**, a scalar value is treated as an FK (an inlined object — a
    /// `Map` — is left as-is). When the schema is unavailable or does not know
    /// the slot, fall back to a structural test: a scalar value (or list of
    /// scalars) whose id(s) are present in the index.
    fn is_fk_slot(
        &self,
        slot_name: &str,
        source_type: &str,
        value: &Value,
        index: &ObjectIndex,
    ) -> bool {
        // An inlined object is never an FK scalar.
        if matches!(value, Value::Map(_)) {
            return false;
        }
        // Schema says range is a class → FK by definition.
        if let Some(ss) = self.source_schema {
            if let Ok(slot) = ss.induced_slot(slot_name, source_type) {
                return slot.range_class().is_some();
            }
        }
        // Fallback: scalar (or list of scalars) that the index can resolve.
        match value {
            Value::Str(s) => index.contains_id(s),
            Value::Int(i) => index.contains_id(&i.to_string()),
            Value::List(items) => items.iter().any(|it| match it {
                Value::Str(s) => index.contains_id(s),
                Value::Int(i) => index.contains_id(&i.to_string()),
                _ => false,
            }),
            _ => false,
        }
    }

    /// Resolve an FK value to its referenced object(s) via the index, keeping
    /// the original value when no entry is found (so a dangling FK degrades to
    /// the bare scalar rather than vanishing). Distributes over lists.
    fn deref_fk_value(
        &self,
        slot_name: &str,
        source_type: &str,
        value: &Value,
        index: &ObjectIndex,
    ) -> Value {
        let range_class: Option<String> = self.source_schema.and_then(|ss| {
            ss.induced_slot(slot_name, source_type)
                .ok()
                .and_then(|s| s.range_class().map(|c| c.to_string()))
        });

        let resolve_one = |v: &Value| -> Value {
            let id = match v {
                Value::Str(s) => Some(s.clone()),
                Value::Int(i) => Some(i.to_string()),
                _ => None,
            };
            match id {
                Some(id) => index
                    .get(range_class.as_deref(), &id)
                    .cloned()
                    .unwrap_or_else(|| v.clone()),
                None => v.clone(),
            }
        };

        match value {
            Value::List(items) => Value::List(items.iter().map(&resolve_one).collect()),
            other => resolve_one(other),
        }
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
        index: Option<&ObjectIndex>,
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
                            .map(|item| self.transform_enum(item, &[enum_name.clone()], item))
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
                                        index,
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
                                            index,
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
                                index,
                            )?;
                            Ok(Value::List(vec![inner]))
                        }
                    }
                } else {
                    self.map_object_with_type_and_target(
                        v,
                        class_name,
                        target_range_str.as_deref(),
                        index,
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
        index: Option<&ObjectIndex>,
    ) -> Result<Value> {
        // For now target_range is only meaningful for scalar type coercions,
        // which are handled by coerce_datatype. The recursive call just needs
        // the source class to find its derivation. The FK index is threaded
        // through so nested class-ranged objects can still dereference FKs.
        self.map_object_with_type(v, source_type, index)
    }

    // ── Collection reshape (dictionary_key / cast_collection_as) ─────────────

    /// Mirror of Python `ObjectTransformer._reshape_collection`.
    ///
    /// Two directions:
    ///
    /// **List → compact dict** (keyed dict):
    ///   Triggered by `dictionary_key: <slot>` on the SlotDerivation.
    ///   Each element of the list becomes a value in the dict, keyed by the
    ///   element's `dictionary_key` field.  The key field is dropped from
    ///   the stored value (Python: `del v1[slot_derivation.dictionary_key]`).
    ///   If `dictionary_key` is absent but `cast_collection_as:
    ///   MultiValuedDict` is set, the source range class's identifier/key
    ///   slot is used as the key (via `SchemaProvider::identifier_slot`).
    ///
    /// **Compact dict → list** (unkey):
    ///   Triggered by `cast_collection_as: MultiValuedList` on a dict value.
    ///   Each `(k, v)` pair is re-emitted as `{…v, <id_slot>: k}` when the
    ///   range class has an identifier slot, otherwise as bare `v` values.
    ///   Mirrors Python: `[{**v1, src_rng_id_slot.name: k} for k, v1 in v.items()]`.
    fn reshape_collection(
        &self,
        v: Value,
        sd: &SlotDerivation,
        source_slot: &SlotDef,
    ) -> Result<Value> {
        // ── List → keyed dict ────────────────────────────────────────────────
        //
        // If `dictionary_key` is explicitly set, use it directly.
        // If only `cast_collection_as: MultiValuedDict` is set, fall back to
        // the identifier slot of the source range class.
        let dict_key: Option<String> = if let Some(dk) = &sd.dictionary_key {
            Some(dk.clone())
        } else if sd
            .cast_collection_as
            .as_ref()
            .map(|c| matches!(c, CollectionType::MultiValuedDict))
            .unwrap_or(false)
        {
            // Try to find the identifier slot from the source range class.
            if let Some(range_class) = source_slot.range_class() {
                if let Some(src_schema) = self.source_schema {
                    src_schema
                        .identifier_slot(range_class)
                        .ok()
                        .flatten()
                        .map(|s| s.name)
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

        if let Some(key_slot) = dict_key {
            if let Value::List(items) = v {
                let mut result: IndexMap<String, Value> = IndexMap::new();
                for item in items {
                    if let Value::Map(mut m) = item {
                        let key_val = m
                            .shift_remove(&key_slot)
                            .map(|kv| value_to_string_key(&kv))
                            .unwrap_or_default();
                        result.insert(key_val, Value::Map(m));
                    }
                    // Non-map items in a list being keyed: skip (no key to extract).
                }
                return Ok(Value::Map(result));
            }
        }

        // ── Keyed dict → list ────────────────────────────────────────────────
        if let Some(CollectionType::MultiValuedList) = &sd.cast_collection_as {
            if let Value::Map(m) = v {
                // Look up the identifier slot of the source range class so we
                // can re-inject the key into each value object.
                let id_slot_name: Option<String> =
                    if let Some(range_class) = source_slot.range_class() {
                        self.source_schema
                            .and_then(|ss| ss.identifier_slot(range_class).ok())
                            .flatten()
                            .map(|s| s.name)
                    } else {
                        None
                    };

                let list: Vec<Value> = m
                    .into_iter()
                    .map(|(k, val)| {
                        if let Some(ref id_slot) = id_slot_name {
                            // Re-inject key into the value map.
                            if let Value::Map(mut inner) = val {
                                inner.insert(id_slot.clone(), Value::Str(k));
                                Value::Map(inner)
                            } else {
                                val
                            }
                        } else {
                            // No identifier slot — return bare values.
                            val
                        }
                    })
                    .collect();
                return Ok(Value::List(list));
            }
        }

        Ok(v)
    }

    // ── Nested object derivations ─────────────────────────────────────────────

    /// Mirror of Python `ObjectTransformer._derive_nested_objects` (v0.6.0).
    ///
    /// Iterates `slot_deriv.class_derivations` (the v0.6.0 slot-level nested
    /// object map, one level flatter than the removed `object_derivations`) and
    /// recursively transforms `source_map` using each `ClassDerivation`.  The
    /// resulting objects are collected into a `Vec`.
    ///
    /// Cardinality is decided from the **target slot** (not the source), as
    /// Python does via `target_schemaview.induced_slot(slot, target_class)`:
    /// - If `target_schema` is available and the induced target slot is
    ///   `multivalued`, the whole `Vec` is returned as `Value::List`.
    /// - If exactly one object was produced and the target slot is
    ///   single-valued (or no target schema is loaded), the single object is
    ///   returned directly.
    /// - An empty `Vec` always yields `Value::Null`.
    ///
    /// After collecting, `reshape_collection` is **not** called here — the
    /// `dictionary_key` / `cast_collection_as` path in `derive_slot` only
    /// fires when a `source_slot_def` is present (populated_from path).
    /// Object-derivation slots that additionally need dict reshape should be
    /// expressed via `dictionary_key` on the outer slot; callers may chain
    /// that separately.
    fn derive_nested_objects(
        &self,
        slot_deriv: &SlotDerivation,
        source_map: &IndexMap<String, Value>,
        source_type: &str,
        class_deriv: &ClassDerivation,
        index: Option<&ObjectIndex>,
    ) -> Result<Value> {
        let cls_derivations = match &slot_deriv.class_derivations {
            Some(v) => v,
            None => return Ok(Value::Null),
        };

        let mut derived: Vec<Value> = Vec::new();

        for (_target_cls, cls_deriv) in cls_derivations {
            // `populated_from` on the ClassDerivation tells us which source
            // class to use.  When absent, fall back to the outer source_type
            // (same-class derivation).
            let effective_source_type = cls_deriv.populated_from.as_deref().unwrap_or(source_type);

            // PARITY: object_transformer.py::_derive_nested_objects (a19eb095, #217).
            // When a nested class_derivation joins to a *different* table and no
            // row matches the join key (sparse miss), upstream does `continue`
            // (`if joined_row is None: ... continue`): it appends no object for
            // that derivation. The enclosing slot is then assigned `None`
            // (singular) or `[]` (multivalued) below — the target key is
            // RETAINED with a null/empty value. This keeps "no joined row exists"
            // distinguishable from "a joined row exists whose field is null",
            // and only `hide` ever removes the key. This supersedes the earlier
            // local #266 behaviour, which over-broadly dropped ANY hollow nested
            // object (including genuine same-table `{slot: null}` objects that
            // upstream retains).
            if self.nested_join_misses(cls_deriv, source_map, class_deriv) {
                continue;
            }

            // Recursively transform the same source map using the nested
            // ClassDerivation.
            let nested =
                self.map_object_internal(source_map, effective_source_type, cls_deriv, index)?;
            derived.push(nested);
        }

        // Decide cardinality from the target schema slot when available,
        // mirroring Python: `target_class_slot = self.target_schemaview.induced_slot(slot, target_type)`.
        // PARITY (a19eb095): cardinality is applied even when `derived` is empty
        // (all nested derivations were skipped as join misses) — multivalued
        // yields `[]`, singular yields `None`. Upstream: `v = derived_objs` when
        // multivalued else `derived_objs[0] if derived_objs else None`. Do NOT
        // early-return Null on empty: that would drop the `[]` for a multivalued
        // all-miss.
        let target_multivalued = self
            .target_schema
            .and_then(|ts| ts.induced_slot(&slot_deriv.name, &class_deriv.name).ok())
            .map(|s| s.multivalued)
            .unwrap_or(false); // default: single-valued when no target schema

        if target_multivalued {
            Ok(Value::List(derived))
        } else {
            // Single-valued: return first; if >1 that's a schema mismatch but
            // we follow Python and return the first without erroring.
            Ok(derived.into_iter().next().unwrap_or(Value::Null))
        }
    }

    /// True when `nested` joins to a different table than its parent and the
    /// join finds no matching row (a sparse join miss).
    ///
    /// Mirrors Python `ObjectTransformer._resolve_joined_row(...) is None` as
    /// used by `_derive_nested_objects` at a19eb095 (#217): the miss condition
    /// is `source_obj.get(source_key) is None` (no key on the parent row) or the
    /// `LookupIndex` finding no row for that key.  Returns `false` for
    /// same-table nested derivations and when no join spec exists — those are
    /// resolved normally and never collapsed.
    fn nested_join_misses(
        &self,
        nested: &ClassDerivation,
        source_map: &IndexMap<String, Value>,
        parent_class_deriv: &ClassDerivation,
    ) -> bool {
        let parent_source = parent_class_deriv
            .populated_from
            .as_deref()
            .unwrap_or(parent_class_deriv.name.as_str());
        let Some(nested_source) = nested.populated_from.as_deref() else {
            return false;
        };
        if nested_source == parent_source {
            return false;
        }
        // Join spec is keyed by the nested source (mirrors upstream
        // `parent_class_deriv.joins[nested_source]`).
        let Some(ac) = parent_class_deriv
            .joins
            .as_ref()
            .and_then(|j| j.get(nested_source))
        else {
            // No join spec: upstream raises rather than collapsing. The error is
            // surfaced by the normal resolution path; do not treat as a miss.
            return false;
        };
        // Python: source_key = spec.source_key or spec.join_on. Missing key spec
        // is a config error handled elsewhere — not a miss.
        let Some(source_key) = join_source_key(ac) else {
            return false;
        };
        // Python: key_val = source_obj.get(source_key); if key_val is None: None.
        let key_str = match source_map.get(source_key) {
            Some(Value::Str(s)) => s.clone(),
            Some(Value::Int(i)) => i.to_string(),
            // Null / absent / non-scalar key ⇒ no join possible ⇒ miss.
            _ => return true,
        };
        let table = ac.class_named.as_deref().unwrap_or(nested_source);
        // Miss when the index is absent or holds no row for this key.
        self.lookup_index
            .as_deref()
            .and_then(|li| li.lookup_row(table, &key_str))
            .is_none()
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
            // Do NOT wrap a Map when reshape_collection will later convert it
            // (MultiValuedList: dict→list; MultiValuedDict / dictionary_key:
            // list→dict). Wrapping the Map in a 1-element List here would
            // prevent reshape_collection from seeing the dict structure.
            let reshape_will_handle = matches!(
                slot_deriv.cast_collection_as,
                Some(CollectionType::MultiValuedList) | Some(CollectionType::MultiValuedDict)
            ) || slot_deriv.dictionary_key.is_some();
            let is_map = matches!(&v, Value::Map(_));
            if !matches!(&v, Value::Null)
                && !matches!(&v, Value::List(_))
                && !(reshape_will_handle && is_map)
            {
                return self.single_to_multivalued(v, slot_deriv);
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
            // MultiValuedDict and MultiValuedList are handled entirely by
            // reshape_collection (dictionary_key keying / compact-dict unkey).
            // They must NOT trigger the scalar→list wrap here because the
            // value at this point is a Map (dict) or List (list), not a bare
            // scalar, and wrapping it would produce a 1-element list instead of
            // letting reshape_collection do the structural conversion.
            if matches!(cast_as, CollectionType::MultiValued) {
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
        // Stringification: join with delimiter, or serialise via a syntax.
        if let Some(s) = &sd.stringification {
            if let Some(delim) = &s.delimiter {
                let parts: Vec<String> = items.iter().map(value_to_string_key).collect();
                return Ok(Value::Str(parts.join(delim)));
            }
            if let Some(syntax) = &s.syntax {
                let list = Value::List(items);
                let out = match syntax {
                    SerializationSyntaxType::Json => py_json(&list),
                    SerializationSyntaxType::Yaml => yaml_flow(&list),
                    SerializationSyntaxType::Turtle => {
                        return Err(Error::SlotTransform {
                            class: String::new(),
                            slot: sd.name.clone(),
                            cause: "TURTLE stringification not supported".into(),
                        })
                    }
                };
                return Ok(Value::Str(out));
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
                let evaled = match (
                    self.compiled.and_then(|c| c.enum_exprs.get(ed_key)),
                    self.strict,
                ) {
                    (Some(parsed), false) => eval_parsed(parsed, &bindings),
                    (Some(parsed), true) => eval_parsed_strict(parsed, &bindings),
                    (None, false) => eval_expr_with_mapping(expr, &bindings),
                    (None, true) => eval_expr_with_mapping_strict(expr, &bindings),
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
                    // populated_from match — v0.6.0 list-form (#250): a single
                    // string or a list of source PVs, any of which maps here.
                    if let Some(sources) = &pvd.populated_from {
                        if sources.iter().any(|s| s == &src_str) {
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

/// Wrap a converted magnitude as a [`Value`].
///
/// Python `convert_units` always returns a float, so unit-conversion results
/// are emitted as [`Value::Float`] to match that observable behaviour (even for
/// whole numbers like `1.0`).
fn float_to_value(f: f64) -> Value {
    Value::Float(f)
}



// ── Join key helpers (mirrors Python `_resolve_joined_row`) ──────────────────

/// Source-side join key: `spec.source_key or spec.join_on`.
///
/// Returns `None` when neither is set — mirrors Python's ValueError
/// "must specify 'join_on' or both 'source_key' and 'lookup_key'".
fn join_source_key(ac: &AliasedClass) -> Option<&str> {
    ac.source_key.as_deref().or(ac.join_on.as_deref())
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

/// Map `v` to `Value::Null` when it matches any sentinel in
/// `slot_deriv.missing_values`.  Returns `v` unchanged when there are no
/// sentinels or `v` is already null.  Mirrors Python
/// `SlotDerivation.missing_values`.
fn apply_missing_values(mut v: Value, slot_deriv: &SlotDerivation) -> Value {
    if v.is_null() {
        return v;
    }
    if let Some(sentinels) = &slot_deriv.missing_values {
        let v_key = value_to_string_key(&v);
        let is_missing = sentinels.iter().any(|s| {
            let sv = Value::from(s);
            sv == v || value_to_string_key(&sv) == v_key
        });
        if is_missing {
            v = Value::Null;
        }
    }
    v
}

/// Resolve the raw [`Value`] for an `aggregation_operation:` arm (arm 1c).
///
/// When `src_key` is an `"alias.field"` dotted path and the engine has both a
/// [`LookupIndex`] and `class_deriv.joins`, gathers ALL matching rows from the
/// join table and returns them as a `Value::List`.  Falls back to a plain
/// `source_map` lookup otherwise.
fn resolve_aggregation_source(
    src_key: &str,
    source_map: &IndexMap<String, Value>,
    lookup_index: Option<&LookupIndex>,
    class_deriv: &ClassDerivation,
) -> Value {
    if let Some(dot) = src_key.find('.') {
        let alias = &src_key[..dot];
        let field = &src_key[dot + 1..];
        if let (Some(li), Some(joins)) = (lookup_index, class_deriv.joins.as_ref()) {
            if let Some(ac) = joins.get(alias) {
                let table = ac.class_named.as_deref().unwrap_or(alias);
                let key_val = join_source_key(ac).and_then(|sk| source_map.get(sk));
                if let Some(key_val) = key_val {
                    let key_str = match key_val {
                        Value::Str(s) => s.clone(),
                        Value::Int(i) => i.to_string(),
                        _ => String::new(),
                    };
                    let items: Vec<Value> = li
                        .lookup_rows(table, &key_str)
                        .iter()
                        .filter_map(|row| row.get(field).cloned())
                        .collect();
                    return Value::List(items);
                } else {
                    return Value::Null;
                }
            }
        }
    }
    source_map.get(src_key).cloned().unwrap_or(Value::Null)
}

/// Resolve the raw [`Value`] for the `populated_from:` arm (arm 3, first half).
///
/// Handles the `_source_class` virtual slot, inlined nested dot-paths (#247),
/// dotted `"alias.field"` join paths, and plain source-map lookups.
///
/// Mirrors Python `ObjectTransformer._resolve_fk_or_literal`
/// (`object_transformer.py`): a dotted path that traverses inlined nested data
/// is walked structurally *before* attempting FK/join resolution.
#[allow(clippy::too_many_arguments)]
fn resolve_populated_from_raw(
    populated_from: &str,
    source_type: &str,
    source_map: &IndexMap<String, Value>,
    lookup_index: Option<&LookupIndex>,
    class_deriv: &ClassDerivation,
    source_schema: Option<&dyn SchemaProvider>,
    slot_deriv: &SlotDerivation,
) -> Result<Value> {
    if populated_from == "_source_class" {
        return Ok(Value::Str(source_type.to_string()));
    }
    if let Some(dot) = populated_from.find('.') {
        // #247: inlined nested dot-path takes precedence over FK/join resolution.
        // Mirrors `if "." in populated_from and self._is_inline_path(...)` at the
        // top of Python `_resolve_fk_or_literal`.
        if is_inline_path(populated_from, source_type, source_map, source_schema) {
            return resolve_inline_path(populated_from, source_type, source_map, source_schema, slot_deriv);
        }
        let alias = &populated_from[..dot];
        let field = &populated_from[dot + 1..];
        if let (Some(li), Some(joins)) = (lookup_index, class_deriv.joins.as_ref()) {
            if let Some(ac) = joins.get(alias) {
                let table = ac.class_named.as_deref().unwrap_or(alias);
                return Ok(source_map
                    .get(join_source_key(ac).unwrap_or(""))
                    .and_then(|kv| {
                        let ks = match kv {
                            Value::Str(s) => s.clone(),
                            Value::Int(i) => i.to_string(),
                            _ => return None,
                        };
                        li.lookup_row(table, &ks)?.get(field).cloned()
                    })
                    .unwrap_or(Value::Null));
            }
        }
        // No join index — fall through to plain map lookup.
        return Ok(source_map
            .get(populated_from)
            .cloned()
            .unwrap_or(Value::Null));
    }
    Ok(source_map
        .get(populated_from)
        .cloned()
        .unwrap_or(Value::Null))
}

/// Decide whether a dot-path traverses inlined nested data rather than an FK.
///
/// Mirrors Python `ObjectTransformer._is_inline_path` (`object_transformer.py`,
/// issue #247). Detection is declarative first, with a runtime fallback:
///
/// * **Declarative:** the first path segment's source slot has a class range and
///   is marked `inlined` / `inlined_as_list`.
/// * **Runtime fallback:** the value at the first segment is actually a nested
///   object (map) or list, even when the schema doesn't declare `inlined`.
///
/// Foreign keys (class range, scalar identifier value, not inlined) fall through
/// to the join/FK resolution as before.
fn is_inline_path(
    populated_from: &str,
    source_type: &str,
    source_map: &IndexMap<String, Value>,
    source_schema: Option<&dyn SchemaProvider>,
) -> bool {
    let first_segment = populated_from.split('.').next().unwrap_or(populated_from);
    let slot = source_schema.and_then(|sv| sv.induced_slot(first_segment, source_type).ok());
    // `slot.range in sv.all_classes()` is encoded directly by `RangeKind::Class`.
    if let Some(slot) = &slot {
        if slot.is_object_range() && (slot.inlined || slot.inlined_as_list) {
            return true;
        }
    }
    matches!(
        source_map.get(first_segment),
        Some(Value::Map(_)) | Some(Value::List(_))
    )
}

/// Walk a dot-path structurally through inlined nested objects (issue #247).
///
/// Mirrors Python `ObjectTransformer._resolve_inline_path`
/// (`object_transformer.py`). Descends into the nested map(s) one segment at a
/// time and returns the leaf value. A segment that is legitimately absent yields
/// `Null` rather than an error.
///
/// # Errors
/// Returns [`Error::InlinePath`] if a segment holds a list (multivalued inline
/// fan-out, tracked in #265) or a non-map value is encountered mid-path.
fn resolve_inline_path(
    populated_from: &str,
    source_type: &str,
    source_map: &IndexMap<String, Value>,
    source_schema: Option<&dyn SchemaProvider>,
    slot_deriv: &SlotDerivation,
) -> Result<Value> {
    let segments: Vec<&str> = populated_from.split('.').collect();
    // Start from the source object (a `Map`) rather than the raw `IndexMap` so
    // the loop can uniformly hold a `&Value`, mirroring Python's `current_val`.
    let mut current_val = Value::Map(source_map.clone());
    let mut current_class: Option<String> = Some(source_type.to_string());

    let inline_err = |message: String| Error::InlinePath {
        message,
        slot_derivation_name: slot_deriv.name.clone(),
        slot_populated_from: populated_from.to_string(),
    };

    let last = segments.len() - 1;
    for (i, segment) in segments.iter().enumerate() {
        let map = match &current_val {
            Value::Map(m) => m,
            other => {
                return Err(inline_err(format!(
                    "Cannot traverse inlined path '{populated_from}': segment '{segment}' \
                     expected a nested object but found {}",
                    value_type_name(other)
                )));
            }
        };
        let slot = current_class
            .as_deref()
            .and_then(|c| source_schema.and_then(|sv| sv.induced_slot(segment, c).ok()));
        let next = map.get(*segment).cloned().unwrap_or(Value::Null);
        if let Value::List(_) = next {
            return Err(inline_err(format!(
                "Inlined path '{populated_from}' reaches a multivalued segment '{segment}'; \
                 per-item fan-out to a matching class_derivation is not yet supported (see #265)"
            )));
        }
        current_val = next;
        if i == last {
            // Final segment — value is the leaf; slot is captured by the caller
            // via `induced_slot` on `populated_from` (as before), so we only
            // return the value here.
        } else if slot.as_ref().is_some_and(|s| s.is_object_range()) {
            current_class = slot.and_then(|s| s.range_class().map(str::to_string));
        } else {
            // PARITY: upstream `_resolve_inline_path` sets
            // `current_class = slot.range if slot else None` on the non-final,
            // non-class-range branch — a *scalar* range becomes the "class" for
            // the next hop (which will then fail `induced_slot` and yield None).
            // Reproduced verbatim rather than cleaned up so a malformed deep path
            // resolves identically to upstream.
            // (linkml-map object_transformer.py::_resolve_inline_path, commit b5fca196)
            current_class = slot.and_then(|s| s.range.name().map(str::to_string));
        }
        if current_val.is_null() {
            break;
        }
    }

    Ok(current_val)
}

/// Human-readable type name for the [`Error::InlinePath`] diagnostic, matching
/// the shape of Python's `type(current_val).__name__` (dict/list/str/int/...).
fn value_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Int(_) => "int",
        Value::Float(_) => "float",
        Value::Str(_) => "str",
        Value::List(_) => "list",
        Value::Map(_) => "dict",
    }
}

// ── Offset / aggregation / pivot helpers ──────────────────────────────────────

/// Apply an [`Offset`]: `value ± offset_value * source[offset_field]`.
/// Mirrors Python `_apply_offset` — a missing/non-numeric offset field, or a
/// non-numeric base value, leaves the value unchanged.
fn apply_offset(off: &Offset, value: Value, source_map: &IndexMap<String, Value>) -> Value {
    let off_field_val = match source_map
        .get(&off.offset_field)
        .and_then(Value::try_numeric)
    {
        Some(n) => n,
        None => return value,
    };
    let base = match value.try_numeric() {
        Some(n) => n,
        None => return value,
    };
    let delta = off.offset_value * off_field_val;
    let result = if off.offset_reverse.unwrap_or(false) {
        base - delta
    } else {
        base + delta
    };
    float_to_value(result)
}

/// Reduce a (multivalued) source value to an aggregate per [`AggregationType`].
///
/// LinkML's Python `ObjectTransformer` does not implement aggregation (it is a
/// datamodel-only construct there); these semantics are defined here. The source
/// is treated as a list (a scalar becomes a one-element list, null an empty one).
/// `Count`/`List`/`Array`/`Set` work on any element type; the arithmetic
/// operators coerce elements to numbers, honouring `invalid_value_handling`
/// (falling back to `null_handling`): `Ignore` skips, `TreatAsZero` substitutes
/// 0, `ErrorOut` raises.
fn apply_aggregation(agg: &AggregationOperation, value: Value, slot: &str) -> Result<Value> {
    let items: Vec<Value> = match value {
        Value::List(items) => items,
        Value::Null => vec![],
        other => vec![other],
    };

    let strategy = agg
        .invalid_value_handling
        .clone()
        .or_else(|| agg.null_handling.clone())
        .unwrap_or(InvalidValueHandlingStrategy::Ignore);

    let numbers = || -> Result<Vec<f64>> {
        let mut out = Vec::new();
        for it in &items {
            match it.try_numeric() {
                Some(n) => out.push(n),
                None => match strategy {
                    InvalidValueHandlingStrategy::Ignore => {}
                    InvalidValueHandlingStrategy::TreatAsZero => out.push(0.0),
                    InvalidValueHandlingStrategy::ErrorOut => {
                        return Err(Error::SlotTransform {
                            class: String::new(),
                            slot: slot.to_string(),
                            cause: "non-numeric value in aggregation".into(),
                        });
                    }
                },
            }
        }
        Ok(out)
    };

    use AggregationType as A;
    let v = match agg.operator {
        A::Count => Value::Int(items.len() as i64),
        A::List | A::Array => Value::List(items),
        A::Set => {
            let mut seen = std::collections::HashSet::new();
            let mut out = Vec::new();
            for it in items {
                if seen.insert(value_to_string_key(&it)) {
                    out.push(it);
                }
            }
            Value::List(out)
        }
        A::Sum => {
            let n = numbers()?;
            let all_int = items.iter().all(|v| matches!(v, Value::Int(_)));
            let sum: f64 = n.iter().sum();
            if all_int {
                Value::Int(sum as i64)
            } else {
                float_to_value(sum)
            }
        }
        A::Average => {
            let n = numbers()?;
            if n.is_empty() {
                Value::Null
            } else {
                Value::Float(n.iter().sum::<f64>() / n.len() as f64)
            }
        }
        A::Min | A::Max => {
            // Return the original element with the min/max numeric value.
            let mut best: Option<(f64, &Value)> = None;
            for it in &items {
                if let Some(n) = it.try_numeric() {
                    let take = match best {
                        None => true,
                        Some((b, _)) => {
                            if matches!(agg.operator, A::Min) {
                                n < b
                            } else {
                                n > b
                            }
                        }
                    };
                    if take {
                        best = Some((n, it));
                    }
                }
            }
            best.map(|(_, v)| v.clone()).unwrap_or(Value::Null)
        }
        A::Median => {
            let mut n = numbers()?;
            if n.is_empty() {
                Value::Null
            } else {
                n.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                let mid = n.len() / 2;
                if n.len() % 2 == 1 {
                    float_to_value(n[mid])
                } else {
                    Value::Float((n[mid - 1] + n[mid]) / 2.0)
                }
            }
        }
        A::Variance | A::StdDev => {
            let n = numbers()?;
            if n.is_empty() {
                Value::Null
            } else {
                let mean = n.iter().sum::<f64>() / n.len() as f64;
                let var = n.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n.len() as f64;
                Value::Float(if matches!(agg.operator, A::StdDev) {
                    var.sqrt()
                } else {
                    var
                })
            }
        }
        A::Mode => {
            // Most frequent element (first to reach the max count wins).
            let mut counts: IndexMap<String, (usize, Value)> = IndexMap::new();
            for it in items {
                let key = value_to_string_key(&it);
                counts.entry(key).or_insert((0, it)).0 += 1;
            }
            counts
                .into_iter()
                .max_by_key(|(_, (c, _))| *c)
                .map(|(_, (_, v))| v)
                .unwrap_or(Value::Null)
        }
        A::Custom => {
            return Err(Error::SlotTransform {
                class: String::new(),
                slot: slot.to_string(),
                cause: "CUSTOM aggregation is not supported".into(),
            });
        }
    };
    Ok(v)
}

/// Fill a `slot_name_template` (`{variable}` / `{unit}` placeholders).
fn fill_template(template: &str, variable: &str, unit: &str) -> String {
    template
        .replace("{variable}", variable)
        .replace("{unit}", unit)
}

/// Unmelt one EAV record `{variable: x, value: v[, unit: u]}` into `{x: v}`
/// (slot name from the template). Mirrors Python `_unmelt_single_record`.
fn unmelt_single_record(pivot: &PivotOperation, record: &IndexMap<String, Value>) -> Value {
    let mut result = IndexMap::new();
    for id in pivot.id_slots.iter().flatten() {
        if let Some(v) = record.get(id) {
            result.insert(id.clone(), v.clone());
        }
    }
    let variable = match record.get(&pivot.variable_slot) {
        Some(v) if !v.is_null() => value_to_string_key(v),
        _ => return Value::Map(result),
    };
    let value = record
        .get(&pivot.value_slot)
        .cloned()
        .unwrap_or(Value::Null);
    let unit = pivot
        .unit_slot
        .as_ref()
        .and_then(|u| record.get(u))
        .map(value_to_string_key)
        .unwrap_or_default();
    let target = fill_template(&pivot.slot_name_template, &variable, &unit);
    result.insert(target, value);
    Value::Map(result)
}

/// Unmelt a list of EAV records into one wide object. Mirrors `_unmelt_collection`.
fn unmelt_collection(pivot: &PivotOperation, records: &[Value]) -> Value {
    let mut result = IndexMap::new();
    for rec in records {
        let Value::Map(rec) = rec else { continue };
        let variable = match rec.get(&pivot.variable_slot) {
            Some(v) if !v.is_null() => value_to_string_key(v),
            _ => continue,
        };
        let value = rec.get(&pivot.value_slot).cloned().unwrap_or(Value::Null);
        let unit = pivot
            .unit_slot
            .as_ref()
            .and_then(|u| rec.get(u))
            .map(value_to_string_key)
            .unwrap_or_default();
        let target = fill_template(&pivot.slot_name_template, &variable, &unit);
        result.insert(target, value);
    }
    // Carry ID slots from the first record (assumed constant across records).
    if let Some(Value::Map(first)) = records.first() {
        for id in pivot.id_slots.iter().flatten() {
            if let Some(v) = first.get(id) {
                result.insert(id.clone(), v.clone());
            }
        }
    }
    Value::Map(result)
}

/// Serialise a [`Value`] to a JSON string matching Python `json.dumps` default
/// spacing (`", "` between items, `": "` in objects) — the form the upstream
/// `stringification: {syntax: JSON}` cases expect.
fn py_json(v: &Value) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        // serde_json gives correct JSON string escaping + quoting.
        Value::Str(s) => serde_json::to_string(s).unwrap_or_else(|_| format!("{s:?}")),
        Value::List(items) => {
            let inner: Vec<String> = items.iter().map(py_json).collect();
            format!("[{}]", inner.join(", "))
        }
        Value::Map(m) => {
            let inner: Vec<String> = m
                .iter()
                .map(|(k, val)| {
                    let key = serde_json::to_string(k).unwrap_or_else(|_| format!("{k:?}"));
                    format!("{key}: {}", py_json(val))
                })
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
    }
}

/// Serialise a [`Value`] to a YAML *flow* string (`[a, b]`), matching the
/// upstream `stringification: {syntax: YAML}` cases. Scalars are emitted bare.
fn yaml_flow(v: &Value) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Str(s) => s.clone(),
        Value::List(items) => {
            let inner: Vec<String> = items.iter().map(yaml_flow).collect();
            format!("[{}]", inner.join(", "))
        }
        Value::Map(m) => {
            let inner: Vec<String> = m
                .iter()
                .map(|(k, val)| format!("{k}: {}", yaml_flow(val)))
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
    }
}

/// Mirror of Python `Transformer._coerce_datatype`.
///
/// Recursively converts scalar values to the named target range type.
/// Unknown range names are passed through unchanged.
fn coerce_datatype(v: Value, target_range: &str) -> Value {
    match v {
        Value::List(items) => Value::List(
            items
                .into_iter()
                .map(|i| coerce_datatype(i, target_range))
                .collect(),
        ),
        Value::Map(m) => Value::Map(
            m.into_iter()
                .map(|(k, i)| (k, coerce_datatype(i, target_range)))
                .collect(),
        ),
        scalar => match target_range {
            "integer" | "int" => match &scalar {
                Value::Int(_) => scalar,
                Value::Float(f) => Value::Int(*f as i64),
                Value::Str(s) => s.trim().parse::<i64>().map(Value::Int).unwrap_or(scalar),
                Value::Bool(b) => Value::Int(if *b { 1 } else { 0 }),
                _ => scalar,
            },
            "float" | "double" => match &scalar {
                Value::Float(_) => scalar,
                Value::Int(i) => Value::Float(*i as f64),
                Value::Str(s) => s.trim().parse::<f64>().map(Value::Float).unwrap_or(scalar),
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
    // PARITY: the 3.14 test literals mirror upstream Python's numeric-coercion
    // fixtures; keep the exact value and silence approx_constant rather than diverge.
    #![allow(clippy::approx_constant)]
    use super::*;
    use crate::{
        datamodel::{
            ClassDerivation, EnumDerivation, KeyVal, PermissibleValueDerivation, SlotDerivation,
            TransformationSpecification,
        },
        schema::{
            ClassDef, EnumDef, InMemorySchema, InMemorySchemaBuilder, PermissibleValue, RangeKind,
            SlotDef,
        },
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
                    PermissibleValue {
                        text: "male".into(),
                        description: None,
                        meaning: None,
                    },
                    PermissibleValue {
                        text: "female".into(),
                        description: None,
                        meaning: None,
                    },
                    PermissibleValue {
                        text: "nonbinary man".into(),
                        description: None,
                        meaning: None,
                    },
                ],
            })
            .add_class(ClassDef {
                name: "Person".into(),
                tree_root: true,
                is_a: None,
                mixins: vec![],
            })
            .add_slot(
                "Person",
                SlotDef {
                    name: "id".into(),
                    range: RangeKind::Type("string".into()),
                    multivalued: false,
                    required: true,
                    identifier: true,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .add_slot(
                "Person",
                SlotDef {
                    name: "name".into(),
                    range: RangeKind::Type("string".into()),
                    multivalued: false,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .add_slot(
                "Person",
                SlotDef {
                    name: "age_in_years".into(),
                    range: RangeKind::Type("integer".into()),
                    multivalued: false,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .add_slot(
                "Person",
                SlotDef {
                    name: "gender".into(),
                    range: RangeKind::Enum("GenderType".into()),
                    multivalued: false,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .add_slot(
                "Person",
                SlotDef {
                    name: "aliases".into(),
                    range: RangeKind::Type("string".into()),
                    multivalued: true,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .add_class(ClassDef {
                name: "Address".into(),
                tree_root: false,
                is_a: None,
                mixins: vec![],
            })
            .add_slot(
                "Address",
                SlotDef {
                    name: "street".into(),
                    range: RangeKind::Type("string".into()),
                    multivalued: false,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .add_slot(
                "Address",
                SlotDef {
                    name: "city".into(),
                    range: RangeKind::Type("string".into()),
                    multivalued: false,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .add_slot(
                "Person",
                SlotDef {
                    name: "current_address".into(),
                    range: RangeKind::Class("Address".into()),
                    multivalued: false,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .add_slot(
                "Person",
                SlotDef {
                    name: "friends".into(),
                    range: RangeKind::Class("Person".into()),
                    multivalued: true,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .build()
    }

    /// Minimal spec: identity copy of `id` and `name`.
    fn make_identity_spec() -> TransformationSpecification {
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert(
            "id".into(),
            SlotDerivation {
                name: "id".into(),
                ..Default::default()
            },
        );
        slots.insert(
            "name".into(),
            SlotDerivation {
                name: "name".into(),
                ..Default::default()
            },
        );
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
        slots.insert(
            "id".into(),
            SlotDerivation {
                name: "id".into(),
                populated_from: Some("id".into()),
                ..Default::default()
            },
        );
        slots.insert(
            "label".into(),
            SlotDerivation {
                name: "label".into(),
                populated_from: Some("name".into()),
                ..Default::default()
            },
        );
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

        let m = match result {
            Value::Map(m) => m,
            _ => panic!("expected Map"),
        };
        assert_eq!(m["id"], Value::Str("P:001".into()));
        assert_eq!(m["label"], Value::Str("Alice".into()));
        assert!(!m.contains_key("name"), "source key should not appear");
    }

    // ── Test 2: expr-derived slot ─────────────────────────────────────────────

    #[test]
    fn test_expr_derived_slot() {
        let schema = make_person_schema();
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert(
            "age_str".into(),
            SlotDerivation {
                name: "age_str".into(),
                expr: Some("str(age_in_years) + \" years\"".into()),
                ..Default::default()
            },
        );
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
        let out = match result {
            Value::Map(m) => m,
            _ => panic!("expected Map"),
        };
        assert_eq!(out["age_str"], Value::Str("33 years".into()));
    }

    // ── Test 2b: cached-AST expr path matches the string path ─────────────────

    #[test]
    fn test_slot_function_references_previous_derived_slot() {
        let schema = make_person_schema();
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert(
            "_label".into(),
            SlotDerivation {
                name: "_label".into(),
                populated_from: Some("name".into()),
                hide: Some(true),
                ..Default::default()
            },
        );
        slots.insert(
            "display".into(),
            SlotDerivation {
                name: "display".into(),
                expr: Some("slot(\"_label\") + \"!\"".into()),
                ..Default::default()
            },
        );
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
        let result = engine
            .map_object(&src_person("P:001", "Alice"), Some("Person"))
            .unwrap();
        let out = match result {
            Value::Map(m) => m,
            _ => panic!("expected Map"),
        };
        assert_eq!(out["display"], Value::Str("Alice!".into()));
        assert!(!out.contains_key("_label"));
    }

    #[test]
    fn test_slot_function_missing_slot_is_null() {
        let schema = make_person_schema();
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert(
            "display".into(),
            SlotDerivation {
                name: "display".into(),
                expr: Some("slot(\"missing\")".into()),
                ..Default::default()
            },
        );
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
        let result = engine
            .map_object(&src_person("P:001", "Alice"), Some("Person"))
            .unwrap();
        let out = match result {
            Value::Map(m) => m,
            _ => panic!("expected Map"),
        };
        assert_eq!(out["display"], Value::Null);
    }

    #[test]
    fn test_expression_mappings_evaluate_mapped_expression() {
        let schema = make_person_schema();
        let mut mappings = IndexMap::new();
        mappings.insert(
            "P:001".into(),
            KeyVal {
                key: "P:001".into(),
                value: Some(serde_json::json!("name + \"!\"")),
            },
        );
        let mut slots = IndexMap::new();
        slots.insert(
            "display".into(),
            SlotDerivation {
                name: "display".into(),
                populated_from: Some("id".into()),
                expression_mappings: Some(mappings),
                ..Default::default()
            },
        );
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
        let result = engine
            .map_object(&src_person("P:001", "Alice"), Some("Person"))
            .unwrap();
        let out = match result {
            Value::Map(m) => m,
            _ => panic!("expected Map"),
        };
        assert_eq!(out["display"], Value::Str("Alice!".into()));
    }

    #[test]
    fn test_compiled_exprs_matches_string_path() {
        let schema = make_person_schema();
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert(
            "age_str".into(),
            SlotDerivation {
                name: "age_str".into(),
                expr: Some("str(age_in_years) + \" years\"".into()),
                ..Default::default()
            },
        );
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

            assert_eq!(
                cached_out, string_out,
                "cached/string mismatch at age={age}"
            );
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
        slots.insert(
            "source".into(),
            SlotDerivation {
                name: "source".into(),
                value: Some(serde_json::json!("database")),
                ..Default::default()
            },
        );
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
        let out = match result {
            Value::Map(m) => m,
            _ => panic!("expected Map"),
        };
        assert_eq!(out["source"], Value::Str("database".into()));
    }

    // ── Test 4: value_mappings ────────────────────────────────────────────────

    #[test]
    fn test_value_mappings() {
        let schema = make_person_schema();
        let mut vm: IndexMap<String, KeyVal> = IndexMap::new();
        vm.insert(
            "M".into(),
            KeyVal {
                key: "M".into(),
                value: Some(serde_json::json!("male")),
            },
        );
        vm.insert(
            "F".into(),
            KeyVal {
                key: "F".into(),
                value: Some(serde_json::json!("female")),
            },
        );
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert(
            "sex".into(),
            SlotDerivation {
                name: "sex".into(),
                populated_from: Some("gender".into()),
                value_mappings: Some(vm),
                ..Default::default()
            },
        );
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
        let out = match result {
            Value::Map(m) => m,
            _ => panic!("expected Map"),
        };
        assert_eq!(out["sex"], Value::Str("male".into()));
    }

    // ── Test 5: enum PV mapping ───────────────────────────────────────────────

    #[test]
    fn test_enum_pv_mapping() {
        let schema = make_person_schema();
        let mut pvds: IndexMap<String, PermissibleValueDerivation> = IndexMap::new();
        pvds.insert(
            "ACTIVE".into(),
            PermissibleValueDerivation {
                name: "ACTIVE".into(),
                populated_from: Some(vec!["active".into()]),
                ..Default::default()
            },
        );
        pvds.insert(
            "INACTIVE".into(),
            PermissibleValueDerivation {
                name: "INACTIVE".into(),
                populated_from: Some(vec!["inactive".into()]),
                ..Default::default()
            },
        );
        let mut enum_derivations: IndexMap<String, EnumDerivation> = IndexMap::new();
        enum_derivations.insert(
            "StatusType".into(),
            EnumDerivation {
                name: "StatusType".into(),
                populated_from: Some("GenderType".into()),
                permissible_value_derivations: Some(pvds),
                ..Default::default()
            },
        );
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert(
            "gender".into(),
            SlotDerivation {
                name: "gender".into(),
                populated_from: Some("gender".into()),
                ..Default::default()
            },
        );
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
        let out = match result {
            Value::Map(m) => m,
            _ => panic!("expected Map"),
        };
        // "active" → "ACTIVE" via PV derivation.
        assert_eq!(out["gender"], Value::Str("ACTIVE".into()));
    }

    // ── Test 6: scalar → list cardinality coercion ────────────────────────────

    #[test]
    fn test_scalar_to_list_coercion() {
        let schema = make_person_schema();
        let target_schema = InMemorySchemaBuilder::new()
            .add_class(ClassDef {
                name: "Agent".into(),
                tree_root: true,
                is_a: None,
                mixins: vec![],
            })
            .add_slot(
                "Agent",
                SlotDef {
                    name: "aliases".into(),
                    range: RangeKind::Type("string".into()),
                    multivalued: true,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .build();
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert(
            "aliases".into(),
            SlotDerivation {
                name: "aliases".into(),
                populated_from: Some("name".into()),
                ..Default::default()
            },
        );
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
        let out = match result {
            Value::Map(m) => m,
            _ => panic!("expected Map"),
        };
        assert_eq!(
            out["aliases"],
            Value::List(vec![Value::Str("Alice".into())])
        );
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
        slots.insert(
            "name".into(),
            SlotDerivation {
                name: "name".into(),
                populated_from: Some("aliases".into()),
                cast_collection_as: Some(CollectionType::SingleValued),
                ..Default::default()
            },
        );
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
        let out = match result {
            Value::Map(m) => m,
            _ => panic!("expected Map"),
        };
        assert_eq!(out["name"], Value::Str("Al".into()));
    }

    // ── Test 8: nested object recursion ──────────────────────────────────────

    #[test]
    fn test_nested_object_recursion() {
        let schema = make_person_schema();
        // Address passthrough.
        let mut addr_slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        addr_slots.insert(
            "street".into(),
            SlotDerivation {
                name: "street".into(),
                ..Default::default()
            },
        );
        addr_slots.insert(
            "city".into(),
            SlotDerivation {
                name: "city".into(),
                ..Default::default()
            },
        );
        // Person with current_address.
        let mut person_slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        person_slots.insert(
            "id".into(),
            SlotDerivation {
                name: "id".into(),
                ..Default::default()
            },
        );
        person_slots.insert(
            "current_address".into(),
            SlotDerivation {
                name: "current_address".into(),
                populated_from: Some("current_address".into()),
                ..Default::default()
            },
        );
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

        let result = engine
            .map_object(&Value::Map(person), Some("Person"))
            .unwrap();
        let out = match result {
            Value::Map(m) => m,
            _ => panic!("expected Map"),
        };
        let addr_out = match &out["current_address"] {
            Value::Map(m) => m,
            _ => panic!("expected nested Map"),
        };
        assert_eq!(addr_out["street"], Value::Str("1 Oak St".into()));
        assert_eq!(addr_out["city"], Value::Str("Oaktown".into()));
    }

    // ── Test 9: multivalued nested list recursion ─────────────────────────────

    #[test]
    fn test_multivalued_nested_list() {
        let schema = make_person_schema();
        let mut person_slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        person_slots.insert(
            "id".into(),
            SlotDerivation {
                name: "id".into(),
                ..Default::default()
            },
        );
        person_slots.insert(
            "friends".into(),
            SlotDerivation {
                name: "friends".into(),
                populated_from: Some("friends".into()),
                ..Default::default()
            },
        );
        // Friend class (same as Person for recursion).
        let mut friend_slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        friend_slots.insert(
            "id".into(),
            SlotDerivation {
                name: "id".into(),
                ..Default::default()
            },
        );
        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "Person".into(),
                populated_from: Some("Person".into()),
                slot_derivations: Some(person_slots),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, Some(&schema), None);

        let mut f1 = IndexMap::new();
        f1.insert("id".into(), Value::Str("P:002".into()));
        let mut f2 = IndexMap::new();
        f2.insert("id".into(), Value::Str("P:003".into()));

        let mut person = IndexMap::new();
        person.insert("id".into(), Value::Str("P:001".into()));
        person.insert(
            "friends".into(),
            Value::List(vec![Value::Map(f1), Value::Map(f2)]),
        );

        let result = engine
            .map_object(&Value::Map(person), Some("Person"))
            .unwrap();
        let out = match result {
            Value::Map(m) => m,
            _ => panic!("expected Map"),
        };
        if let Value::List(friends) = &out["friends"] {
            assert_eq!(friends.len(), 2);
            if let Value::Map(f) = &friends[0] {
                assert_eq!(f["id"], Value::Str("P:002".into()));
            } else {
                panic!("expected Map in list");
            }
        } else {
            panic!("expected List");
        }
    }

    // ── Test 10: implicit same-name copy (no populated_from) ─────────────────

    #[test]
    fn test_implicit_copy() {
        let schema = make_person_schema();
        let spec = make_identity_spec();
        let engine = ObjectTransformer::new(spec, Some(&schema), None);
        let src = src_person("P:042", "Bob");
        let result = engine.map_object(&src, None).unwrap(); // None → tree_root
        let out = match result {
            Value::Map(m) => m,
            _ => panic!("expected Map"),
        };
        assert_eq!(out["id"], Value::Str("P:042".into()));
        assert_eq!(out["name"], Value::Str("Bob".into()));
    }

    // ── Test 11: NULL expr sets slot to Null ──────────────────────────────────

    #[test]
    fn test_null_expr() {
        let schema = make_person_schema();
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert(
            "name".into(),
            SlotDerivation {
                name: "name".into(),
                expr: Some("NULL".into()),
                ..Default::default()
            },
        );
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
        let out = match result {
            Value::Map(m) => m,
            _ => panic!("expected Map"),
        };
        assert_eq!(out["name"], Value::Null);
    }

    // (v0.6.0) `sources` removed — the multi-source-single-wins behaviour no
    // longer exists; slots use `populated_from` (single) instead.

    // ── Test 13: enum mirror_source ───────────────────────────────────────────

    #[test]
    fn test_enum_mirror_source() {
        let schema = make_person_schema();
        let mut enum_derivations: IndexMap<String, EnumDerivation> = IndexMap::new();
        enum_derivations.insert(
            "GenderType".into(),
            EnumDerivation {
                name: "GenderType".into(),
                populated_from: Some("GenderType".into()),
                mirror_source: Some(true),
                ..Default::default()
            },
        );
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert(
            "gender".into(),
            SlotDerivation {
                name: "gender".into(),
                populated_from: Some("gender".into()),
                ..Default::default()
            },
        );
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
        let out = match result {
            Value::Map(m) => m,
            _ => panic!(),
        };
        assert_eq!(out["gender"], Value::Str("nonbinary man".into()));
    }

    // ── Test 14: coerce_datatype ──────────────────────────────────────────────

    #[test]
    fn test_coerce_datatype_integer() {
        assert_eq!(
            coerce_datatype(Value::Str("42".into()), "integer"),
            Value::Int(42)
        );
        assert_eq!(coerce_datatype(Value::Float(3.7), "integer"), Value::Int(3));
        assert_eq!(coerce_datatype(Value::Int(5), "integer"), Value::Int(5));
    }

    #[test]
    fn test_coerce_datatype_float() {
        assert_eq!(
            coerce_datatype(Value::Str("3.14".into()), "float"),
            Value::Float(3.14)
        );
        assert_eq!(coerce_datatype(Value::Int(2), "float"), Value::Float(2.0));
    }

    #[test]
    fn test_coerce_datatype_string() {
        assert_eq!(
            coerce_datatype(Value::Int(99), "string"),
            Value::Str("99".into())
        );
    }

    #[test]
    fn test_coerce_datatype_bool() {
        assert_eq!(
            coerce_datatype(Value::Str("true".into()), "boolean"),
            Value::Bool(true)
        );
        assert_eq!(
            coerce_datatype(Value::Int(0), "boolean"),
            Value::Bool(false)
        );
    }

    // ── Test 15: json_to_value round-trip ─────────────────────────────────────

    #[test]
    fn test_json_to_value() {
        assert_eq!(Value::from(&serde_json::json!(null)), Value::Null);
        assert_eq!(Value::from(&serde_json::json!(true)), Value::Bool(true));
        assert_eq!(Value::from(&serde_json::json!(42)), Value::Int(42));
        assert_eq!(Value::from(&serde_json::json!(3.14)), Value::Float(3.14));
        assert_eq!(
            Value::from(&serde_json::json!("hello")),
            Value::Str("hello".into())
        );
        assert_eq!(
            Value::from(&serde_json::json!([1, 2])),
            Value::List(vec![Value::Int(1), Value::Int(2)])
        );
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

    // (v0.6.0) class-level `sources` multi-source dispatch removed — cover
    // multiple source classes with one ClassDerivation each (per upstream).

    // ── Test: _source_class virtual binding (#193) ────────────────────────────

    #[test]
    fn test_source_class_binding() {
        // `populated_from: _source_class` returns the source class name, enabling
        // value_mappings keyed by source class.
        let schema = make_person_schema();
        let mut vm: IndexMap<String, KeyVal> = IndexMap::new();
        vm.insert(
            "PersonV1".into(),
            KeyVal {
                key: "PersonV1".into(),
                value: Some(serde_json::Value::String("visit_1".into())),
            },
        );
        vm.insert(
            "PersonV2".into(),
            KeyVal {
                key: "PersonV2".into(),
                value: Some(serde_json::Value::String("visit_2".into())),
            },
        );
        let visit_slot = || {
            let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
            slots.insert(
                "visit_label".into(),
                SlotDerivation {
                    name: "visit_label".into(),
                    populated_from: Some("_source_class".into()),
                    value_mappings: Some(vm.clone()),
                    ..Default::default()
                },
            );
            slots
        };
        // One ClassDerivation per source class (v0.6.0 multi-source pattern).
        let spec = TransformationSpecification {
            class_derivations: Some(vec![
                ClassDerivation {
                    name: "FromV1".into(),
                    populated_from: Some("PersonV1".into()),
                    slot_derivations: Some(visit_slot()),
                    ..Default::default()
                },
                ClassDerivation {
                    name: "FromV2".into(),
                    populated_from: Some("PersonV2".into()),
                    slot_derivations: Some(visit_slot()),
                    ..Default::default()
                },
            ]),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, Some(&schema), None);

        let obj = Value::Map(IndexMap::new());
        let r1 = engine.map_object(&obj, Some("PersonV1")).unwrap();
        let r2 = engine.map_object(&obj, Some("PersonV2")).unwrap();
        if let (Value::Map(m1), Value::Map(m2)) = (&r1, &r2) {
            assert_eq!(m1["visit_label"], Value::Str("visit_1".into()));
            assert_eq!(m2["visit_label"], Value::Str("visit_2".into()));
        } else {
            panic!("expected maps");
        }
    }

    // ── Test: _source_class in expr (#193) ───────────────────────────────────

    #[test]
    fn test_source_class_in_expr() {
        let schema = make_person_schema();
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert(
            "label".into(),
            SlotDerivation {
                name: "label".into(),
                expr: Some("{_source_class}".into()),
                ..Default::default()
            },
        );
        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "FromV1".into(),
                populated_from: Some("PersonV1".into()),
                slot_derivations: Some(slots),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, Some(&schema), None);
        let obj = Value::Map(IndexMap::new());
        let r = engine.map_object(&obj, Some("PersonV1")).unwrap();
        if let Value::Map(m) = r {
            assert_eq!(m["label"], Value::Str("PersonV1".into()));
        } else {
            panic!("expected map");
        }
    }

    // ── Test 17: is_a ancestor slot inheritance ───────────────────────────────

    #[test]
    fn test_is_a_ancestor_slots() {
        let schema = make_person_schema();
        // Entity has id slot. Agent is_a Entity and adds label.
        let mut entity_slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        entity_slots.insert(
            "id".into(),
            SlotDerivation {
                name: "id".into(),
                ..Default::default()
            },
        );
        let mut agent_slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        agent_slots.insert(
            "label".into(),
            SlotDerivation {
                name: "label".into(),
                populated_from: Some("name".into()),
                ..Default::default()
            },
        );
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
        let out = match result {
            Value::Map(m) => m,
            _ => panic!(),
        };
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
        slots.insert(
            "aliases".into(),
            SlotDerivation {
                name: "aliases".into(),
                populated_from: Some("name".into()),
                stringification: Some(crate::datamodel::StringificationConfiguration {
                    delimiter: Some("|".into()),
                    reversed: Some(true),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
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
        let out = match result {
            Value::Map(m) => m,
            _ => panic!(),
        };
        assert_eq!(
            out["aliases"],
            Value::List(vec![
                Value::Str("Alice".into()),
                Value::Str("Bob".into()),
                Value::Str("Carol".into()),
            ])
        );
    }

    // ── Test 19: list join to string (stringification delimiter) ─────────────

    #[test]
    fn test_list_join_to_string() {
        let schema = make_person_schema();
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert(
            "name".into(),
            SlotDerivation {
                name: "name".into(),
                populated_from: Some("aliases".into()),
                stringification: Some(crate::datamodel::StringificationConfiguration {
                    delimiter: Some(", ".into()),
                    reversed: Some(false),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
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
        m.insert(
            "aliases".into(),
            Value::List(vec![Value::Str("Alice".into()), Value::Str("Smith".into())]),
        );
        let result = engine.map_object(&Value::Map(m), Some("Person")).unwrap();
        let out = match result {
            Value::Map(m) => m,
            _ => panic!(),
        };
        assert_eq!(out["name"], Value::Str("Alice, Smith".into()));
    }

    // ── FK object-index / flattening tests ────────────────────────────────────

    /// Build the flattening source schema (normalized mappings model):
    ///   MappingSet (tree_root): mappings -> Mapping[], entities -> Entity[]
    ///   Mapping: subject -> Entity, object -> Entity, predicate
    ///   Entity: id (identifier), name
    fn make_mappings_norm_schema() -> InMemorySchema {
        InMemorySchemaBuilder::new()
            .add_type("string")
            .add_class(ClassDef {
                name: "MappingSet".into(),
                tree_root: true,
                is_a: None,
                mixins: vec![],
            })
            .add_slot(
                "MappingSet",
                SlotDef {
                    name: "mappings".into(),
                    range: RangeKind::Class("Mapping".into()),
                    multivalued: true,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .add_slot(
                "MappingSet",
                SlotDef {
                    name: "entities".into(),
                    range: RangeKind::Class("Entity".into()),
                    multivalued: true,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .add_class(ClassDef {
                name: "Mapping".into(),
                tree_root: false,
                is_a: None,
                mixins: vec![],
            })
            .add_slot(
                "Mapping",
                SlotDef {
                    name: "subject".into(),
                    range: RangeKind::Class("Entity".into()),
                    multivalued: false,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .add_slot(
                "Mapping",
                SlotDef {
                    name: "object".into(),
                    range: RangeKind::Class("Entity".into()),
                    multivalued: false,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .add_slot(
                "Mapping",
                SlotDef {
                    name: "predicate".into(),
                    range: RangeKind::Type("string".into()),
                    multivalued: false,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .add_class(ClassDef {
                name: "Entity".into(),
                tree_root: false,
                is_a: None,
                mixins: vec![],
            })
            .add_slot(
                "Entity",
                SlotDef {
                    name: "id".into(),
                    range: RangeKind::Type("string".into()),
                    multivalued: false,
                    required: true,
                    identifier: true,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .add_slot(
                "Entity",
                SlotDef {
                    name: "name".into(),
                    range: RangeKind::Type("string".into()),
                    multivalued: false,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .build()
    }

    /// Build the flattening input container: one mapping referencing two
    /// entities by id, plus an `entities` dict keyed by identifier.
    fn make_mappings_norm_input() -> Value {
        let mut x1 = IndexMap::new();
        x1.insert("name".into(), Value::Str("x1".into()));
        let mut y1 = IndexMap::new();
        y1.insert("name".into(), Value::Str("y1".into()));
        let mut entities = IndexMap::new();
        entities.insert("X:1".into(), Value::Map(x1));
        entities.insert("Y:1".into(), Value::Map(y1));

        let mut mapping = IndexMap::new();
        mapping.insert("subject".into(), Value::Str("X:1".into()));
        mapping.insert("object".into(), Value::Str("Y:1".into()));
        mapping.insert("predicate".into(), Value::Str("P:1".into()));

        let mut root = IndexMap::new();
        root.insert("mappings".into(), Value::List(vec![Value::Map(mapping)]));
        root.insert("entities".into(), Value::Map(entities));
        Value::Map(root)
    }

    #[test]
    fn object_index_keys_dict_inlined_entities_by_id() {
        let schema = make_mappings_norm_schema();
        let input = make_mappings_norm_input();
        let idx = ObjectIndex::build(&input, Some("MappingSet"), Some(&schema));
        assert!(!idx.is_empty());
        assert!(idx.contains_id("X:1"));
        assert!(idx.contains_id("Y:1"));
        // The resolved object carries its identifier slot back as a field.
        let x = idx.get(Some("Entity"), "X:1").unwrap();
        match x {
            Value::Map(m) => {
                assert_eq!(m["id"], Value::Str("X:1".into()));
                assert_eq!(m["name"], Value::Str("x1".into()));
            }
            _ => panic!("expected map"),
        }
        // Flat fallback also resolves without the class hint.
        assert!(idx.get(None, "Y:1").is_some());
    }

    #[test]
    fn fk_expr_resolves_subject_name_and_id() {
        // subject.id / subject.name / object.id / object.name flatten the FK.
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        for (slot, expr) in [
            ("subject_id", "subject.id"),
            ("subject_name", "subject.name"),
            ("object_id", "object.id"),
            ("object_name", "object.name"),
        ] {
            slots.insert(
                slot.into(),
                SlotDerivation {
                    name: slot.into(),
                    expr: Some(expr.into()),
                    ..Default::default()
                },
            );
        }
        // predicate_id is a plain copy (populated_from), not an FK.
        slots.insert(
            "predicate_id".into(),
            SlotDerivation {
                name: "predicate_id".into(),
                populated_from: Some("predicate".into()),
                ..Default::default()
            },
        );

        let mappings_slot = {
            let mut m: IndexMap<String, SlotDerivation> = IndexMap::new();
            m.insert(
                "mappings".into(),
                SlotDerivation {
                    name: "mappings".into(),
                    populated_from: Some("mappings".into()),
                    ..Default::default()
                },
            );
            m
        };

        let spec = TransformationSpecification {
            class_derivations: Some(vec![
                ClassDerivation {
                    name: "MappingSet".into(),
                    populated_from: Some("MappingSet".into()),
                    slot_derivations: Some(mappings_slot),
                    ..Default::default()
                },
                ClassDerivation {
                    name: "Mapping".into(),
                    populated_from: Some("Mapping".into()),
                    slot_derivations: Some(slots),
                    ..Default::default()
                },
            ]),
            ..Default::default()
        };

        let schema = make_mappings_norm_schema();
        let input = make_mappings_norm_input();
        let engine = ObjectTransformer::new(spec, Some(&schema), None);

        // map_container builds the FK index and resolves the references.
        let out = engine.map_container(&input, Some("MappingSet")).unwrap();
        let mappings = match &out {
            Value::Map(m) => match &m["mappings"] {
                Value::List(items) => items.clone(),
                _ => panic!("mappings not a list"),
            },
            _ => panic!("not a map"),
        };
        assert_eq!(mappings.len(), 1);
        let row = match &mappings[0] {
            Value::Map(m) => m,
            _ => panic!(),
        };
        assert_eq!(row["subject_id"], Value::Str("X:1".into()));
        assert_eq!(row["subject_name"], Value::Str("x1".into()));
        assert_eq!(row["object_id"], Value::Str("Y:1".into()));
        assert_eq!(row["object_name"], Value::Str("y1".into()));
        assert_eq!(row["predicate_id"], Value::Str("P:1".into()));
    }

    #[test]
    fn non_fk_spec_unaffected_by_index_path() {
        // A spec with no FK access produces the same result via map_container as
        // via map_object — the index is built but never consulted.
        let schema = make_person_schema();
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert(
            "name".into(),
            SlotDerivation {
                name: "name".into(),
                populated_from: Some("name".into()),
                ..Default::default()
            },
        );
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
        m.insert("id".into(), Value::Str("P1".into()));
        m.insert("name".into(), Value::Str("Alice".into()));
        let input = Value::Map(m);
        let via_object = engine.map_object(&input, Some("Person")).unwrap();
        let via_container = engine.map_container(&input, Some("Person")).unwrap();
        assert_eq!(via_object, via_container);
        let out = match via_container {
            Value::Map(m) => m,
            _ => panic!(),
        };
        assert_eq!(out["name"], Value::Str("Alice".into()));
    }

    // ── unit_conversion tests ────────────────────────────────────────

    /// Source schema with a height slot whose schema unit annotation is `in`.
    fn make_measure_schema() -> InMemorySchema {
        InMemorySchemaBuilder::new()
            .add_type("float")
            .add_type("string")
            .add_class(ClassDef {
                name: "Obs".into(),
                tree_root: true,
                is_a: None,
                mixins: vec![],
            })
            .add_slot(
                "Obs",
                SlotDef {
                    name: "height_in".into(),
                    range: RangeKind::Type("float".into()),
                    multivalued: false,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: Some(crate::schema::UnitRef {
                        code: "in".into(),
                        system: crate::schema::UnitSystem::Ucum,
                    }),
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .add_slot(
                "Obs",
                SlotDef {
                    name: "glucose".into(),
                    range: RangeKind::Type("float".into()),
                    multivalued: false,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .build()
    }

    /// Convert height (schema unit `in`) to cm via target_unit only.
    #[test]
    fn unit_conversion_schema_unit_to_cm() {
        let schema = make_measure_schema();
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert(
            "height_cm".into(),
            SlotDerivation {
                name: "height_cm".into(),
                populated_from: Some("height_in".into()),
                unit_conversion: Some(crate::datamodel::UnitConversionConfiguration {
                    target_unit: Some("cm".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "ObsOut".into(),
                populated_from: Some("Obs".into()),
                slot_derivations: Some(slots),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, Some(&schema), None);
        let mut m = IndexMap::new();
        m.insert("height_in".into(), Value::Float(10.0));
        let out = match engine.map_object(&Value::Map(m), Some("Obs")).unwrap() {
            Value::Map(m) => m,
            _ => panic!(),
        };
        // 10 in = 25.4 cm
        match out["height_cm"] {
            Value::Float(f) => assert!((f - 25.4).abs() < 1e-6, "got {f}"),
            ref other => panic!("expected Float, got {other:?}"),
        }
    }

    /// Convert using spec source_unit + target_unit (no schema unit).
    #[test]
    fn unit_conversion_spec_units_mg_to_g() {
        let schema = make_measure_schema();
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert(
            "dose_g".into(),
            SlotDerivation {
                name: "dose_g".into(),
                populated_from: Some("glucose".into()),
                unit_conversion: Some(crate::datamodel::UnitConversionConfiguration {
                    source_unit: Some("mg".into()),
                    target_unit: Some("g".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "ObsOut".into(),
                populated_from: Some("Obs".into()),
                slot_derivations: Some(slots),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, Some(&schema), None);
        let mut m = IndexMap::new();
        m.insert("glucose".into(), Value::Float(2500.0));
        let out = match engine.map_object(&Value::Map(m), Some("Obs")).unwrap() {
            Value::Map(m) => m,
            _ => panic!(),
        };
        match out["dose_g"] {
            Value::Float(f) => assert!((f - 2.5).abs() < 1e-9, "got {f}"),
            ref other => panic!("expected Float, got {other:?}"),
        }
    }

    /// Incompatible / unknown units now raise (parity with Python
    /// `DimensionalityError`), rather than silently passing the value through.
    #[test]
    fn unit_conversion_incompatible_raises() {
        let schema = make_measure_schema();
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert(
            "out".into(),
            SlotDerivation {
                name: "out".into(),
                populated_from: Some("glucose".into()),
                unit_conversion: Some(crate::datamodel::UnitConversionConfiguration {
                    source_unit: Some("mmol/L".into()),
                    target_unit: Some("mg/dL".into()), // needs molecular weight
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "ObsOut".into(),
                populated_from: Some("Obs".into()),
                slot_derivations: Some(slots),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, Some(&schema), None);
        let mut m = IndexMap::new();
        m.insert("glucose".into(), Value::Float(5.0));
        // mmol/L → mg/dL is a dimensionality mismatch (molar↔mass) → error.
        // (The slot-level UnitConversion error is wrapped by the outer handler.)
        let err = engine.map_object(&Value::Map(m), Some("Obs")).unwrap_err();
        assert!(
            err.to_string().contains("incompatible dimensions"),
            "expected a dimensionality error, got {err:?}"
        );
    }

    /// Structured {value, unit} input with target_magnitude_slot output.
    #[test]
    fn unit_conversion_structured_value_and_output_map() {
        // No schema (units come entirely from the structured value + spec).
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert(
            "height".into(),
            SlotDerivation {
                name: "height".into(),
                populated_from: Some("height".into()),
                unit_conversion: Some(crate::datamodel::UnitConversionConfiguration {
                    target_unit: Some("m".into()),
                    source_unit_slot: Some("unit".into()),
                    source_magnitude_slot: Some("value".into()),
                    target_magnitude_slot: Some("value".into()),
                    target_unit_slot: Some("unit".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "Out".into(),
                populated_from: Some("Rec".into()),
                slot_derivations: Some(slots),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, None, None);
        let mut inner = IndexMap::new();
        inner.insert("value".into(), Value::Float(150.0));
        inner.insert("unit".into(), Value::Str("cm".into()));
        let mut m = IndexMap::new();
        m.insert("height".into(), Value::Map(inner));
        let out = match engine.map_object(&Value::Map(m), Some("Rec")).unwrap() {
            Value::Map(m) => m,
            _ => panic!(),
        };
        let hm = match &out["height"] {
            Value::Map(m) => m,
            other => panic!("{other:?}"),
        };
        match hm["value"] {
            Value::Float(f) => assert!((f - 1.5).abs() < 1e-9, "got {f}"),
            ref other => panic!("expected Float, got {other:?}"),
        }
        assert_eq!(hm["unit"], Value::Str("m".into()));
    }

    /// Non-numeric magnitude with none_if_non_numeric returns Null.
    #[test]
    fn unit_conversion_non_numeric_none() {
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert(
            "out".into(),
            SlotDerivation {
                name: "out".into(),
                populated_from: Some("v".into()),
                unit_conversion: Some(crate::datamodel::UnitConversionConfiguration {
                    source_unit: Some("cm".into()),
                    target_unit: Some("m".into()),
                    none_if_non_numeric: Some(true),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "Out".into(),
                populated_from: Some("Rec".into()),
                slot_derivations: Some(slots),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, None, None);
        let mut m = IndexMap::new();
        m.insert("v".into(), Value::Str("not-a-number".into()));
        let out = match engine.map_object(&Value::Map(m), Some("Rec")).unwrap() {
            Value::Map(m) => m,
            _ => panic!(),
        };
        assert_eq!(out["out"], Value::Null);
    }

    // ── reshape_collection tests ──────────────────────────────────────────────

    /// Helper: build a two-class schema with identifier slots used by reshape tests.
    fn build_reshape_schema() -> InMemorySchema {
        use crate::schema::{ClassDef, InMemorySchemaBuilder, RangeKind, SlotDef};
        InMemorySchemaBuilder::new()
            .add_type("string")
            // "Item" class with identifier slot "id"
            .add_class(ClassDef {
                name: "Item".into(),
                tree_root: false,
                is_a: None,
                mixins: vec![],
            })
            .add_slot(
                "Item",
                SlotDef {
                    name: "id".into(),
                    range: RangeKind::Type("string".into()),
                    multivalued: false,
                    required: true,
                    identifier: true,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .add_slot(
                "Item",
                SlotDef {
                    name: "value".into(),
                    range: RangeKind::Type("string".into()),
                    multivalued: false,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            // "Container" class with a multivalued "items" slot (range: Item)
            .add_class(ClassDef {
                name: "Container".into(),
                tree_root: true,
                is_a: None,
                mixins: vec![],
            })
            .add_slot(
                "Container",
                SlotDef {
                    name: "items".into(),
                    range: RangeKind::Class("Item".into()),
                    multivalued: true,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .build()
    }

    /// List of objects → keyed dict via explicit `dictionary_key`.
    ///
    /// Python equivalent:
    ///   `v = {v1["id"]: v1 for v1 in v}; for v1 in v.values(): del v1["id"]`
    #[test]
    fn reshape_list_to_dict_via_dictionary_key() {
        let schema = build_reshape_schema();

        // SlotDerivation for "items" with dictionary_key = "id"
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert(
            "items".into(),
            SlotDerivation {
                name: "items".into(),
                populated_from: Some("items".into()),
                dictionary_key: Some("id".into()),
                ..Default::default()
            },
        );
        // Identity class derivation for "Item" so map_value_by_range can recurse.
        let mut item_slot_derivs: IndexMap<String, SlotDerivation> = IndexMap::new();
        item_slot_derivs.insert(
            "id".into(),
            SlotDerivation {
                name: "id".into(),
                ..Default::default()
            },
        );
        item_slot_derivs.insert(
            "value".into(),
            SlotDerivation {
                name: "value".into(),
                ..Default::default()
            },
        );
        let spec = TransformationSpecification {
            class_derivations: Some(vec![
                ClassDerivation {
                    name: "Container".into(),
                    populated_from: Some("Container".into()),
                    slot_derivations: Some(slots),
                    ..Default::default()
                },
                ClassDerivation {
                    name: "Item".into(),
                    populated_from: Some("Item".into()),
                    slot_derivations: Some(item_slot_derivs),
                    ..Default::default()
                },
            ]),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, Some(&schema), None);

        // Source: [{id: "a", value: "alpha"}, {id: "b", value: "beta"}]
        let mut item_a = IndexMap::new();
        item_a.insert("id".into(), Value::Str("a".into()));
        item_a.insert("value".into(), Value::Str("alpha".into()));
        let mut item_b = IndexMap::new();
        item_b.insert("id".into(), Value::Str("b".into()));
        item_b.insert("value".into(), Value::Str("beta".into()));
        let mut src = IndexMap::new();
        src.insert(
            "items".into(),
            Value::List(vec![Value::Map(item_a), Value::Map(item_b)]),
        );

        let result = engine
            .map_object(&Value::Map(src), Some("Container"))
            .unwrap();
        let out = match result {
            Value::Map(m) => m,
            other => panic!("expected Map, got {other:?}"),
        };

        // Result should be a dict keyed by "id", with "id" dropped from values.
        let items_val = &out["items"];
        let dict = match items_val {
            Value::Map(m) => m,
            other => panic!("expected Map (keyed dict), got {other:?}"),
        };
        assert_eq!(dict.len(), 2);
        let val_a = match dict.get("a") {
            Some(Value::Map(m)) => m,
            other => panic!("expected Map for key 'a', got {other:?}"),
        };
        // "id" key must have been dropped from the value
        assert!(
            !val_a.contains_key("id"),
            "key 'id' should be dropped from value"
        );
        assert_eq!(val_a["value"], Value::Str("alpha".into()));

        let val_b = match dict.get("b") {
            Some(Value::Map(m)) => m,
            other => panic!("expected Map for key 'b', got {other:?}"),
        };
        assert!(
            !val_b.contains_key("id"),
            "key 'id' should be dropped from value"
        );
        assert_eq!(val_b["value"], Value::Str("beta".into()));
    }

    /// Keyed dict → list round-trip via `cast_collection_as: MultiValuedList`.
    ///
    /// Python equivalent:
    ///   `[{**v1, id_slot: k} for k, v1 in v.items()]`
    ///   where id_slot = schema identifier slot name ("id").
    #[test]
    fn reshape_dict_to_list_via_cast_collection_as() {
        let schema = build_reshape_schema();

        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert(
            "items".into(),
            SlotDerivation {
                name: "items".into(),
                populated_from: Some("items".into()),
                cast_collection_as: Some(CollectionType::MultiValuedList),
                ..Default::default()
            },
        );
        // Identity class derivation for "Item" so map_value_by_range can recurse.
        let mut item_slot_derivs2: IndexMap<String, SlotDerivation> = IndexMap::new();
        item_slot_derivs2.insert(
            "id".into(),
            SlotDerivation {
                name: "id".into(),
                ..Default::default()
            },
        );
        item_slot_derivs2.insert(
            "value".into(),
            SlotDerivation {
                name: "value".into(),
                ..Default::default()
            },
        );
        let spec = TransformationSpecification {
            class_derivations: Some(vec![
                ClassDerivation {
                    name: "Container".into(),
                    populated_from: Some("Container".into()),
                    slot_derivations: Some(slots),
                    ..Default::default()
                },
                ClassDerivation {
                    name: "Item".into(),
                    populated_from: Some("Item".into()),
                    slot_derivations: Some(item_slot_derivs2),
                    ..Default::default()
                },
            ]),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, Some(&schema), None);

        // Source: {a: {value: "alpha"}, b: {value: "beta"}}
        let mut val_a = IndexMap::new();
        val_a.insert("value".into(), Value::Str("alpha".into()));
        let mut val_b = IndexMap::new();
        val_b.insert("value".into(), Value::Str("beta".into()));
        let mut items_dict = IndexMap::new();
        items_dict.insert("a".into(), Value::Map(val_a));
        items_dict.insert("b".into(), Value::Map(val_b));
        let mut src = IndexMap::new();
        src.insert("items".into(), Value::Map(items_dict));

        let result = engine
            .map_object(&Value::Map(src), Some("Container"))
            .unwrap();
        let out = match result {
            Value::Map(m) => m,
            other => panic!("expected Map, got {other:?}"),
        };

        let items_val = &out["items"];
        let list = match items_val {
            Value::List(v) => v,
            other => panic!("expected List, got {other:?}"),
        };
        assert_eq!(list.len(), 2);

        // Each element should be a map with the key re-injected as "id".
        for item in list {
            let m = match item {
                Value::Map(m) => m,
                other => panic!("expected Map element, got {other:?}"),
            };
            assert!(m.contains_key("id"), "re-injected 'id' key must be present");
            assert!(m.contains_key("value"));
        }
        // Check specific entries (insertion order preserved via IndexMap).
        let m0 = match &list[0] {
            Value::Map(m) => m,
            _ => panic!(),
        };
        assert_eq!(m0["id"], Value::Str("a".into()));
        assert_eq!(m0["value"], Value::Str("alpha".into()));
        let m1 = match &list[1] {
            Value::Map(m) => m,
            _ => panic!(),
        };
        assert_eq!(m1["id"], Value::Str("b".into()));
        assert_eq!(m1["value"], Value::Str("beta".into()));
    }

    // ── object_derivations tests ──────────────────────────────────────────────

    /// Single nested object produced by `object_derivations`.
    ///
    /// Source class has flat fields; the transform produces a nested target
    /// class by picking fields via an explicit ClassDerivation inside
    /// `object_derivations`.
    #[test]
    fn object_derivations_single_nested() {
        use crate::schema::{ClassDef, InMemorySchemaBuilder, RangeKind, SlotDef};

        // Source: flat Person {first: "Ada", last: "Lovelace", age: 36}
        // Target: Person { full_name: <nested FullName>, age: 36 }
        // FullName { first: "Ada", last: "Lovelace" }

        // Build a target schema that says `full_name` is single-valued.
        let target_schema = InMemorySchemaBuilder::new()
            .add_type("string")
            .add_type("integer")
            .add_class(ClassDef {
                name: "Person".into(),
                tree_root: true,
                is_a: None,
                mixins: vec![],
            })
            .add_slot(
                "Person",
                SlotDef {
                    name: "full_name".into(),
                    range: RangeKind::Class("FullName".into()),
                    multivalued: false,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .add_slot(
                "Person",
                SlotDef {
                    name: "age".into(),
                    range: RangeKind::Type("integer".into()),
                    multivalued: false,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .add_class(ClassDef {
                name: "FullName".into(),
                tree_root: false,
                is_a: None,
                mixins: vec![],
            })
            .add_slot(
                "FullName",
                SlotDef {
                    name: "first".into(),
                    range: RangeKind::Type("string".into()),
                    multivalued: false,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .add_slot(
                "FullName",
                SlotDef {
                    name: "last".into(),
                    range: RangeKind::Type("string".into()),
                    multivalued: false,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .build();

        // Build the nested ClassDerivation for FullName.
        let mut fullname_slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        fullname_slots.insert(
            "first".into(),
            SlotDerivation {
                name: "first".into(),
                populated_from: Some("first".into()),
                ..Default::default()
            },
        );
        fullname_slots.insert(
            "last".into(),
            SlotDerivation {
                name: "last".into(),
                populated_from: Some("last".into()),
                ..Default::default()
            },
        );
        let fullname_cls_deriv = ClassDerivation {
            name: "FullName".into(),
            populated_from: Some("Person".into()),
            slot_derivations: Some(fullname_slots),
            ..Default::default()
        };
        let mut inner_cls_derivs: IndexMap<String, ClassDerivation> = IndexMap::new();
        inner_cls_derivs.insert("FullName".into(), fullname_cls_deriv);

        // Outer spec: Person → Person with full_name via slot-level
        // class_derivations (v0.6.0), age direct.
        let mut outer_slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        outer_slots.insert(
            "full_name".into(),
            SlotDerivation {
                name: "full_name".into(),
                class_derivations: Some(inner_cls_derivs),
                ..Default::default()
            },
        );
        outer_slots.insert(
            "age".into(),
            SlotDerivation {
                name: "age".into(),
                populated_from: Some("age".into()),
                ..Default::default()
            },
        );

        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "Person".into(),
                populated_from: Some("Person".into()),
                slot_derivations: Some(outer_slots),
                ..Default::default()
            }]),
            ..Default::default()
        };

        let engine = ObjectTransformer::new(spec, None, Some(&target_schema));

        let mut src = IndexMap::new();
        src.insert("first".into(), Value::Str("Ada".into()));
        src.insert("last".into(), Value::Str("Lovelace".into()));
        src.insert("age".into(), Value::Int(36));

        let result = engine.map_object(&Value::Map(src), Some("Person")).unwrap();
        let out = match result {
            Value::Map(m) => m,
            other => panic!("expected Map, got {other:?}"),
        };

        // age passes through directly
        assert_eq!(out["age"], Value::Int(36));

        // full_name should be a single Map (not a List), because target schema says single-valued
        let fn_val = match &out["full_name"] {
            Value::Map(m) => m,
            other => panic!("expected Map for full_name, got {other:?}"),
        };
        assert_eq!(fn_val["first"], Value::Str("Ada".into()));
        assert_eq!(fn_val["last"], Value::Str("Lovelace".into()));
    }

    /// Multivalued `object_derivations`: target slot is multivalued, result is List.
    #[test]
    fn object_derivations_multivalued_from_target_schema() {
        use crate::schema::{ClassDef, InMemorySchemaBuilder, RangeKind, SlotDef};

        // Target schema: Container.tags is multivalued (List of Tag).
        let target_schema = InMemorySchemaBuilder::new()
            .add_type("string")
            .add_class(ClassDef {
                name: "Container".into(),
                tree_root: true,
                is_a: None,
                mixins: vec![],
            })
            .add_slot(
                "Container",
                SlotDef {
                    name: "tags".into(),
                    range: RangeKind::Class("Tag".into()),
                    multivalued: true, // <-- key: forces List output
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .add_class(ClassDef {
                name: "Tag".into(),
                tree_root: false,
                is_a: None,
                mixins: vec![],
            })
            .add_slot(
                "Tag",
                SlotDef {
                    name: "label".into(),
                    range: RangeKind::Type("string".into()),
                    multivalued: false,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .build();

        // ObjectDerivation with a single ClassDerivation for Tag.
        let mut tag_slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        tag_slots.insert(
            "label".into(),
            SlotDerivation {
                name: "label".into(),
                populated_from: Some("tag".into()),
                ..Default::default()
            },
        );
        let tag_cls_deriv = ClassDerivation {
            name: "Tag".into(),
            populated_from: Some("Container".into()),
            slot_derivations: Some(tag_slots),
            ..Default::default()
        };
        let mut inner: IndexMap<String, ClassDerivation> = IndexMap::new();
        inner.insert("Tag".into(), tag_cls_deriv);

        let mut outer_slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        outer_slots.insert(
            "tags".into(),
            SlotDerivation {
                name: "tags".into(),
                class_derivations: Some(inner),
                ..Default::default()
            },
        );

        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "Container".into(),
                populated_from: Some("Container".into()),
                slot_derivations: Some(outer_slots),
                ..Default::default()
            }]),
            ..Default::default()
        };

        let engine = ObjectTransformer::new(spec, None, Some(&target_schema));

        let mut src = IndexMap::new();
        src.insert("tag".into(), Value::Str("rust".into()));

        let result = engine
            .map_object(&Value::Map(src), Some("Container"))
            .unwrap();
        let out = match result {
            Value::Map(m) => m,
            other => panic!("expected Map, got {other:?}"),
        };

        // tags must be a List because target slot is multivalued
        let tags = match &out["tags"] {
            Value::List(v) => v,
            other => panic!("expected List for tags (multivalued target), got {other:?}"),
        };
        assert_eq!(tags.len(), 1, "one ObjectDerivation → one element");
        let tag0 = match &tags[0] {
            Value::Map(m) => m,
            other => panic!("expected Map in tags list, got {other:?}"),
        };
        assert_eq!(tag0["label"], Value::Str("rust".into()));
    }

    /// #269: a sentinel source value maps to null; real values pass through.
    #[test]
    fn missing_values_maps_sentinel_to_null() {
        let schema = make_person_schema();
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert(
            "name".into(),
            SlotDerivation {
                name: "name".into(),
                populated_from: Some("name".into()),
                missing_values: Some(vec![serde_json::json!("NA"), serde_json::json!(-9)]),
                ..Default::default()
            },
        );
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

        let mut sentinel = IndexMap::new();
        sentinel.insert("name".into(), Value::Str("NA".into()));
        let r = engine
            .map_object(&Value::Map(sentinel), Some("Person"))
            .unwrap();
        match r {
            Value::Map(o) => assert_eq!(o["name"], Value::Null, "sentinel -> null"),
            _ => panic!("expected map"),
        }

        let mut real = IndexMap::new();
        real.insert("name".into(), Value::Str("Bob".into()));
        let r2 = engine
            .map_object(&Value::Map(real), Some("Person"))
            .unwrap();
        match r2 {
            Value::Map(o) => assert_eq!(o["name"], Value::Str("Bob".into())),
            _ => panic!("expected map"),
        }
    }

    /// #217 (a19eb095): a same-table nested object whose inner slot is null is
    /// RETAINED with the key present (value `{first: null}`), and a scalar null
    /// slot also stays present. Upstream `_derive_nested_objects` only collapses
    /// a nested slot to null/[] on a genuine cross-table *join* miss; a
    /// same-table hollow object is kept. (This corrects the earlier local #266
    /// behaviour that omitted any hollow nested object.)
    #[test]
    fn same_table_hollow_nested_object_is_retained() {
        let schema = make_person_schema();
        let mut inner_slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        inner_slots.insert(
            "first".into(),
            SlotDerivation {
                name: "first".into(),
                populated_from: Some("first".into()),
                ..Default::default()
            },
        );
        let nested = ClassDerivation {
            name: "FullName".into(),
            populated_from: Some("Person".into()),
            slot_derivations: Some(inner_slots),
            ..Default::default()
        };
        let mut inner: IndexMap<String, ClassDerivation> = IndexMap::new();
        inner.insert("FullName".into(), nested);

        let mut outer: IndexMap<String, SlotDerivation> = IndexMap::new();
        outer.insert(
            "full_name".into(),
            SlotDerivation {
                name: "full_name".into(),
                class_derivations: Some(inner),
                ..Default::default()
            },
        );
        // A scalar slot that resolves to null must stay present (null != absent).
        outer.insert(
            "note".into(),
            SlotDerivation {
                name: "note".into(),
                populated_from: Some("missing_note".into()),
                ..Default::default()
            },
        );
        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "Person".into(),
                populated_from: Some("Person".into()),
                slot_derivations: Some(outer),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, Some(&schema), None);

        // No `first` in source, same-table nested (no join miss) → nested object
        // is retained as `{first: null}`; scalar null slot also stays present.
        let r = engine
            .map_object(&Value::Map(IndexMap::new()), Some("Person"))
            .unwrap();
        match &r {
            Value::Map(o) => {
                assert!(
                    o.contains_key("full_name"),
                    "same-table hollow nested object retained"
                );
                match &o["full_name"] {
                    Value::Map(inner) => assert_eq!(
                        inner["first"],
                        Value::Null,
                        "inner null slot stays present"
                    ),
                    other => panic!("expected Map for full_name, got {other:?}"),
                }
                assert_eq!(o["note"], Value::Null, "scalar null stays present");
            }
            _ => panic!("expected map"),
        }

        // With `first` present → full_name is a real object.
        let mut m = IndexMap::new();
        m.insert("first".into(), Value::Str("Ada".into()));
        let r2 = engine.map_object(&Value::Map(m), Some("Person")).unwrap();
        match &r2 {
            Value::Map(o) => assert!(o.contains_key("full_name"), "populated nested object kept"),
            _ => panic!("expected map"),
        }
    }

    /// #232: strict mode errors on an unbound name in an expr; lax yields null.
    #[test]
    fn strict_exprs_error_on_unbound_name() {
        let schema = make_person_schema();
        let build_spec = || {
            let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
            slots.insert(
                "label".into(),
                SlotDerivation {
                    name: "label".into(),
                    expr: Some("{nonexistent_slot}".into()),
                    ..Default::default()
                },
            );
            TransformationSpecification {
                class_derivations: Some(vec![ClassDerivation {
                    name: "Person".into(),
                    populated_from: Some("Person".into()),
                    slot_derivations: Some(slots),
                    ..Default::default()
                }]),
                ..Default::default()
            }
        };
        let obj = Value::Map(IndexMap::new());

        // Lax (default): unbound name → null, no error.
        let lax = ObjectTransformer::new(build_spec(), Some(&schema), None);
        match lax.map_object(&obj, Some("Person")).unwrap() {
            Value::Map(o) => assert_eq!(o["label"], Value::Null),
            _ => panic!("expected map"),
        }

        // Strict: unbound name → error.
        let strict =
            ObjectTransformer::new(build_spec(), Some(&schema), None).with_strict_exprs(true);
        assert!(
            strict.map_object(&obj, Some("Person")).is_err(),
            "strict mode must error on unbound name"
        );
    }
}

#[cfg(test)]
mod op_tests {
    //! Tests for offset / aggregation / pivot operations.
    use super::*;
    use crate::datamodel::{
        AggregationOperation, AggregationType, ClassDerivation, Offset, PivotDirectionType,
        PivotOperation, SlotDerivation, TransformationSpecification,
    };

    fn pivot(direction: PivotDirectionType) -> PivotOperation {
        PivotOperation {
            direction,
            variable_slot: "variable".into(),
            value_slot: "value".into(),
            unmelt_to_class: None,
            unmelt_to_slots: None,
            unit_slot: None,
            slot_name_template: "{variable}".into(),
            source_slots: None,
            id_slots: None,
        }
    }

    fn run(spec: TransformationSpecification, src: IndexMap<String, Value>, ty: &str) -> Value {
        ObjectTransformer::new(spec, None, None)
            .map_object(&Value::Map(src), Some(ty))
            .unwrap()
    }

    fn map(pairs: &[(&str, Value)]) -> IndexMap<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    fn class_with_slot(name: &str, sd: SlotDerivation) -> TransformationSpecification {
        let mut slots = IndexMap::new();
        slots.insert(sd.name.clone(), sd);
        TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: name.into(),
                populated_from: Some(name.into()),
                slot_derivations: Some(slots),
                ..Default::default()
            }]),
            ..Default::default()
        }
    }

    #[test]
    fn offset_adds_and_reverses() {
        // result = base + offset_value * factor
        let sd = SlotDerivation {
            name: "out".into(),
            populated_from: Some("base".into()),
            offset: Some(Offset {
                offset_value: 2.0,
                offset_field: "factor".into(),
                offset_reverse: None,
            }),
            ..Default::default()
        };
        let out = run(
            class_with_slot("C", sd),
            map(&[("base", Value::Int(10)), ("factor", Value::Int(3))]),
            "C",
        );
        // 10 + 2*3 = 16
        assert_eq!(out, Value::Map(map(&[("out", Value::Float(16.0))])));

        let sd = SlotDerivation {
            name: "out".into(),
            populated_from: Some("base".into()),
            offset: Some(Offset {
                offset_value: 2.0,
                offset_field: "factor".into(),
                offset_reverse: Some(true),
            }),
            ..Default::default()
        };
        let out = run(
            class_with_slot("C", sd),
            map(&[("base", Value::Int(10)), ("factor", Value::Int(3))]),
            "C",
        );
        // 10 - 2*3 = 4
        assert_eq!(out, Value::Map(map(&[("out", Value::Float(4.0))])));
    }

    fn agg(op: AggregationType) -> AggregationOperation {
        AggregationOperation {
            operator: op,
            null_handling: None,
            invalid_value_handling: None,
        }
    }

    #[test]
    fn aggregation_operators() {
        let list = Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
        let cases = [
            (AggregationType::Sum, Value::Int(6)),
            (AggregationType::Count, Value::Int(3)),
            (AggregationType::Average, Value::Float(2.0)),
            (AggregationType::Min, Value::Int(1)),
            (AggregationType::Max, Value::Int(3)),
            (AggregationType::Median, Value::Float(2.0)),
        ];
        for (op, expected) in cases {
            let sd = SlotDerivation {
                name: "out".into(),
                populated_from: Some("vals".into()),
                aggregation_operation: Some(agg(op)),
                ..Default::default()
            };
            let out = run(
                class_with_slot("C", sd),
                map(&[("vals", list.clone())]),
                "C",
            );
            assert_eq!(out, Value::Map(map(&[("out", expected)])), "op mismatch");
        }
    }

    #[test]
    fn pivot_melt_to_eav() {
        let mut pv = pivot(PivotDirectionType::Melt);
        pv.source_slots = Some(vec!["height".into(), "weight".into()]);
        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "C".into(),
                populated_from: Some("C".into()),
                pivot_operation: Some(pv),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let out = run(
            spec,
            map(&[
                ("height", Value::Float(1.8)),
                ("weight", Value::Float(75.0)),
            ]),
            "C",
        );
        assert_eq!(
            out,
            Value::List(vec![
                Value::Map(map(&[
                    ("variable", Value::Str("height".into())),
                    ("value", Value::Float(1.8))
                ])),
                Value::Map(map(&[
                    ("variable", Value::Str("weight".into())),
                    ("value", Value::Float(75.0))
                ])),
            ])
        );
    }

    #[test]
    fn pivot_unmelt_collection_to_wide() {
        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "C".into(),
                populated_from: Some("C".into()),
                pivot_operation: Some(pivot(PivotDirectionType::Unmelt)),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let records = Value::List(vec![
            Value::Map(map(&[
                ("variable", Value::Str("h".into())),
                ("value", Value::Float(1.8)),
            ])),
            Value::Map(map(&[
                ("variable", Value::Str("w".into())),
                ("value", Value::Float(75.0)),
            ])),
        ]);
        let out = run(spec, map(&[("measurements", records)]), "C");
        assert_eq!(
            out,
            Value::Map(map(&[("h", Value::Float(1.8)), ("w", Value::Float(75.0))]))
        );
    }
}

#[cfg(test)]
mod lookup_index_tests {
    //! Tests for LookupIndex join wiring (#188).
    use super::*;
    use crate::datamodel::{
        AggregationOperation, AggregationType, AliasedClass, ClassDerivation, SlotDerivation,
        TransformationSpecification,
    };
    use crate::engine::lookup_index::LookupIndex;
    use indexmap::IndexMap;
    use std::sync::Arc;

    fn get(v: &Value, key: &str) -> Value {
        match v {
            Value::Map(m) => m.get(key).cloned().unwrap_or(Value::Null),
            _ => Value::Null,
        }
    }

    fn imap(pairs: &[(&str, Value)]) -> IndexMap<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    fn val_map(pairs: &[(&str, Value)]) -> Value {
        Value::Map(imap(pairs))
    }

    fn slot(name: &str) -> SlotDerivation {
        SlotDerivation {
            name: name.to_string(),
            ..Default::default()
        }
    }

    fn slot_pf(name: &str, pf: &str) -> SlotDerivation {
        SlotDerivation {
            name: name.to_string(),
            populated_from: Some(pf.to_string()),
            ..Default::default()
        }
    }

    fn slot_expr(name: &str, expr: &str) -> SlotDerivation {
        SlotDerivation {
            name: name.to_string(),
            expr: Some(expr.to_string()),
            ..Default::default()
        }
    }

    fn slot_agg(name: &str, pf: &str, agg: AggregationType) -> SlotDerivation {
        SlotDerivation {
            name: name.to_string(),
            populated_from: Some(pf.to_string()),
            aggregation_operation: Some(AggregationOperation {
                operator: agg,
                null_handling: None,
                invalid_value_handling: None,
            }),
            ..Default::default()
        }
    }

    fn make_spec(
        class_name: &str,
        slots: Vec<SlotDerivation>,
        joins: Option<IndexMap<String, AliasedClass>>,
    ) -> TransformationSpecification {
        let mut cd = ClassDerivation {
            name: class_name.to_string(),
            ..Default::default()
        };
        cd.joins = joins;
        let mut sd: IndexMap<String, SlotDerivation> = IndexMap::new();
        for s in slots {
            sd.insert(s.name.clone(), s);
        }
        cd.slot_derivations = Some(sd);
        let mut spec = TransformationSpecification::default();
        spec.class_derivations = Some(vec![cd]);
        spec
    }

    fn join_alias(alias: &str, table: &str, src_key: &str) -> AliasedClass {
        AliasedClass {
            alias: alias.to_string(),
            class_named: Some(table.to_string()),
            source_key: Some(src_key.to_string()),
            lookup_key: None,
            join_on: None,
        }
    }

    // ── Test 1: populated_from "alias.field" single-row join ─────────────────

    #[test]
    fn test_join_populated_from_dot_notation() {
        // demographics table keyed by patient_id
        let demo_rows = vec![val_map(&[
            ("patient_id", Value::Str("P1".into())),
            ("age", Value::Int(42)),
            ("city", Value::Str("London".into())),
        ])];
        let mut li = LookupIndex::new();
        li.register_table("demographics", &demo_rows, "patient_id");

        let mut joins: IndexMap<String, AliasedClass> = IndexMap::new();
        joins.insert(
            "demo".to_string(),
            join_alias("demo", "demographics", "pid"),
        );

        let spec = make_spec(
            "Patient",
            vec![
                slot("pid"),
                slot_pf("age", "demo.age"),
                slot_pf("city", "demo.city"),
            ],
            Some(joins),
        );

        let source = val_map(&[("pid", Value::Str("P1".into()))]);
        let result = ObjectTransformer::new(spec, None, None)
            .with_lookup_index(Arc::new(li))
            .map_object(&source, Some("Patient"))
            .unwrap();

        assert_eq!(get(&result, "age"), Value::Int(42));
        assert_eq!(get(&result, "city"), Value::Str("London".into()));
    }

    // ── Test 2: expr "{alias.field}" join binding ─────────────────────────────

    #[test]
    fn test_join_expr_binding() {
        let demo_rows = vec![val_map(&[
            ("patient_id", Value::Str("P2".into())),
            ("score", Value::Int(10)),
        ])];
        let mut li = LookupIndex::new();
        li.register_table("scores", &demo_rows, "patient_id");

        let mut joins: IndexMap<String, AliasedClass> = IndexMap::new();
        joins.insert("sc".to_string(), join_alias("sc", "scores", "pid"));

        let spec = make_spec(
            "Result",
            vec![slot_expr("doubled", "{sc.score} * 2")],
            Some(joins),
        );

        let source = val_map(&[("pid", Value::Str("P2".into()))]);
        let result = ObjectTransformer::new(spec, None, None)
            .with_lookup_index(Arc::new(li))
            .map_object(&source, Some("Result"))
            .unwrap();

        assert_eq!(get(&result, "doubled"), Value::Int(20));
    }

    // ── Test 3: aggregation_operation + multi-row join (COUNT / SUM) ──────────

    #[test]
    fn test_join_multi_row_aggregation() {
        // Multiple visits for patient P3
        let visits = vec![
            val_map(&[
                ("patient_id", Value::Str("P3".into())),
                ("cost", Value::Float(100.0)),
            ]),
            val_map(&[
                ("patient_id", Value::Str("P3".into())),
                ("cost", Value::Float(200.0)),
            ]),
            val_map(&[
                ("patient_id", Value::Str("P3".into())),
                ("cost", Value::Float(50.0)),
            ]),
        ];
        let mut li = LookupIndex::new();
        li.register_table("visits", &visits, "patient_id");

        let mut joins: IndexMap<String, AliasedClass> = IndexMap::new();
        joins.insert("v".to_string(), join_alias("v", "visits", "pid"));

        let spec = make_spec(
            "Summary",
            vec![
                slot_agg("visit_count", "v.cost", AggregationType::Count),
                slot_agg("total_cost", "v.cost", AggregationType::Sum),
            ],
            Some(joins),
        );

        let source = val_map(&[("pid", Value::Str("P3".into()))]);
        let result = ObjectTransformer::new(spec, None, None)
            .with_lookup_index(Arc::new(li))
            .map_object(&source, Some("Summary"))
            .unwrap();

        assert_eq!(get(&result, "visit_count"), Value::Int(3));
        assert_eq!(get(&result, "total_cost"), Value::Float(350.0));
    }

    // ── Test 4: no index → dot-notation falls back to source_map get ─────────

    #[test]
    fn test_join_no_index_fallback() {
        // Without a LookupIndex, "alias.field" just does a raw source_map lookup
        // (which returns Null since "demo.age" is not a key).
        let spec = make_spec("Patient", vec![slot_pf("age", "demo.age")], None);
        let source = val_map(&[("pid", Value::Str("P1".into()))]);
        let result = ObjectTransformer::new(spec, None, None)
            .map_object(&source, Some("Patient"))
            .unwrap();
        assert_eq!(get(&result, "age"), Value::Null);
    }

    // ── #217 (a19eb095): nested cross-table join MISS retains the key ─────────

    /// Build a slot whose value is a nested class_derivation joined to `table`.
    fn slot_nested(name: &str, nested_class: &str, table: &str, inner: &str) -> SlotDerivation {
        let mut inner_slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        inner_slots.insert(inner.to_string(), slot_pf(inner, inner));
        let nested = ClassDerivation {
            name: nested_class.to_string(),
            populated_from: Some(table.to_string()),
            slot_derivations: Some(inner_slots),
            ..Default::default()
        };
        let mut cds: IndexMap<String, ClassDerivation> = IndexMap::new();
        cds.insert(nested_class.to_string(), nested);
        SlotDerivation {
            name: name.to_string(),
            class_derivations: Some(cds),
            ..Default::default()
        }
    }

    /// Singular nested slot, cross-table join with no matching row → the key is
    /// RETAINED with value `null` (not omitted, not `{value: null}`). Mirrors
    /// upstream `_derive_nested_objects` `continue` on a sparse miss.
    #[test]
    fn nested_join_miss_singular_retains_null() {
        // readings keyed by subject_id — only S_OTHER present.
        let readings = vec![val_map(&[
            ("subject_id", Value::Str("S_OTHER".into())),
            ("score", Value::Float(95.5)),
        ])];
        let mut li = LookupIndex::new();
        li.register_table("readings", &readings, "subject_id");

        let mut joins: IndexMap<String, AliasedClass> = IndexMap::new();
        // Join keyed by the nested source ("readings"), mirroring upstream
        // `joins[nested_source]`. Source key column is `subject_id`.
        joins.insert(
            "readings".to_string(),
            join_alias("readings", "readings", "subject_id"),
        );

        let spec = make_spec(
            "Measurement",
            vec![
                slot_pf("id", "id"),
                slot_nested("observation", "Observation", "readings", "score"),
            ],
            Some(joins),
        );

        // subject_id S_NODATA has no matching reading → sparse miss.
        let source = val_map(&[
            ("id", Value::Str("M2".into())),
            ("subject_id", Value::Str("S_NODATA".into())),
        ]);
        let result = ObjectTransformer::new(spec, None, None)
            .with_lookup_index(Arc::new(li))
            .map_object(&source, Some("Measurement"))
            .unwrap();

        match &result {
            Value::Map(o) => {
                assert!(o.contains_key("observation"), "key retained on miss");
                assert_eq!(o["observation"], Value::Null, "miss → null, not omitted");
                assert_eq!(o["id"], Value::Str("M2".into()));
            }
            other => panic!("expected map, got {other:?}"),
        }
    }

    /// A matching joined row is NOT collapsed: the nested object is built and
    /// retained (control for the miss test above — the collapse in
    /// `nested_join_misses` fires only when the join finds no row). The inner
    /// `score` uses a plain `populated_from` that reads the parent row, so it is
    /// null here; the point of this test is that the object survives as a `Map`
    /// rather than becoming `Null` as it would on a miss.
    #[test]
    fn nested_join_hit_builds_object() {
        let readings = vec![val_map(&[
            ("subject_id", Value::Str("S1".into())),
            ("score", Value::Float(95.5)),
        ])];
        let mut li = LookupIndex::new();
        li.register_table("readings", &readings, "subject_id");

        let mut joins: IndexMap<String, AliasedClass> = IndexMap::new();
        joins.insert(
            "readings".to_string(),
            join_alias("readings", "readings", "subject_id"),
        );

        let spec = make_spec(
            "Measurement",
            vec![slot_nested("observation", "Observation", "readings", "score")],
            Some(joins),
        );

        // subject_id S1 HAS a matching reading → no miss → object built.
        let source = val_map(&[("subject_id", Value::Str("S1".into()))]);
        let result = ObjectTransformer::new(spec, None, None)
            .with_lookup_index(Arc::new(li))
            .map_object(&source, Some("Measurement"))
            .unwrap();

        match &result {
            Value::Map(o) => {
                assert!(o.contains_key("observation"), "hit keeps the slot");
                assert!(
                    matches!(o["observation"], Value::Map(_)),
                    "hit builds a real nested object (not collapsed to null), got {:?}",
                    o["observation"]
                );
            }
            other => panic!("expected map, got {other:?}"),
        }
    }

    /// Multivalued nested slot whose join misses → `[]` retained (not `[null]`,
    /// not omitted). Requires a target schema marking the slot multivalued.
    #[test]
    fn nested_join_miss_multivalued_retains_empty_list() {
        use crate::schema::{ClassDef, InMemorySchemaBuilder, RangeKind, SlotDef};

        // Target: Result.observations is multivalued (List of Observation).
        let target = InMemorySchemaBuilder::new()
            .add_type("float")
            .add_class(ClassDef {
                name: "Result".into(),
                tree_root: true,
                is_a: None,
                mixins: vec![],
            })
            .add_slot(
                "Result",
                SlotDef {
                    name: "observations".into(),
                    range: RangeKind::Class("Observation".into()),
                    multivalued: true,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: true,
                },
            )
            .build();

        let readings = vec![val_map(&[
            ("subject_id", Value::Str("S_OTHER".into())),
            ("score", Value::Float(95.5)),
        ])];
        let mut li = LookupIndex::new();
        li.register_table("readings", &readings, "subject_id");

        let mut joins: IndexMap<String, AliasedClass> = IndexMap::new();
        joins.insert(
            "readings".to_string(),
            join_alias("readings", "readings", "subject_id"),
        );

        // Parent class must be "Result" so the target-schema slot lookup matches.
        let spec = make_spec(
            "Result",
            vec![slot_nested("observations", "Observation", "readings", "score")],
            Some(joins),
        );

        let source = val_map(&[("subject_id", Value::Str("S_NODATA".into()))]);
        let result = ObjectTransformer::new(spec, None, Some(&target))
            .with_lookup_index(Arc::new(li))
            .map_object(&source, Some("Result"))
            .unwrap();

        match &result {
            Value::Map(o) => {
                assert!(o.contains_key("observations"), "multivalued key retained");
                assert_eq!(
                    o["observations"],
                    Value::List(vec![]),
                    "all-miss multivalued → [] retained"
                );
            }
            other => panic!("expected map, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod inline_dotpath_tests {
    //! Tests for inlined `populated_from` dot-path traversal (issue #247).
    //!
    //! Mirrors upstream `tests/test_transformer/test_inline_dotpath.py`
    //! (commit b5fca196). `populated_from` dot-paths normally walk FK joins via
    //! a LookupIndex; #247 extends them to traverse *inlined* nested data
    //! (XML/JSON/OWL/EML-shaped trees) by walking into the nested object
    //! structurally instead of treating it as a foreign key.
    use super::*;
    use crate::datamodel::SlotDerivation;
    use crate::schema::{ClassDef, InMemorySchemaBuilder, RangeKind, SlotDef};

    fn get(v: &Value, key: &str) -> Value {
        match v {
            Value::Map(m) => m.get(key).cloned().unwrap_or(Value::Null),
            _ => Value::Null,
        }
    }

    fn imap(pairs: &[(&str, Value)]) -> IndexMap<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    fn slot_pf(name: &str, pf: &str) -> SlotDerivation {
        SlotDerivation {
            name: name.to_string(),
            populated_from: Some(pf.to_string()),
            ..Default::default()
        }
    }

    fn make_spec(class_name: &str, slots: Vec<SlotDerivation>) -> TransformationSpecification {
        let mut cd = ClassDerivation {
            name: class_name.to_string(),
            ..Default::default()
        };
        let mut sd: IndexMap<String, SlotDerivation> = IndexMap::new();
        for s in slots {
            sd.insert(s.name.clone(), s);
        }
        cd.slot_derivations = Some(sd);
        TransformationSpecification {
            class_derivations: Some(vec![cd]),
            ..Default::default()
        }
    }

    fn class(name: &str, tree_root: bool) -> ClassDef {
        ClassDef {
            name: name.into(),
            tree_root,
            is_a: None,
            mixins: vec![],
        }
    }

    fn scalar_slot(name: &str) -> SlotDef {
        SlotDef {
            name: name.into(),
            range: RangeKind::Type("string".into()),
            multivalued: false,
            required: false,
            identifier: false,
            key: false,
            unit: None,
            any_of_enums: vec![],
            inlined: false,
            inlined_as_list: false,
        }
    }

    fn class_slot(name: &str, range: &str, multivalued: bool, inlined_as_list: bool) -> SlotDef {
        SlotDef {
            name: name.into(),
            range: RangeKind::Class(range.into()),
            multivalued,
            required: false,
            identifier: false,
            key: false,
            unit: None,
            any_of_enums: vec![],
            inlined: !inlined_as_list,
            inlined_as_list,
        }
    }

    /// Build the EML-ish inlined source schema:
    /// `EMLDocument -> dataset (inlined) -> {title, dataTable (inlined list)}`.
    /// When `declare_inlined` is false the `dataset` slot omits `inlined`, to
    /// exercise the runtime dict fallback.
    fn eml_schema(declare_inlined: bool) -> InMemorySchemaBuilder {
        let dataset = SlotDef {
            inlined: declare_inlined,
            ..class_slot("dataset", "Dataset", false, false)
        };
        InMemorySchemaBuilder::new()
            .add_class(class("EMLDocument", true))
            .add_slot("EMLDocument", dataset)
            .add_class(class("Dataset", false))
            .add_slot("Dataset", scalar_slot("title"))
            .add_slot("Dataset", class_slot("dataTable", "DataTable", true, true))
            .add_class(class("DataTable", false))
            .add_slot("DataTable", scalar_slot("entityName"))
    }

    fn input_data() -> Value {
        Value::Map(imap(&[(
            "dataset",
            Value::Map(imap(&[
                ("title", Value::Str("My Dataset".into())),
                (
                    "dataTable",
                    Value::List(vec![
                        Value::Map(imap(&[("entityName", Value::Str("table_one".into()))])),
                        Value::Map(imap(&[("entityName", Value::Str("table_two".into()))])),
                    ]),
                ),
            ])),
        )]))
    }

    /// `title: {populated_from: dataset.title}` spec.
    fn title_spec() -> TransformationSpecification {
        make_spec("EMLDocument", vec![slot_pf("title", "dataset.title")])
    }

    // #247 test 1: dot-path into an inlined object reaches the scalar leaf,
    // declaratively (dataset is marked `inlined: true`), with no ObjectIndex.
    #[test]
    fn test_slot_level_inline_deep_scalar() {
        let schema = eml_schema(true).build();
        let result = ObjectTransformer::new(title_spec(), Some(&schema), None)
            .map_object(&input_data(), Some("EMLDocument"))
            .unwrap();
        assert_eq!(get(&result, "title"), Value::Str("My Dataset".into()));
    }

    // #247 test 2: runtime fallback — a dict value traverses even when the
    // source schema omits `inlined: true` on the first segment.
    #[test]
    fn test_inline_path_via_runtime_fallback_without_inlined_declaration() {
        let schema = eml_schema(false).build();
        let result = ObjectTransformer::new(title_spec(), Some(&schema), None)
            .map_object(&input_data(), Some("EMLDocument"))
            .unwrap();
        assert_eq!(get(&result, "title"), Value::Str("My Dataset".into()));
    }

    // #247 test 3: a legitimately missing leaf yields Null rather than erroring.
    #[test]
    fn test_inline_path_absent_segment_yields_none() {
        let schema = eml_schema(true).build();
        let source = Value::Map(imap(&[("dataset", Value::Map(IndexMap::new()))]));
        let result = ObjectTransformer::new(title_spec(), Some(&schema), None)
            .map_object(&source, Some("EMLDocument"))
            .unwrap();
        assert_eq!(get(&result, "title"), Value::Null);
    }

    // #265 test: a list segment mid-path is the not-yet-supported inline
    // fan-out case — it raises naming the multivalued segment, not silent Null.
    #[test]
    fn test_multivalued_inline_segment_raises_clear_diagnostic() {
        let schema = eml_schema(true).build();
        let spec = make_spec("EMLDocument", vec![slot_pf("classes", "dataset.dataTable")]);
        let err = ObjectTransformer::new(spec, Some(&schema), None)
            .map_object(&input_data(), Some("EMLDocument"))
            .unwrap_err();
        // The engine wraps per-slot errors in `SlotTransform` (mirroring Python
        // `map_object`'s incremental context enrichment); the `InlinePath`
        // diagnostic — segment name + #265 + slot context — is preserved verbatim.
        let msg = err.to_string();
        assert!(
            msg.contains("dataTable") && msg.contains("#265"),
            "diagnostic should name the segment and cite #265: {msg}"
        );
        assert!(
            msg.contains("slot_derivation=classes"),
            "diagnostic should carry slot-derivation context: {msg}"
        );
    }

    // #247: a non-dict value mid-path (scalar where a nested object is expected)
    // errors rather than silently returning Null.
    #[test]
    fn test_non_dict_mid_path_raises() {
        let schema = eml_schema(true).build();
        let spec = make_spec("EMLDocument", vec![slot_pf("title", "dataset.title.deeper")]);
        // `dataset.title` is a scalar string; descending into `.deeper` must error.
        let err = ObjectTransformer::new(spec, Some(&schema), None)
            .map_object(&input_data(), Some("EMLDocument"))
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("expected a nested object") && msg.contains("deeper"),
            "diagnostic should explain the non-object segment: {msg}"
        );
    }
}
