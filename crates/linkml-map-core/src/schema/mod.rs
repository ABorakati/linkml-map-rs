//! Schema introspection abstraction for the linkml-map transform engine.
//!
//! The engine needs to interrogate schema metadata at transform time — slot
//! ranges, cardinalities, enum values, identifier slots, etc. — without
//! depending on any particular schema backend (Python SchemaView, serde-parsed
//! YAML, an in-memory stub for tests, …).
//!
//! This module defines:
//! - Plain data structs (`ClassDef`, `SlotDef`, `EnumDef`, `PermissibleValue`)
//!   that carry exactly the fields the engine needs.
//! - The [`SchemaProvider`] trait abstracting the lookups.
//! - [`InMemorySchema`]: a builder-based in-process stub, dependency-free,
//!   used by unit tests and as a reference implementation.

use std::collections::HashMap;

// ── Data structs ─────────────────────────────────────────────────────────────

/// The kind of value a slot's `range` points to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RangeKind {
    /// A named LinkML class.
    Class(String),
    /// A scalar type (string, integer, float, boolean, date, …).
    Type(String),
    /// A named LinkML enum.
    Enum(String),
    /// No range declared (treated as `string` by the Python engine).
    None,
}

/// A single permissible value within an enum.
#[derive(Debug, Clone)]
pub struct PermissibleValue {
    /// The text value (key in the permissible_values dict).
    pub text: String,
    /// Optional human-readable description.
    pub description: Option<String>,
    /// Optional meaning CURIE / URI.
    pub meaning: Option<String>,
}

/// Minimal projection of a LinkML `EnumDefinition` that the engine needs.
#[derive(Debug, Clone)]
pub struct EnumDef {
    pub name: String,
    pub permissible_values: Vec<PermissibleValue>,
}

/// Which unit metaslot a [`UnitRef`] came from. Mirrors the Python
/// `UnitSystem` dispatch in `linkml_map.functions.unit_conversion`: `ucum_code`
/// → UCUM (ucumvert registry), `iec61360code` → IEC61360, everything else
/// (`symbol`/`abbreviation`/`descriptive_name`) → a plain pint registry that
/// does not understand UCUM-only spellings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UnitSystem {
    Ucum,
    Iec61360,
    #[default]
    Other,
}

/// A unit annotation plus the metaslot scheme it was declared under.
#[derive(Debug, Clone)]
pub struct UnitRef {
    pub code: String,
    pub system: UnitSystem,
}

/// Minimal projection of a LinkML `SlotDefinition` (induced / effective).
///
/// All fields here reflect the *induced* (inheritance-resolved) value as the
/// Python engine calls `sv.induced_slot(name, class_name)`.
#[derive(Debug, Clone)]
pub struct SlotDef {
    /// Slot name.
    pub name: String,
    /// Where the value lives (`RangeKind::Class`, `::Type`, `::Enum`, `::None`).
    pub range: RangeKind,
    /// True if the slot may hold multiple values (Python: `multivalued`).
    pub multivalued: bool,
    /// True if a class-ranged value is inlined (nested) rather than referenced.
    pub inlined: bool,
    /// True if an inlined multivalued slot is serialised as a list (not a dict).
    pub inlined_as_list: bool,
    /// True if the slot must be present (Python: `required`).
    pub required: bool,
    /// True if this slot is marked `identifier: true`.
    pub identifier: bool,
    /// True if this slot is marked `key: true` (unique within container).
    pub key: bool,
    /// Unit annotation, if present, with the metaslot it came from.
    pub unit: Option<UnitRef>,
    /// Any-of enum names when range is None/Any but `any_of` declares enums.
    pub any_of_enums: Vec<String>,
}

impl SlotDef {
    /// True for slots whose range is a class (not a scalar or enum).
    pub fn is_object_range(&self) -> bool {
        matches!(self.range, RangeKind::Class(_))
    }

    /// Returns the range class name, if this slot's range is a class.
    pub fn range_class(&self) -> Option<&str> {
        match &self.range {
            RangeKind::Class(c) => Some(c.as_str()),
            _ => None,
        }
    }

    /// Returns the range enum name, if this slot's range is an enum.
    pub fn range_enum(&self) -> Option<&str> {
        match &self.range {
            RangeKind::Enum(e) => Some(e.as_str()),
            _ => None,
        }
    }

    /// Returns the range type name, if this slot's range is a scalar type.
    pub fn range_type(&self) -> Option<&str> {
        match &self.range {
            RangeKind::Type(t) => Some(t.as_str()),
            _ => None,
        }
    }
}

/// Minimal projection of a LinkML `ClassDefinition`.
#[derive(Debug, Clone)]
pub struct ClassDef {
    pub name: String,
    /// True if this class is the schema's tree root.
    pub tree_root: bool,
    /// is_a parent class name, if any.
    pub is_a: Option<String>,
    /// Mixin class names.
    pub mixins: Vec<String>,
}

// ── Error type ────────────────────────────────────────────────────────────────

/// Errors returned by [`SchemaProvider`] methods.
#[derive(Debug, thiserror::Error)]
pub enum SchemaError {
    #[error("class not found: {0}")]
    ClassNotFound(String),
    #[error("slot not found: {slot} on class {class}")]
    SlotNotFound { class: String, slot: String },
    #[error("enum not found: {0}")]
    EnumNotFound(String),
    #[error("schema error: {0}")]
    Other(String),
}

pub type SchemaResult<T> = Result<T, SchemaError>;

// ── Trait ─────────────────────────────────────────────────────────────────────

/// Abstraction over LinkML schema introspection.
///
/// The transform engine calls exactly these methods at transform time.  Any
/// implementation — in-memory stub, serde-parsed YAML, or a wrapper around
/// the `linkml_schemaview` crate — only needs to satisfy this interface.
///
/// All methods take `&self` and return owned values so the trait can be used
/// behind `dyn SchemaProvider` without lifetime gymnastics.
pub trait SchemaProvider: Send + Sync {
    // ── Class-level queries ──────────────────────────────────────────────────

    /// Look up a class definition by name.
    ///
    /// Returns `Err(SchemaError::ClassNotFound)` if the name is unknown.
    fn get_class(&self, class_name: &str) -> SchemaResult<ClassDef>;

    /// Return all class names defined in the schema (unordered).
    fn all_class_names(&self) -> Vec<String>;

    /// Return the *induced* (inheritance-resolved) slots for a class.
    ///
    /// This mirrors `sv.class_induced_slots(class_name)` in Python: the
    /// result includes slots inherited from is_a parents and mixins, with
    /// slot_usage overrides applied.
    fn induced_slots(&self, class_name: &str) -> SchemaResult<Vec<SlotDef>>;

    /// Return the identifier slot (or key slot) for a class, if any.
    ///
    /// Mirrors `sv.get_identifier_slot(class_name, use_key=True)`.
    fn identifier_slot(&self, class_name: &str) -> SchemaResult<Option<SlotDef>>;

    // ── Slot-level queries ───────────────────────────────────────────────────

    /// Return the induced slot definition for a single named slot on a class.
    ///
    /// Mirrors `sv.induced_slot(slot_name, class_name)`.
    fn induced_slot(&self, slot_name: &str, class_name: &str) -> SchemaResult<SlotDef>;

    // ── Enum-level queries ───────────────────────────────────────────────────

    /// Look up an enum definition by name.
    fn get_enum(&self, enum_name: &str) -> SchemaResult<EnumDef>;

    /// Return all enum names defined in the schema (unordered).
    fn all_enum_names(&self) -> Vec<String>;

    // ── Schema-level queries ─────────────────────────────────────────────────

    /// Return the names of all top-level types (scalars) in the schema.
    fn all_type_names(&self) -> Vec<String>;

    /// Return the tree-root class name, if exactly one exists.
    ///
    /// Mirrors the pattern `[c.name for c in sv.all_classes().values() if c.tree_root]`.
    fn tree_root_class(&self) -> Option<String> {
        self.all_class_names()
            .into_iter()
            .find(|name| self.get_class(name).map(|c| c.tree_root).unwrap_or(false))
    }

    // ── CURIE / URI coercion helpers ─────────────────────────────────────────

    /// Expand a CURIE (e.g. `"P:1"`) to a full URI using the schema's prefix map.
    ///
    /// Returns `None` if the prefix is unknown or the provider has no prefix
    /// map.  Already-absolute URIs (starting with a scheme like `https://`)
    /// are also returned as `None` so the caller leaves them unchanged.
    ///
    /// The engine calls this when the target slot's `range` is `uri` or
    /// `uriorcurie`.  The default implementation is a safe no-op returning
    /// `None`; override in providers that carry a prefix map.
    fn expand_curie(&self, curie: &str) -> Option<String> {
        let _ = curie;
        None
    }

    /// Compress a full URI (e.g. `"https://example.org/foo"`) to a CURIE
    /// (e.g. `"example:foo"`) using the schema's prefix map.
    ///
    /// Returns `None` if no matching URI prefix exists or the provider has no
    /// prefix map.  The engine calls this when the target slot's `range` is
    /// `curie`.  The default implementation is a safe no-op returning `None`;
    /// override in providers that carry a prefix map.
    fn compress_uri(&self, uri: &str) -> Option<String> {
        let _ = uri;
        None
    }
}

// ── InMemorySchema ────────────────────────────────────────────────────────────

/// An in-process schema built from plain data structures.
///
/// Used by tests and as the reference implementation of [`SchemaProvider`].
/// Build with [`InMemorySchemaBuilder`].
#[derive(Debug, Default, Clone)]
pub struct InMemorySchema {
    classes: HashMap<String, ClassRecord>,
    enums: HashMap<String, EnumDef>,
    types: Vec<String>,
}

/// Internal record for a class + its induced-slot table.
#[derive(Debug, Clone)]
struct ClassRecord {
    def: ClassDef,
    /// Slots in declaration order.
    slots: Vec<SlotDef>,
}

/// Builder for [`InMemorySchema`].
#[derive(Debug, Default)]
pub struct InMemorySchemaBuilder {
    schema: InMemorySchema,
}

impl InMemorySchemaBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a class.  Slots can be added later with [`add_slot`].
    pub fn add_class(mut self, def: ClassDef) -> Self {
        self.schema.classes.insert(
            def.name.clone(),
            ClassRecord {
                def,
                slots: Vec::new(),
            },
        );
        self
    }

    /// Add an induced slot to an existing class.
    ///
    /// # Panics
    /// Panics if `class_name` was not registered first.
    pub fn add_slot(mut self, class_name: &str, slot: SlotDef) -> Self {
        self.schema
            .classes
            .get_mut(class_name)
            .unwrap_or_else(|| panic!("class not found: {class_name}"))
            .slots
            .push(slot);
        self
    }

    /// Register an enum.
    pub fn add_enum(mut self, def: EnumDef) -> Self {
        self.schema.enums.insert(def.name.clone(), def);
        self
    }

    /// Register a scalar type name.
    pub fn add_type(mut self, name: impl Into<String>) -> Self {
        self.schema.types.push(name.into());
        self
    }

    pub fn build(self) -> InMemorySchema {
        self.schema
    }
}

impl SchemaProvider for InMemorySchema {
    fn get_class(&self, class_name: &str) -> SchemaResult<ClassDef> {
        self.classes
            .get(class_name)
            .map(|r| r.def.clone())
            .ok_or_else(|| SchemaError::ClassNotFound(class_name.to_owned()))
    }

    fn all_class_names(&self) -> Vec<String> {
        self.classes.keys().cloned().collect()
    }

    fn induced_slots(&self, class_name: &str) -> SchemaResult<Vec<SlotDef>> {
        self.classes
            .get(class_name)
            .map(|r| r.slots.clone())
            .ok_or_else(|| SchemaError::ClassNotFound(class_name.to_owned()))
    }

    fn identifier_slot(&self, class_name: &str) -> SchemaResult<Option<SlotDef>> {
        let record = self
            .classes
            .get(class_name)
            .ok_or_else(|| SchemaError::ClassNotFound(class_name.to_owned()))?;
        Ok(record.slots.iter().find(|s| s.identifier || s.key).cloned())
    }

    fn induced_slot(&self, slot_name: &str, class_name: &str) -> SchemaResult<SlotDef> {
        let record = self
            .classes
            .get(class_name)
            .ok_or_else(|| SchemaError::ClassNotFound(class_name.to_owned()))?;
        record
            .slots
            .iter()
            .find(|s| s.name == slot_name)
            .cloned()
            .ok_or_else(|| SchemaError::SlotNotFound {
                class: class_name.to_owned(),
                slot: slot_name.to_owned(),
            })
    }

    fn get_enum(&self, enum_name: &str) -> SchemaResult<EnumDef> {
        self.enums
            .get(enum_name)
            .cloned()
            .ok_or_else(|| SchemaError::EnumNotFound(enum_name.to_owned()))
    }

    fn all_enum_names(&self) -> Vec<String> {
        self.enums.keys().cloned().collect()
    }

    fn all_type_names(&self) -> Vec<String> {
        self.types.clone()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a small schema used by multiple tests.
    ///
    /// Classes:
    ///   Person  (tree_root)
    ///     slots: id (identifier, string), name (string), age (integer),
    ///            status (StatusEnum), friends (Person, multivalued)
    ///   Address
    ///     slots: street (string, required), city (string)
    ///
    /// Enums:
    ///   StatusEnum  – Active, Inactive
    ///
    /// Types: string, integer
    fn build_test_schema() -> InMemorySchema {
        InMemorySchemaBuilder::new()
            // types
            .add_type("string")
            .add_type("integer")
            // enums
            .add_enum(EnumDef {
                name: "StatusEnum".into(),
                permissible_values: vec![
                    PermissibleValue {
                        text: "Active".into(),
                        description: Some("Currently active".into()),
                        meaning: Some("ex:Active".into()),
                    },
                    PermissibleValue {
                        text: "Inactive".into(),
                        description: None,
                        meaning: None,
                    },
                ],
            })
            // classes
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
            .add_slot(
                "Person",
                SlotDef {
                    name: "status".into(),
                    range: RangeKind::Enum("StatusEnum".into()),
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
                    required: true,
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
            .build()
    }

    #[test]
    fn get_class_returns_def() {
        let schema = build_test_schema();
        let cls = schema.get_class("Person").unwrap();
        assert_eq!(cls.name, "Person");
        assert!(cls.tree_root);
        assert!(cls.is_a.is_none());
    }

    #[test]
    fn get_class_unknown_is_err() {
        let schema = build_test_schema();
        let err = schema.get_class("NoSuchClass").unwrap_err();
        assert!(matches!(err, SchemaError::ClassNotFound(_)));
    }

    #[test]
    fn all_class_names_contains_registered_classes() {
        let schema = build_test_schema();
        let mut names = schema.all_class_names();
        names.sort();
        assert_eq!(names, vec!["Address", "Person"]);
    }

    #[test]
    fn tree_root_class_finds_root() {
        let schema = build_test_schema();
        assert_eq!(schema.tree_root_class(), Some("Person".to_owned()));
    }

    #[test]
    fn tree_root_class_none_when_no_root() {
        let schema = InMemorySchemaBuilder::new()
            .add_class(ClassDef {
                name: "A".into(),
                tree_root: false,
                is_a: None,
                mixins: vec![],
            })
            .build();
        assert!(schema.tree_root_class().is_none());
    }

    #[test]
    fn induced_slots_returns_all_slots() {
        let schema = build_test_schema();
        let slots = schema.induced_slots("Person").unwrap();
        let names: Vec<&str> = slots.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"id"));
        assert!(names.contains(&"name"));
        assert!(names.contains(&"age"));
        assert!(names.contains(&"status"));
        assert!(names.contains(&"friends"));
        assert_eq!(slots.len(), 5);
    }

    #[test]
    fn induced_slots_unknown_class_is_err() {
        let schema = build_test_schema();
        assert!(matches!(
            schema.induced_slots("Ghost").unwrap_err(),
            SchemaError::ClassNotFound(_)
        ));
    }

    #[test]
    fn induced_slot_returns_correct_slot() {
        let schema = build_test_schema();
        let slot = schema.induced_slot("age", "Person").unwrap();
        assert_eq!(slot.name, "age");
        assert_eq!(slot.range, RangeKind::Type("integer".into()));
        assert!(!slot.multivalued);
        assert!(!slot.required);
    }

    #[test]
    fn induced_slot_multivalued_flag() {
        let schema = build_test_schema();
        let slot = schema.induced_slot("friends", "Person").unwrap();
        assert!(slot.multivalued);
        assert_eq!(slot.range_class(), Some("Person"));
    }

    #[test]
    fn induced_slot_unknown_slot_is_err() {
        let schema = build_test_schema();
        assert!(matches!(
            schema.induced_slot("nonexistent", "Person").unwrap_err(),
            SchemaError::SlotNotFound { .. }
        ));
    }

    #[test]
    fn identifier_slot_found() {
        let schema = build_test_schema();
        let slot = schema.identifier_slot("Person").unwrap().unwrap();
        assert_eq!(slot.name, "id");
        assert!(slot.identifier);
    }

    #[test]
    fn identifier_slot_none_when_absent() {
        let schema = build_test_schema();
        let result = schema.identifier_slot("Address").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn identifier_slot_uses_key_flag() {
        let schema = InMemorySchemaBuilder::new()
            .add_class(ClassDef {
                name: "Thing".into(),
                tree_root: false,
                is_a: None,
                mixins: vec![],
            })
            .add_slot(
                "Thing",
                SlotDef {
                    name: "code".into(),
                    range: RangeKind::Type("string".into()),
                    multivalued: false,
                    required: true,
                    identifier: false,
                    key: true,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .build();
        let slot = schema.identifier_slot("Thing").unwrap().unwrap();
        assert_eq!(slot.name, "code");
        assert!(slot.key);
    }

    #[test]
    fn get_enum_returns_permissible_values() {
        let schema = build_test_schema();
        let e = schema.get_enum("StatusEnum").unwrap();
        assert_eq!(e.name, "StatusEnum");
        assert_eq!(e.permissible_values.len(), 2);
        let texts: Vec<&str> = e
            .permissible_values
            .iter()
            .map(|p| p.text.as_str())
            .collect();
        assert!(texts.contains(&"Active"));
        assert!(texts.contains(&"Inactive"));
    }

    #[test]
    fn get_enum_meaning_curie() {
        let schema = build_test_schema();
        let e = schema.get_enum("StatusEnum").unwrap();
        let active = e
            .permissible_values
            .iter()
            .find(|p| p.text == "Active")
            .unwrap();
        assert_eq!(active.meaning.as_deref(), Some("ex:Active"));
    }

    #[test]
    fn get_enum_unknown_is_err() {
        let schema = build_test_schema();
        assert!(matches!(
            schema.get_enum("NoEnum").unwrap_err(),
            SchemaError::EnumNotFound(_)
        ));
    }

    #[test]
    fn all_enum_names_returns_registered() {
        let schema = build_test_schema();
        assert_eq!(schema.all_enum_names(), vec!["StatusEnum"]);
    }

    #[test]
    fn all_type_names_contains_registered() {
        let schema = build_test_schema();
        let mut types = schema.all_type_names();
        types.sort();
        assert_eq!(types, vec!["integer", "string"]);
    }

    #[test]
    fn slot_range_helpers() {
        let schema = build_test_schema();

        let id_slot = schema.induced_slot("id", "Person").unwrap();
        assert_eq!(id_slot.range_type(), Some("string"));
        assert!(id_slot.range_class().is_none());
        assert!(id_slot.range_enum().is_none());
        assert!(!id_slot.is_object_range());

        let status_slot = schema.induced_slot("status", "Person").unwrap();
        assert_eq!(status_slot.range_enum(), Some("StatusEnum"));
        assert!(status_slot.range_class().is_none());
        assert!(status_slot.range_type().is_none());

        let friends_slot = schema.induced_slot("friends", "Person").unwrap();
        assert_eq!(friends_slot.range_class(), Some("Person"));
        assert!(friends_slot.is_object_range());
        assert!(friends_slot.range_type().is_none());
        assert!(friends_slot.range_enum().is_none());
    }

    #[test]
    fn trait_object_dispatch() {
        let schema = build_test_schema();
        let provider: &dyn SchemaProvider = &schema;
        assert!(provider.get_class("Person").is_ok());
        assert_eq!(provider.tree_root_class(), Some("Person".to_owned()));
    }

    #[test]
    fn any_of_enums_field_carried() {
        let schema = InMemorySchemaBuilder::new()
            .add_class(ClassDef {
                name: "Container".into(),
                tree_root: false,
                is_a: None,
                mixins: vec![],
            })
            .add_slot(
                "Container",
                SlotDef {
                    name: "value".into(),
                    range: RangeKind::None,
                    multivalued: false,
                    inlined: false,
                    inlined_as_list: false,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec!["ColorEnum".into(), "SizeEnum".into()],
                },
            )
            .build();

        let slot = schema.induced_slot("value", "Container").unwrap();
        assert_eq!(slot.any_of_enums, vec!["ColorEnum", "SizeEnum"]);
    }
}
