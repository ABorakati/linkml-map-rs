//! A small port of the `linkml_runtime` dataclasses that `SchemaMapper` builds.
//!
//! Upstream `schema_mapper.py` manipulates `SchemaDefinition`, `ClassDefinition`,
//! `SlotDefinition`, `EnumDefinition`, ... objects and finally writes them with
//! `yaml_dumper.dumps`. Both the *key order* of that YAML (dataclass field order)
//! and its *content* (the normalisation each generated `__post_init__` applies:
//! `name` filled in from a dict key, `"true"` turned into `True`, `str` wrapped
//! into a list, ...) matter for byte-identical output, so the objects are modelled
//! here rather than as free-form maps.
//!
//! The per-class field table is generated from `linkml_runtime`'s `meta.py` by
//! `generate_metamodel.py` into `metamodel.json`.
//!
//! Containers are reference-counted: Python's `copy(obj)` is a *shallow* copy and
//! `SchemaMapper` leans on the resulting aliasing (`_copy_dict` shares element
//! objects with the source schema, `copy(source_enum)` shares its
//! `permissible_values` dict with the source), so [`Node::clone`] shares containers
//! exactly as `copy` does.

use std::{cell::RefCell, collections::HashMap, rc::Rc, sync::OnceLock};

use indexmap::IndexMap;

use crate::value::Value;

/// A shared, mutable cell: Python reference semantics.
pub type Shared<T> = Rc<RefCell<T>>;

/// Error raised where `linkml_runtime` raises (`ValueError`, `KeyError`, ...).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct MetamodelError(pub String);

/// Result alias for the schema-derivation modules.
pub type MetamodelResult<T> = Result<T, MetamodelError>;

pub(crate) fn err<T>(msg: impl Into<String>) -> MetamodelResult<T> {
    Err(MetamodelError(msg.into()))
}

/// A Python value as far as `SchemaMapper` needs one.
#[derive(Debug, Clone, Default)]
pub enum Node {
    /// `None`.
    #[default]
    None,
    /// `bool`.
    Bool(bool),
    /// `int`.
    Int(i64),
    /// `float`.
    Float(f64),
    /// `str` (and the `str` subclasses `linkml_runtime` uses for names and URIs).
    Str(String),
    /// `list`.
    List(Shared<Vec<Node>>),
    /// `dict`.
    Dict(Shared<IndexMap<String, Node>>),
    /// A `YAMLRoot` dataclass instance.
    Obj(Shared<Object>),
}

/// A `YAMLRoot` instance: its class and its `__dict__` (dataclass fields in order,
/// then any attribute Python code added later).
#[derive(Debug, Clone)]
pub struct Object {
    /// Metamodel class name, e.g. `"ClassDefinition"`.
    pub class: &'static str,
    /// Instance attributes in `__dict__` order.
    pub fields: IndexMap<String, Node>,
}

impl Node {
    /// A fresh list.
    pub fn list(items: Vec<Node>) -> Node {
        Node::List(Rc::new(RefCell::new(items)))
    }

    /// A fresh dict.
    pub fn dict(items: IndexMap<String, Node>) -> Node {
        Node::Dict(Rc::new(RefCell::new(items)))
    }

    /// A fresh `str`.
    pub fn str(s: impl Into<String>) -> Node {
        Node::Str(s.into())
    }

    /// Wrap an object.
    pub fn obj(o: Object) -> Node {
        Node::Obj(Rc::new(RefCell::new(o)))
    }

    /// Convert parsed YAML/JSON into Python-style nodes.
    pub fn from_value(v: &Value) -> Node {
        match v {
            Value::Null => Node::None,
            Value::Bool(b) => Node::Bool(*b),
            Value::Int(i) => Node::Int(*i),
            // Schemas and specs carry no integer beyond `i64`; keep its digits.
            Value::BigInt(n) => Node::Str(n.to_string()),
            Value::Complex(z) => Node::Str(crate::pynum::complex::repr(**z)),
            Value::Float(f) => Node::Float(*f),
            // A decimal only arises from engine coercion, never from a schema or spec.
            Value::Decimal(d) => Node::Str(d.to_sci_string()),
            // Schemas and specs are loaded as YAML 1.2, so a date never arrives
            // as a `Temporal`; if one does, keep its `str()` text.
            Value::Temporal(t) => Node::Str(t.to_string()),
            Value::Str(s) => Node::Str(s.clone()),
            Value::List(items) | Value::Tuple(items) => {
                Node::list(items.iter().map(Node::from_value).collect())
            }
            Value::Set(set) => Node::list(set.iter().map(Node::from_value).collect()),
            Value::Map(m) => Node::dict(
                m.iter()
                    .map(|(k, v)| (k.clone(), Node::from_value(v)))
                    .collect(),
            ),
            Value::Dict(d) => Node::dict(
                d.iter()
                    .map(|(k, v)| (crate::expr::eval::py_str(k), Node::from_value(v)))
                    .collect(),
            ),
        }
    }

    /// Convert plain JSON (spec fields such as `target_definition`).
    pub fn from_json(v: &serde_json::Value) -> Node {
        match v {
            serde_json::Value::Null => Node::None,
            serde_json::Value::Bool(b) => Node::Bool(*b),
            serde_json::Value::Number(n) => match n.as_i64() {
                Some(i) => Node::Int(i),
                None => Node::Float(n.as_f64().unwrap_or(f64::NAN)),
            },
            serde_json::Value::String(s) => Node::Str(s.clone()),
            serde_json::Value::Array(a) => Node::list(a.iter().map(Node::from_json).collect()),
            serde_json::Value::Object(m) => Node::dict(
                m.iter()
                    .map(|(k, v)| (k.clone(), Node::from_json(v)))
                    .collect(),
            ),
        }
    }

    /// `is_empty` from `linkml_runtime.utils.formatutils`: `None` or an empty container.
    pub fn is_empty(&self) -> bool {
        match self {
            Node::None => true,
            Node::List(l) => l.borrow().is_empty(),
            Node::Dict(d) => d.borrow().is_empty(),
            _ => false,
        }
    }

    /// Python truthiness.
    pub fn is_truthy(&self) -> bool {
        match self {
            Node::None => false,
            Node::Bool(b) => *b,
            Node::Int(i) => *i != 0,
            Node::Float(f) => *f != 0.0,
            Node::Str(s) => !s.is_empty(),
            Node::List(l) => !l.borrow().is_empty(),
            Node::Dict(d) => !d.borrow().is_empty(),
            Node::Obj(_) => true,
        }
    }

    /// The string, if this is a `str`.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Node::Str(s) => Some(s),
            _ => None,
        }
    }

    /// `copy.deepcopy`.
    pub fn deep_clone(&self) -> Node {
        match self {
            Node::List(l) => Node::list(l.borrow().iter().map(Node::deep_clone).collect()),
            Node::Dict(d) => Node::dict(
                d.borrow()
                    .iter()
                    .map(|(k, v)| (k.clone(), v.deep_clone()))
                    .collect(),
            ),
            Node::Obj(o) => {
                let o = o.borrow();
                Node::obj(Object {
                    class: o.class,
                    fields: o
                        .fields
                        .iter()
                        .map(|(k, v)| (k.clone(), v.deep_clone()))
                        .collect(),
                })
            }
            other => other.clone(),
        }
    }

    /// Python `==` over the data `SchemaMapper` compares (rules are equal when their
    /// dumped forms are).
    pub fn py_eq(&self, other: &Node) -> bool {
        match (self, other) {
            (Node::None, Node::None) => true,
            (Node::Bool(a), Node::Bool(b)) => a == b,
            (Node::Int(a), Node::Int(b)) => a == b,
            (Node::Float(a), Node::Float(b)) => a == b,
            (Node::Int(a), Node::Float(b)) | (Node::Float(b), Node::Int(a)) => *a as f64 == *b,
            (Node::Bool(a), Node::Int(b)) | (Node::Int(b), Node::Bool(a)) => i64::from(*a) == *b,
            (Node::Str(a), Node::Str(b)) => a == b,
            (Node::List(a), Node::List(b)) => {
                let (a, b) = (a.borrow(), b.borrow());
                a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| x.py_eq(y))
            }
            (Node::Dict(a), Node::Dict(b)) => {
                let (a, b) = (a.borrow(), b.borrow());
                a.len() == b.len() && a.iter().all(|(k, v)| b.get(k).is_some_and(|w| v.py_eq(w)))
            }
            (Node::Obj(a), Node::Obj(b)) => {
                Rc::ptr_eq(a, b) || {
                    let (a, b) = (a.borrow(), b.borrow());
                    a.class == b.class
                        && a.fields.len() == b.fields.len()
                        && a.fields
                            .iter()
                            .all(|(k, v)| b.fields.get(k).is_some_and(|w| v.py_eq(w)))
                }
            }
            _ => false,
        }
    }

    /// Python `str(x)`.
    pub fn py_str(&self) -> String {
        match self {
            Node::None => "None".into(),
            Node::Bool(true) => "True".into(),
            Node::Bool(false) => "False".into(),
            Node::Int(i) => i.to_string(),
            Node::Float(f) => py_float_repr(*f),
            Node::Str(s) => s.clone(),
            other => other.py_repr(),
        }
    }

    /// Python `repr(x)` (enough of it for coercing a container to `str`).
    pub fn py_repr(&self) -> String {
        match self {
            Node::Str(s) => {
                let quote = if s.contains('\'') && !s.contains('"') {
                    '"'
                } else {
                    '\''
                };
                let mut out = String::from(quote);
                for c in s.chars() {
                    match c {
                        '\\' => out.push_str("\\\\"),
                        '\n' => out.push_str("\\n"),
                        '\r' => out.push_str("\\r"),
                        '\t' => out.push_str("\\t"),
                        c if c == quote => {
                            out.push('\\');
                            out.push(c);
                        }
                        c => out.push(c),
                    }
                }
                out.push(quote);
                out
            }
            Node::List(l) => format!(
                "[{}]",
                l.borrow()
                    .iter()
                    .map(Node::py_repr)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Node::Dict(d) => format!(
                "{{{}}}",
                d.borrow()
                    .iter()
                    .map(|(k, v)| format!("{}: {}", Node::str(k.clone()).py_repr(), v.py_repr()))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Node::Obj(o) => format!("{}(...)", o.borrow().class),
            other => other.py_str(),
        }
    }
}

fn py_float_repr(f: f64) -> String {
    crate::pyfmt::py_float_repr(f)
}

// ── The metamodel field table ─────────────────────────────────────────────────

/// Scalar coercions `__post_init__` applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scalar {
    /// `str(x)`.
    Str,
    /// `int(x)`.
    Int,
    /// `float(x)`.
    Float,
    /// `Bool(x)`.
    Bool,
    /// `Decimal`-typed fields keep their value (they are dumped as `int`/`float`).
    Decimal,
}

/// How a field's value is normalised.
#[derive(Debug, Clone)]
pub enum Kind {
    /// No normalisation.
    Any,
    /// A scalar.
    Scalar(Scalar),
    /// A list of scalars (a lone value is wrapped).
    ListScalar(Scalar),
    /// A single nested object.
    Obj(String),
    /// A list of nested objects.
    ListObj(String),
    /// `_normalize_inlined_as_dict`.
    Dict {
        /// Class of the values.
        class: String,
        /// Name of the key field.
        key: String,
        /// Whether keys must be unique.
        keyed: bool,
    },
    /// `_normalize_inlined_as_list`.
    ListInlined {
        /// Class of the values.
        class: String,
        /// Name of the key field.
        key: String,
        /// Whether keys must be unique.
        keyed: bool,
    },
}

/// One dataclass field.
#[derive(Debug)]
pub struct Field {
    /// Field name.
    pub name: String,
    /// Normalisation.
    pub kind: Kind,
}

/// One metamodel class.
#[derive(Debug)]
pub struct ClassMeta {
    /// Class name as a `'static` string (so [`Object`] can name its class cheaply).
    pub name: &'static str,
    /// Dataclass fields in `dataclasses.fields()` order.
    pub fields: Vec<Field>,
}

static TABLE: OnceLock<HashMap<&'static str, ClassMeta>> = OnceLock::new();

fn parse_scalar(s: &str) -> Scalar {
    match s {
        "int" => Scalar::Int,
        "float" => Scalar::Float,
        "bool" => Scalar::Bool,
        "decimal" => Scalar::Decimal,
        _ => Scalar::Str,
    }
}

fn parse_kind(s: &str) -> Kind {
    let parts: Vec<&str> = s.split(':').collect();
    match parts.as_slice() {
        ["any"] => Kind::Any,
        ["list", scalar] => Kind::ListScalar(parse_scalar(scalar)),
        ["obj", class] => Kind::Obj((*class).to_string()),
        ["list_obj", class] => Kind::ListObj((*class).to_string()),
        ["dict", class, key, keyed] => Kind::Dict {
            class: (*class).to_string(),
            key: (*key).to_string(),
            keyed: *keyed == "True",
        },
        ["list_inlined", class, key, keyed] => Kind::ListInlined {
            class: (*class).to_string(),
            key: (*key).to_string(),
            keyed: *keyed == "True",
        },
        [scalar] => Kind::Scalar(parse_scalar(scalar)),
        _ => Kind::Any,
    }
}

fn table() -> &'static HashMap<&'static str, ClassMeta> {
    TABLE.get_or_init(|| {
        let raw: IndexMap<String, Vec<(String, String)>> =
            serde_json::from_str(include_str!("metamodel.json"))
                .expect("metamodel.json is generated and valid");
        raw.into_iter()
            .map(|(name, fields)| {
                // The table lives for the whole process; leaking the names once lets
                // `Object` carry a plain `&'static str`.
                let name: &'static str = Box::leak(name.into_boxed_str());
                let fields = fields
                    .into_iter()
                    .map(|(name, kind)| Field {
                        name,
                        kind: parse_kind(&kind),
                    })
                    .collect();
                (name, ClassMeta { name, fields })
            })
            .collect()
    })
}

/// Look up a metamodel class.
pub fn class_meta(name: &str) -> MetamodelResult<&'static ClassMeta> {
    table()
        .get(name)
        .ok_or_else(|| MetamodelError(format!("unknown metamodel class {name}")))
}

// ── Construction (`__post_init__`) ────────────────────────────────────────────

impl Object {
    /// `Class(**kwargs)`: every dataclass field in order, defaulted and normalised
    /// the way the generated `__post_init__` does.
    pub fn construct(class: &str, mut kwargs: IndexMap<String, Node>) -> MetamodelResult<Object> {
        let meta = class_meta(class)?;
        let mut fields = IndexMap::with_capacity(meta.fields.len());
        for field in &meta.fields {
            let given = kwargs.shift_remove(&field.name).unwrap_or_default();
            fields.insert(field.name.clone(), normalise(&field.kind, given)?);
        }
        if !kwargs.is_empty() {
            let unknown: Vec<String> = kwargs
                .iter()
                .map(|(k, v)| format!("Unknown argument: {k} = {}", truncate_repr(v)))
                .collect();
            return err(format!(
                "Unknown arguments for: {class}\n{}",
                unknown.join("\n")
            ));
        }
        let mut object = Object {
            class: meta.name,
            fields,
        };
        if class == "SchemaDefinition" {
            // `if self.default_prefix is None: self.default_prefix = sfx(str(self.id))`
            let missing = matches!(object.get("default_prefix"), Node::None);
            if missing {
                let id = object.get("id").py_str();
                object.set("default_prefix", Node::Str(sfx(&id)));
            }
        }
        Ok(object)
    }

    /// `Class(name=...)` and friends, from `&str` pairs.
    pub fn new(class: &str, pairs: Vec<(&str, Node)>) -> MetamodelResult<Object> {
        Object::construct(
            class,
            pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
        )
    }

    /// `getattr`, `None` when absent.
    pub fn get(&self, name: &str) -> Node {
        self.fields.get(name).cloned().unwrap_or_default()
    }

    /// `setattr`.
    pub fn set(&mut self, name: &str, value: Node) {
        self.fields.insert(name.to_string(), value);
    }

    /// `getattr` as a string.
    pub fn get_str(&self, name: &str) -> Option<String> {
        self.fields
            .get(name)
            .and_then(|n| n.as_str().map(String::from))
    }

    /// The dict stored in `name`.
    pub fn dict(&self, name: &str) -> MetamodelResult<Shared<IndexMap<String, Node>>> {
        match self.fields.get(name) {
            Some(Node::Dict(d)) => Ok(Rc::clone(d)),
            _ => err(format!(
                "'{}' object has no dict attribute '{name}'",
                self.class
            )),
        }
    }

    /// The list stored in `name`.
    pub fn list(&self, name: &str) -> MetamodelResult<Shared<Vec<Node>>> {
        match self.fields.get(name) {
            Some(Node::List(l)) => Ok(Rc::clone(l)),
            _ => err(format!(
                "'{}' object has no list attribute '{name}'",
                self.class
            )),
        }
    }

    /// `copy.copy`: a new object sharing every field value.
    pub fn shallow_copy(&self) -> Object {
        self.clone()
    }

    /// Python `getattr(obj, name)` that raises `AttributeError` for non-fields.
    pub fn require_field(&self, name: &str) -> MetamodelResult<()> {
        if self.fields.contains_key(name) {
            Ok(())
        } else {
            err(format!("'{}' object has no attribute '{name}'", self.class))
        }
    }
}

fn truncate_repr(v: &Node) -> String {
    let r = v.py_repr();
    r.chars().take(40).collect::<String>().replace('\n', "\\n")
}

/// `linkml_runtime.utils.formatutils.sfx`: ensure a URI ends in a namespace delimiter.
pub fn sfx(uri: &str) -> String {
    if uri.ends_with(['/', '#', '_', ':']) {
        uri.to_string()
    } else {
        format!("{uri}/")
    }
}

fn normalise(kind: &Kind, value: Node) -> MetamodelResult<Node> {
    match kind {
        Kind::Any => Ok(value),
        Kind::Scalar(s) => coerce_scalar(*s, value),
        Kind::ListScalar(s) => {
            let items = wrap_list(value);
            let out = items
                .into_iter()
                .map(|v| coerce_scalar(*s, v))
                .collect::<MetamodelResult<Vec<_>>>()?;
            Ok(Node::list(out))
        }
        Kind::Obj(class) => match value {
            Node::None => Ok(Node::None),
            v => Ok(Node::Obj(as_object(class, v)?)),
        },
        Kind::ListObj(class) => {
            let items = wrap_list(value);
            let out = items
                .into_iter()
                .map(|v| as_object(class, v).map(Node::Obj))
                .collect::<MetamodelResult<Vec<_>>>()?;
            Ok(Node::list(out))
        }
        Kind::Dict { class, key, keyed } => normalise_inlined(value, class, key, *keyed, false),
        Kind::ListInlined { class, key, keyed } => {
            normalise_inlined(value, class, key, *keyed, true)
        }
    }
}

/// `if not isinstance(x, list): x = [x] if x is not None else []`
fn wrap_list(value: Node) -> Vec<Node> {
    match value {
        Node::None => Vec::new(),
        Node::List(l) => l.borrow().clone(),
        other => vec![other],
    }
}

fn coerce_scalar(scalar: Scalar, value: Node) -> MetamodelResult<Node> {
    if matches!(value, Node::None) {
        return Ok(Node::None);
    }
    Ok(match scalar {
        Scalar::Decimal => value,
        Scalar::Str => match value {
            Node::Str(_) => value,
            other => Node::Str(other.py_str()),
        },
        Scalar::Bool => match value {
            Node::Bool(_) => value,
            other => {
                // metamodelcore.Bool: `([Tt]rue)|(1)$` / `([Ff]alse)|(0)$` via `re.match`,
                // else a ValueError (`is_strict()` is on by default).
                let text = other.py_str();
                if text.starts_with("True") || text.starts_with("true") || text == "1" {
                    Node::Bool(true)
                } else if text.starts_with("False") || text.starts_with("false") || text == "0" {
                    Node::Bool(false)
                } else {
                    return err(format!("{text}: Must be a boolean value"));
                }
            }
        },
        Scalar::Int => match value {
            Node::Int(_) => value,
            Node::Bool(b) => Node::Int(i64::from(b)),
            Node::Float(f) => Node::Int(f.trunc() as i64),
            Node::Str(s) => match s.trim().parse::<i64>() {
                Ok(i) => Node::Int(i),
                Err(_) => {
                    return err(format!("invalid literal for int() with base 10: '{s}'"));
                }
            },
            other => {
                return err(format!(
                    "int() argument must be a string or a real number, not '{}'",
                    other.py_repr()
                ));
            }
        },
        Scalar::Float => match value {
            Node::Float(_) => value,
            Node::Int(i) => Node::Float(i as f64),
            Node::Bool(b) => Node::Float(f64::from(u8::from(b))),
            Node::Str(s) => match s.trim().parse::<f64>() {
                Ok(f) => Node::Float(f),
                Err(_) => return err(format!("could not convert string to float: '{s}'")),
            },
            other => {
                return err(format!(
                    "float() argument must be a string or a real number, not '{}'",
                    other.py_repr()
                ));
            }
        },
    })
}

/// `Class(**as_dict(v))`, or `v` itself when it already is a `Class`.
fn as_object(class: &str, value: Node) -> MetamodelResult<Shared<Object>> {
    match value {
        Node::Obj(o) if o.borrow().class == class => Ok(o),
        Node::Dict(d) => {
            let kwargs = d.borrow().clone();
            Ok(Rc::new(RefCell::new(Object::construct(class, kwargs)?)))
        }
        other => err(format!(
            "{class}(**as_dict(v)): cannot build from {}",
            other.py_repr()
        )),
    }
}

/// `YAMLRoot._normalize_inlined`.
fn normalise_inlined(
    raw: Node,
    class: &str,
    key_name: &str,
    keyed: bool,
    is_list: bool,
) -> MetamodelResult<Node> {
    let slot_meta = class_meta(class)?;
    // `next(f.name for f in fields(slot_type) if f.name != key_name)`
    let value_field: Option<&str> = slot_meta
        .fields
        .iter()
        .map(|f| f.name.as_str())
        .find(|n| *n != key_name);

    let raw = match raw {
        Node::None => Node::list(Vec::new()),
        Node::Dict(_) | Node::List(_) => raw,
        other => Node::list(vec![other]),
    };

    let mut cooked_list: Vec<Node> = Vec::new();
    let mut cooked_dict: IndexMap<String, Node> = IndexMap::new();
    let mut cooked_keys: Vec<String> = Vec::new();

    let mut order_up = |key: &str, entry: Object| -> MetamodelResult<()> {
        let entry_key = entry.get(key_name);
        if entry_key.as_str() != Some(key) {
            return err(format!(
                "Slot: {class} - attribute {key_name} value ({}) does not match key ({key})",
                entry_key.py_str()
            ));
        }
        if keyed {
            if cooked_keys.iter().any(|k| k == key) {
                return err(format!("{key}: duplicate key"));
            }
            cooked_keys.push(key.to_string());
        }
        let entry = Node::obj(entry);
        if is_list {
            cooked_list.push(entry);
        } else {
            cooked_dict.insert(key.to_string(), entry);
        }
        Ok(())
    };

    let build = |kwargs: Vec<(String, Node)>| -> MetamodelResult<Object> {
        Object::construct(class, kwargs.into_iter().collect())
    };

    // `form_1`: a dict of `key: dict-or-None` entries.
    let form_1 = |entries: &IndexMap<String, Node>,
                  order_up: &mut dyn FnMut(&str, Object) -> MetamodelResult<()>|
     -> MetamodelResult<()> {
        for (key, raw_obj) in entries {
            let mut obj_fields: IndexMap<String, Node> = match raw_obj {
                Node::None => IndexMap::new(),
                Node::Dict(d) => d.borrow().clone(),
                Node::Obj(o) if o.borrow().class == class => {
                    order_up(key, o.borrow().clone())?;
                    continue;
                }
                other => {
                    return err(format!("cannot build {class} from {}", other.py_repr()));
                }
            };
            if !obj_fields.contains_key(key_name) {
                obj_fields.insert(key_name.to_string(), Node::Str(key.clone()));
            }
            order_up(key, build(obj_fields.into_iter().collect())?)?;
        }
        Ok(())
    };

    match &raw {
        Node::List(list) => {
            for entry in list.borrow().iter() {
                match entry {
                    Node::Obj(o) if o.borrow().class == class => {
                        let key = o.borrow().get(key_name).py_str();
                        order_up(&key, o.borrow().clone())?;
                    }
                    Node::Dict(d) => {
                        let d = d.borrow();
                        if d.len() == 1 {
                            let (lek, lev) = d.iter().next().expect("len == 1");
                            if lek == key_name && !matches!(lev, Node::Dict(_)) {
                                // key_name: value
                                let key = lev.py_str();
                                order_up(&key, build(vec![(key_name.to_string(), lev.clone())])?)?;
                            } else if !matches!(lev, Node::List(_) | Node::Dict(_)) {
                                // key: value, the SimpleDict shorthand
                                let mut kwargs =
                                    vec![(key_name.to_string(), Node::Str(lek.clone()))];
                                if let Some(vf) = value_field {
                                    kwargs.push((vf.to_string(), lev.clone()));
                                }
                                order_up(lek, build(kwargs)?)?;
                            } else {
                                form_1(&d, &mut order_up)?;
                            }
                        } else {
                            // **kwargs
                            let obj = build(d.clone().into_iter().collect())?;
                            let key = obj.get(key_name).py_str();
                            order_up(&key, obj)?;
                        }
                    }
                    Node::List(inner) => {
                        // first element is the key; the rest map to the non-key fields in order
                        let inner = inner.borrow();
                        let Some(first) = inner.first() else {
                            return err("empty list entry in inlined slot");
                        };
                        let mut kwargs = vec![(key_name.to_string(), first.clone())];
                        let non_key = slot_meta.fields.iter().filter(|f| f.name != key_name);
                        for (field, val) in non_key.zip(inner.iter().skip(1)) {
                            kwargs.push((field.name.clone(), val.clone()));
                        }
                        let obj = build(kwargs)?;
                        let key = obj.get(key_name).py_str();
                        order_up(&key, obj)?;
                    }
                    lone => {
                        // a lone key
                        let key = lone.py_str();
                        order_up(&key, build(vec![(key_name.to_string(), lone.clone())])?)?;
                    }
                }
            }
        }
        Node::Dict(dict) => {
            let dict = dict.borrow();
            let vanilla = dict
                .get(key_name)
                .is_some_and(|v| !matches!(v, Node::None | Node::List(_) | Node::Dict(_)));
            if vanilla {
                // {key: v11, s12: v12, ...}
                let obj = build(dict.clone().into_iter().collect())?;
                let key = obj.get(key_name).py_str();
                order_up(&key, obj)?;
            } else {
                // {key1: {obj1}, key2: {obj2}...} or {key1:, key2:, ...}
                for (k, v) in dict.iter() {
                    match v {
                        Node::None => {
                            let single: IndexMap<String, Node> =
                                IndexMap::from([(k.clone(), Node::dict(IndexMap::new()))]);
                            form_1(&single, &mut order_up)?;
                        }
                        Node::Obj(_) | Node::Dict(_) => {
                            let single: IndexMap<String, Node> =
                                IndexMap::from([(k.clone(), v.clone())]);
                            form_1(&single, &mut order_up)?;
                        }
                        scalar_or_list => {
                            // SimpleDict form: value may be scalar or list
                            let mut kwargs = vec![(key_name.to_string(), Node::Str(k.clone()))];
                            if let Some(vf) = value_field {
                                kwargs.push((vf.to_string(), scalar_or_list.clone()));
                            }
                            order_up(k, build(kwargs)?)?;
                        }
                    }
                }
            }
        }
        _ => unreachable!("raw was wrapped into a list or dict above"),
    }

    Ok(if is_list {
        Node::list(cooked_list)
    } else {
        Node::dict(cooked_dict)
    })
}

// ── Dumping (`remove_empty_items`) ────────────────────────────────────────────

/// `linkml_runtime.utils.formatutils.remove_empty_items(obj, hide_protected_keys=True)`.
///
/// Returns `None` where Python returns `None` (an "empty" nested value); the
/// outermost container is kept even when empty.
pub fn remove_empty_items(node: &Node, inside: bool) -> Option<Value> {
    match node {
        Node::None => None,
        Node::Bool(b) => Some(Value::Bool(*b)),
        Node::Int(i) => Some(Value::Int(*i)),
        Node::Float(f) => Some(Value::Float(*f)),
        Node::Str(s) => Some(Value::Str(s.clone())),
        Node::List(l) => {
            let items: Vec<Value> = l
                .borrow()
                .iter()
                .filter(|e| e.as_str() != Some("_root"))
                .filter_map(|e| remove_empty_items(e, true))
                .collect();
            (!inside || !items.is_empty()).then_some(Value::List(items))
        }
        Node::Dict(d) => remove_empty_pairs(d.borrow().iter(), inside),
        Node::Obj(o) => remove_empty_pairs(o.borrow().fields.iter(), inside),
    }
}

fn remove_empty_pairs<'a>(
    pairs: impl Iterator<Item = (&'a String, &'a Node)>,
    inside: bool,
) -> Option<Value> {
    let mut out: IndexMap<String, Value> = pairs
        .filter_map(|(k, v)| remove_empty_items(v, true).map(|v| (k.clone(), v)))
        .collect();
    // Remove the additional level of nesting with enums.
    if out.len() == 1
        && let Some(Value::Map(code)) = out.get("_code")
        && let Some(text) = code.get("text")
        && !text.is_null()
    {
        return Some(text.clone());
    }
    // `hide_protected_keys`: a lone `_key: {..}` is replaced by its value.
    if out.len() == 1
        && out.keys().next().is_some_and(|k| k.starts_with('_'))
        && let Some(Value::Map(_)) = out.values().next()
        && let Some(Value::Map(inner)) = out.shift_remove_index(0).map(|(_, v)| v)
    {
        out = inner;
    }
    (!inside || !out.is_empty()).then_some(Value::Map(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict(pairs: Vec<(&str, Node)>) -> Node {
        Node::dict(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }

    #[test]
    fn field_order_and_defaults() {
        let o = Object::new("ClassDefinition", vec![("name", Node::str("A"))]).unwrap();
        let keys: Vec<_> = o.fields.keys().collect();
        assert_eq!(keys[0], "name");
        let pos = |k: &str| keys.iter().position(|x| *x == k).unwrap();
        assert!(pos("description") < pos("is_a"));
        assert!(pos("is_a") < pos("attributes"));
        assert!(pos("attributes") < pos("tree_root"));
        let value = remove_empty_items(&Node::obj(o), false).unwrap();
        assert_eq!(serde_json::to_string(&value).unwrap(), r#"{"name":"A"}"#);
    }

    #[test]
    fn bool_coercion_follows_metamodelcore() {
        let bad = Object::new(
            "SlotDefinition",
            vec![("name", Node::str("s")), ("required", Node::str("no"))],
        );
        assert_eq!(bad.unwrap_err().0, "no: Must be a boolean value");
        let o = Object::new(
            "SlotDefinition",
            vec![
                ("name", Node::str("s")),
                ("identifier", Node::str("true")),
                ("multivalued", Node::str("False")),
            ],
        )
        .unwrap();
        assert!(matches!(o.get("identifier"), Node::Bool(true)));
        assert!(matches!(o.get("multivalued"), Node::Bool(false)));
    }

    #[test]
    fn inlined_dict_forms() {
        // key filled in from the dict key, `None` body, SimpleDict shorthand.
        let o = Object::new(
            "ClassDefinition",
            vec![
                ("name", Node::str("C")),
                (
                    "attributes",
                    dict(vec![
                        ("a", dict(vec![("range", Node::str("string"))])),
                        ("b", Node::None),
                    ]),
                ),
                ("annotations", dict(vec![("k", Node::str("v"))])),
            ],
        )
        .unwrap();
        let v = remove_empty_items(&Node::obj(o), false).unwrap();
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            r#"{"name":"C","annotations":{"k":{"tag":"k","value":"v"}},"attributes":{"a":{"name":"a","range":"string"},"b":{"name":"b"}}}"#
        );
    }

    #[test]
    fn schema_default_prefix_is_derived_from_id() {
        let o = Object::new(
            "SchemaDefinition",
            vec![
                ("id", Node::str("https://x.org/s")),
                ("name", Node::str("s")),
            ],
        )
        .unwrap();
        assert_eq!(
            o.get_str("default_prefix").as_deref(),
            Some("https://x.org/s/")
        );
    }

    #[test]
    fn unknown_argument_is_an_error() {
        let e = Object::new(
            "SlotDefinition",
            vec![("name", Node::str("s")), ("nope", Node::Int(1))],
        );
        assert!(e.unwrap_err().0.contains("Unknown argument: nope"));
    }

    #[test]
    fn remove_empty_items_drops_nested_empties_and_root_strings() {
        let n = dict(vec![
            (
                "a",
                Node::list(vec![
                    Node::str("_root"),
                    Node::dict(IndexMap::new()),
                    Node::Int(1),
                ]),
            ),
            (
                "b",
                Node::dict(IndexMap::from([("c".to_string(), Node::None)])),
            ),
            ("d", Node::Bool(false)),
        ]);
        let v = remove_empty_items(&n, false).unwrap();
        assert_eq!(serde_json::to_string(&v).unwrap(), r#"{"a":[1],"d":false}"#);
    }
}
