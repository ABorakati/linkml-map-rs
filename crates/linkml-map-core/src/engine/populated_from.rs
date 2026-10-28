//! The `populated_from:` arm of `ObjectTransformer._derive_slot`: the raw value of
//! a source slot, and the source slot it was read from.
//!
//! The source slot matters as much as the value. `_derive_slot` coerces a value to
//! the target slot's range (`_map_value_by_range`, `_coerce_cardinality`,
//! `_coerce_datatype`, `_resolve_enum_value`, `_reshape_collection`) only when the
//! resolution hands back a source slot, so each path below returns exactly the slot
//! upstream does, `None` included.
//!
//! A dotted `populated_from` is resolved in upstream's order:
//!
//! 1. `Alias.field`, where `Alias` is a join of the class derivation:
//!    `_perform_join_resolution`;
//! 2. `Class.field`, where `Class` is the source type: the row's `field`;
//! 3. otherwise `_resolve_fk_or_literal(.., require_fk=True)`: a path through
//!    inlined nested objects (`_resolve_inline_path`), else through a foreign key
//!    (`resolve_fk_path`, `_perform_fk_resolution`), else a `ValueError`.

use std::borrow::Cow;

use indexmap::IndexMap;

use super::{
    LookupIndex, MergedRow, ObjectIndex, ObjectTransformer, induced_slot_cow, join_source_key,
    merged_row::ambiguous_column_error, value_to_string_key, value_type_name,
};
use crate::{
    datamodel::{ClassDerivation, SlotDerivation},
    error::{Error, Result},
    schema::{SchemaProvider, SlotDef},
    value::Value,
};

/// The raw value of a `populated_from`, with the source slot it came from.
pub(super) type Populated<'s> = (Value, Option<Cow<'s, SlotDef>>);

impl<'s> ObjectTransformer<'s> {
    /// Resolve `populated_from` on the row `source_map` of `source_type`: the
    /// `populated_from` branch of `_derive_slot` up to (excluding)
    /// `_nullify_missing_values`.
    ///
    /// `_source_class` is a virtual slot naming the source class itself (#193), so
    /// it has no schema slot.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn resolve_populated_from(
        &self,
        populated_from: &str,
        slot_deriv: &SlotDerivation,
        source_map: &IndexMap<String, Value>,
        source_type: &str,
        class_deriv: &ClassDerivation,
        index: Option<&ObjectIndex>,
        merged: Option<&MergedRow<'_>>,
    ) -> Result<Populated<'s>> {
        if populated_from == "_source_class" {
            return Ok((Value::Str(source_type.to_string()), None));
        }
        let Some((table_name, field_path)) = populated_from.split_once('.') else {
            // `_resolve_fk_or_literal` without a dot: the row's value, and the
            // source type's slot of that name (which must exist).
            // PARITY: the value is read, and an ambiguous column raised, before the
            // slot is looked up.
            if merged.is_some_and(|m| m.is_ambiguous(populated_from)) {
                return Err(ambiguous_column_error(populated_from));
            }
            return Ok((
                source_map
                    .get(populated_from)
                    .cloned()
                    .unwrap_or(Value::Null),
                self.require_source_slot(populated_from, source_type)?,
            ));
        };

        if let Some(ac) = class_deriv
            .joins
            .as_ref()
            .and_then(|joins| joins.get(table_name))
        {
            // A merged row that holds the table's row hands it over as it is
            // (`_resolve_joined_row`'s pre-joined fast path).
            let v = match merged.and_then(|m| m.table(table_name)) {
                Some(row) => row
                    .and_then(|row| row.get(field_path))
                    .cloned()
                    .unwrap_or(Value::Null),
                None => self.resolve_join_value(table_name, field_path, ac, source_map)?,
            };
            // PARITY: `_perform_join_resolution` looks the slot up with
            // `field_path in context.sv.class_induced_slots(joined_class)`, but
            // `class_induced_slots` returns `SlotDefinition` objects, never
            // strings, so the test is always false and a joined value reaches
            // `_derive_slot` with no source slot: no range, cardinality, datatype
            // or enum coercion, whatever the target slot says. Matched here, not
            // fixed.
            return Ok((v, None));
        }
        if let Some(row) = merged.and_then(|m| m.table(table_name)) {
            // A table of the merged row: `rows_by_table[table].get(field)`.
            let Some(row) = row else {
                return Err(Error::Python(
                    "AttributeError: 'NoneType' object has no attribute 'get'".to_owned(),
                ));
            };
            // PARITY: the slot is `induced_slot(field, table)` when the table is a
            // class of the source schema (which raises for a field it lacks), else none.
            let slot = match self.source_schema {
                Some(schema) if schema.get_class(table_name).is_ok() => {
                    self.require_source_slot(field_path, table_name)?
                }
                _ => None,
            };
            return Ok((row.get(field_path).cloned().unwrap_or(Value::Null), slot));
        }
        if table_name == source_type {
            // `Class.field` on the row of that class.
            if merged.is_some_and(|m| m.is_ambiguous(field_path)) {
                return Err(ambiguous_column_error(field_path));
            }
            return Ok((
                source_map.get(field_path).cloned().unwrap_or(Value::Null),
                self.require_source_slot(field_path, source_type)?,
            ));
        }
        self.resolve_fk_or_literal(populated_from, slot_deriv, source_map, source_type, index)
    }

    /// `_resolve_fk_or_literal(.., require_fk=True)` for a dotted path.
    fn resolve_fk_or_literal(
        &self,
        populated_from: &str,
        slot_deriv: &SlotDerivation,
        source_map: &IndexMap<String, Value>,
        source_type: &str,
        index: Option<&ObjectIndex>,
    ) -> Result<Populated<'s>> {
        // #247: a path through inlined nested data is walked structurally, ahead
        // of any foreign key.
        if is_inline_path(populated_from, source_type, source_map, self.source_schema) {
            return resolve_inline_path(
                populated_from,
                source_type,
                source_map,
                self.source_schema,
                slot_deriv,
            );
        }
        let Some(schema) = self.source_schema else {
            // No source schema to resolve a foreign key against (upstream always
            // has one): the literal key, as a flat row names it.
            return Ok((
                source_map
                    .get(populated_from)
                    .cloned()
                    .unwrap_or(Value::Null),
                None,
            ));
        };
        let Some(fk) = resolve_fk_path(schema, source_type, populated_from) else {
            return Err(Error::Python(format!(
                "ValueError: Dot-notation '{populated_from}' in populated_from requires a matching \
                 join spec or FK path, but neither was found"
            )));
        };
        let fk_value = source_map.get(fk.fk_slot_name);
        Ok(perform_fk_resolution(fk, slot_deriv, fk_value, index))
    }

    /// `_perform_join_resolution`'s value: the `field` of the row of the joined
    /// table the row's key points at.
    ///
    /// PARITY: `_resolve_joined_row`: `join_keys(spec)` raises on an unresolved key;
    /// no key value on the row is a miss (`None`); but a key value with no
    /// `lookup_index` at all (single-object input) raises `ValueError`, and with an
    /// index that has no such table (a join table with no file) DuckDB raises a
    /// `CatalogException`. Only a registered table can miss quietly.
    fn resolve_join_value(
        &self,
        alias: &str,
        field: &str,
        ac: &crate::datamodel::AliasedClass,
        source_map: &IndexMap<String, Value>,
    ) -> Result<Value> {
        let table = alias;
        let Some(source_key) = join_source_key(ac) else {
            return Err(Error::Python(format!(
                "ValueError: Join {alias:?} must specify 'join_on' or both 'source_key' and 'lookup_key'"
            )));
        };
        let Some(key) = source_map.get(source_key).filter(|kv| !kv.is_null()) else {
            return Ok(Value::Null);
        };
        let Some(li) = self.lookup_index.as_deref() else {
            return Err(Error::Python(format!(
                "ValueError: Join configured for {alias:?} but lookup_index has not been initialized"
            )));
        };
        lookup_joined_value(li, table, field, key)
    }
}

/// The `field` of the row of `table` keyed by `key`, or null.
fn lookup_joined_value(li: &LookupIndex, table: &str, field: &str, key: &Value) -> Result<Value> {
    if !li.is_registered(table) {
        return Err(Error::Python(format!(
            "duckdb.CatalogException: Catalog Error: Table with name {table} does not exist!"
        )));
    }
    let ks = match key {
        Value::Str(s) => s.clone(),
        Value::Int(i) => i.to_string(),
        Value::BigInt(n) => n.to_string(),
        _ => return Ok(Value::Null),
    };
    Ok(li
        .lookup_row(table, &ks)
        .and_then(|row| row.get(field))
        .cloned()
        .unwrap_or(Value::Null))
}

// ── foreign-key paths ───────────────────────────────────────────────────────

/// `FKResolution` (`utils/fk_utils.py`): a `populated_from` path read through a
/// foreign-key slot.
struct FkResolution<'p, 's> {
    /// The row's slot holding the foreign key: the path's first segment.
    fk_slot_name: &'p str,
    /// The class the foreign key refers to.
    target_class: String,
    /// The path to read within the referenced object.
    remaining_path: &'p str,
    /// The slot of the last segment, when the path could be followed that far.
    final_slot: Option<Cow<'s, SlotDef>>,
}

/// `resolve_fk_path`: resolve `path` (`org_id.name`) on `source_class` to the
/// class the foreign key refers to, the rest of the path, and the final slot.
/// `None` when the first segment is no slot of the source class, or its range is
/// not a class.
fn resolve_fk_path<'p, 's>(
    schema: &'s dyn SchemaProvider,
    source_class: &str,
    path: &'p str,
) -> Option<FkResolution<'p, 's>> {
    let (fk_slot_name, remaining_path) = path.split_once('.')?;
    let fk_slot = induced_slot_cow(Some(schema), fk_slot_name, source_class)?;
    let target_class = fk_slot.range_class()?.to_owned();

    // Walk the remaining path to find the final slot.
    let mut current_class = target_class.clone();
    let mut final_slot = None;
    let mut segments = remaining_path.split('.').peekable();
    while let Some(attr) = segments.next() {
        let Some(slot) = induced_slot_cow(Some(schema), attr, &current_class) else {
            break;
        };
        if segments.peek().is_none() {
            final_slot = Some(slot);
        } else if let Some(class) = slot.range_class() {
            current_class = class.to_owned();
        } else {
            break;
        }
    }
    Some(FkResolution {
        fk_slot_name,
        target_class,
        remaining_path,
        final_slot,
    })
}

/// `_perform_fk_resolution`: read the rest of the path from the object the
/// foreign-key value refers to.
fn perform_fk_resolution<'s>(
    fk: FkResolution<'_, 's>,
    slot_deriv: &SlotDerivation,
    fk_value: Option<&Value>,
    index: Option<&ObjectIndex>,
) -> Populated<'s> {
    let v = match (fk_value.filter(|v| !v.is_null()), index) {
        (Some(fk_value), Some(index)) => {
            // The index keys an object by its class and `str(identifier)`.
            let referenced = index
                .get_in_class(&fk.target_class, &value_to_string_key(fk_value))
                .filter(|obj| obj.is_truthy());
            match referenced {
                Some(obj) => read_path(obj, fk.remaining_path),
                None => {
                    log::debug!("FK reference not found for {}", slot_deriv.name);
                    Value::Null
                }
            }
        }
        (Some(_), None) => {
            log::warn!(
                "Cross-class lookup requires object_index. Call transformer.index(container_data) first."
            );
            Value::Null
        }
        (None, _) => Value::Null,
    };
    (v, fk.final_slot)
}

/// Follow `path` through a referenced object: a mapping yields its value, anything
/// else (`getattr(v, attr, None)` on a scalar) nothing, and a null ends the walk.
fn read_path(obj: &Value, path: &str) -> Value {
    let mut current = Some(obj);
    for attr in path.split('.') {
        current = match current {
            Some(Value::Map(m)) => m.get(attr),
            _ => None,
        };
        if current.is_none_or(Value::is_null) {
            break;
        }
    }
    current.cloned().unwrap_or(Value::Null)
}

// ── inlined nested paths ────────────────────────────────────────────────────

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
/// to the FK resolution.
fn is_inline_path(
    populated_from: &str,
    source_type: &str,
    source_map: &IndexMap<String, Value>,
    source_schema: Option<&dyn SchemaProvider>,
) -> bool {
    let first_segment = populated_from.split('.').next().unwrap_or(populated_from);
    let slot = source_schema.and_then(|sv| sv.induced_slot(first_segment, source_type).ok());
    // `slot.range in sv.all_classes()` is encoded directly by `RangeKind::Class`.
    if let Some(slot) = &slot
        && slot.is_object_range()
        && (slot.inlined || slot.inlined_as_list)
    {
        return true;
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
/// time and returns the leaf value together with its source slot (the last
/// segment's, for range mapping; none when the walk ended early on a null). A
/// segment that is legitimately absent yields `Null` rather than an error.
///
/// # Errors
/// Returns [`Error::InlinePath`] if a segment holds a list (multivalued inline
/// fan-out, tracked in #265) or a non-map value is encountered mid-path.
fn resolve_inline_path<'a>(
    populated_from: &str,
    source_type: &str,
    source_map: &IndexMap<String, Value>,
    source_schema: Option<&'a dyn SchemaProvider>,
    slot_deriv: &SlotDerivation,
) -> Result<Populated<'a>> {
    static NULL: Value = Value::Null;
    let inline_err = |message: String| Error::InlinePath {
        message,
        slot_derivation_name: slot_deriv.name.clone(),
        slot_populated_from: populated_from.to_string(),
    };

    // `current_val`, as a reference into the row rather than a copy of it; the
    // row itself is the start.
    let mut current_val: Option<&Value> = None;
    let mut current_class: Option<String> = Some(source_type.to_string());
    let mut final_slot = None;

    let mut segments = populated_from.split('.').peekable();
    while let Some(segment) = segments.next() {
        let map = match current_val {
            None => source_map,
            Some(Value::Map(m)) => m,
            Some(other) => {
                return Err(inline_err(format!(
                    "Cannot traverse inlined path '{populated_from}': segment '{segment}' \
                     expected a nested object but found {}",
                    value_type_name(other)
                )));
            }
        };
        let slot = current_class
            .as_deref()
            .and_then(|c| induced_slot_cow(source_schema, segment, c));
        let next = map.get(segment).unwrap_or(&NULL);
        if let Value::List(_) = next {
            return Err(inline_err(format!(
                "Inlined path '{populated_from}' reaches a multivalued segment '{segment}'; \
                 per-item fan-out to a matching class_derivation is not yet supported (see #265)"
            )));
        }
        current_val = Some(next);
        if segments.peek().is_none() {
            final_slot = slot;
        } else {
            // PARITY: upstream `_resolve_inline_path` sets
            // `current_class = slot.range if slot else None` on the non-final
            // branch, a class range and a *scalar* range alike: a scalar range
            // becomes the "class" of the next hop (which will then fail
            // `induced_slot` and yield None). Reproduced verbatim rather than
            // cleaned up so a malformed deep path resolves identically.
            // (linkml-map object_transformer.py::_resolve_inline_path, commit b5fca196)
            current_class = slot
                .as_deref()
                .and_then(|s| s.range.name())
                .map(str::to_string);
        }
        if next.is_null() {
            break;
        }
    }
    Ok((current_val.cloned().unwrap_or(Value::Null), final_slot))
}
