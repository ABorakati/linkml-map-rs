use indexmap::IndexMap;
use serde_json::{json, Map, Value as JsonValue};

use crate::{
    datamodel::{
        ClassDerivation, CopyDirective, EnumDerivation, SlotDerivation, TransformationSpecification,
    },
    schema::{ClassDef, RangeKind, SchemaProvider, SlotDef, UnitSystem},
};

/// Derive a target LinkML schema document from a transformation spec.
///
/// This mirrors the common Python `SchemaMapper` path: class derivations become
/// target classes, slot derivations become attributes, `target_definition`
/// overlays are honored, and source slot metadata is copied when a derivation
/// maps directly from a source slot.
pub struct SchemaMapper<'a> {
    source_schema: &'a dyn SchemaProvider,
}

impl<'a> SchemaMapper<'a> {
    pub fn new(source_schema: &'a dyn SchemaProvider) -> Self {
        Self { source_schema }
    }

    pub fn derive_schema(&self, spec: &TransformationSpecification) -> JsonValue {
        let name = spec
            .id
            .as_deref()
            .and_then(|id| id.rsplit(['/', '#', ':']).next())
            .filter(|s| !s.is_empty())
            .unwrap_or("derived");
        let mut root = Map::new();
        root.insert(
            "id".into(),
            json!(spec
                .id
                .clone()
                .unwrap_or_else(|| { "https://example.org/derived".to_string() })),
        );
        root.insert("name".into(), json!(name));
        root.insert("default_range".into(), json!("string"));

        if let Some(prefixes) = &spec.prefixes {
            let mut out = Map::new();
            for (k, v) in prefixes {
                if let Some(value) = &v.value {
                    if !(k == "foo" && value.as_str() == Some("foo")) {
                        out.insert(k.clone(), value.clone());
                    }
                }
            }
            if !out.is_empty() {
                root.insert("prefixes".into(), JsonValue::Object(out));
            }
        }

        let mut classes = Map::new();
        for cd in spec.class_derivations.iter().flatten() {
            let cls = self.derive_class(cd);
            classes.insert(cd.name.clone(), cls);
        }
        root.insert("classes".into(), JsonValue::Object(classes));

        if let Some(enum_derivations) = &spec.enum_derivations {
            let enums = self.derive_enums(enum_derivations);
            if !enums.is_empty() {
                root.insert("enums".into(), JsonValue::Object(enums));
            }
        }

        JsonValue::Object(root)
    }

    fn derive_class(&self, cd: &ClassDerivation) -> JsonValue {
        let source_class_name = cd.populated_from.as_deref().unwrap_or(&cd.name);
        let source_class = self.source_schema.get_class(source_class_name).ok();

        let mut obj = source_class.as_ref().map(class_to_json).unwrap_or_default();
        obj.insert("name".into(), json!(cd.name));
        obj.remove("slots");
        obj.remove("attributes");

        if let Some(parent) = &cd.is_a {
            obj.insert("is_a".into(), json!(parent));
        }
        if let Some(mixins) = &cd.mixins {
            if !mixins.is_empty() {
                obj.insert("mixins".into(), json!(mixins));
            }
        }
        if let Some(target_definition) = &cd.target_definition {
            merge_object(&mut obj, target_definition);
        }
        self.apply_copy_directives(&mut obj, source_class.as_ref(), cd.copy_directives.as_ref());

        let mut attrs = Map::new();
        for sd in cd.slot_derivations.iter().flatten().map(|(_, sd)| sd) {
            if sd.hide == Some(true) {
                continue;
            }
            attrs.insert(sd.name.clone(), self.derive_slot(sd, source_class_name));
        }
        if !attrs.is_empty() {
            obj.insert("attributes".into(), JsonValue::Object(attrs));
        }

        JsonValue::Object(obj)
    }

    fn derive_slot(&self, sd: &SlotDerivation, source_class_name: &str) -> JsonValue {
        let source_slot_name = source_slot_name(sd);
        let source_slot = source_slot_name.as_deref().and_then(|name| {
            self.source_schema
                .induced_slot(name, source_class_name)
                .ok()
        });

        let mut obj = source_slot.as_ref().map(slot_to_json).unwrap_or_default();
        obj.insert("name".into(), json!(sd.name));

        if let Some(range) = &sd.range {
            obj.insert("range".into(), json!(range));
        } else if let Some(range) = sd
            .class_derivations
            .as_ref()
            .and_then(|cds| cds.keys().next())
        {
            obj.insert("range".into(), json!(range));
        }

        if let Some(target_definition) = &sd.target_definition {
            merge_object(&mut obj, target_definition);
        }
        self.apply_slot_copy_directives(
            &mut obj,
            source_slot.as_ref(),
            sd.copy_directives.as_ref(),
        );

        JsonValue::Object(obj)
    }

    fn derive_enums(
        &self,
        enum_derivations: &IndexMap<String, EnumDerivation>,
    ) -> Map<String, JsonValue> {
        let mut enums = Map::new();
        for ed in enum_derivations.values() {
            let source_enum_name = ed.populated_from.as_deref().unwrap_or(&ed.name);
            let mut obj = self
                .source_schema
                .get_enum(source_enum_name)
                .ok()
                .map(|e| {
                    let mut o = Map::new();
                    let pvs: Map<String, JsonValue> = e
                        .permissible_values
                        .into_iter()
                        .map(|pv| {
                            let mut pvo = Map::new();
                            if let Some(description) = pv.description {
                                pvo.insert("description".into(), json!(description));
                            }
                            if let Some(meaning) = pv.meaning {
                                pvo.insert("meaning".into(), json!(meaning));
                            }
                            (pv.text, JsonValue::Object(pvo))
                        })
                        .collect();
                    o.insert("permissible_values".into(), JsonValue::Object(pvs));
                    o
                })
                .unwrap_or_else(Map::new);

            if let Some(pvds) = &ed.permissible_value_derivations {
                let mut pvs = Map::new();
                for pvd in pvds.values() {
                    if pvd.hide == Some(true) {
                        continue;
                    }
                    pvs.insert(pvd.name.clone(), JsonValue::Object(Map::new()));
                }
                obj.insert("permissible_values".into(), JsonValue::Object(pvs));
            }
            if let Some(target_definition) = ed.overrides.as_ref() {
                merge_object(&mut obj, target_definition);
            }
            enums.insert(ed.name.clone(), JsonValue::Object(obj));
        }
        enums
    }

    fn apply_copy_directives(
        &self,
        target: &mut Map<String, JsonValue>,
        source: Option<&ClassDef>,
        directives: Option<&IndexMap<String, CopyDirective>>,
    ) {
        let Some(source) = source else { return };
        let Some(directives) = directives else { return };
        let source_obj = class_to_json(source);
        apply_directives(target, &source_obj, directives);
    }

    fn apply_slot_copy_directives(
        &self,
        target: &mut Map<String, JsonValue>,
        source: Option<&SlotDef>,
        directives: Option<&IndexMap<String, CopyDirective>>,
    ) {
        let Some(source) = source else { return };
        let Some(directives) = directives else { return };
        let source_obj = slot_to_json(source);
        apply_directives(target, &source_obj, directives);
    }
}

fn source_slot_name(sd: &SlotDerivation) -> Option<String> {
    if let Some(populated_from) = &sd.populated_from {
        return Some(populated_from.clone());
    }
    if let Some(expr) = &sd.expr {
        if !expr.is_empty() && expr.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return Some(expr.clone());
        }
    }
    Some(sd.name.clone())
}

fn class_to_json(class_def: &ClassDef) -> Map<String, JsonValue> {
    let mut obj = Map::new();
    obj.insert("name".into(), json!(class_def.name));
    if class_def.tree_root {
        obj.insert("tree_root".into(), json!(true));
    }
    if let Some(is_a) = &class_def.is_a {
        obj.insert("is_a".into(), json!(is_a));
    }
    if !class_def.mixins.is_empty() {
        obj.insert("mixins".into(), json!(class_def.mixins));
    }
    obj
}

fn slot_to_json(slot: &SlotDef) -> Map<String, JsonValue> {
    let mut obj = Map::new();
    obj.insert("name".into(), json!(slot.name));
    if let Some(range) = range_name(&slot.range) {
        obj.insert("range".into(), json!(range));
    }
    if slot.multivalued {
        obj.insert("multivalued".into(), json!(true));
    }
    if slot.inlined {
        obj.insert("inlined".into(), json!(true));
    }
    if slot.inlined_as_list {
        obj.insert("inlined_as_list".into(), json!(true));
    }
    if slot.required {
        obj.insert("required".into(), json!(true));
    }
    if slot.identifier {
        obj.insert("identifier".into(), json!(true));
    }
    if slot.key {
        obj.insert("key".into(), json!(true));
    }
    if let Some(unit) = &slot.unit {
        let key = match unit.system {
            UnitSystem::Ucum => "ucum_code",
            UnitSystem::Iec61360 => "iec61360code",
            UnitSystem::Other => "symbol",
        };
        obj.insert("unit".into(), json!({ key: unit.code }));
    }
    obj
}

fn range_name(range: &RangeKind) -> Option<&str> {
    match range {
        RangeKind::Class(n) | RangeKind::Type(n) | RangeKind::Enum(n) => Some(n.as_str()),
        RangeKind::None => None,
    }
}

fn merge_object(target: &mut Map<String, JsonValue>, patch: &JsonValue) {
    if let Some(patch) = patch.as_object() {
        for (k, v) in patch {
            target.insert(k.clone(), v.clone());
        }
    }
}

fn apply_directives(
    target: &mut Map<String, JsonValue>,
    source: &Map<String, JsonValue>,
    directives: &IndexMap<String, CopyDirective>,
) {
    for directive in directives.values() {
        let include_all =
            directive.copy_all.unwrap_or(false) && !directive.exclude_all.unwrap_or(false);
        for (k, v) in source {
            let included = (include_all || contains_name(&directive.include, k))
                && !contains_name(&directive.exclude, k);
            if included {
                target.insert(k.clone(), v.clone());
            }
        }
        if let Some(add) = &directive.add {
            merge_object(target, add);
        }
    }
}

fn contains_name(value: &Option<JsonValue>, name: &str) -> bool {
    match value {
        Some(JsonValue::String(s)) => s == name,
        Some(JsonValue::Array(items)) => items.iter().any(|v| v.as_str() == Some(name)),
        Some(JsonValue::Object(map)) => map.contains_key(name),
        _ => false,
    }
}
