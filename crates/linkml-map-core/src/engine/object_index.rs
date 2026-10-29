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
//! # Upstream's index, and where it exists
//! Upstream builds the index in exactly one place, `ObjectTransformer.index`, which
//! `cli.py::_map_data_single` (YAML / JSON input), `multi_file_transformer` and Python callers
//! invoke; the streaming path (`_map_data_streaming`, a table or directory) never does, so there a
//! foreign key stays its bare identifier. This module only models the index; the callers decide
//! whether one exists (see `RowSegment::is_indexed` in the pipeline).
//!
//! # Typed index (`ObjectIndex._index`)
//! With a schema and a known container class the index is a port of
//! `ObjectIndex._index` over `dynamic_object`'s tree: **every** object in the tree is indexed,
//! the root included, at any depth (an inlined object nested in another object, not only a
//! collection of the container), under `(class name, identifier value)` where the value is the
//! raw, typed value (`1` and `"1"` are different keys; a null identifier is the key
//! `(class, None)`), and the last object registered under a key wins. A dict-keyed collection
//! injects the dict key as the object's identifier (`dynamic_object`'s `setattr`).
//!
//! # Schema-optional
//! Without a [`SchemaProvider`] (or when the container's class is unknown to it) the index
//! degrades gracefully: any container slot whose value is a *map of maps* or a *list of
//! identified maps* is indexed by best-effort identifier inference, and FK detection falls back
//! to "scalar value that is a known index id".

use std::collections::{HashMap, HashSet};

use indexmap::IndexMap;

use crate::{
    schema::{RangeKind, SchemaProvider},
    value::Value,
};

/// A non-string identifier, as a Python dict key: numbers compare by value (`True == 1 == 1.0`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum IdKey {
    /// `None`: an object without its identifier is keyed `(class, None)`.
    Null,
    Int(i64),
    Big(String),
    Float(u64),
}

/// The identifier of a lookup or registration, borrowed.
enum KeyRef<'a> {
    Str(&'a str),
    Other(IdKey),
}

/// `value` as a dict key; `None` for a kind of value never used as an identifier here.
fn key_of(value: &Value) -> Option<KeyRef<'_>> {
    Some(match value {
        Value::Str(s) => KeyRef::Str(s),
        Value::Null => KeyRef::Other(IdKey::Null),
        Value::Bool(b) => KeyRef::Other(IdKey::Int(i64::from(*b))),
        Value::Int(i) => KeyRef::Other(IdKey::Int(*i)),
        Value::BigInt(n) => KeyRef::Other(IdKey::Big(n.to_string())),
        Value::Float(f) if f.fract() == 0.0 && f.abs() < 9.0e18 => {
            KeyRef::Other(IdKey::Int(*f as i64))
        }
        Value::Float(f) => KeyRef::Other(IdKey::Float(f.to_bits())),
        _ => return None,
    })
}

/// The objects of one class, by identifier.
#[derive(Debug, Default, Clone)]
struct ClassObjects {
    /// String identifiers, so a lookup by `&str` allocates nothing.
    by_str: HashMap<String, Value>,
    other: HashMap<IdKey, Value>,
}

impl ClassObjects {
    fn get(&self, key: &KeyRef<'_>) -> Option<&Value> {
        match key {
            KeyRef::Str(s) => self.by_str.get(*s),
            KeyRef::Other(k) => self.other.get(k),
        }
    }

    fn insert(&mut self, key: KeyRef<'_>, obj: Value) {
        match key {
            KeyRef::Str(s) => {
                self.by_str.insert(s.to_owned(), obj);
            }
            KeyRef::Other(k) => {
                self.other.insert(k, obj);
            }
        }
    }
}

/// How far a reached object's own references are followed (see [`ObjectIndex::with_expansion`]).
#[derive(Debug, Default, Clone)]
struct Expansion {
    /// Attribute names the spec's expressions access (`x.<name>`).
    names: HashSet<String>,
    /// References followed below the bound value.
    depth: usize,
}

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
    /// Typed `(class_name, identifier) -> resolved object`, identifiers as strings.
    by_class_id: HashMap<(String, String), Value>,
    /// Upstream's `_source_object_cache`: `class name -> identifier -> object`, built from the
    /// schema (see the module docs); empty when the index was built structurally.
    typed: HashMap<String, ClassObjects>,
    /// Whether [`ObjectIndex::typed`] was built (a schema and a container class were known).
    typed_built: bool,
    /// Per class, the slots whose range is a class: `class -> slot -> range class`.
    ref_slots: HashMap<String, HashMap<String, String>>,
    expansion: Expansion,
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
    /// Build an index by scanning a container object.
    ///
    /// `container` is the whole source dataset/root object (e.g. a
    /// `MappingSet`). `container_type` names its class. When `schema` knows that class the index
    /// is upstream's typed one (see the module docs); otherwise it falls back to structural
    /// inference over the container's collections.
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

        if let (Some(sp), Some(class)) = (schema, container_type)
            && sp.get_class(class).is_ok()
        {
            idx.index_typed(sp, container, class);
            return idx;
        }

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

    /// Follow references below a bound value: `names` are the attributes the spec's expressions
    /// access and `depth` the longest chain of accesses (see `fk_expansion`).
    ///
    /// Upstream's `ProxyObject` dereferences a reference on **every** attribute access, so
    /// `{boss.boss.label}` reads the boss's boss. A [`Value`] cannot dereference lazily, so the
    /// referenced objects are expanded eagerly instead, only along the attributes the
    /// expressions name and only as deep as their longest chain.
    #[must_use]
    pub fn with_expansion(mut self, names: HashSet<String>, depth: usize) -> Self {
        self.expansion = Expansion { names, depth };
        self
    }

    // ── typed index (`ObjectIndex._index` over `dynamic_object`) ───────────────

    fn index_typed(&mut self, sp: &dyn SchemaProvider, container: &Value, class: &str) {
        for name in sp.all_class_names() {
            let refs: HashMap<String, String> = sp
                .induced_slots(&name)
                .unwrap_or_default()
                .into_iter()
                .filter_map(|s| s.range_class().map(|rc| (s.name.clone(), rc.to_owned())))
                .collect();
            if !refs.is_empty() {
                self.ref_slots.insert(name, refs);
            }
        }
        self.typed_built = true;
        self.register_object(sp, container, class, None);
    }

    /// `dynamic_object` + `_index` for one object: its slots are indexed (children first, as the
    /// objects of a dict-keyed collection take their identifier from the key), then the object
    /// itself under `(class, identifier)`. Returns the object as `dynamic_object` builds it.
    fn register_object(
        &mut self,
        sp: &dyn SchemaProvider,
        obj: &Value,
        class: &str,
        key_as_id: Option<(&str, &str)>,
    ) -> Value {
        let Value::Map(m) = obj else {
            return obj.clone();
        };
        let mut built: IndexMap<String, Value> = IndexMap::with_capacity(m.len() + 1);
        for (k, v) in m {
            let nv = match sp.induced_slot(k, class) {
                Ok(slot) => self.register_slot_value(sp, v, &slot),
                // `dynamic_object` raises on a slot the schema does not know; keep the data
                // reachable the way the structural index would.
                Err(_) => {
                    self.index_collection(v, None, None, Some(sp));
                    v.clone()
                }
            };
            built.insert(k.clone(), nv);
        }
        if let Some((id_slot, key)) = key_as_id {
            // PARITY: `setattr(v1, id_slot.name, k1)` replaces an identifier the object carries.
            built.insert(id_slot.to_owned(), Value::Str(key.to_owned()));
        }
        let built = Value::Map(built);
        // PARITY: `_key` uses `get_identifier_slot(cls.name)` (a `key` slot does not count) and
        // `getattr` answers a missing identifier with `None`.
        if let Some(id_slot) = sp.identifier_slot_without_key(class) {
            let id = match &built {
                Value::Map(b) => b.get(&id_slot).unwrap_or(&Value::Null),
                _ => &Value::Null,
            };
            self.insert_typed(class, id, &built);
        }
        built
    }

    fn register_slot_value(
        &mut self,
        sp: &dyn SchemaProvider,
        v: &Value,
        slot: &crate::schema::SlotDef,
    ) -> Value {
        // A type or an enum is returned as it is; so is a class the schema does not define.
        let Some(range) = slot.range_class().filter(|rc| sp.get_class(rc).is_ok()) else {
            return v.clone();
        };
        if !slot.multivalued {
            return match v {
                Value::Map(_) => self.register_object(sp, v, range, None),
                other => other.clone(),
            };
        }
        match v {
            Value::List(items) => Value::List(
                items
                    .iter()
                    .map(|item| self.register_object(sp, item, range, None))
                    .collect(),
            ),
            Value::Map(entries) => {
                // PARITY: the identifier (or key slot) takes the dict key (`use_key=True`).
                let id_slot = sp.identifier_slot(range).ok().flatten().map(|s| s.name);
                Value::Map(
                    entries
                        .iter()
                        .map(|(key, item)| {
                            let key_as_id = id_slot.as_deref().map(|s| (s, key.as_str()));
                            let item_is_object = matches!(item, Value::Map(_));
                            let built = self.register_object(
                                sp,
                                item,
                                range,
                                key_as_id.filter(|_| item_is_object),
                            );
                            (key.clone(), built)
                        })
                        .collect(),
                )
            }
            other => other.clone(),
        }
    }

    fn insert_typed(&mut self, class: &str, id: &Value, obj: &Value) {
        let Some(key) = key_of(id) else { return };
        if let Some(id_str) = value_as_id_string(id) {
            self.by_class_id
                .insert((class.to_owned(), id_str.clone()), obj.clone());
            self.by_id.entry(id_str).or_insert_with(|| obj.clone());
        }
        self.typed
            .entry(class.to_owned())
            .or_default()
            .insert(key, obj.clone());
    }

    // ── lookups ──────────────────────────────────────────────────────────────────

    /// Upstream's `_source_object_cache.get((class, id))`: the object of exactly this class whose
    /// identifier is exactly `id` (`1` is not `"1"`). `None` when the index was built without a
    /// schema, whose lookups are by identifier string: [`ObjectIndex::get`].
    pub fn get_typed(&self, class: &str, id: &Value) -> Option<&Value> {
        let key = key_of(id)?;
        self.typed.get(class)?.get(&key)
    }

    /// `ProxyObject._map` for a reference: the object `v` refers to, or `v` itself when the
    /// reference is dangling (a null one is); an inlined object and a list are mapped in turn. `range` is the
    /// slot's range class and `depth` how many references to follow below the result.
    pub fn map_reference(&self, v: &Value, range: &str, depth: usize) -> Value {
        match v {
            Value::List(items) => Value::List(
                items
                    .iter()
                    .map(|item| self.map_reference(item, range, depth))
                    .collect(),
            ),
            Value::Map(_) => self.expand(v, range, depth),
            Value::Str(_) | Value::Int(_) | Value::BigInt(_) | Value::Float(_) | Value::Bool(_) => {
                match self.get_typed(range, v) {
                    Some(obj) => self.expand(obj, range, depth),
                    None => v.clone(),
                }
            }
            other => other.clone(),
        }
    }

    /// [`ObjectIndex::map_reference`] for the bound value of a source slot, followed as deep as
    /// [`ObjectIndex::with_expansion`] says.
    pub fn bind_reference(&self, v: &Value, range: &str) -> Value {
        self.map_reference(v, range, self.expansion.depth)
    }

    /// `obj` (of class `class`) with the references it holds along the accessed attributes
    /// replaced by their objects, `depth` levels deep.
    fn expand(&self, obj: &Value, class: &str, depth: usize) -> Value {
        let (Value::Map(m), true) = (obj, depth > 0) else {
            return obj.clone();
        };
        let Some(refs) = self.ref_slots.get(class) else {
            return obj.clone();
        };
        let follow = |attr: &String| self.expansion.names.contains(attr) && m.contains_key(attr);
        if !refs.keys().any(follow) {
            return obj.clone();
        }
        let mut out = m.clone();
        for (attr, range) in refs {
            if follow(attr)
                && let Some(slot) = out.get_mut(attr)
            {
                *slot = self.map_reference(slot, range, depth - 1);
            }
        }
        Value::Map(out)
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
        if let Some(c) = class
            && let Some(v) = self.by_class_id.get(&(c.to_string(), id.to_string()))
        {
            return Some(v);
        }
        self.by_id.get(id)
    }

    /// Look up an object by exactly its class and identifier, without
    /// [`ObjectIndex::get`]'s fallback to an object of another class: upstream's
    /// `_source_object_cache.get((class, id))`.
    // gate:internal — engine-only helper, not part of the binding-consumed surface.
    pub fn get_in_class(&self, class: &str, id: &str) -> Option<&Value> {
        if self.typed_built {
            // The key is the *string* `str(fk_value)`: an object whose identifier is an integer
            // is never found by it.
            return self.typed.get(class)?.by_str.get(id);
        }
        self.by_class_id.get(&(class.to_string(), id.to_string()))
    }

    /// True if `id` is a known identifier in the flat index.
    pub fn contains_id(&self, id: &str) -> bool {
        self.by_id.contains_key(id)
    }

    /// Whether the index is upstream's typed one: a schema and the container's class were known
    /// when it was built (see the module docs).
    pub fn is_typed(&self) -> bool {
        self.typed_built
    }

    /// True if the index holds no objects.
    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty() && self.typed.is_empty()
    }

    /// Number of objects in the flat index.
    pub fn len(&self) -> usize {
        self.by_id.len()
    }
}

/// The failure of `dynamic_object(obj, sv, class)`, which `ObjectTransformer.index` builds its tree
/// with: a key the schema does not know on a class aborts the run before any row is mapped,
/// whatever the spec reads. `None` when `obj` is acceptable.
///
/// Mirrors `linkml_map.utils.dynamic_object.dynamic_object`: a type or an enum, and a value
/// that is not a mapping, are returned as they are; every key of a mapping must be an induced
/// slot of its class, and the value is checked against the slot's class range, as a list or
/// dict of objects when multivalued.
pub fn dynamic_object_error(sp: &dyn SchemaProvider, obj: &Value, class: &str) -> Option<String> {
    if sp.has_enum(class) || sp.has_type(class) {
        return None;
    }
    let Value::Map(m) = obj else { return None };
    for (k, v) in m {
        let Ok(slot) = sp.induced_slot(k, class) else {
            return Some(if sp.get_class(class).is_ok() {
                format!(
                    "ValueError: No such slot {k} as an attribute of {class} ancestors or as a slot definition in the schema"
                )
            } else {
                format!("ValueError: Class: \"{class}\" not found")
            });
        };
        let RangeKind::Class(range) = &slot.range else {
            continue;
        };
        let items: Vec<&Value> = match v {
            Value::List(items) if slot.multivalued => items.iter().collect(),
            Value::Map(entries) if slot.multivalued => entries.values().collect(),
            other if !slot.multivalued => vec![other],
            _ => continue,
        };
        if let Some(err) = items
            .into_iter()
            .find_map(|item| dynamic_object_error(sp, item, range))
        {
            return Some(err);
        }
    }
    None
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
        Value::BigInt(n) => Some(n.to_string()),
        _ => None,
    }
}
