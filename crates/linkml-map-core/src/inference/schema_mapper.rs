//! Port of upstream `linkml_map/inference/schema_mapper.py`.
//!
//! A [`SchemaMapper`] translates a source schema and a transformation specification
//! into a target schema ("profiling"). Every method mirrors the upstream method of
//! the same name, including its shallow-copy aliasing and its crashes (a Python
//! exception is an [`Err`] here), because the derived YAML must match byte for byte.
//!
//! The target schema is built from [`metamodel`](super::metamodel) objects, exactly
//! as upstream builds `linkml_runtime` dataclasses, and dumped like
//! `yaml_dumper.dumps` (see [`remove_empty_items`]).

use std::{path::Path, rc::Rc};

use indexmap::IndexMap;
use serde_json::Value as Json;

use super::{
    metamodel::{MetamodelResult, Node, Object, Shared, err, remove_empty_items},
    schema_view::SchemaView,
};
use crate::{
    datamodel::{
        ClassDerivation, CollectionType, CopyDirective, EnumDerivation, SlotDerivation,
        TransformationSpecification,
    },
    value::Value,
};

/// Translates a source schema and transformation specification into a target schema.
///
/// Upstream: `linkml_map.inference.schema_mapper.SchemaMapper`.
pub struct SchemaMapper {
    /// `source_schemaview`.
    source_schemaview: SchemaView,
    /// `source_to_target_class_mappings` (a `defaultdict(list)` upstream).
    source_to_target_class_mappings: IndexMap<String, Vec<String>>,
    /// `slot_info`: `(range, dictionary_key)` pairs whose slot becomes an identifier
    /// (the upstream value is always `{"identifier": True}`).
    slot_info: IndexMap<(Option<String>, String), ()>,
}

// ── Python truthiness / `in` helpers for the untyped copy-directive fields ───

fn json_truthy(v: Option<&Json>) -> bool {
    match v {
        None | Some(Json::Null) => false,
        Some(Json::Bool(b)) => *b,
        Some(Json::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Json::String(s)) => !s.is_empty(),
        Some(Json::Array(a)) => !a.is_empty(),
        Some(Json::Object(o)) => !o.is_empty(),
    }
}

/// `element in container` for a `str`, `list` or `dict` container.
fn py_in(container: &Json, element: &str) -> MetamodelResult<bool> {
    match container {
        Json::String(s) => Ok(s.contains(element)),
        Json::Array(a) => Ok(a.iter().any(|v| v.as_str() == Some(element))),
        Json::Object(o) => Ok(o.contains_key(element)),
        other => err(format!("argument of type '{other}' is not iterable")),
    }
}

/// `for element in container` (string elements only: anything else can never be a
/// key of a schema's element dicts).
fn py_iter(container: &Json) -> MetamodelResult<Vec<String>> {
    match container {
        Json::String(s) => Ok(s.chars().map(String::from).collect()),
        Json::Array(a) => Ok(a
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect()),
        Json::Object(o) => Ok(o.keys().cloned().collect()),
        other => err(format!("'{other}' object is not iterable")),
    }
}

fn non_empty(s: &Option<String>) -> Option<&str> {
    s.as_deref().filter(|s| !s.is_empty())
}

impl SchemaMapper {
    /// `SchemaMapper(source_schemaview=SchemaView(path))`.
    pub fn from_path(source_schema: &Path) -> MetamodelResult<SchemaMapper> {
        Ok(SchemaMapper::new(SchemaView::from_path(source_schema)?))
    }

    /// `SchemaMapper(source_schemaview=...)`.
    pub fn new(source_schemaview: SchemaView) -> SchemaMapper {
        SchemaMapper {
            source_schemaview,
            source_to_target_class_mappings: IndexMap::new(),
            slot_info: IndexMap::new(),
        }
    }

    fn copy_dict(
        &self,
        copy_directive: &CopyDirective,
        src_elements: &Shared<IndexMap<String, Node>>,
        tgt_elements: &Shared<IndexMap<String, Node>>,
    ) -> MetamodelResult<()> {
        if copy_directive.copy_all == Some(true) {
            for (element, v) in src_elements.borrow().iter() {
                tgt_elements.borrow_mut().insert(element.clone(), v.clone());
            }
        }
        if json_truthy(copy_directive.exclude.as_ref()) {
            let exclude = copy_directive.exclude.as_ref().expect("truthy");
            let src_keys: Vec<String> = src_elements.borrow().keys().cloned().collect();
            for element in src_keys {
                if py_in(exclude, &element)?
                    && tgt_elements.borrow_mut().shift_remove(&element).is_none()
                {
                    // PARITY: schema_mapper.py `_copy_dict` does `del tgt_elements[element]`
                    // for every excluded source element, so excluding something that was
                    // never copied (no `copy_all`) raises KeyError.
                    return err(format!("KeyError: '{element}'"));
                }
            }
        }
        if copy_directive.exclude_all == Some(true) {
            tgt_elements.borrow_mut().clear();
        }
        if json_truthy(copy_directive.include.as_ref()) {
            for element in py_iter(copy_directive.include.as_ref().expect("truthy"))? {
                let found = src_elements.borrow().get(&element).cloned();
                if let Some(v) = found {
                    tgt_elements.borrow_mut().insert(element, v);
                }
            }
        }
        Ok(())
    }

    fn copy_list(
        &self,
        copy_directive: &CopyDirective,
        src_elements: &Shared<Vec<Node>>,
        tgt_elements: &Shared<Vec<Node>>,
    ) -> MetamodelResult<()> {
        let contains = |list: &Shared<Vec<Node>>, element: &str| {
            list.borrow().iter().any(|n| n.as_str() == Some(element))
        };
        if copy_directive.copy_all == Some(true) {
            let src: Vec<Node> = src_elements.borrow().clone();
            for element in src {
                if !contains(tgt_elements, element.as_str().unwrap_or_default()) {
                    tgt_elements.borrow_mut().push(element);
                }
            }
        }
        if json_truthy(copy_directive.exclude.as_ref()) {
            for element in py_iter(copy_directive.exclude.as_ref().expect("truthy"))? {
                let pos = tgt_elements
                    .borrow()
                    .iter()
                    .position(|n| n.as_str() == Some(element.as_str()));
                if let Some(pos) = pos {
                    tgt_elements.borrow_mut().remove(pos);
                }
            }
        }
        if copy_directive.exclude_all == Some(true) {
            tgt_elements.borrow_mut().clear();
        }
        if json_truthy(copy_directive.include.as_ref()) {
            for element in py_iter(copy_directive.include.as_ref().expect("truthy"))? {
                if contains(src_elements, &element) && !contains(tgt_elements, &element) {
                    tgt_elements.borrow_mut().push(Node::str(element));
                }
            }
        }
        Ok(())
    }

    fn copy_schema(
        &self,
        copy_directives: Option<&IndexMap<String, CopyDirective>>,
        source: &Object,
        target: &Object,
    ) -> MetamodelResult<()> {
        for copy_directive in copy_directives.into_iter().flat_map(IndexMap::values) {
            for element_type in ["classes", "slots", "enums", "types"] {
                self.copy_dict(
                    copy_directive,
                    &source.dict(element_type)?,
                    &target.dict(element_type)?,
                )?;
            }
        }
        Ok(())
    }

    fn copy_class(
        &self,
        copy_directives: Option<&IndexMap<String, CopyDirective>>,
        source: &Object,
        target: &Object,
    ) -> MetamodelResult<()> {
        for copy_directive in copy_directives.into_iter().flat_map(IndexMap::values) {
            // copy attributes (which is a dict)
            self.copy_dict(
                copy_directive,
                &source.dict("attributes")?,
                &target.dict("attributes")?,
            )?;
            // copy slots (which is a list)
            self.copy_list(
                copy_directive,
                &source.list("slots")?,
                &target.list("slots")?,
            )?;
        }
        Ok(())
    }

    /// Use a transformation specification to generate a target/profile schema from a
    /// source schema (`derive_schema(specification)` with the default `suffix`).
    ///
    /// Returns what `yaml_dumper.dumps` would serialise: the derived `SchemaDefinition`
    /// with empty values removed.
    pub fn derive_schema(
        &mut self,
        specification: &TransformationSpecification,
    ) -> MetamodelResult<Value> {
        self.derive_schema_with(specification, None, None, "-derived")
    }

    /// [`derive_schema`](Self::derive_schema) with upstream's `target_schema_id`,
    /// `target_schema_name` and `suffix` parameters.
    pub fn derive_schema_with(
        &mut self,
        specification: &TransformationSpecification,
        target_schema_id: Option<&str>,
        target_schema_name: Option<&str>,
        suffix: &str,
    ) -> MetamodelResult<Value> {
        let source_schema_rc = Rc::clone(&self.source_schemaview.schema);
        let source_schema = source_schema_rc.borrow();
        let target_schema_id = target_schema_id.map_or_else(
            || format!("{}{suffix}", source_schema.get("id").py_str()),
            String::from,
        );
        let target_schema_name = target_schema_name.map_or_else(
            || format!("{}{suffix}", source_schema.get("name").py_str()),
            String::from,
        );
        let mut target_schema = Object::new(
            "SchemaDefinition",
            vec![
                ("id", Node::str(target_schema_id)),
                ("name", Node::str(target_schema_name)),
            ],
        )?;
        self.copy_schema(
            specification.copy_directives.as_ref(),
            &source_schema,
            &target_schema,
        )?;
        for im in source_schema.list("imports")?.borrow().iter() {
            target_schema.list("imports")?.borrow_mut().push(im.clone());
        }
        for prefix in source_schema.dict("prefixes")?.borrow().values() {
            let Node::Obj(p) = prefix else { continue };
            let key = p.borrow().get("prefix_prefix").py_str();
            target_schema
                .dict("prefixes")?
                .borrow_mut()
                .insert(key, prefix.clone());
        }
        let target_classes = target_schema.dict("classes")?;
        for class_derivation in specification.class_derivations.iter().flatten() {
            let class_definition = self.derive_class(class_derivation)?;
            let name = class_definition.get("name").py_str();
            let existing = target_classes.borrow().get(&name).cloned();
            match existing {
                Some(Node::Obj(existing)) => {
                    self.merge_class_definition(&existing, &class_definition)?
                }
                _ => {
                    target_classes
                        .borrow_mut()
                        .insert(name, Node::obj(class_definition));
                }
            }
        }
        let target_enums = target_schema.dict("enums")?;
        for enum_derivation in specification
            .enum_derivations
            .iter()
            .flat_map(IndexMap::values)
        {
            let enum_definition = self.derive_enum(enum_derivation)?;
            let name = enum_definition.get("name").py_str();
            target_enums
                .borrow_mut()
                .insert(name, Node::obj(enum_definition));
        }
        target_schema.set("default_range", source_schema.get("default_range"));
        let classes: Vec<Node> = target_classes.borrow().values().cloned().collect();
        for cd in classes {
            if let Node::Obj(cd) = cd {
                self.rewire_class(&cd)?;
            }
        }
        for ((cn, sn), ()) in &self.slot_info {
            // PARITY: `target_schema.classes[cn]` / `cd.attributes[sn]` raise KeyError when
            // the `dictionary_key` slot's range is not a derived class (or has no such
            // attribute); `cn` is `None` when the slot has no range at all.
            let cd = cn
                .as_ref()
                .and_then(|cn| target_classes.borrow().get(cn).cloned())
                .ok_or_else(|| super::metamodel::MetamodelError(format!("KeyError: {cn:?}")))?;
            let Node::Obj(cd) = cd else {
                return err("KeyError");
            };
            let sd = cd.borrow().dict("attributes")?.borrow().get(sn).cloned();
            let Some(Node::Obj(sd)) = sd else {
                return err(format!("KeyError: '{sn}'"));
            };
            sd.borrow_mut().set("identifier", Node::Bool(true));
        }
        Ok(remove_empty_items(&Node::obj(target_schema), false).unwrap_or(Value::Null))
    }

    /// Derive a class from a class derivation.
    fn derive_class(&mut self, class_derivation: &ClassDerivation) -> MetamodelResult<Object> {
        let populated_from = non_empty(&class_derivation.populated_from)
            .unwrap_or(&class_derivation.name)
            .to_string();
        log::info!("Populating {} from {populated_from}", class_derivation.name);
        let source_class = self.source_schemaview.get_class(&populated_from)?;
        let had_source = source_class.is_some();
        let mut target_class = match source_class {
            None => {
                log::warn!("No such class {populated_from}");
                Object::new(
                    "ClassDefinition",
                    vec![("name", Node::str(&class_derivation.name))],
                )?
            }
            Some(source_class) => {
                let source_class = source_class.borrow();
                let mut target_class = source_class.shallow_copy();
                target_class.set("from_schema", Node::None);
                target_class.set("name", Node::str(&class_derivation.name));
                target_class.set("slots", Node::list(Vec::new()));
                target_class.set("attributes", Node::dict(IndexMap::new()));
                target_class.set("slot_usage", Node::dict(IndexMap::new()));
                self.copy_class(
                    class_derivation.copy_directives.as_ref(),
                    &source_class,
                    &target_class,
                )?;
                target_class
            }
        };
        for slot_derivation in class_derivation
            .slot_derivations
            .iter()
            .flat_map(IndexMap::values)
        {
            if slot_derivation.hide == Some(true) {
                // Hidden slots are intermediates for slot() references at runtime
                // and have no corresponding attribute in the target schema.
                continue;
            }
            let slot_definition = self.derive_slot(slot_derivation)?;
            let name = slot_definition.get("name").py_str();
            target_class
                .dict("attributes")?
                .borrow_mut()
                .insert(name, Node::obj(slot_definition));
        }
        if let Some(is_a) = non_empty(&class_derivation.is_a) {
            target_class.set("is_a", Node::str(is_a));
        }
        if let Some(mixins) = class_derivation.mixins.as_ref().filter(|m| !m.is_empty()) {
            target_class.set("mixins", Node::list(mixins.iter().map(Node::str).collect()));
        }
        if let Some(target_definition) = class_derivation
            .target_definition
            .as_ref()
            .filter(|t| json_truthy(Some(t)))
        {
            let spec_defn = build_with_name(
                "ClassDefinition",
                &target_class.get("name"),
                target_definition,
            )?;
            for (k, v) in &spec_defn.fields {
                // PARITY: for a class copied from a source class, `target_class.attributes = {}`
                // / `.slot_usage = {}` turn the dicts into (empty) `JsonObj`s, and
                // `curr_v in ([], {})` is False for those: schema_mapper.py `_derive_class`
                // therefore never fills `attributes` / `slot_usage` from `target_definition`
                // there. Without a source class they are plain dicts and are filled.
                let jsonobj = had_source && matches!(k.as_str(), "attributes" | "slot_usage");
                let curr_v = target_class.get(k);
                if curr_v.is_empty() && !jsonobj {
                    target_class.set(k, v.clone());
                }
            }
        }
        self.source_to_target_class_mappings
            .entry(populated_from)
            .or_default()
            .push(target_class.get("name").py_str());
        if let Some(overrides) = class_derivation
            .overrides
            .as_ref()
            .filter(|o| json_truthy(Some(o)))
        {
            target_class = apply_overrides("ClassDefinition", &target_class, overrides)?;
        }
        Ok(target_class)
    }

    /// Merge an incoming ClassDefinition into an existing one.
    ///
    /// Used when multiple ClassDerivations target the same class name. Attributes from
    /// the incoming definition are added to the existing one; on conflict the incoming
    /// value wins with a warning. Merged fields: `attributes`, `slots`, `mixins`, `is_a`.
    fn merge_class_definition(
        &self,
        existing: &Shared<Object>,
        incoming: &Object,
    ) -> MetamodelResult<()> {
        let mut existing = existing.borrow_mut();
        let existing_name = existing.get("name").py_str();
        let incoming_attributes: Vec<(String, Node)> = incoming
            .dict("attributes")?
            .borrow()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for (attr_name, attr) in incoming_attributes {
            if attr_name.starts_with('_') {
                continue;
            }
            let attributes = existing.dict("attributes")?;
            if attributes.borrow().contains_key(&attr_name) {
                log::warn!(
                    "Slot '{attr_name}' in class '{existing_name}' defined by multiple derivations; later derivation wins"
                );
            }
            attributes.borrow_mut().insert(attr_name, attr);
        }

        for field in ["slots", "mixins"] {
            let target = existing.list(field)?;
            for item in incoming.list(field)?.borrow().iter() {
                if !target.borrow().iter().any(|n| n.py_eq(item)) {
                    target.borrow_mut().push(item.clone());
                }
            }
        }

        let incoming_is_a = incoming.get("is_a");
        if incoming_is_a.is_truthy() {
            let existing_is_a = existing.get("is_a");
            if !existing_is_a.is_truthy() {
                existing.set("is_a", incoming_is_a);
            } else if !incoming_is_a.py_eq(&existing_is_a) {
                log::warn!(
                    "Class '{existing_name}' has conflicting is_a: '{}' vs '{}'; keeping '{}'",
                    existing_is_a.py_str(),
                    incoming_is_a.py_str(),
                    existing_is_a.py_str()
                );
            }
        }
        Ok(())
    }

    /// Derive an enum from an enum derivation.
    fn derive_enum(&mut self, enum_derivation: &EnumDerivation) -> MetamodelResult<Object> {
        let populated_from = non_empty(&enum_derivation.populated_from)
            .unwrap_or(&enum_derivation.name)
            .to_string();
        let source_enum = self.source_schemaview.get_enum(&populated_from)?;
        let target_enum = match &source_enum {
            None => {
                log::warn!("No such enum {populated_from}");
                // PARITY: schema_mapper.py `_derive_enum` builds a `ClassDefinition` (not an
                // `EnumDefinition`) here; any permissible-value derivation or
                // `mirror_source` then raises AttributeError.
                Object::new(
                    "ClassDefinition",
                    vec![("name", Node::str(&enum_derivation.name))],
                )?
            }
            Some(source_enum) => {
                let mut target_enum = source_enum.borrow().shallow_copy();
                target_enum.set("from_schema", Node::None);
                target_enum.set("name", Node::str(&enum_derivation.name));
                // An EnumDefinition has no such fields: Python just adds (empty) attributes.
                target_enum.set("slots", Node::list(Vec::new()));
                target_enum.set("attributes", Node::dict(IndexMap::new()));
                target_enum.set("slot_usage", Node::dict(IndexMap::new()));
                target_enum
            }
        };
        for pv_derivation in enum_derivation
            .permissible_value_derivations
            .iter()
            .flat_map(IndexMap::values)
        {
            let sources = pv_derivation.populated_from.as_deref().unwrap_or_default();
            if sources.is_empty() {
                return err(format!(
                    "Missing populated_from or sources for {}",
                    pv_derivation.name
                ));
            }
            target_enum.require_field("permissible_values")?;
            for source in sources {
                let pv = Object::new("PermissibleValue", vec![("text", Node::str(source))])?;
                // PARITY: `copy(source_enum)` is shallow, so this writes into the *source*
                // enum's `permissible_values` dict too (and replaces its PVs, dropping
                // their `meaning`, `description`, ...).
                target_enum
                    .dict("permissible_values")?
                    .borrow_mut()
                    .insert(source.clone(), Node::obj(pv));
            }
        }
        if enum_derivation.mirror_source == Some(true) {
            let Some(source_enum) = &source_enum else {
                return err("'NoneType' object has no attribute 'permissible_values'");
            };
            target_enum.require_field("permissible_values")?;
            let source_pvs: Vec<(String, Node)> = source_enum
                .borrow()
                .dict("permissible_values")?
                .borrow()
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            for (text, pv) in source_pvs {
                let target_pvs = target_enum.dict("permissible_values")?;
                if !target_pvs.borrow().contains_key(&text) {
                    let copy = match &pv {
                        Node::Obj(o) => Node::obj(o.borrow().shallow_copy()),
                        other => other.clone(),
                    };
                    target_pvs.borrow_mut().insert(text, copy);
                }
            }
        }
        self.source_to_target_class_mappings
            .entry(populated_from)
            .or_default()
            .push(target_enum.get("name").py_str());
        Ok(target_enum)
    }

    /// Derive a slot from a slot derivation.
    fn derive_slot(&mut self, slot_derivation: &SlotDerivation) -> MetamodelResult<Object> {
        let populated_from = non_empty(&slot_derivation.populated_from)
            .unwrap_or(&slot_derivation.name)
            .to_string();
        let source_slot = self.source_schemaview.get_slot(&populated_from)?;
        let mut target_slot = match source_slot {
            None => Object::new(
                "SlotDefinition",
                vec![("name", Node::str(&slot_derivation.name))],
            )?,
            Some(source_slot) => {
                let mut target_slot = source_slot.shallow_copy();
                target_slot.set("from_schema", Node::None);
                target_slot.set("owner", Node::None);
                target_slot.set("name", Node::str(&slot_derivation.name));
                target_slot
            }
        };
        if let Some(range) = non_empty(&slot_derivation.range) {
            target_slot.set("range", Node::str(range));
        }
        if let Some(target_definition) = slot_derivation
            .target_definition
            .as_ref()
            .filter(|t| json_truthy(Some(t)))
        {
            let spec_defn = build_with_name(
                "SlotDefinition",
                &target_slot.get("name"),
                target_definition,
            )?;
            // PARITY: schema_mapper.py `_derive_slot` copies *every* attribute of the
            // spec-built definition, including its unset (None / empty) ones, so a
            // `target_definition` replaces whatever was copied from the source slot.
            for (k, v) in &spec_defn.fields {
                target_slot.set(k, v.clone());
            }
        }
        if let Some(unit_conversion) = &slot_derivation.unit_conversion {
            let ucum_code = unit_conversion
                .target_unit
                .as_deref()
                .map_or(Node::None, Node::str);
            target_slot.set(
                "unit",
                Node::obj(Object::new(
                    "UnitOfMeasure",
                    vec![("ucum_code", ucum_code)],
                )?),
            );
        }
        if let Some(stringification) = &slot_derivation.stringification {
            target_slot.set(
                "multivalued",
                Node::Bool(stringification.reversed == Some(true)),
            );
        }
        if let Some(dictionary_key) = non_empty(&slot_derivation.dictionary_key) {
            target_slot.set("inlined", Node::Bool(true));
            target_slot.set("inlined_as_list", Node::Bool(false));
            let range = target_slot.get_str("range");
            self.slot_info
                .insert((range, dictionary_key.to_string()), ());
        }
        match &slot_derivation.cast_collection_as {
            Some(CollectionType::MultiValued) => target_slot.set("inlined", Node::Bool(true)),
            Some(CollectionType::MultiValuedList) => {
                target_slot.set("inlined_as_list", Node::Bool(true))
            }
            Some(CollectionType::MultiValuedDict) => {
                target_slot.set("inlined", Node::Bool(true));
                target_slot.set("inlined_as_list", Node::Bool(false));
            }
            Some(CollectionType::SingleValued) | None => {}
        }
        if let Some(overrides) = slot_derivation
            .overrides
            .as_ref()
            .filter(|o| json_truthy(Some(o)))
        {
            target_slot = apply_overrides("SlotDefinition", &target_slot, overrides)?;
        }
        Ok(target_slot)
    }

    fn rewire_class(&self, class_definition: &Shared<Object>) -> MetamodelResult<()> {
        let is_a = class_definition.borrow().get("is_a");
        if is_a.is_truthy() {
            let new_is_a = self.rewire_parent(&is_a.py_str())?;
            class_definition
                .borrow_mut()
                .set("is_a", new_is_a.map_or(Node::None, Node::str));
        }
        let mixins: Vec<Node> = class_definition.borrow().list("mixins")?.borrow().clone();
        let mut rewired = Vec::new();
        for m in mixins {
            if let Some(m) = self.rewire_parent(&m.py_str())? {
                rewired.push(Node::str(m));
            }
        }
        class_definition
            .borrow_mut()
            .set("mixins", Node::list(rewired));
        Ok(())
    }

    fn rewire_parent(&self, parent: &str) -> MetamodelResult<Option<String>> {
        if let Some(new_parents) = self.source_to_target_class_mappings.get(parent) {
            if new_parents.len() > 1 {
                let shown = new_parents
                    .iter()
                    .map(|p| format!("'{p}'"))
                    .collect::<Vec<_>>()
                    .join(", ");
                return err(format!(
                    "Cannot rewire to non-isomorphic mappings {parent} => [{shown}]"
                ));
            }
            if let [only] = new_parents.as_slice() {
                return Ok(Some(only.clone()));
            }
        }
        // PARITY: `parent_cls.is_a` raises AttributeError when the parent is not a class of
        // the source schema; the lookup also sees `is_a` values this very loop has already
        // rewired on shared (copied) source classes.
        let Some(parent_cls) = self.source_schemaview.get_class(parent)? else {
            return err("'NoneType' object has no attribute 'is_a'");
        };
        let grandparent = parent_cls.borrow().get("is_a");
        if grandparent.is_truthy() {
            return self.rewire_parent(&grandparent.py_str());
        }
        Ok(None)
    }
}

/// `Cls(name=name, **target_definition)`.
fn build_with_name(class: &str, name: &Node, target_definition: &Json) -> MetamodelResult<Object> {
    let Node::Dict(kwargs) = Node::from_json(target_definition) else {
        return err("argument after ** must be a mapping");
    };
    let mut kwargs = kwargs.borrow().clone();
    if kwargs.contains_key("name") {
        return err(format!(
            "{class}() got multiple values for keyword argument 'name'"
        ));
    }
    kwargs.shift_insert(0, "name".to_string(), name.clone());
    Object::construct(class, kwargs)
}

/// `curr = json_dumper.to_dict(target); curr.update(overrides); Cls(**curr)`.
fn apply_overrides(class: &str, target: &Object, overrides: &Json) -> MetamodelResult<Object> {
    let dumped =
        remove_empty_items(&Node::obj(target.shallow_copy()), false).unwrap_or(Value::Null);
    let Node::Dict(curr) = Node::from_value(&dumped) else {
        return err("to_dict did not return a mapping");
    };
    let Json::Object(overrides) = overrides else {
        return err("'overrides' must be a mapping");
    };
    for (k, v) in overrides {
        curr.borrow_mut().insert(k.clone(), Node::from_json(v));
    }
    let kwargs = curr.borrow().clone();
    Object::construct(class, kwargs)
}
