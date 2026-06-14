//! Foreign-key object index for the transform engine.
//!
//! Port of the relevant slice of `linkml_runtime.index.object_index.ObjectIndex`
//! as used by `linkml_map`'s flattening / FK-dereference path.
//!
//! # What it does
//! Some transform specs flatten a *normalized* source model into a
//! *denormalized* target. In the normalized model an object refers to another
//! object **by identifier** (a foreign key), and the referenced objects live in
//! a separate collection on the container, e.g.:
//!
//! ```yaml
//! mappings:
//!   - subject: X:1        # FK — an Entity identifier, not an inlined object
//!     object: Y:1
//!     predicate: P:1
//! entities:               # the referenced objects, keyed by identifier
//!   "X:1": { name: x1 }
//!   "Y:1": { name: y1 }
//! ```
//!
//! A transform like `subject_id: {expr: subject.id}` / `subject_name:
//! {expr: subject.name}` needs `subject` (a scalar id) to *dereference* to the
//! `Entity` object `{id: "X:1", name: "x1"}` so that `.id` / `.name` resolve.
//!
//! [`ObjectIndex`] scans the container's collections once and builds:
//! - a **flat** `id -> object` map (ids are globally unique in the normalized
//!   model, matching how Python keys the `_source_object_cache` by id), and
//! - a **typed** `(class, id) -> object` map when the schema is available.
//!
//! The resolved object always carries its identifier slot back as a field, so
//! `obj.id` works even when the source collection was *inlined as a dict*
//! (identifier is the dict key, not a field of the value).
//!
//! # Schema-optional
//! When a [`SchemaProvider`] is supplied, collection ranges + identifier slots
//! drive both indexing and FK detection. The flattening golden schema cannot be
//! fully loaded by every backend (unresolved CURIE prefixes), so the index also
//! degrades gracefully: any container slot whose value is a *map of maps* or a
//! *list of identified maps* is indexed by best-effort identifier inference, and
//! FK detection falls back to "scalar value that is a known index id".

use std::collections::HashMap;

use crate::{schema::SchemaProvider, value::Value};

/// An index over a source container, mapping identifiers to their objects.
///
/// `ObjectIndex` is `Send + Sync` — its fields contain only `String` and
/// [`Value`] (which are themselves `Send + Sync`), so a single built index
/// can be wrapped in `Arc` and shared across rayon / tokio worker threads
/// without any per-row cloning of the index data.
#[derive(Debug, Default, Clone)]
pub struct ObjectIndex {
    /// Flat `identifier -> resolved object` (object carries its id field).
    by_id: HashMap<String, Value>,
    /// Typed `(class_name, identifier) -> resolved object`.
    by_class_id: HashMap<(String, String), Value>,
}

// Compile-time proof that ObjectIndex is Send + Sync.
// Value contains only String/bool/i64/f64/Vec/IndexMap — all Send+Sync.
const _: () = {
    fn _assert_send_sync<T: Send + Sync>() {}
    fn _check() {
        _assert_send_sync::<ObjectIndex>();
    }
};

impl ObjectIndex {
    /// Build an index by scanning a container object's collections.
    ///
    /// `container` is the whole source dataset/root object (e.g. a
    /// `MappingSet`). `container_type` names its class. `schema` is consulted
    /// when present to find each collection slot's range class + identifier
    /// slot; when absent (or the class is unknown to the provider) the index
    /// falls back to structural inference.
    pub fn build(
        container: &Value,
        container_type: Option<&str>,
        schema: Option<&dyn SchemaProvider>,
    ) -> Self {
        let mut idx = ObjectIndex::default();
        let map = match container {
            Value::Map(m) => m,
            _ => return idx,
        };

        for (slot_name, slot_value) in map {
            // Determine the range class + identifier slot for this collection,
            // schema-first with a structural fallback.
            let (range_class, id_slot): (Option<String>, Option<String>) =
                match (schema, container_type) {
                    (Some(sp), Some(ct)) => match sp.induced_slot(slot_name, ct) {
                        Ok(slot) => {
                            let rc = slot.range_class().map(|s| s.to_string());
                            let id = rc
                                .as_deref()
                                .and_then(|c| sp.identifier_slot(c).ok().flatten().map(|s| s.name));
                            (rc, id)
                        }
                        Err(_) => (None, None),
                    },
                    _ => (None, None),
                };

            idx.index_collection(
                slot_value,
                range_class.as_deref(),
                id_slot.as_deref(),
                schema,
            );
        }

        idx
    }

    /// Index a single collection value (dict-of-objects or list-of-objects).
    fn index_collection(
        &mut self,
        collection: &Value,
        range_class: Option<&str>,
        id_slot: Option<&str>,
        schema: Option<&dyn SchemaProvider>,
    ) {
        match collection {
            // Inlined-as-dict: the key IS the identifier; the value may omit it.
            Value::Map(entries) => {
                for (key, val) in entries {
                    if let Value::Map(_) = val {
                        let obj = ensure_id_field(val, id_slot, key);
                        self.insert(range_class, key, obj);
                    }
                }
            }
            // Inlined-as-list: each item carries its identifier inline.
            Value::List(items) => {
                for item in items {
                    if let Value::Map(m) = item {
                        // Resolve the id field: schema id_slot, else structural.
                        let id_key = id_slot
                            .and_then(|s| m.get(s))
                            .or_else(|| infer_id_value(m))
                            .and_then(value_as_id_string);
                        if let Some(id) = id_key {
                            self.insert(range_class, &id, item.clone());
                        }
                    }
                }
            }
            _ => {}
        }
        let _ = schema;
    }

    fn insert(&mut self, range_class: Option<&str>, id: &str, obj: Value) {
        if let Some(rc) = range_class {
            self.by_class_id
                .insert((rc.to_string(), id.to_string()), obj.clone());
        }
        // Flat index: first writer wins is fine (ids are unique). Don't clobber
        // a typed entry with a less-specific one.
        self.by_id.entry(id.to_string()).or_insert(obj);
    }

    /// Look up a referenced object by `(class, id)`, falling back to the flat
    /// `id` index when the class is unknown / not indexed typed.
    pub fn get(&self, class: Option<&str>, id: &str) -> Option<&Value> {
        if let Some(c) = class {
            if let Some(v) = self.by_class_id.get(&(c.to_string(), id.to_string())) {
                return Some(v);
            }
        }
        self.by_id.get(id)
    }

    /// True if `id` is a known identifier in the flat index.
    pub fn contains_id(&self, id: &str) -> bool {
        self.by_id.contains_key(id)
    }

    /// True if the index holds no objects.
    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    /// Number of objects in the flat index.
    pub fn len(&self) -> usize {
        self.by_id.len()
    }
}

/// Return a clone of `obj` (a `Value::Map`) guaranteed to carry its identifier
/// as a field. When the object was inlined-as-dict the identifier is the dict
/// key and may be missing from the value; we inject it under `id_slot` (or a
/// best-effort `"id"` when no schema id slot is known).
fn ensure_id_field(obj: &Value, id_slot: Option<&str>, id_value: &str) -> Value {
    let mut m = match obj {
        Value::Map(m) => m.clone(),
        other => return other.clone(),
    };
    let key = id_slot.unwrap_or("id");
    if !m.contains_key(key) {
        m.insert(key.to_string(), Value::Str(id_value.to_string()));
    }
    Value::Map(m)
}

/// Best-effort identifier inference for a list-inlined object without schema:
/// prefer a field literally named `id`.
fn infer_id_value(m: &indexmap::IndexMap<String, Value>) -> Option<&Value> {
    m.get("id")
}

fn value_as_id_string(v: &Value) -> Option<String> {
    match v {
        Value::Str(s) => Some(s.clone()),
        Value::Int(i) => Some(i.to_string()),
        _ => None,
    }
}
