//! `SchemaView.induced_slot` and the lookups it rests on, ported over the raw
//! metamodel objects of [`SchemaView`](super::SchemaView).
//!
//! Upstream: `linkml_runtime/utils/schemaview.py` (`induced_slot`, `get_slot`,
//! `slot_name_mappings`, `class_ancestors`, `slot_ancestors`, `_closure`,
//! `class_slots`, `get_identifier_slot`, `get_key_slot`). The inverter
//! (`linkml_map.inference.inverter`) reads the *induced* source slot, which depends
//! on `slot_usage`, `attributes`, slot inheritance and the imports closure; the
//! engine's [`SchemaProvider`](crate::schema::SchemaProvider) projects those away,
//! so this port works on the objects themselves.
//!
//! Not ported, because nothing here reads the result: `resolve_pattern` (it can
//! only change the induced `pattern`), `alias`/`domain_of` bookkeeping, `owner`,
//! and `mangle_name`.

use indexmap::IndexMap;

use super::{
    metamodel::{MetamodelResult, Node, Object, Shared, err},
    schema_view::SchemaView,
};

/// `SlotDefinition._inherited_slots` (`linkml_runtime/linkml_model/meta.py`).
const INHERITED_SLOTS: &[&str] = &[
    "domain",
    "inherited",
    "readonly",
    "ifabsent",
    "list_elements_unique",
    "list_elements_ordered",
    "shared",
    "key",
    "identifier",
    "designates_type",
    "role",
    "relational_role",
    "range",
    "required",
    "recommended",
    "multivalued",
    "inlined",
    "inlined_as_list",
    "minimum_value",
    "maximum_value",
    "pattern",
    "structured_pattern",
    "value_presence",
    "equals_string",
    "equals_string_in",
    "equals_number",
    "equals_expression",
    "exact_cardinality",
    "minimum_cardinality",
    "maximum_cardinality",
    "array",
];

/// `formatutils.underscore`: `ws_pattern.sub("_", txt.strip()).replace(",", "").replace("-", "_")`.
fn underscore(txt: &str) -> String {
    let mut out = String::new();
    let mut in_ws = false;
    for c in txt.trim().chars() {
        if c.is_whitespace() {
            if !in_ws {
                out.push('_');
            }
            in_ws = true;
        } else {
            in_ws = false;
            out.push(c);
        }
    }
    out.replace(',', "").replace('-', "_")
}

/// `schemaview._closure(f, x, reflexive=True, depth_first=True)`.
fn closure(
    start: &str,
    mut parents: impl FnMut(&str) -> MetamodelResult<Vec<String>>,
) -> MetamodelResult<Vec<String>> {
    let mut rv = vec![start.to_string()];
    let mut rv_set: std::collections::HashSet<String> =
        std::collections::HashSet::from([rv[0].clone()]);
    let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut todo = vec![start.to_string()];
    while let Some(i) = todo.pop() {
        visited.insert(i.clone());
        for v in parents(&i)? {
            if !visited.contains(&v) && !rv_set.contains(&v) {
                todo.push(v.clone());
                rv.push(v.clone());
                rv_set.insert(v);
            }
        }
    }
    Ok(rv)
}

/// `_parents(element, mixins=True, is_a=True)`: the mixins, then `is_a`.
fn parents_of(element: &Object) -> Vec<String> {
    let mut out: Vec<String> = match element.get("mixins") {
        Node::List(l) => l.borrow().iter().map(Node::py_str).collect(),
        _ => Vec::new(),
    };
    if let Some(is_a) = element.get_str("is_a") {
        out.push(is_a);
    }
    out
}

impl SchemaView {
    /// `all_classes()`: the imports closure's classes, later schemas overriding earlier.
    pub fn all_classes(&self) -> MetamodelResult<IndexMap<String, Node>> {
        self.get_dict("classes")
    }

    /// `all_slots(imports=True, attributes=False)`.
    fn schema_slots(&self) -> MetamodelResult<IndexMap<String, Node>> {
        self.get_dict("slots")
    }

    /// `get_class(name, strict=True)`.
    fn get_class_strict(&self, name: &str) -> MetamodelResult<Shared<Object>> {
        match self.all_classes()?.get(name) {
            Some(Node::Obj(o)) => Ok(std::rc::Rc::clone(o)),
            _ => err(format!("No such class: \"{name}\"")),
        }
    }

    /// `get_slot(name, attributes=attributes)` without upstream's `lru_cache` (this
    /// port never mutates the schema, so a cached answer cannot go stale).
    fn get_slot_uncached(&self, name: &str, attributes: bool) -> MetamodelResult<Option<Object>> {
        let mut slot: Option<Object> = match self.schema_slots()?.get(name) {
            Some(Node::Obj(o)) => Some(o.borrow().clone()),
            _ => None,
        };
        if slot.is_none() && attributes {
            for class in self.all_classes()?.values() {
                let Node::Obj(class) = class else { continue };
                let class = class.borrow();
                let attrs = class.dict("attributes")?;
                if let Some(Node::Obj(attr)) = attrs.borrow().get(name) {
                    if slot.is_some() {
                        // slot name is ambiguous: return a stub slot
                        return Ok(Some(Object::new(
                            "SlotDefinition",
                            vec![("name", Node::str(name))],
                        )?));
                    }
                    let mut copy = attr.borrow().shallow_copy();
                    copy.set("from_schema", class.get("from_schema"));
                    copy.set("owner", class.get("name"));
                    slot = Some(copy);
                }
            }
        }
        Ok(slot)
    }

    /// `class_ancestors(name)`: reflexive, depth-first over mixins then `is_a`.
    pub fn class_ancestors(&self, name: &str) -> MetamodelResult<Vec<String>> {
        closure(name, |c| {
            Ok(parents_of(&self.get_class_strict(c)?.borrow()))
        })
    }

    /// `slot_ancestors(name)`: reflexive; `slot_parents` looks the slot up strictly.
    fn slot_ancestors(&self, name: &str) -> MetamodelResult<Vec<String>> {
        closure(name, |s| match self.get_slot_uncached(s, true)? {
            Some(slot) => Ok(parents_of(&slot)),
            None => err(format!("No such slot: \"{s}\"")),
        })
    }

    /// `class_slots(name)`: slots then attributes of each ancestor, de-duplicated in order.
    pub fn class_slots(&self, name: &str) -> MetamodelResult<Vec<String>> {
        let mut slots: Vec<String> = Vec::new();
        for an in self.class_ancestors(name)? {
            let Some(Node::Obj(a)) = self.all_classes()?.get(&an).cloned() else {
                return err("'NoneType' object has no attribute 'slots'");
            };
            let a = a.borrow();
            if let Node::List(l) = a.get("slots") {
                slots.extend(l.borrow().iter().map(Node::py_str));
            }
            slots.extend(a.dict("attributes")?.borrow().keys().cloned());
        }
        let mut seen = std::collections::HashSet::new();
        slots.retain(|s| seen.insert(s.clone()));
        Ok(slots)
    }

    /// `slot_name_mappings()`: `underscore(s.name) -> s` over `all_slots()` (attributes
    /// included); a later slot wins a clash.
    fn slot_name_mapping(&self, wanted: &str) -> MetamodelResult<Option<String>> {
        let mut slots = self.schema_slots()?;
        for class in self.all_classes()?.values() {
            let Node::Obj(class) = class else { continue };
            for (aname, a) in class.borrow().dict("attributes")?.borrow().iter() {
                if !slots.contains_key(aname) {
                    slots.insert(aname.clone(), a.clone());
                }
            }
        }
        let mut found = None;
        for s in slots.values() {
            if let Node::Obj(s) = s {
                let name = s.borrow().get("name").py_str();
                if underscore(&name) == wanted {
                    found = Some(name);
                }
            }
        }
        Ok(found)
    }

    /// `induced_slot(slot_name, class_name)`: the slot with `slot_usage`, `attributes`
    /// and inherited metaslots applied. `class_name` of `None` (or empty) looks the slot
    /// up without a class context.
    pub fn induced_slot(
        &self,
        slot_name: &str,
        class_name: Option<&str>,
    ) -> MetamodelResult<Object> {
        let class_name = class_name.filter(|c| !c.is_empty());
        if let Some(c) = class_name {
            self.get_class_strict(c)?;
        }

        // attributes take priority over schema-level slot definitions, IF the attribute
        // is declared for the class or an ancestor
        let mut slot_comes_from_attribute = false;
        let mut slot;
        if let Some(class_name) = class_name {
            slot = self.get_slot_uncached(slot_name, false)?;
            for class_ancestor in self.class_ancestors(class_name)? {
                let Some(Node::Obj(a)) = self.all_classes()?.get(&class_ancestor).cloned() else {
                    return err("'NoneType' object has no attribute 'attributes'");
                };
                let attribute = a
                    .borrow()
                    .dict("attributes")?
                    .borrow()
                    .get(slot_name)
                    .cloned();
                if let Some(Node::Obj(attribute)) = attribute {
                    slot = Some(attribute.borrow().clone());
                    slot_comes_from_attribute = true;
                    break;
                }
            }
        } else {
            slot = self.get_slot_uncached(slot_name, true)?;
        }

        let Some(slot) = slot else {
            // Callers may normalise slot names with `underscore()`: resolve the name back
            // to the canonical (hyphenated / space-containing) slot name, once.
            if let Some(canonical) = self.slot_name_mapping(slot_name)?
                && canonical != slot_name
            {
                return self.induced_slot(&canonical, class_name);
            }
            return err(format!(
                "No such slot {slot_name} as an attribute of {} ancestors or as a slot definition in the schema",
                class_name.map_or("None".to_string(), str::to_string)
            ));
        };

        // copy the slot, as it will be modified
        let mut induced = slot.shallow_copy();

        if !slot_comes_from_attribute {
            // inheritable slot: first propagate from ancestors
            let mut ancestors = self.slot_ancestors(slot_name)?;
            ancestors.reverse();
            for anc_sn in ancestors {
                let Some(anc_slot) = self.get_slot_uncached(&anc_sn, false)? else {
                    continue;
                };
                for metaslot in INHERITED_SLOTS {
                    let value = anc_slot.get(metaslot);
                    if value.is_truthy() {
                        induced.set(metaslot, value);
                    }
                }
            }
        }

        let class_ancestors = match class_name {
            Some(c) => self.class_ancestors(c)?,
            None => Vec::new(),
        };

        // slot_usage definitions, root-most ancestor first
        let mut slot_usages: Vec<Option<Object>> = Vec::new();
        for class_ancestor in class_ancestors.iter().rev() {
            let Some(Node::Obj(owning)) = self.all_classes()?.get(class_ancestor).cloned() else {
                return err("'NoneType' object has no attribute 'slot_usage'");
            };
            let usage = owning
                .borrow()
                .dict("slot_usage")?
                .borrow()
                .get(slot_name)
                .cloned();
            slot_usages.push(match usage {
                Some(Node::Obj(u)) => Some(u.borrow().clone()),
                _ => None,
            });
        }

        // iterate through all metaslots, and potentially populate the metaslot value
        // slot-level assignment < ancestor slot_usage < self slot_usage
        let metaslots: Vec<String> = induced.fields.keys().cloned().collect();
        for metaslot in &metaslots {
            let mut v = induced.get(metaslot);
            for usage in &slot_usages {
                let v2 = usage.as_ref().map_or(Node::None, |u| u.get(metaslot));
                if matches!(v, Node::None) {
                    v = v2;
                } else if metaslot == "maximum_value" || metaslot == "minimum_value" {
                    if !matches!(v2, Node::None) {
                        v = pick_numeric(metaslot == "maximum_value", &v, v2);
                    }
                } else if !v2.is_empty() {
                    v = v2;
                }
            }
            if matches!(v, Node::None) && metaslot == "range" {
                v = self
                    .schema
                    .borrow()
                    .get_str("default_range")
                    .map_or(Node::None, Node::Str);
            }
            if !matches!(v, Node::None) {
                induced.set(metaslot, v);
            }
        }
        if induced.get("inlined_as_list").is_truthy() {
            induced.set("inlined", Node::Bool(true));
        }
        if induced.get("identifier").is_truthy() || induced.get("key").is_truthy() {
            induced.set("required", Node::Bool(true));
        }
        Ok(induced)
    }

    /// `class_induced_slots(class_name)`: every slot of the class, induced.
    pub fn class_induced_slots(&self, class_name: &str) -> MetamodelResult<Vec<Object>> {
        self.class_slots(class_name)?
            .iter()
            .map(|sn| self.induced_slot(sn, Some(class_name)))
            .collect()
    }

    /// `get_identifier_slot(class_name, use_key)`: the first `identifier` slot in name
    /// order, else (with `use_key`) the first `key` slot.
    pub fn get_identifier_slot(
        &self,
        class_name: &str,
        use_key: bool,
    ) -> MetamodelResult<Option<Object>> {
        let mut names = self.class_slots(class_name)?;
        names.sort();
        for sn in &names {
            let s = self.induced_slot(sn, Some(class_name))?;
            if s.get("identifier").is_truthy() {
                return Ok(Some(s));
            }
        }
        if use_key {
            for sn in &names {
                let s = self.induced_slot(sn, Some(class_name))?;
                if s.get("key").is_truthy() {
                    return Ok(Some(s));
                }
            }
        }
        Ok(None)
    }
}

/// `min(v, v2)` for `maximum_value`, `max(v, v2)` for `minimum_value` (numbers only;
/// anything else keeps `v`, where Python would raise `TypeError` or compare strings).
fn pick_numeric(take_min: bool, v: &Node, v2: Node) -> Node {
    let as_f64 = |n: &Node| match n {
        Node::Int(i) => Some(*i as f64),
        Node::Float(f) => Some(*f),
        _ => None,
    };
    match (as_f64(v), as_f64(&v2)) {
        (Some(a), Some(b)) => {
            // `min(x, y)` returns `y` only when `y < x` (and `max` when `y > x`).
            let take_second = if take_min { b < a } else { b > a };
            if take_second { v2 } else { v.clone() }
        }
        _ => v.clone(),
    }
}
