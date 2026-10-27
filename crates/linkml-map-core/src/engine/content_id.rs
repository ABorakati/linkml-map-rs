//! Identifiers synthesised from the content of a derived record.
//!
//! Port of `linkml_map.transformer.content_id` plus the
//! `ObjectTransformer` hooks of upstream `76db207` (#342) and `e70715e`.
//!
//! When a target class has an identifier slot and the spec derives none, the
//! record's id is a UUID v5 hash of the record itself:
//!
//! ```text
//! id = uuid5(f"{target schema id}/{TargetClass}", canonical(record))
//! ```
//!
//! The same harmonised record always gets the same id, independent of row
//! order, chunking or parallel execution: [`ContentIds`] is an immutable,
//! pure function of the record content, so one instance can be shared by
//! reference across rayon workers. Minting is opt-in with `mint_ids: true` on
//! the specification, a target class's entry in `class_defaults`, or a class
//! derivation (see [`mints_ids`]); an explicit derivation of the identifier
//! slot always wins. An identifier that is neither derived nor minted is an
//! error.
//!
//! The canonical form is a contract; changing it changes every synthesised id:
//!
//! - objects: keys sorted; entries whose value is null, an empty list or an
//!   empty object are dropped
//! - lists: items canonicalised and sorted by their encoding, unless the target
//!   slot declares `list_elements_ordered`
//! - numbers: ints and floats share one format, fixed-point, no trailing zeros,
//!   no exponent, `-0` as `0`; floats are first rounded to 15 significant
//!   digits so unit-conversion noise does not split a record
//! - booleans and strings: JSON literals; strings escaped as `json.dumps` does
//!   with `ensure_ascii=False`
//! - whitespace: none
//!
//! TODO(decimal): `Value::Decimal` (added by the `range: decimal` change,
//! upstream `72c0c28`) is not handled here yet. Upstream encodes a `Decimal`
//! exactly like an int/float after `Decimal.normalize()`, which also rounds to
//! the active context precision (28 significant digits, half-even); see
//! [`canonical`]'s catch-all arm.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::OnceLock;

use indexmap::IndexMap;
use uuid::Uuid;

use super::ObjectTransformer;
use crate::datamodel::{ClassDerivation, TransformationSpecification};
use crate::error::{Error, Result};
use crate::schema::{RangeKind, SchemaProvider};
use crate::value::Value;

// ── Settings ─────────────────────────────────────────────────────────────────

/// Whether a class derivation mints ids.
///
/// The first setting given wins: the derivation's own, then its target class's
/// entry in `class_defaults`, then the specification's; with none given,
/// minting is off. Mirrors `content_id.mints_ids`.
///
/// ```ignore
/// use linkml_map_core::engine::content_id::mints_ids;
/// assert_eq!((mints_ids(None, None, None), mints_ids(Some(true), None, None)), (false, true));
/// assert_eq!(
///     (mints_ids(Some(false), Some(true), None), mints_ids(Some(false), Some(true), Some(false))),
///     (true, false)
/// );
/// ```
pub fn mints_ids(
    spec_mint_ids: Option<bool>,
    default_mint_ids: Option<bool>,
    class_mint_ids: Option<bool>,
) -> bool {
    class_mint_ids
        .or(default_mint_ids)
        .or(spec_mint_ids)
        .unwrap_or(false)
}

/// Explain an identifier slot that a class derivation neither derives nor
/// mints. Mirrors `content_id.missing_identifier_message`.
pub fn missing_identifier_message(class_name: &str, id_slot: &str) -> String {
    format!(
        "Class derivation '{class_name}' does not derive identifier slot '{id_slot}' and mint_ids is not true; \
         derive it explicitly or set mint_ids: true"
    )
}

// ── Canonical form ───────────────────────────────────────────────────────────

/// A value reduced to its canonical structure (Python: nested dicts, lists,
/// strings, booleans and normalised decimals).
#[derive(Debug, Clone, PartialEq)]
pub enum Canon {
    Null,
    Bool(bool),
    Str(String),
    /// A normalised number, already laid out in fixed-point notation
    /// (Python: `format(Decimal, "f")`).
    Num(String),
    List(Vec<Canon>),
    /// Keys are sorted (Python: `sorted(value.items())`); code-point order of a
    /// `str` equals the byte order of its UTF-8, which is what `BTreeMap` uses.
    Map(BTreeMap<String, Canon>),
}

/// `{slot: (list_elements_ordered, range class or None)}` for one target class.
type SlotDescription = HashMap<String, (bool, Option<String>)>;

/// Reduce *value* to its canonical structure.
///
/// Mirrors `content_id._canonical`.
///
/// - `ordered`: whether a list here keeps its order.
/// - `ids`: supplies the per-class slot descriptions (`describe`); `None`
///   describes every class as empty.
/// - `class_name`: the target class *value* instantiates, if it is an object.
///
/// # Errors
/// A value with no canonical form (Python: `TypeError`).
pub fn canonical(
    value: &Value,
    ordered: bool,
    ids: Option<&ContentIds>,
    class_name: Option<&str>,
) -> std::result::Result<Canon, String> {
    // The match is exhaustive over today's `Value`; the trailing arm is the
    // Python `TypeError` branch and also catches any variant added later.
    #[allow(unreachable_patterns)]
    match value {
        Value::Map(map) => canonical_map(map, ids, class_name),
        Value::List(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                let c = canonical(item, false, ids, class_name)?;
                if !is_absent(&c) {
                    out.push(c);
                }
            }
            if !ordered {
                // PARITY: content_id.py::_canonical sorts by the *encoded text*
                // (`sorted(items, key=_encode)`), so numbers sort as text.
                out.sort_by_cached_key(encode);
            }
            Ok(Canon::List(out))
        }
        Value::Null => Ok(Canon::Null),
        Value::Bool(b) => Ok(Canon::Bool(*b)),
        Value::Str(s) => Ok(Canon::Str(s.clone())),
        Value::Int(i) => Ok(Canon::Num(canonical_int(*i))),
        Value::Float(f) => Ok(Canon::Num(canonical_float(*f))),
        // TODO(decimal): add a `Value::Decimal` arm once that variant exists:
        // upstream `_canonical_number` takes `Decimal(value)`, returns `0` for
        // any zero, else `number.normalize()` — which rounds to 28 significant
        // digits, half-even (PARITY, probably unintended upstream) — encoded
        // with `format(d, "f")`.
        other => Err(format!(
            "Cannot synthesize an identifier from a value of type {}: {other:?}",
            variant_name(other)
        )),
    }
}

/// The dict branch of [`canonical`], taking the map directly so the record
/// being hashed need not be wrapped (and cloned) as a [`Value`].
fn canonical_map(
    map: &IndexMap<String, Value>,
    ids: Option<&ContentIds>,
    class_name: Option<&str>,
) -> std::result::Result<Canon, String> {
    let slots: Option<&SlotDescription> = match (ids, class_name) {
        (Some(ids), Some(class)) if !class.is_empty() => ids.describe(class),
        _ => None,
    };
    let mut out = BTreeMap::new();
    for (key, item) in map {
        let (item_ordered, item_class) = match slots.and_then(|s| s.get(key)) {
            Some((ordered, range)) => (*ordered, range.as_deref()),
            None => (false, None),
        };
        let c = canonical(item, item_ordered, ids, item_class)?;
        if !is_absent(&c) {
            out.insert(key.clone(), c);
        }
    }
    Ok(Canon::Map(out))
}

fn variant_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Int(_) => "int",
        Value::Float(_) => "float",
        Value::Str(_) => "str",
        Value::List(_) => "list",
        Value::Map(_) => "dict",
        #[allow(unreachable_patterns)]
        _ => "unsupported",
    }
}

/// Normalise an integer (Python: `Decimal(value).normalize()`, `0` for zero).
///
/// An `i64` has at most 19 digits, well inside the 28-digit decimal context,
/// so no rounding applies.
fn canonical_int(value: i64) -> String {
    value.to_string()
}

/// Normalise a float so equal values encode identically.
///
/// Python: `Decimal(format(value, ".15g"))`, zero (also `-0.0`) becomes `0`,
/// else `normalize()`, encoded with `format(d, "f")`. Here: round to 15
/// significant digits with `{:.14e}` (correctly rounded, half-even on exact
/// ties, like Python's `.15g`), drop trailing zeros and lay the digits out in
/// fixed-point notation. Non-finite floats give `NaN` / `Infinity` /
/// `-Infinity`, as `Decimal` spells them.
fn canonical_float(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_string();
    }
    if value.is_infinite() {
        return if value < 0.0 { "-Infinity" } else { "Infinity" }.to_string();
    }
    if value == 0.0 {
        return "0".to_string();
    }
    let sci = format!("{:.14e}", value.abs());
    let (mantissa, exp) = sci
        .split_once('e')
        .expect("`{:e}` formatting always contains an exponent");
    let exp: i32 = exp.parse().expect("`{:e}` exponent is an integer");
    let mut digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let trimmed = digits.trim_end_matches('0').len().max(1);
    digits.truncate(trimmed);
    let sign = if value < 0.0 { "-" } else { "" };
    format!("{sign}{}", lay_out_fixed(&digits, exp))
}

/// Place a decimal point in `digits` (`d.ddd × 10^exp`) without an exponent.
fn lay_out_fixed(digits: &str, exp: i32) -> String {
    let point = exp + 1; // number of digits before the decimal point
    let n = digits.len() as i32;
    if point >= n {
        format!("{digits}{}", "0".repeat((point - n) as usize))
    } else if point > 0 {
        format!(
            "{}.{}",
            &digits[..point as usize],
            &digits[point as usize..]
        )
    } else {
        format!("0.{}{digits}", "0".repeat((-point) as usize))
    }
}

/// Whether a canonical value counts as missing: null, `[]` or `{}`.
fn is_absent(value: &Canon) -> bool {
    match value {
        Canon::Null => true,
        Canon::List(l) => l.is_empty(),
        Canon::Map(m) => m.is_empty(),
        _ => false,
    }
}

/// Serialise a canonical structure; see the module docs for the format.
pub fn encode(value: &Canon) -> String {
    let mut out = String::new();
    encode_into(value, &mut out);
    out
}

fn encode_into(value: &Canon, out: &mut String) {
    match value {
        Canon::Map(map) => {
            out.push('{');
            for (i, (k, v)) in map.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                encode_str(k, out);
                out.push(':');
                encode_into(v, out);
            }
            out.push('}');
        }
        Canon::List(items) => {
            out.push('[');
            for (i, v) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                encode_into(v, out);
            }
            out.push(']');
        }
        Canon::Num(n) => out.push_str(n),
        Canon::Str(s) => encode_str(s, out),
        Canon::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Canon::Null => out.push_str("null"),
    }
}

/// What `json.dumps(value, ensure_ascii=False)` does for a string
/// (`json.encoder.encode_basestring`): escape `"`, `\`, the C0 controls
/// (`\n \r \t \b \f` by name, the rest as lower-case `\u00xx`); everything else,
/// DEL and non-ASCII included, is emitted raw.
fn encode_str(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Encode *value* in the canonical form, ignoring any schema-declared list
/// order. Mirrors `content_id.canonical_json`.
///
/// ```ignore
/// use linkml_map_core::engine::content_id::canonical_json;
/// use linkml_map_core::value::Value;
/// use indexmap::IndexMap;
///
/// let mut m = IndexMap::new();
/// m.insert("b".to_string(), Value::Float(5.0));
/// m.insert("a".to_string(), Value::Null);
/// m.insert("c".to_string(), Value::List(vec![]));
/// m.insert("d".to_string(), Value::List(vec![Value::Str("y".into()), Value::Str("x".into())]));
/// assert_eq!(canonical_json(&Value::Map(m)).unwrap(), r#"{"b":5,"d":["x","y"]}"#);
/// ```
///
/// # Errors
/// A value with no canonical form.
pub fn canonical_json(value: &Value) -> std::result::Result<String, String> {
    Ok(encode(&canonical(value, false, None, None)?))
}

// ── Synthesizer ──────────────────────────────────────────────────────────────

/// Everything content-id synthesis needs to know about one target class.
#[derive(Debug)]
struct ClassInfo {
    /// The identifier slot of records of this class, if they need one: `None`
    /// when the class has no identifier, is not in the schema, or is abstract
    /// or a mixin (it never has instances of its own).
    identifier_slot: Option<String>,
    /// `{slot: (list_elements_ordered, range class or None)}`.
    slots: SlotDescription,
    /// `uuid5(NAMESPACE_URL, "<target schema id>/<class>")`.
    namespace: Uuid,
}

/// Synthesises content-hash identifiers for records of one target schema.
///
/// Mirrors `content_id.ContentIdSynthesizer`, but the per-class caches are
/// built eagerly and are immutable afterwards, so a single instance is
/// `Send + Sync` without locks and can be borrowed by every per-row engine.
#[derive(Debug)]
pub struct ContentIds {
    /// The target schema's `id`; the base of every namespace.
    schema_id: String,
    classes: HashMap<String, ClassInfo>,
}

impl ContentIds {
    /// Describe `class_names` and every class reachable from them through
    /// class-ranged slots (the classes whose slot descriptions canonicalisation
    /// can ask for).
    pub fn build<'a>(
        target: &dyn SchemaProvider,
        class_names: impl IntoIterator<Item = &'a str>,
    ) -> Self {
        // PARITY: content_id.py builds the namespace with an f-string, so a
        // schema without an `id` would render as "None".
        let schema_id = target.schema_id().unwrap_or_else(|| "None".to_string());
        let mut classes = HashMap::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<String> = VecDeque::new();
        for name in class_names {
            if seen.insert(name.to_string()) {
                queue.push_back(name.to_string());
            }
        }
        while let Some(name) = queue.pop_front() {
            let mut slots = SlotDescription::new();
            let mut identifier_slot = None;
            if let Ok(induced) = target.induced_slots(&name) {
                let ordered: HashSet<String> = target
                    .list_elements_ordered_slots(&name)
                    .into_iter()
                    .collect();
                for slot in induced {
                    let range_class = match &slot.range {
                        RangeKind::Class(c) => Some(c.clone()),
                        _ => None,
                    };
                    if let Some(c) = &range_class
                        && seen.insert(c.clone())
                    {
                        queue.push_back(c.clone());
                    }
                    slots.insert(
                        slot.name.clone(),
                        (ordered.contains(&slot.name), range_class),
                    );
                }
                if !target.is_abstract_or_mixin(&name) {
                    identifier_slot = target.identifier_slot_without_key(&name);
                }
            }
            // Same two-level scheme as the `uuid5()` expression function.
            let namespace = Uuid::new_v5(
                &Uuid::NAMESPACE_URL,
                format!("{schema_id}/{name}").as_bytes(),
            );
            classes.insert(
                name,
                ClassInfo {
                    identifier_slot,
                    slots,
                    namespace,
                },
            );
        }
        Self { schema_id, classes }
    }

    /// [`ContentIds::build`] over every class derivation name of `spec`, nested
    /// ones included (the classes whose records can be minted).
    pub fn for_spec(target: &dyn SchemaProvider, spec: &TransformationSpecification) -> Self {
        let mut names = Vec::new();
        for cd in spec.class_derivations.iter().flatten() {
            collect_class_derivation_names(cd, &mut names);
        }
        Self::build(target, names.iter().map(String::as_str))
    }

    /// The identifier slot of `class_name`, if records of it need one.
    ///
    /// `None` if the class has no identifier, is not in the schema, or is
    /// abstract or a mixin. Mirrors `ContentIdSynthesizer.identifier_slot`.
    pub fn identifier_slot(&self, class_name: &str) -> Option<&str> {
        self.classes.get(class_name)?.identifier_slot.as_deref()
    }

    fn describe(&self, class_name: &str) -> Option<&SlotDescription> {
        self.classes.get(class_name).map(|c| &c.slots)
    }

    /// Hash `record`, an instance of target class `class_name`, into a UUID v5
    /// string. Mirrors `ContentIdSynthesizer.content_id`.
    ///
    /// # Errors
    /// A value in `record` with no canonical form.
    pub fn content_id(
        &self,
        record: &IndexMap<String, Value>,
        class_name: &str,
    ) -> std::result::Result<String, String> {
        let namespace = match self.classes.get(class_name) {
            Some(info) => info.namespace,
            None => Uuid::new_v5(
                &Uuid::NAMESPACE_URL,
                format!("{}/{class_name}", self.schema_id).as_bytes(),
            ),
        };
        let canon = canonical_map(record, Some(self), Some(class_name))?;
        Ok(Uuid::new_v5(&namespace, encode(&canon).as_bytes()).to_string())
    }
}

/// Names of `class_derivations` and every class derivation nested under their
/// slot derivations, outer before inner.
fn collect_class_derivation_names(cd: &ClassDerivation, out: &mut Vec<String>) {
    out.push(cd.name.clone());
    for sd in cd.slot_derivations.iter().flatten().map(|(_, v)| v) {
        for nested in sd.class_derivations.iter().flatten().map(|(_, v)| v) {
            collect_class_derivation_names(nested, out);
        }
    }
}

/// `_all_class_derivations`: yield `cd` and every class derivation nested under
/// its slots.
fn all_class_derivations<'a>(cd: &'a ClassDerivation, out: &mut Vec<&'a ClassDerivation>) {
    out.push(cd);
    for sd in cd.slot_derivations.iter().flatten().map(|(_, v)| v) {
        for nested in sd.class_derivations.iter().flatten().map(|(_, v)| v) {
            all_class_derivations(nested, out);
        }
    }
}

// ── Engine hooks ─────────────────────────────────────────────────────────────

impl<'s> ObjectTransformer<'s> {
    /// Share a pre-built [`ContentIds`] (built once per plan from the target
    /// schema) instead of building one lazily per engine.
    pub fn with_content_ids(mut self, content_ids: &'s ContentIds) -> Self {
        self.content_ids = Some(content_ids);
        self
    }

    /// Emit records whose identifier is neither derived nor minted, instead of
    /// raising. For `--continue-on-error` runs, whose caller reports the
    /// missing identifiers once, up front (see
    /// [`ObjectTransformer::missing_identifiers`]). Mirrors
    /// `ObjectTransformer.emit_without_identifiers`.
    pub fn with_emit_without_identifiers(mut self, emit: bool) -> Self {
        self.emit_without_identifiers = emit;
        self
    }

    /// The synthesiser for the target schema; `None` without a target schema.
    fn content_ids(&self) -> Option<&ContentIds> {
        let target = self.target_schema?;
        if let Some(ids) = self.content_ids {
            return Some(ids);
        }
        Some(
            self.owned_content_ids
                .get_or_init(|| ContentIds::for_spec(target, &self.spec)),
        )
    }

    /// Prepend a content-hash id when the target identifier is minted rather
    /// than derived. Mirrors `ObjectTransformer._with_synthesized_id`.
    ///
    /// # Errors
    /// [`Error::MissingIdentifier`] if the identifier is neither derived nor
    /// minted.
    pub(super) fn with_synthesized_id(
        &self,
        record: IndexMap<String, Value>,
        class_deriv: &ClassDerivation,
    ) -> Result<Value> {
        let Some(id_slot) = self.underived_identifier(class_deriv) else {
            return Ok(Value::Map(record));
        };
        if !self.mints_ids(class_deriv) {
            if self.emit_without_identifiers {
                return Ok(Value::Map(record));
            }
            return Err(Error::MissingIdentifier {
                message: missing_identifier_message(&class_deriv.name, id_slot),
                class_derivation_name: class_deriv.name.clone(),
                class_populated_from: class_deriv.populated_from.clone(),
            });
        }
        // `underived_identifier` only returns `Some` when a synthesiser exists.
        let ids = self.content_ids().expect("target schema present");
        let id = ids
            .content_id(&record, &class_deriv.name)
            .map_err(|cause| Error::SlotTransform {
                class: class_deriv.name.clone(),
                slot: id_slot.to_string(),
                cause,
            })?;
        // `{id_slot: id, **record}`: the id leads; a (never expected) existing
        // key of the same name keeps the first position but takes the record's
        // value, as in a Python dict display.
        let mut out = IndexMap::with_capacity(record.len() + 1);
        out.insert(id_slot.to_string(), Value::Str(id));
        out.extend(record);
        Ok(Value::Map(out))
    }

    /// Whether `class_deriv` mints ids, per [`mints_ids`]. Mirrors
    /// `ObjectTransformer._mints_ids`.
    fn mints_ids(&self, class_deriv: &ClassDerivation) -> bool {
        let spec = &self.spec;
        let default = spec
            .class_defaults
            .as_ref()
            .and_then(|d| d.get(&class_deriv.name))
            .and_then(|d| d.mint_ids);
        mints_ids(spec.mint_ids, default, class_deriv.mint_ids)
    }

    /// The target class's identifier slot when `class_deriv` does not derive
    /// it, else `None`. Always `None` without a target schema. Mirrors
    /// `ObjectTransformer._underived_identifier`.
    fn underived_identifier(&self, class_deriv: &ClassDerivation) -> Option<&str> {
        let id_slot = self.content_ids()?.identifier_slot(&class_deriv.name)?;
        let derived = class_deriv
            .slot_derivations
            .as_ref()
            .is_some_and(|sds| sds.contains_key(id_slot));
        (!derived).then_some(id_slot)
    }

    /// Describe every class derivation that would emit records without their
    /// identifier.
    ///
    /// Checks the whole specification, nested class derivations included: each
    /// target identifier slot must be derived explicitly or minted
    /// (`mint_ids: true`). One message per offending class derivation, top-level
    /// derivations (merged with their ancestors) first. Mirrors
    /// `ObjectTransformer.missing_identifiers`.
    ///
    /// # Errors
    /// An `is_a` / `mixins` parent that is not a top-level class derivation.
    pub fn missing_identifiers(&self) -> Result<Vec<String>> {
        let all: &[ClassDerivation] = self.spec.class_derivations.as_deref().unwrap_or(&[]);
        let top_level = all
            .iter()
            .map(|cd| self.resolve_class_derivation_ancestors(cd.clone(), all))
            .collect::<Result<Vec<_>>>()?;
        let mut flat = Vec::new();
        for cd in &top_level {
            all_class_derivations(cd, &mut flat);
        }
        Ok(flat
            .into_iter()
            .filter_map(|cd| {
                let id_slot = self.underived_identifier(cd)?;
                (!self.mints_ids(cd)).then(|| missing_identifier_message(&cd.name, id_slot))
            })
            .collect())
    }

    /// Fail before any output if a class derivation would emit records without
    /// their identifier. Mirrors `ObjectTransformer.check_identifiers`
    /// (Python raises `ValueError`).
    ///
    /// # Errors
    /// [`Error::MissingIdentifier`] listing every problem from
    /// [`ObjectTransformer::missing_identifiers`], one per line.
    pub fn check_identifiers(&self) -> Result<()> {
        let problems = self.missing_identifiers()?;
        if problems.is_empty() {
            return Ok(());
        }
        Err(Error::MissingIdentifier {
            message: problems.join("\n"),
            class_derivation_name: String::new(),
            class_populated_from: None,
        })
    }
}

/// Lazily-built synthesiser storage for engines that were not handed a shared
/// one (plain library use); see [`ObjectTransformer::content_ids`].
pub(super) type OwnedContentIds = OnceLock<ContentIds>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{ClassDef, InMemorySchemaBuilder, SlotDef};
    use serde_json::json;

    fn v(j: serde_json::Value) -> Value {
        serde_json::from_value(j).unwrap()
    }

    fn cj(value: &Value) -> String {
        canonical_json(value).unwrap()
    }

    #[test]
    fn content_ids_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ContentIds>();
        assert_send_sync::<ObjectTransformer<'static>>();
    }

    // content_id.mints_ids doctests
    #[test]
    fn mints_ids_precedence() {
        assert!(!mints_ids(None, None, None));
        assert!(mints_ids(Some(true), None, None));
        assert!(mints_ids(Some(false), Some(true), None));
        assert!(!mints_ids(Some(false), Some(true), Some(false)));
    }

    // content_id.canonical_json doctests
    #[test]
    fn canonical_json_doctests() {
        let record = v(json!({"b": 5.0, "a": null, "c": [], "d": ["y", "x"]}));
        assert_eq!(cj(&record), r#"{"b":5,"d":["x","y"]}"#);
        // Decimal("52.30") is covered once `Value::Decimal` exists (TODO(decimal)).
        let list = Value::List(vec![
            Value::Float(52.3),
            Value::Float(155.448_000_000_000_01),
            Value::Float(-0.0),
            Value::Float(1e-7),
            Value::Int(100),
        ]);
        // Sorted by each item's encoding, as text.
        assert_eq!(cj(&list), "[0,0.0000001,100,155.448,52.3]");
    }

    #[test]
    fn number_formats() {
        let n = |f: f64| canonical_float(f);
        assert_eq!(n(5.0), "5");
        assert_eq!(n(52.30), "52.3");
        assert_eq!(n(0.1 + 0.2), "0.3");
        assert_eq!(n(1e-7), "0.0000001");
        assert_eq!(n(1e22), "10000000000000000000000");
        assert_eq!(n(-0.0), "0");
        assert_eq!(n(0.0), "0");
        assert_eq!(n(-52.3), "-52.3");
        assert_eq!(n(123_456_789.123_456_789), "123456789.123457");
        // `.15g` rounds exact ties half-even, like Python.
        assert_eq!(n(1_000_000_000_000_005.0), "1000000000000000");
        assert_eq!(n(1_000_000_000_000_015.0), "1000000000000020");
        assert_eq!(n(f64::NAN), "NaN");
        assert_eq!(n(f64::INFINITY), "Infinity");
        assert_eq!(n(f64::NEG_INFINITY), "-Infinity");
        assert_eq!(canonical_int(5), "5");
        assert_eq!(canonical_int(-7), "-7");
        assert_eq!(canonical_int(0), "0");
        // int and float spellings of one number hash alike.
        assert_eq!(cj(&Value::Int(5)), cj(&Value::Float(5.0)),);
    }

    #[test]
    fn booleans_encode_as_json_not_numbers() {
        assert_eq!(
            cj(&v(json!({"a": true, "b": false}))),
            r#"{"a":true,"b":false}"#
        );
    }

    #[test]
    fn string_escapes_match_json_dumps_without_ensure_ascii() {
        let s = Value::Str("é \"q\" \\ \n \r \t \u{8} \u{c} \u{1f} \u{7f} 😀".into());
        assert_eq!(
            cj(&s),
            "\"é \\\"q\\\" \\\\ \\n \\r \\t \\b \\f \\u001f \u{7f} 😀\""
        );
    }

    #[test]
    fn keys_sorted_by_code_point_and_empties_dropped() {
        let record = v(json!({"b": {"y": 1, "x": {}}, "a": [[], [1, 0]], "é": 1, "Z": 2}));
        assert_eq!(cj(&record), r#"{"Z":2,"a":[[0,1]],"b":{"y":1},"é":1}"#);
    }

    #[test]
    fn lists_sort_as_text() {
        let list = v(json!([100, 52.3, 0, 155.448, 52.3]));
        assert_eq!(cj(&list), "[0,100,155.448,52.3,52.3]");
    }

    fn schema() -> crate::schema::InMemorySchema {
        let slot = |name: &str, id: bool, key: bool| SlotDef {
            name: name.into(),
            range: RangeKind::Type("string".into()),
            multivalued: false,
            inlined: false,
            inlined_as_list: false,
            required: false,
            identifier: id,
            key,
            unit: None,
            any_of_enums: vec![],
        };
        let class = |name: &str| ClassDef {
            name: name.into(),
            tree_root: false,
            is_a: None,
            mixins: vec![],
        };
        InMemorySchemaBuilder::new()
            .with_schema_id("https://example.org/t")
            .add_class(class("Thing"))
            .add_slot("Thing", slot("id", true, false))
            .add_slot("Thing", slot("label", false, false))
            .add_class(class("Keyed"))
            .add_slot("Keyed", slot("k", false, true))
            .add_class(class("Base"))
            .add_slot("Base", slot("id", true, false))
            .mark_abstract("Base")
            .build()
    }

    #[test]
    fn identifier_slot_ignores_keys_and_abstract_classes() {
        let schema = schema();
        let ids = ContentIds::build(&schema, ["Thing", "Keyed", "Base", "Missing"]);
        assert_eq!(ids.identifier_slot("Thing"), Some("id"));
        // `get_identifier_slot(use_key=False)`: a `key:` slot does not count.
        assert_eq!(ids.identifier_slot("Keyed"), None);
        assert_eq!(ids.identifier_slot("Base"), None);
        assert_eq!(ids.identifier_slot("Missing"), None);
    }

    #[test]
    fn content_id_is_a_pure_function_of_content() {
        let schema = schema();
        let ids = ContentIds::build(&schema, ["Thing"]);
        let rec = |pairs: &[(&str, &str)]| -> IndexMap<String, Value> {
            pairs
                .iter()
                .map(|(k, val)| (k.to_string(), Value::Str(val.to_string())))
                .collect()
        };
        let a = ids.content_id(&rec(&[("label", "x")]), "Thing").unwrap();
        let b = ids.content_id(&rec(&[("label", "x")]), "Thing").unwrap();
        let c = ids.content_id(&rec(&[("label", "y")]), "Thing").unwrap();
        assert_eq!(a, b);
        assert_ne!(a, c);
        // namespace = uuid5(NAMESPACE_URL, "<schema id>/<class>"), id = uuid5(namespace, canonical)
        let ns = Uuid::new_v5(&Uuid::NAMESPACE_URL, b"https://example.org/t/Thing");
        assert_eq!(a, Uuid::new_v5(&ns, br#"{"label":"x"}"#).to_string());
    }

    fn spec(extra: serde_json::Value) -> TransformationSpecification {
        let mut j = json!({
            "class_derivations": {
                "Thing": {"populated_from": "Thing", "slot_derivations": {"label": {"populated_from": "label"}}}
            }
        });
        for (k, val) in extra.as_object().unwrap() {
            j[k] = val.clone();
        }
        crate::datamodel::normalise_spec_json(&mut j);
        serde_json::from_value(j).unwrap()
    }

    #[test]
    fn engine_mints_only_with_a_target_schema_and_opt_in() {
        let schema = schema();
        let row = v(json!({"label": "x"}));

        // No target schema: nothing to mint, nothing to check.
        let tr = ObjectTransformer::new(spec(json!({"mint_ids": true})), Some(&schema), None);
        assert_eq!(
            serde_json::to_value(tr.map_object(&row, Some("Thing")).unwrap()).unwrap(),
            json!({"label": "x"})
        );
        assert!(tr.missing_identifiers().unwrap().is_empty());

        // Target, no opt-in: error with upstream's message.
        let tr = ObjectTransformer::new(spec(json!({})), Some(&schema), Some(&schema));
        let err = tr.map_object(&row, Some("Thing")).unwrap_err();
        assert!(matches!(err, Error::MissingIdentifier { .. }));
        assert_eq!(err.to_string(), missing_identifier_message("Thing", "id"));
        assert_eq!(
            tr.missing_identifiers().unwrap(),
            vec![missing_identifier_message("Thing", "id")]
        );

        // `emit_without_identifiers`: records pass through without an id.
        let tr = tr.with_emit_without_identifiers(true);
        assert_eq!(
            serde_json::to_value(tr.map_object(&row, Some("Thing")).unwrap()).unwrap(),
            json!({"label": "x"})
        );

        // Opt-in: the id leads the record.
        let tr = ObjectTransformer::new(
            spec(json!({"mint_ids": true})),
            Some(&schema),
            Some(&schema),
        );
        let out = tr.map_object(&row, Some("Thing")).unwrap();
        let Value::Map(m) = out else { panic!() };
        assert_eq!(m.keys().collect::<Vec<_>>(), ["id", "label"]);
    }

    #[test]
    fn shared_content_ids_match_lazily_built_ones() {
        let schema = schema();
        let row = v(json!({"label": "x"}));
        let s = spec(json!({"mint_ids": true}));
        let shared = ContentIds::for_spec(&schema, &s);
        let a = ObjectTransformer::new_borrowed(&s, Some(&schema), Some(&schema))
            .with_content_ids(&shared)
            .map_object(&row, Some("Thing"))
            .unwrap();
        let b = ObjectTransformer::new_borrowed(&s, Some(&schema), Some(&schema))
            .map_object(&row, Some("Thing"))
            .unwrap();
        assert_eq!(
            serde_json::to_value(&a).unwrap(),
            serde_json::to_value(&b).unwrap()
        );
    }

    #[test]
    fn minting_is_deterministic_across_threads() {
        let schema = schema();
        let s = spec(json!({"mint_ids": true}));
        let shared = ContentIds::for_spec(&schema, &s);
        let row = v(json!({"label": "x"}));
        let expected = serde_json::to_value(
            ObjectTransformer::new_borrowed(&s, Some(&schema), Some(&schema))
                .with_content_ids(&shared)
                .map_object(&row, Some("Thing"))
                .unwrap(),
        )
        .unwrap();
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    for _ in 0..100 {
                        let out = ObjectTransformer::new_borrowed(&s, Some(&schema), Some(&schema))
                            .with_content_ids(&shared)
                            .map_object(&row, Some("Thing"))
                            .unwrap();
                        assert_eq!(serde_json::to_value(&out).unwrap(), expected);
                    }
                });
            }
        });
    }
}
