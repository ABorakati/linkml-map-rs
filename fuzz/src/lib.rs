//! Helpers shared by the fuzz targets.

use arbitrary::Arbitrary;
use indexmap::IndexMap;
use linkml_map_core::decimal::Decimal;
use linkml_map_core::schema::{
    ClassDef, EnumDef, InMemorySchema, InMemorySchemaBuilder, PermissibleValue, RangeKind, SlotDef,
};
use linkml_map_core::value::{Temporal, Value};
use linkml_map_io::Format;

/// An arbitrary [`Value`] (no `Value: Arbitrary` in the library).
#[derive(Debug, Arbitrary)]
pub enum FuzzValue {
    Null,
    Bool(bool),
    Int(i64),
    /// Digits of an integer of any size (anything else is 0).
    BigInt(String),
    Float(f64),
    Decimal(String),
    /// A `datetime.date` / `datetime.datetime` from timestamp text (a fixed date if invalid).
    Temporal(String),
    Str(String),
    List(Vec<FuzzValue>),
    Map(Vec<(String, FuzzValue)>),
}

impl FuzzValue {
    pub fn into_value(self) -> Value {
        match self {
            FuzzValue::Null => Value::Null,
            FuzzValue::Bool(b) => Value::Bool(b),
            FuzzValue::Int(i) => Value::Int(i),
            FuzzValue::BigInt(s) => Value::from_bigint(
                // Within CPython's 4300-digit limit, as the readers enforce.
                s.get(..s.len().min(4_000))
                    .and_then(|t| t.parse::<linkml_map_core::pynum::BigInt>().ok())
                    .unwrap_or_default(),
            ),
            FuzzValue::Float(f) => Value::Float(f),
            FuzzValue::Decimal(s) => {
                let d = Decimal::parse_python(&s).unwrap_or_else(|_| Decimal::from_i64(0));
                // `1e300000000` is written out digit by digit (`format(d, 'f')`, as upstream
                // does too): 300 MB of output from a 12-byte input. Not a finding.
                let huge = sci_exponent(&d.to_sci_string()).is_some_and(|e| e.unsigned_abs() > 5_000);
                Value::Decimal(if huge && !UNGUARDED { Decimal::from_i64(0) } else { d })
            }
            FuzzValue::Temporal(s) => Value::Temporal(
                Temporal::from_yaml_timestamp(&s)
                    .unwrap_or_else(|_| Temporal::from_yaml_timestamp("2000-01-01").expect("date")),
            ),
            FuzzValue::Str(s) => Value::Str(s),
            FuzzValue::List(items) => Value::List(items.into_iter().map(Self::into_value).collect()),
            FuzzValue::Map(pairs) => Value::Map(
                pairs
                    .into_iter()
                    .map(|(k, v)| (k, v.into_value()))
                    .collect::<IndexMap<_, _>>(),
            ),
        }
    }
}

/// True when `src` has a single-`*` operator (list repetition: `[1] * 10**12` aborts the
/// process; reported, see `list_repetition_overflow_is_an_error`).
pub fn has_repetition(src: &str) -> bool {
    src.replace("**", "").contains('*')
}

/// Whether the known, reported crashes are in play (`--features unguarded`).
pub const UNGUARDED: bool = cfg!(feature = "unguarded");

/// Longest source the expression fuzzers explore: the parser and evaluator recurse once
/// per nesting level and overflow a 2 MiB stack at ~600 levels (reported).
pub const MAX_EXPR_LEN: usize = 400;

/// Exponent of a `Decimal`'s `to_sci_string()` text (`E+X` / `E-X`), if any.
pub fn sci_exponent(sci: &str) -> Option<i64> {
    let (_, exp) = sci.split_once('E')?;
    exp.parse().ok()
}

/// A small schema whose names match the specs the fuzzers produce.
pub fn tiny_schema() -> InMemorySchema {
    fn slot(name: &str, range: RangeKind) -> SlotDef {
        SlotDef {
            name: name.into(),
            range,
            multivalued: false,
            inlined: false,
            inlined_as_list: false,
            required: false,
            identifier: false,
            key: false,
            unit: None,
            any_of_enums: vec![],
        }
    }
    let mut b = InMemorySchemaBuilder::new()
        .with_schema_id("https://example.org/tiny")
        .add_type("string")
        .add_type("integer")
        .add_type("float")
        .add_type("decimal")
        .add_enum(EnumDef {
            name: "Kind".into(),
            permissible_values: ["A", "B"]
                .iter()
                .map(|t| PermissibleValue {
                    text: (*t).into(),
                    ..Default::default()
                })
                .collect(),
            is_dynamic: false,
        });
    for c in ["Person", "Row", "Entity", "Address", "Agent"] {
        b = b.add_class(ClassDef {
            name: c.into(),
            tree_root: c == "Person",
            is_a: None,
            mixins: vec![],
        });
        let mut id = slot("id", RangeKind::Type("string".into()));
        id.identifier = true;
        let mut address = slot("address", RangeKind::Class("Address".into()));
        address.inlined = true;
        for s in [
            id,
            slot("name", RangeKind::Type("string".into())),
            slot("x", RangeKind::Type("integer".into())),
            slot("y", RangeKind::Type("float".into())),
            slot("value", RangeKind::Type("decimal".into())),
            slot("kind", RangeKind::Enum("Kind".into())),
            address,
        ] {
            b = b.add_slot(c, s);
        }
    }
    b.build()
}

/// Run `Format`'s loader over `bytes` (written to a scratch file: the loaders are
/// file-based) and return how many records it produced, or the error text.
pub fn load_bytes(format: Format, bytes: &[u8]) -> Result<usize, String> {
    use std::sync::OnceLock;
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    let rt = RT.get_or_init(|| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
    });
    let dir = if std::path::Path::new("/dev/shm").is_dir() {
        std::path::PathBuf::from("/dev/shm")
    } else {
        std::env::temp_dir()
    };
    let path = dir.join(format!("linkml-map-fuzz-{}.{}", std::process::id(), format.extension()));
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    let out = rt
        .block_on(linkml_map_io::load_all(&path, format))
        .map(|v| v.len())
        .map_err(|e| format!("{e:#}"));
    let _ = std::fs::remove_file(&path);
    out
}

/// Whether any string or key in `v` contains U+0085 (NEL): a YAML 1.1 line break that
/// PyYAML writes inside a quoted scalar and reads back as a space, so it cannot round-trip
/// (the port is byte-identical to PyYAML's emitter).
pub fn value_has_nel(v: &Value) -> bool {
    match v {
        Value::Str(s) => s.contains('\u{85}'),
        Value::List(items) => items.iter().any(value_has_nel),
        Value::Map(m) => m.iter().any(|(k, v)| k.contains('\u{85}') || value_has_nel(v)),
        _ => false,
    }
}

/// Equality for round trips: NaN equals NaN, map key order is ignored.
pub fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Float(x), Value::Float(y)) => x == y || (x.is_nan() && y.is_nan()),
        (Value::List(x), Value::List(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q)),
        (Value::Map(x), Value::Map(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w)))
        }
        _ => a == b,
    }
}
