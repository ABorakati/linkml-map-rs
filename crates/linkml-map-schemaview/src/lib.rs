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
        Self::load_from_path_with_patch(path, None)
    }

    /// Load a LinkML schema YAML from a file path, applying an optional
    /// `source_schema_patches` block (LinkML-shaped JSON) before the schema is
    /// indexed. See [`apply_schema_patch_to_definition`].
    pub fn load_from_path_with_patch(
        path: &Path,
        patch: Option<&serde_json::Value>,
    ) -> anyhow::Result<Self> {
        let mut schema = load_schema_definition_from_path(path)
            .map_err(|e| anyhow::anyhow!("failed to load schema from {}: {}", path.display(), e))?;

        if let Some(patch) = patch {
            apply_schema_patch_to_definition(&mut schema, patch)?;
        }

        let mut sv = SchemaView::new();
        sv.add_schema(schema)
            .map_err(|e| anyhow::anyhow!("SchemaView::add_schema failed: {}", e))?;

        Ok(Self { sv })
    }

    /// Load a LinkML schema from a YAML string.
    pub fn from_yaml_str(yaml: &str) -> anyhow::Result<Self> {
        Self::from_yaml_str_with_patch(yaml, None)
    }

    /// Load a LinkML schema from a YAML string, applying an optional
    /// `source_schema_patches` block before indexing.
    pub fn from_yaml_str_with_patch(
        yaml: &str,
        patch: Option<&serde_json::Value>,
    ) -> anyhow::Result<Self> {
        let mut schema = load_schema_definition_from_str(yaml)
            .map_err(|e| anyhow::anyhow!("failed to parse schema YAML: {}", e))?;

        if let Some(patch) = patch {
            apply_schema_patch_to_definition(&mut schema, patch)?;
        }

        let mut sv = SchemaView::new();
        sv.add_schema(schema)
            .map_err(|e| anyhow::anyhow!("SchemaView::add_schema failed: {}", e))?;

        Ok(Self { sv })
    }
}

// ── source_schema_patches support ──────────────────────────────────────────────

fn load_schema_definition_from_path(path: &Path) -> anyhow::Result<linkml_meta::SchemaDefinition> {
    let yaml = std::fs::read_to_string(path)?;
    load_schema_definition_from_str(&yaml)
}

fn load_schema_definition_from_str(yaml: &str) -> anyhow::Result<linkml_meta::SchemaDefinition> {
    let mut value: serde_json::Value = serde_yaml::from_str(yaml)?;
    normalise_python_compatible_metaslots(&mut value, false);
    let mut schema: linkml_meta::SchemaDefinition = serde_json::from_value(value)?;
    resolve_default_curi_maps(&mut schema);
    Ok(schema)
}

// ── builtin prefix resolution (default_curi_maps + builtin `linkml:` imports) ────
//
// The vendored `linkml_schemaview` backend never reads `default_curi_maps`, and it
// does not fetch/merge the prefixes of builtin `linkml:` imports (`linkml:types`
// etc.). Its `converter_from_schemas` (identifier.rs) only registers prefixes
// declared inline under `schema.prefixes`, plus a hardcoded `rdfs`/`rdf`/`dcterms`
// fallback. So a schema that omits e.g. `schema:`/`xsd:` from `prefixes:` and
// relies on `default_curi_maps: [semweb_context]` and/or `imports: [linkml:types]`
// to supply them makes `add_schema` fail with `CurieError(NotFound("schema"))`
// while indexing a `slot_uri: schema:...` (see `tests/golden/flattening`).
//
// Python `linkml_runtime.SchemaView` builds its converter from ALL loaded schemas
// (the main schema + every resolved import) via `SchemaLoader.load()`
// (linkml `utils/schemaloader.py`): it seeds the namespace from each schema's
// explicit `prefixes`, then runs
// `for cmap in default_curi_maps: namespaces.add_prefixmap(cmap, include_defaults=False)`.
// `Namespaces.add_prefixmap` (linkml_runtime `utils/namespaces.py`) adds each
// prefix **only if not already defined** (`elif k not in self`), and imported
// schemas' prefixes are likewise merged without clobbering the importer's.
//
// We reproduce the *prefix side* of that resolution here (the only part
// `add_schema` needs) by expanding the known builtin curie maps and builtin
// `linkml:` import prefixes into `schema.prefixes` before `add_schema`, never
// overriding a prefix that is already present — matching the `k not in self`
// precedence exactly. We do NOT attempt to load import *content*; the vendored
// backend already tolerates unresolved builtin imports for everything else.
fn resolve_default_curi_maps(schema: &mut linkml_meta::SchemaDefinition) {
    // Collect (prefix, uri) contributions in Python's merge order: explicit
    // `prefixes:` win, then `default_curi_maps`, then builtin `linkml:` imports.
    // A single non-overriding merge into `prefixes` realises that precedence.
    let curi_maps = schema.default_curi_maps.clone().unwrap_or_default();
    let imports = schema.imports.clone().unwrap_or_default();
    if curi_maps.is_empty() && imports.is_empty() {
        return;
    }

    let mut contributions: Vec<(&'static str, &'static str)> = Vec::new();
    for map_name in &curi_maps {
        if let Some(entries) = builtin_curi_map(map_name) {
            contributions.extend_from_slice(entries);
        }
        // Unknown map names (e.g. a `prefixmaps` context we don't bundle) are left
        // for the backend, matching its existing "unresolved is not fatal here"
        // behaviour for anything we can't supply.
    }
    for import in &imports {
        if let Some(entries) = builtin_import_prefixes(import) {
            contributions.extend_from_slice(entries);
        }
    }
    if contributions.is_empty() {
        return;
    }

    let prefixes = schema.prefixes.get_or_insert_with(Default::default);
    for (pfx, reference) in contributions {
        // PARITY: linkml_runtime Namespaces.add_prefixmap only adds a prefix when
        // `k not in self`, and imported-schema prefixes never clobber the
        // importer's — so an explicit `prefixes:` entry (and any earlier
        // contributor) always wins over a later one.
        prefixes
            .entry(pfx.to_string())
            .or_insert_with(|| linkml_meta::Prefix {
                prefix_prefix: pfx.to_string(),
                prefix_reference: reference.to_string(),
            });
    }
}

/// Return the prefix→URI pairs for a known builtin LinkML curie map, or `None`
/// if we don't bundle that map.
///
/// Currently only `semweb_context` is bundled — the sole `default_curi_maps`
/// entry the LinkML schemas in this repo declare, and a `BIOCONTEXT_CONTEXTS`
/// name. Its contents mirror `prefixcommons`'
/// `prefixcommons/registry/semweb_context.jsonld` (read by `linkml_runtime` via
/// `curie_util.read_biocontext`), verbatim.
fn builtin_curi_map(name: &str) -> Option<&'static [(&'static str, &'static str)]> {
    match name {
        "semweb_context" => Some(SEMWEB_CONTEXT),
        _ => None,
    }
}

/// Return the prefixes contributed by a builtin `linkml:` import that the
/// vendored backend cannot fetch, or `None` for anything we don't bundle
/// (local imports are resolved by the backend; other remote imports are left
/// unresolved exactly as before).
///
/// Only `linkml:types` is bundled — it is the sole builtin import the repo's
/// fixtures use, and (as in real Python) it is imported to make `xsd:`/`schema:`
/// resolvable. Its prefixes mirror `linkml_runtime`'s
/// `linkml_model/model/schema/types.yaml` `prefixes:` block, verbatim.
fn builtin_import_prefixes(import: &str) -> Option<&'static [(&'static str, &'static str)]> {
    match import {
        "linkml:types" => Some(LINKML_TYPES_PREFIXES),
        _ => None,
    }
}

/// The `semweb_context` prefix map, copied from `prefixcommons`
/// `prefixcommons/registry/semweb_context.jsonld` (the `@context` object).
/// This is what `linkml_runtime`'s `default_curi_maps: [semweb_context]` expands
/// to. All keys are valid NCNames, so all pass `is_ncname` and are registered.
const SEMWEB_CONTEXT: &[(&str, &str)] = &[
    ("dc", "http://purl.org/dc/terms/"),
    ("dcat", "http://www.w3.org/ns/dcat#"),
    ("dcterms", "http://purl.org/dc/terms/"),
    ("faldo", "http://biohackathon.org/resource/faldo#"),
    ("foaf", "http://xmlns.com/foaf/0.1/"),
    ("idot", "http://identifiers.org/"),
    ("oa", "http://www.w3.org/ns/oa#"),
    ("owl", "http://www.w3.org/2002/07/owl#"),
    ("prov", "http://www.w3.org/ns/prov#"),
    ("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
    ("rdfs", "http://www.w3.org/2000/01/rdf-schema#"),
    ("void", "http://rdfs.org/ns/void#"),
    ("xsd", "http://www.w3.org/2001/XMLSchema#"),
    ("oboInOwl", "http://www.geneontology.org/formats/oboInOwl#"),
];

/// The `prefixes:` block of LinkML's builtin `types` schema, copied from
/// `linkml_runtime`'s `linkml_model/model/schema/types.yaml`. Importing
/// `linkml:types` is how a schema makes `schema:`/`xsd:` resolvable without
/// listing them inline.
const LINKML_TYPES_PREFIXES: &[(&str, &str)] = &[
    ("linkml", "https://w3id.org/linkml/"),
    ("xsd", "http://www.w3.org/2001/XMLSchema#"),
    ("shex", "http://www.w3.org/ns/shex#"),
    ("schema", "http://schema.org/"),
];

fn normalise_python_compatible_metaslots(value: &mut serde_json::Value, in_examples: bool) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                if key == "deprecated" {
                    if let Some(b) = child.as_bool() {
                        *child = serde_json::Value::String(b.to_string());
                        continue;
                    }
                }
                if key == "in_subset" && child.is_string() {
                    *child = serde_json::Value::Array(vec![child.clone()]);
                    continue;
                }
                if in_examples && key == "value" && !child.is_string() && !child.is_object() {
                    *child = serde_json::Value::String(match child {
                        serde_json::Value::Null => String::new(),
                        serde_json::Value::Bool(b) => b.to_string(),
                        serde_json::Value::Number(n) => n.to_string(),
                        serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
                            unreachable!()
                        }
                        serde_json::Value::String(_) => unreachable!(),
                    });
                    continue;
                }
                normalise_python_compatible_metaslots(child, key == "examples");
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                normalise_python_compatible_metaslots(item, in_examples);
            }
        }
        _ => {}
    }
}

type JsonMap = serde_json::Map<String, serde_json::Value>;

/// Apply a `source_schema_patches` block (LinkML-shaped JSON) to a parsed
/// [`linkml_meta::SchemaDefinition`] in place.
///
/// Mirrors the Python `linkml_map.utils.schema_patch.apply_schema_patch`
/// semantics: it is **additive** — append `slots`, create-or-update `attributes`,
/// `classes`, `slots`, `enums`, `types`, `subsets`; append unique `imports`; set
/// `prefixes`; and set the scalar header fields (`id`, `name`, `description`,
/// `default_prefix`). Its main use is augmenting auto-generated source schemas
/// (which lack foreign-key `range`s) so object joins resolve.
///
/// Implemented as a serde_json round-trip so it stays faithful to the metamodel
/// without enumerating every field by hand.
pub fn apply_schema_patch_to_definition(
    schema: &mut linkml_meta::SchemaDefinition,
    patch: &serde_json::Value,
) -> anyhow::Result<()> {
    if patch.is_null() {
        return Ok(());
    }
    let patch_obj = patch
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("source_schema_patches must be a mapping/object"))?;

    let mut doc = serde_json::to_value(&*schema)
        .map_err(|e| anyhow::anyhow!("failed to serialise source schema for patching: {e}"))?;
    {
        let doc_obj = doc
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("source schema did not serialise to a JSON object"))?;
        merge_schema_patch(doc_obj, patch_obj);
    }
    *schema = serde_json::from_value(doc)
        .map_err(|e| anyhow::anyhow!("failed to apply source_schema_patches: {e}"))?;
    Ok(())
}

fn merge_schema_patch(doc: &mut JsonMap, patch: &JsonMap) {
    use serde_json::Value;

    // Scalar header fields: set.
    for field in ["id", "name", "description", "default_prefix"] {
        if let Some(v) = patch.get(field) {
            doc.insert(field.to_string(), v.clone());
        }
    }

    // imports: append unique.
    if let Some(Value::Array(imports)) = patch.get("imports") {
        let arr = doc
            .entry("imports".to_string())
            .or_insert_with(|| Value::Array(Vec::new()));
        if let Some(a) = arr.as_array_mut() {
            for imp in imports {
                if !a.contains(imp) {
                    a.push(imp.clone());
                }
            }
        }
    }

    // prefixes: create-or-set per prefix.
    if let Some(Value::Object(prefixes)) = patch.get("prefixes") {
        let target = doc
            .entry("prefixes".to_string())
            .or_insert_with(|| Value::Object(JsonMap::new()));
        if let Some(t) = target.as_object_mut() {
            for (name, pdef) in prefixes {
                t.insert(name.clone(), pdef.clone());
            }
        }
    }

    // classes: create-or-merge, with slot append + attribute merge.
    if let Some(Value::Object(classes)) = patch.get("classes") {
        let target = doc
            .entry("classes".to_string())
            .or_insert_with(|| Value::Object(JsonMap::new()));
        if let Some(t) = target.as_object_mut() {
            for (cname, cpatch) in classes {
                let entry = t
                    .entry(cname.clone())
                    .or_insert_with(|| Value::Object(named_object(cname)));
                merge_class_patch(entry, cpatch);
            }
        }
    }

    // Simple create-or-update definition maps. The patch uses LinkML names; the
    // serialised SchemaDefinition names top-level slots `slot_definitions`
    // (serde alias `slots`), so map that one key. The rest match 1:1.
    for (patch_key, doc_key) in [
        ("slots", "slot_definitions"),
        ("enums", "enums"),
        ("types", "types"),
        ("subsets", "subsets"),
    ] {
        if let Some(Value::Object(items)) = patch.get(patch_key) {
            let target = doc
                .entry(doc_key.to_string())
                .or_insert_with(|| Value::Object(JsonMap::new()));
            if let Some(t) = target.as_object_mut() {
                for (name, pdef) in items {
                    match t.get_mut(name) {
                        Some(existing) => set_fields(existing, pdef),
                        None => {
                            let mut created = Value::Object(named_object(name));
                            set_fields(&mut created, pdef);
                            t.insert(name.clone(), created);
                        }
                    }
                }
            }
        }
    }
}

fn merge_class_patch(class: &mut serde_json::Value, cpatch: &serde_json::Value) {
    use serde_json::Value;
    let (Some(cobj), Some(cp)) = (class.as_object_mut(), cpatch.as_object()) else {
        return;
    };
    for (key, val) in cp {
        match key.as_str() {
            "slots" => {
                let arr = cobj
                    .entry("slots".to_string())
                    .or_insert_with(|| Value::Array(Vec::new()));
                if let (Some(a), Some(ps)) = (arr.as_array_mut(), val.as_array()) {
                    for s in ps {
                        if !a.contains(s) {
                            a.push(s.clone());
                        }
                    }
                }
            }
            "attributes" => {
                let attrs = cobj
                    .entry("attributes".to_string())
                    .or_insert_with(|| Value::Object(JsonMap::new()));
                if let (Some(am), Some(pm)) = (attrs.as_object_mut(), val.as_object()) {
                    for (an, ad) in pm {
                        match am.get_mut(an) {
                            Some(existing) => set_fields(existing, ad),
                            None => {
                                let mut created = Value::Object(named_object(an));
                                set_fields(&mut created, ad);
                                am.insert(an.clone(), created);
                            }
                        }
                    }
                }
            }
            // Other class-level fields (is_a, mixins, tree_root, ...) are set.
            _ => {
                cobj.insert(key.clone(), val.clone());
            }
        }
    }
}

/// Shallow-set each field from `patch` onto `target` (mirrors Python `setattr`).
fn set_fields(target: &mut serde_json::Value, patch: &serde_json::Value) {
    if let (Some(t), Some(p)) = (target.as_object_mut(), patch.as_object()) {
        for (k, v) in p {
            t.insert(k.clone(), v.clone());
        }
    }
}

/// A fresh JSON object carrying the required `name` field.
fn named_object(name: &str) -> JsonMap {
    let mut m = JsonMap::new();
    m.insert(
        "name".to_string(),
        serde_json::Value::String(name.to_string()),
    );
    m
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
    fn slot_view_to_def(&self, slot_view: &linkml_schemaview::schemaview::SlotView) -> SlotDef {
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

        // -- unit: capture the metaslot scheme, mirroring Python's UnitSystem
        //    dispatch (ucum_code → UCUM, iec61360code → IEC61360, else pint). --
        let unit = def.unit.as_ref().and_then(|u| {
            use linkml_map_core::schema::{UnitRef, UnitSystem};
            if let Some(c) = u.ucum_code.clone() {
                Some(UnitRef {
                    code: c,
                    system: UnitSystem::Ucum,
                })
            } else if let Some(c) = u.iec61360code.clone() {
                Some(UnitRef {
                    code: c,
                    system: UnitSystem::Iec61360,
                })
            } else if let Some(c) = u.symbol.clone() {
                Some(UnitRef {
                    code: c,
                    system: UnitSystem::Other,
                })
            } else if let Some(c) = u.abbreviation.clone() {
                Some(UnitRef {
                    code: c,
                    system: UnitSystem::Other,
                })
            } else {
                u.descriptive_name.clone().map(|c| UnitRef {
                    code: c,
                    system: UnitSystem::Other,
                })
            }
        });

        // -- any_of enums: scan any_of branches for enum ranges --
        let any_of_enums = self.collect_any_of_enums(def);

        // -- inlined / inlined_as_list (needed for inverse-spec derivation) --
        let inlined = def.inlined.unwrap_or(false);
        let inlined_as_list = def.inlined_as_list.unwrap_or(false);

        SlotDef {
            name: slot_view.name.clone(),
            range,
            multivalued,
            inlined,
            inlined_as_list,
            required,
            identifier,
            key,
            unit,
            any_of_enums,
        }
    }

    /// Scan the `any_of` expressions on a slot definition and return the names
    /// of any enum ranges found there.
    fn collect_any_of_enums(&self, def: &linkml_meta::SlotDefinition) -> Vec<String> {
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
        let enum_def_raw = self
            .sv
            .get_enum_definition(&Identifier::Name(enum_name.to_owned()));

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

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod patch_tests {
    use super::*;
    use linkml_map_core::schema::{RangeKind, SchemaProvider};

    const SCHEMA: &str = r#"
id: https://example.org/s
name: s
prefixes:
  linkml: https://w3id.org/linkml/
default_range: string
classes:
  Donor:
    attributes:
      donor_id:
        identifier: true
        range: string
  Row:
    attributes:
      DONOR_ID:
        range: string
"#;

    #[test]
    fn patch_adds_fk_range() {
        // Without a patch, DONOR_ID resolves to a scalar type, not a class.
        let plain = SchemaViewProvider::from_yaml_str(SCHEMA).unwrap();
        let s0 = plain.induced_slot("DONOR_ID", "Row").unwrap();
        assert!(
            !matches!(s0.range, RangeKind::Class(_)),
            "unpatched range should not be a class: {:?}",
            s0.range
        );

        // The patch points DONOR_ID at the Donor class (the FK the inferred
        // schema lacked); the object join can now resolve.
        let patch = serde_json::json!({
            "classes": { "Row": { "attributes": { "DONOR_ID": { "range": "Donor" } } } }
        });
        let patched = SchemaViewProvider::from_yaml_str_with_patch(SCHEMA, Some(&patch)).unwrap();
        let s1 = patched.induced_slot("DONOR_ID", "Row").unwrap();
        assert!(
            matches!(s1.range, RangeKind::Class(ref c) if c == "Donor"),
            "patched range should be Class(Donor): {:?}",
            s1.range
        );
    }

    #[test]
    fn patch_creates_new_class_with_attribute() {
        // A patch can introduce a class the inferred schema never had, with its
        // own attributes — e.g. a lookup table referenced by an FK range.
        let patch = serde_json::json!({
            "classes": { "Extra": { "attributes": { "note": { "range": "string" } } } }
        });
        let patched = SchemaViewProvider::from_yaml_str_with_patch(SCHEMA, Some(&patch)).unwrap();
        assert!(patched.all_class_names().iter().any(|c| c == "Extra"));
        let slot = patched.induced_slot("note", "Extra").unwrap();
        assert_eq!(slot.name, "note");
    }

    #[test]
    fn null_patch_is_noop() {
        let mut schema: linkml_meta::SchemaDefinition = serde_yaml::from_str(SCHEMA).unwrap();
        apply_schema_patch_to_definition(&mut schema, &serde_json::Value::Null).unwrap();
        assert!(schema.classes.as_ref().unwrap().contains_key("Row"));
    }

    // A schema that declares neither `schema:` nor `xsd:` under `prefixes:` but
    // relies on `default_curi_maps: [semweb_context]` + `imports: [linkml:types]`
    // to make them resolvable. The vendored backend cannot resolve these on its
    // own, so this used to fail `add_schema` with `CurieError(NotFound("schema"))`.
    const CURI_MAP_SCHEMA: &str = r#"
id: https://example.org/mappings-norm
name: mappings_norm
default_curi_maps:
  - semweb_context
imports:
  - linkml:types
prefixes:
  mappings: https://example.org/mappings-norm/
  linkml: https://w3id.org/linkml/
default_prefix: mappings
default_range: string
classes:
  Entity:
    slots:
      - id
      - name
slots:
  id:
    identifier: true
    slot_uri: schema:identifier
  name:
    slot_uri: rdfs:label
"#;

    #[test]
    fn default_curi_maps_and_builtin_import_resolve_prefixes() {
        // schema: comes from linkml:types; rdfs: from semweb_context — neither is
        // declared inline, yet the strict loader now succeeds.
        let prov = SchemaViewProvider::from_yaml_str(CURI_MAP_SCHEMA).unwrap();
        assert!(prov.all_class_names().iter().any(|c| c == "Entity"));
    }

    #[test]
    fn explicit_prefix_wins_over_curi_map() {
        // PARITY: linkml_runtime's add_prefixmap only adds `k not in self`, so an
        // explicit `prefixes:` entry is never overridden by a curie map.
        let yaml = r#"
id: https://example.org/s
name: s
default_curi_maps:
  - semweb_context
prefixes:
  rdfs: http://example.org/OVERRIDDEN#
default_range: string
classes:
  C:
    attributes:
      x:
        range: string
"#;
        let mut schema: linkml_meta::SchemaDefinition = serde_yaml::from_str(yaml).unwrap();
        resolve_default_curi_maps(&mut schema);
        let rdfs = &schema.prefixes.as_ref().unwrap()["rdfs"];
        assert_eq!(rdfs.prefix_reference, "http://example.org/OVERRIDDEN#");
    }
}
