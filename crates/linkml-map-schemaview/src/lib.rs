//! `linkml-map-schemaview` — A [`SchemaProvider`] backed by the pure-Rust
//! `linkml_schemaview` crate.
//!
//! # Usage
//!
//! ```no_run
//! use std::path::Path;
//! use linkml_map_schemaview::SchemaViewProvider;
//! use linkml_map_core::schema::SchemaProvider;
//!
//! let provider = SchemaViewProvider::load_from_path(
//!     Path::new("my_schema.yaml")
//! ).unwrap();
//!
//! let class = provider.get_class("Person").unwrap();
//! println!("class: {}", class.name);
//! ```

use std::collections::HashSet;
use std::path::Path;

use linkml_map_core::schema::{
    ClassDef, EnumDef, PermissibleValue, RangeKind, SchemaError, SchemaProvider, SchemaResult,
    SlotDef,
};
use linkml_schemaview::identifier::Identifier;
use linkml_schemaview::schemaview::SchemaView;

/// A [`SchemaProvider`] backed by the pure-Rust `linkml_schemaview` crate.
///
/// Load with [`SchemaViewProvider::load_from_path`] or
/// [`SchemaViewProvider::from_yaml_str`].
pub struct SchemaViewProvider {
    sv: SchemaView,
    /// Name of the primary schema (reserved for future per-schema filtering).
    #[allow(dead_code)]
    primary_schema_id: String,
}

// ── Constructors ──────────────────────────────────────────────────────────────

impl SchemaViewProvider {
    /// Load a LinkML schema YAML from a file path.
    ///
    /// The file must be self-contained (i.e. all `imports` must be resolvable
    /// on the local filesystem relative to the file's directory, or absent).
    /// Schemas that import remote URLs (e.g. `linkml:types`) will fail unless
    /// the `resolve` feature is enabled on the backend crate.
    pub fn load_from_path(path: &Path) -> anyhow::Result<Self> {
        let schema = linkml_schemaview::io::from_yaml(path)
            .map_err(|e| anyhow::anyhow!("failed to load schema from {}: {}", path.display(), e))?;

        let primary_id = schema.id.clone();
        let mut sv = SchemaView::new();
        sv.add_schema(schema)
            .map_err(|e| anyhow::anyhow!("SchemaView::add_schema failed: {}", e))?;

        Ok(Self {
            sv,
            primary_schema_id: primary_id,
        })
    }

    /// Load a LinkML schema from a YAML string.
    pub fn from_yaml_str(yaml: &str) -> anyhow::Result<Self> {
        let schema: linkml_meta::SchemaDefinition = serde_yaml::from_str(yaml)
            .map_err(|e| anyhow::anyhow!("failed to parse schema YAML: {}", e))?;

        let primary_id = schema.id.clone();
        let mut sv = SchemaView::new();
        sv.add_schema(schema)
            .map_err(|e| anyhow::anyhow!("SchemaView::add_schema failed: {}", e))?;

        Ok(Self {
            sv,
            primary_schema_id: primary_id,
        })
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

impl SchemaViewProvider {
    /// Collect all class names across all loaded schemas.
    fn all_class_names_internal(&self) -> Vec<String> {
        let mut names = Vec::new();
        self.sv.with_schema_definitions(|schemas| {
            for schema in schemas.values() {
                if let Some(classes) = &schema.classes {
                    for name in classes.keys() {
                        names.push(name.clone());
                    }
                }
            }
        });
        names
    }

    /// Collect all enum names across all loaded schemas.
    fn all_enum_names_internal(&self) -> Vec<String> {
        let mut names = Vec::new();
        self.sv.with_schema_definitions(|schemas| {
            for schema in schemas.values() {
                if let Some(enums) = &schema.enums {
                    for name in enums.keys() {
                        names.push(name.clone());
                    }
                }
            }
        });
        names
    }

    /// Collect all type names across all loaded schemas.
    fn all_type_names_internal(&self) -> Vec<String> {
        let mut names = Vec::new();
        self.sv.with_schema_definitions(|schemas| {
            for schema in schemas.values() {
                if let Some(types) = &schema.types {
                    for name in types.keys() {
                        names.push(name.clone());
                    }
                }
            }
        });
        names
    }

    /// Classify a range name string into a `RangeKind` by checking whether
    /// it matches a known class, enum, or type across loaded schemas.
    ///
    /// Resolution order: class > enum > type > None.
    fn classify_range(&self, range_name: &str) -> RangeKind {
        // Check classes first
        let mut is_class = false;
        let mut is_enum = false;
        let mut is_type = false;

        self.sv.with_schema_definitions(|schemas| {
            for schema in schemas.values() {
                if schema
                    .classes
                    .as_ref()
                    .map(|m| m.contains_key(range_name))
                    .unwrap_or(false)
                {
                    is_class = true;
                }
                if schema
                    .enums
                    .as_ref()
                    .map(|m| m.contains_key(range_name))
                    .unwrap_or(false)
                {
                    is_enum = true;
                }
                if schema
                    .types
                    .as_ref()
                    .map(|m| m.contains_key(range_name))
                    .unwrap_or(false)
                {
                    is_type = true;
                }
            }
        });

        if is_class {
            RangeKind::Class(range_name.to_owned())
        } else if is_enum {
            RangeKind::Enum(range_name.to_owned())
        } else if is_type {
            RangeKind::Type(range_name.to_owned())
        } else {
            // Well-known built-in scalar types from linkml:types that won't be in
            // the schema definitions when the import isn't loaded.
            if is_builtin_type(range_name) {
                RangeKind::Type(range_name.to_owned())
            } else {
                RangeKind::None
            }
        }
    }

    /// Convert a `SlotView` (merged effective slot) into our `SlotDef`.
    fn slot_view_to_def(
        &self,
        slot_view: &linkml_schemaview::schemaview::SlotView,
    ) -> SlotDef {
        let def = slot_view.definition();

        // -- range --
        let range = match def.range.as_deref() {
            Some(r) => self.classify_range(r),
            None => RangeKind::None,
        };

        // -- multivalued / required / identifier / key --
        let multivalued = def.multivalued.unwrap_or(false);
        let required = def.required.unwrap_or(false);
        let identifier = def.identifier.unwrap_or(false);
        let key = def.key.unwrap_or(false);

        // -- unit: prefer ucum_code, fall back to symbol --
        let unit = def.unit.as_ref().and_then(|u| {
            u.ucum_code
                .clone()
                .or_else(|| u.symbol.clone())
                .or_else(|| u.abbreviation.clone())
        });

        // -- any_of enums: scan any_of branches for enum ranges --
        let any_of_enums = self.collect_any_of_enums(def);

        SlotDef {
            name: slot_view.name.clone(),
            range,
            multivalued,
            required,
            identifier,
            key,
            unit,
            any_of_enums,
        }
    }

    /// Scan the `any_of` expressions on a slot definition and return the names
    /// of any enum ranges found there.
    fn collect_any_of_enums(
        &self,
        def: &linkml_meta::SlotDefinition,
    ) -> Vec<String> {
        let mut result = Vec::new();
        let Some(any_of) = &def.any_of else {
            return result;
        };
        let enum_names: HashSet<_> = self.all_enum_names_internal().into_iter().collect();
        for expr in any_of {
            if let Some(range) = &expr.range {
                if enum_names.contains(range.as_str()) {
                    result.push(range.clone());
                }
            }
        }
        result
    }

    /// Fetch a `ClassView` by simple name using the SchemaView.
    fn get_class_view(
        &self,
        class_name: &str,
    ) -> SchemaResult<linkml_schemaview::classview::ClassView> {
        let conv = self.sv.converter();
        self.sv
            .get_class(&Identifier::Name(class_name.to_owned()), &conv)
            .map_err(|e| SchemaError::Other(format!("{e:?}")))?
            .ok_or_else(|| SchemaError::ClassNotFound(class_name.to_owned()))
    }
}

// ── SchemaProvider impl ───────────────────────────────────────────────────────

impl SchemaProvider for SchemaViewProvider {
    // ── Class queries ────────────────────────────────────────────────────────

    fn get_class(&self, class_name: &str) -> SchemaResult<ClassDef> {
        let cv = self.get_class_view(class_name)?;
        let class_def = cv.def();
        Ok(ClassDef {
            name: cv.name().to_owned(),
            tree_root: class_def.tree_root.unwrap_or(false),
            is_a: class_def.is_a.clone(),
            mixins: class_def.mixins.clone().unwrap_or_default(),
        })
    }

    fn all_class_names(&self) -> Vec<String> {
        self.all_class_names_internal()
    }

    fn induced_slots(&self, class_name: &str) -> SchemaResult<Vec<SlotDef>> {
        let cv = self.get_class_view(class_name)?;
        let slots = cv
            .slots()
            .iter()
            .map(|sv| self.slot_view_to_def(sv))
            .collect();
        Ok(slots)
    }

    fn identifier_slot(&self, class_name: &str) -> SchemaResult<Option<SlotDef>> {
        let cv = self.get_class_view(class_name)?;
        Ok(cv
            .key_or_identifier_slot()
            .map(|sv| self.slot_view_to_def(sv)))
    }

    // ── Slot queries ─────────────────────────────────────────────────────────

    fn induced_slot(&self, slot_name: &str, class_name: &str) -> SchemaResult<SlotDef> {
        let cv = self.get_class_view(class_name)?;
        cv.slots()
            .iter()
            .find(|sv| sv.name == slot_name)
            .map(|sv| self.slot_view_to_def(sv))
            .ok_or_else(|| SchemaError::SlotNotFound {
                class: class_name.to_owned(),
                slot: slot_name.to_owned(),
            })
    }

    // ── Enum queries ─────────────────────────────────────────────────────────

    fn get_enum(&self, enum_name: &str) -> SchemaResult<EnumDef> {
        let conv = self.sv.converter();
        let ev = self
            .sv
            .get_enum(&Identifier::Name(enum_name.to_owned()), &conv)
            .map_err(|e| SchemaError::Other(format!("{e:?}")))?
            .ok_or_else(|| SchemaError::EnumNotFound(enum_name.to_owned()))?;

        let pv_keys = ev
            .permissible_value_keys()
            .map_err(|e| SchemaError::Other(format!("{e:?}")))?;

        // Re-read the raw enum definition for descriptions and meanings.
        let enum_def_raw = self.sv.get_enum_definition(&Identifier::Name(enum_name.to_owned()));

        let pvs = pv_keys
            .iter()
            .map(|key| {
                let (description, meaning) = enum_def_raw
                    .as_ref()
                    .and_then(|ed| ed.permissible_values.as_ref())
                    .and_then(|pvmap| pvmap.get(key.as_str()))
                    .map(|pv| {
                        let desc = pv.description.clone();
                        let meaning = pv.meaning.clone();
                        (desc, meaning)
                    })
                    .unwrap_or((None, None));

                PermissibleValue {
                    text: key.clone(),
                    description,
                    meaning,
                }
            })
            .collect();

        Ok(EnumDef {
            name: enum_name.to_owned(),
            permissible_values: pvs,
        })
    }

    fn all_enum_names(&self) -> Vec<String> {
        self.all_enum_names_internal()
    }

    // ── Schema-level queries ─────────────────────────────────────────────────

    fn all_type_names(&self) -> Vec<String> {
        self.all_type_names_internal()
    }

    // ── CURIE / URI coercion ─────────────────────────────────────────────────

    fn expand_curie(&self, curie: &str) -> Option<String> {
        // Already an absolute URI — nothing to expand.
        if curie.contains("://") {
            return None;
        }
        let conv = self.sv.converter();
        conv.expand(curie).ok()
    }

    fn compress_uri(&self, uri: &str) -> Option<String> {
        // Already looks like a CURIE (no scheme) — nothing to compress.
        if !uri.contains("://") {
            return None;
        }
        let conv = self.sv.converter();
        conv.compress(uri).ok()
    }
}

// ── Built-in type recognition ─────────────────────────────────────────────────

/// Returns `true` for well-known LinkML built-in scalar type names that appear
/// in `linkml:types` but may not be loaded when imports aren't resolved.
fn is_builtin_type(name: &str) -> bool {
    matches!(
        name,
        "string"
            | "integer"
            | "boolean"
            | "float"
            | "double"
            | "decimal"
            | "time"
            | "date"
            | "datetime"
            | "date_or_datetime"
            | "uriorcurie"
            | "curie"
            | "uri"
            | "ncname"
            | "objectidentifier"
            | "nodeidentifier"
            | "jsonpointer"
            | "jsonpath"
            | "sparqlpath"
            | "int"
            | "Bool"
            | "Curie"
            | "Uri"
            | "Uriorcurie"
    )
}
