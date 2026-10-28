//! The slice of `linkml_runtime.SchemaView` that `SchemaMapper` uses.
//!
//! Upstream: `linkml_runtime/utils/schemaview.py` (`SchemaView.__init__`,
//! `load_import`, `imports_closure`, `all_classes`, `all_slots`, `all_enums`,
//! `get_class`, `get_slot`, `get_enum`). Unlike [`crate::schema::SchemaProvider`]
//! this keeps the *raw* metamodel objects (every field, in dataclass order), because
//! `SchemaMapper` copies whole definitions into the derived schema.

use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    rc::Rc,
};

use indexmap::IndexMap;

use super::metamodel::{MetamodelResult, Node, Object, Shared, err};
use crate::value::Value;

/// `linkml_runtime.SchemaView`, minus everything `SchemaMapper` does not call.
pub struct SchemaView {
    /// `schema_view.schema`: the root `SchemaDefinition`.
    pub schema: Shared<Object>,
    /// `schema_map`, keyed by closure key (`schema.name` for the root, the import
    /// string otherwise).
    schema_map: RefCell<IndexMap<String, Shared<Object>>>,
    /// Memoised `imports_closure`; upstream is `lru_cache`d, and its first call also
    /// injects `from_schema` into every element (`inject_metadata=True`).
    closure: RefCell<Option<Vec<String>>>,
    /// `get_slot` is `lru_cache`d upstream: a lookup is answered from the first result.
    slot_cache: RefCell<std::collections::HashMap<String, Option<Object>>>,
}

/// Parse YAML text with the loader upstream uses for schemas.
fn parse_yaml(text: &str) -> MetamodelResult<Value> {
    // PARITY: linkml_runtime reads schemas with PyYAML (YAML 1.1: `yes`/`no`/`on`/`off`
    // are booleans, unquoted dates are `datetime.date`); serde_yaml_ng follows YAML
    // 1.2 core scalars. Schemas rarely depend on the difference.
    let json: serde_json::Value = serde_yaml_ng::from_str(text)
        .map_err(|e| super::metamodel::MetamodelError(format!("parsing schema YAML: {e}")))?;
    serde_json::from_value(json)
        .map_err(|e| super::metamodel::MetamodelError(format!("parsing schema YAML: {e}")))
}

/// `YAMLLoader().load(path, target_class=SchemaDefinition)` followed by
/// `source_file` bookkeeping.
fn load_schema_text(text: &str, source_file: Option<&str>) -> MetamodelResult<Shared<Object>> {
    let value = parse_yaml(text)?;
    let Value::Map(map) = value else {
        return err("schema YAML must be a mapping");
    };
    let kwargs: IndexMap<String, Node> = map
        .iter()
        .map(|(k, v)| (k.clone(), Node::from_value(v)))
        .collect();
    let mut schema = Object::construct("SchemaDefinition", kwargs)?;
    if let Some(path) = source_file {
        schema.set("source_file", Node::str(path));
    }
    Ok(Rc::new(RefCell::new(schema)))
}

impl SchemaView {
    /// `SchemaView(path)`.
    pub fn from_path(path: &Path) -> MetamodelResult<SchemaView> {
        let text = std::fs::read_to_string(path).map_err(|e| {
            super::metamodel::MetamodelError(format!("reading {}: {e}", path.display()))
        })?;
        SchemaView::from_yaml_str(&text, Some(&path.to_string_lossy()))
    }

    /// `SchemaView(schema_text)`; `source_file` anchors relative imports.
    pub fn from_yaml_str(text: &str, source_file: Option<&str>) -> MetamodelResult<SchemaView> {
        let schema = load_schema_text(text, source_file)?;
        Ok(SchemaView::from_schema(schema))
    }

    /// `SchemaView(schema_definition)`.
    pub fn from_schema(schema: Shared<Object>) -> SchemaView {
        let name = schema.borrow().get("name").py_str();
        SchemaView {
            schema_map: RefCell::new(IndexMap::from([(name, Rc::clone(&schema))])),
            schema,
            closure: RefCell::new(None),
            slot_cache: RefCell::default(),
        }
    }

    /// `SchemaView.load_import` for the cases that need no network: relative file
    /// imports, and the metamodel's own `linkml:` schemas.
    fn load_import(
        &self,
        imp: &str,
        from_source_file: Option<&str>,
    ) -> MetamodelResult<Shared<Object>> {
        if let Some(name) = imp.strip_prefix("linkml:") {
            // PARITY (divergence): upstream loads `linkml_runtime`'s bundled copy of the
            // metamodel schema. Only `linkml:types` is common in practice and it has no
            // classes, slots or enums, so an empty schema of that name is equivalent for
            // everything SchemaMapper reads.
            return Ok(Rc::new(RefCell::new(Object::new(
                "SchemaDefinition",
                vec![
                    ("id", Node::str(format!("https://w3id.org/linkml/{name}"))),
                    ("name", Node::str(name)),
                ],
            )?)));
        }
        if imp.contains("://") {
            return err(format!(
                "cannot resolve import {imp}: remote imports are not supported"
            ));
        }
        if imp.contains(':') {
            return err(format!("cannot resolve CURIE import {imp}"));
        }
        let mut path = PathBuf::from(format!("{imp}.yaml"));
        if !path.is_absolute()
            && let Some(dir) = from_source_file.and_then(|f| Path::new(f).parent())
        {
            path = dir.join(path);
        }
        let text = std::fs::read_to_string(&path).map_err(|e| {
            super::metamodel::MetamodelError(format!(
                "Importing {imp} from source {}: {e}",
                from_source_file.unwrap_or("None")
            ))
        })?;
        load_schema_text(&text, Some(&path.to_string_lossy()))
    }

    /// `imports_closure(imports=True, inject_metadata=True)`.
    fn imports_closure(&self) -> MetamodelResult<Vec<String>> {
        if let Some(c) = self.closure.borrow().as_ref() {
            return Ok(c.clone());
        }
        let root_name = self.schema.borrow().get("name").py_str();
        // (closure key, import as written, importing schema's key)
        let mut todo: Vec<(String, Option<String>, Option<String>)> = vec![(root_name, None, None)];
        let mut closure: std::collections::VecDeque<String> = std::collections::VecDeque::new();
        let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();

        while let Some((sn, raw_imp, importer_sn)) = todo.pop() {
            if !self.schema_map.borrow().contains_key(&sn) {
                let loaded = match (&raw_imp, &importer_sn) {
                    (Some(raw), Some(importer)) => {
                        let source_file = self
                            .schema_map
                            .borrow()
                            .get(importer)
                            .and_then(|s| s.borrow().get_str("source_file"));
                        self.load_import(raw, source_file.as_deref())?
                    }
                    _ => unreachable!("the root schema is always in schema_map"),
                };
                self.schema_map.borrow_mut().insert(sn.clone(), loaded);
            }
            if !visited.contains(&sn) {
                let imports: Vec<String> = {
                    let map = self.schema_map.borrow();
                    let list = map[&sn].borrow().list("imports")?;
                    let list = list.borrow();
                    list.iter().map(Node::py_str).collect()
                };
                for i in imports {
                    if i == sn {
                        continue;
                    }
                    let key = if sn.contains('/') && !sn.contains("://") && !i.contains(':') {
                        normpath(&Path::new(&sn).parent().unwrap_or(Path::new("")).join(&i))
                    } else {
                        i.clone()
                    };
                    todo.push((key, Some(i), Some(sn.clone())));
                }
            }
            closure.push_front(sn.clone());
            visited.insert(sn);
        }
        // filter duplicates, keeping the first entry
        let mut seen = std::collections::HashSet::new();
        let closure: Vec<String> = closure
            .into_iter()
            .filter(|s| seen.insert(s.clone()))
            .collect();

        // inject_metadata
        for schema in self.schema_map.borrow().values() {
            let schema = schema.borrow();
            let id = schema.get("id");
            for element_type in ["classes", "enums", "slots", "subsets", "types"] {
                let dict = schema.dict(element_type)?;
                for element in dict.borrow().values() {
                    if let Node::Obj(o) = element {
                        o.borrow_mut().set("from_schema", id.clone());
                    }
                }
            }
            for class in schema.dict("classes")?.borrow().values() {
                let Node::Obj(class) = class else { continue };
                for attribute in class.borrow().dict("attributes")?.borrow().values() {
                    if let Node::Obj(a) = attribute {
                        a.borrow_mut().set("from_schema", id.clone());
                    }
                }
            }
        }
        *self.closure.borrow_mut() = Some(closure.clone());
        Ok(closure)
    }

    /// `_get_dict(element_name, imports=True)`: the closure's dicts merged in order.
    pub(super) fn get_dict(&self, element_name: &str) -> MetamodelResult<IndexMap<String, Node>> {
        let closure = self.imports_closure()?;
        let mut d: IndexMap<String, Node> = IndexMap::new();
        for sn in closure {
            let schema = Rc::clone(&self.schema_map.borrow()[&sn]);
            let mut d1: IndexMap<String, Node> =
                schema.borrow().dict(element_name)?.borrow().clone();
            if element_name == "classes" {
                // For classes that appear in multiple schemas, additively merge rules and
                // classification_rules so imported rules are not silently lost.
                for (k, v) in d1.iter_mut() {
                    let Some(Node::Obj(prev)) = d.get(k) else {
                        continue;
                    };
                    let merged = v.deep_clone();
                    let Node::Obj(merged_obj) = &merged else {
                        continue;
                    };
                    for field in ["rules", "classification_rules"] {
                        let existing = merged_obj.borrow().list(field)?;
                        for rule in prev.borrow().list(field)?.borrow().iter() {
                            if !existing.borrow().iter().any(|r| r.py_eq(rule)) {
                                existing.borrow_mut().push(rule.deep_clone());
                            }
                        }
                    }
                    *v = merged;
                }
            }
            for (k, v) in d1 {
                d.insert(k, v);
            }
        }
        Ok(d)
    }

    /// `get_class(name)`.
    pub fn get_class(&self, name: &str) -> MetamodelResult<Option<Shared<Object>>> {
        Ok(match self.get_dict("classes")?.get(name) {
            Some(Node::Obj(o)) => Some(Rc::clone(o)),
            _ => None,
        })
    }

    /// `get_enum(name)`.
    pub fn get_enum(&self, name: &str) -> MetamodelResult<Option<Shared<Object>>> {
        Ok(match self.get_dict("enums")?.get(name) {
            Some(Node::Obj(o)) => Some(Rc::clone(o)),
            _ => None,
        })
    }

    /// `get_slot(name)` (with `attributes=True`).
    pub fn get_slot(&self, name: &str) -> MetamodelResult<Option<Object>> {
        // PARITY: schemaview.py decorates `get_slot` with `@lru_cache(None)`, so SchemaMapper
        // sees the first answer for a name even if a later derivation merges a new
        // attribute into the (shared) source class: a miss stays a miss.
        if let Some(cached) = self.slot_cache.borrow().get(name) {
            return Ok(cached.clone());
        }
        let found = self.lookup_slot(name)?;
        self.slot_cache
            .borrow_mut()
            .insert(name.to_string(), found.clone());
        Ok(found)
    }

    fn lookup_slot(&self, name: &str) -> MetamodelResult<Option<Object>> {
        if let Some(Node::Obj(o)) = self.get_dict("slots")?.get(name) {
            // `all_slots(attributes=False)` hands out the stored object itself.
            return Ok(Some(o.borrow().clone()));
        }
        let mut found: Option<Object> = None;
        for class in self.get_dict("classes")?.values() {
            let Node::Obj(class) = class else { continue };
            let class = class.borrow();
            let attributes = class.dict("attributes")?;
            if let Some(Node::Obj(attribute)) = attributes.borrow().get(name) {
                if found.is_some() {
                    // slot name is ambiguous: return a stub slot
                    return Ok(Some(Object::new(
                        "SlotDefinition",
                        vec![("name", Node::str(name))],
                    )?));
                }
                let mut slot = attribute.borrow().shallow_copy();
                slot.set("from_schema", class.get("from_schema"));
                slot.set("owner", class.get("name"));
                found = Some(slot);
            }
        }
        Ok(found)
    }
}

/// `os.path.normpath` for relative POSIX-style paths.
fn normpath(path: &Path) -> String {
    let mut parts: Vec<String> = Vec::new();
    for comp in path.to_string_lossy().split('/') {
        match comp {
            "" | "." => {}
            ".." if parts.last().is_some_and(|p| p != "..") => {
                parts.pop();
            }
            other => parts.push(other.to_string()),
        }
    }
    if parts.is_empty() {
        ".".into()
    } else {
        parts.join("/")
    }
}
