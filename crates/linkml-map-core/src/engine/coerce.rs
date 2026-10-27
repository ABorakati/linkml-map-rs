//! Target-schema-driven value coercion.
//!
//! Ports the coercion helpers of upstream `linkml_map.transformer.transformer`
//! (`_coerce_datatype`, `_to_bool`, `Transformer._target_slot_datatype`; the
//! shared `coercible_datatype` lives in [`crate::schema`]): a slot value is coerced to the range
//! the spec declares, or — when the spec declares none — to the built-in
//! datatype the *target schema* declares for the slot.
//!
//! The engine is a synchronous `Send + Sync` pure function that the pipeline
//! rebuilds per row, so upstream's per-instance dict cache is replaced by
//! [`TargetCoercions`]: a read-only table built once from the spec and target
//! schema (the pipeline shares one per plan) and consulted per slot.

use std::borrow::Cow;
use std::collections::HashSet;
use std::sync::Mutex;

use super::FastMap;
use crate::{
    datamodel::{ClassDerivation, SlotDerivation, TransformationSpecification},
    decimal::{Decimal, DecimalError},
    enum_index::{EnumIndex, enum_index},
    error::{Error, Result},
    expr::{
        ExprError,
        eval::{py_float, py_int, py_repr, py_str},
    },
    pyfmt::py_repr_str,
    schema::{SchemaProvider, coercible_datatype},
    value::Value,
};

/// Mirror of `Transformer._resolve_target_slot_datatype`: the built-in datatype
/// the target schema declares for `slot_name` on `class_name`, or `None`.
///
/// A slot with no declared range takes the schema's `default_range` (what
/// `induced_slot` substitutes); when the schema has none, no coercion applies.
/// `RangeKind::None` is therefore NOT treated as `string`.
fn resolve_target_slot_datatype(
    sv: &dyn SchemaProvider,
    class_name: &str,
    slot_name: &str,
) -> Option<&'static str> {
    // PARITY: Python's `induced_slot` raises for a slot absent from the target
    // class; the Rust engine tolerates it (matching `is_coerce_to_multivalued`).
    let slot = sv.induced_slot(slot_name, class_name).ok()?;
    let range = match slot.range.name() {
        Some(name) => name.to_owned(),
        None => sv.default_range()?,
    };
    coercible_datatype(sv, Some(&range))
}

/// Mirror of `Transformer._target_enum_index`'s resolution: the index of the
/// target enum a slot's values must resolve to, or `None` when the range is not
/// an enum or the enum has no closed list of permissible values.
///
/// The spec's `range` wins over the target schema's, as for datatypes.
fn resolve_target_enum_index(
    sv: &dyn SchemaProvider,
    class_name: &str,
    sd: &SlotDerivation,
) -> Option<EnumIndex> {
    let range = match sd.range.as_deref().filter(|r| !r.is_empty()) {
        Some(range) => range.to_owned(),
        None => {
            // PARITY: Python's `induced_slot` raises for a slot absent from the
            // target class; the Rust engine skips it (as for datatypes).
            let slot = sv.induced_slot(&sd.name, class_name).ok()?;
            match slot.range.name() {
                Some(name) => name.to_owned(),
                None => sv.default_range()?,
            }
        }
    };
    if !sv.has_enum(&range) {
        return None;
    }
    enum_index(sv, &range)
}

/// Read-only, shareable table of target-schema lookups, keyed by
/// `(target class, slot)`, plus the warn-once state of enum resolution.
///
/// Built once (see [`TargetCoercions::build`]) and borrowed by every engine, so
/// a pipeline that builds an engine per row/chunk does no repeated schema work.
/// A key missing from the table (a spec the table was not built from) is
/// resolved on demand, uncached.
#[derive(Debug, Default)]
pub struct TargetCoercions {
    /// Target class name -> slot name -> what the target schema says of it.
    /// Nested maps so a lookup takes the two `&str`s without allocating.
    slots: FastMap<String, FastMap<String, TargetSlot>>,
    /// Upstream `_warned_unresolved_enum_values`: `(enum, value)` pairs already
    /// warned about in non-strict mode, so each logs once per run. Shared
    /// across worker threads through the plan.
    warned_enum_values: Mutex<HashSet<(String, String)>>,
}

/// What the target schema says of one slot of one target class.
#[derive(Debug)]
pub(crate) struct TargetSlot {
    /// `Transformer._target_slot_datatype`.
    pub(crate) datatype: Option<&'static str>,
    /// Whether the induced slot is multivalued; `None` when the target class
    /// has no such slot.
    pub(crate) multivalued: Option<bool>,
    /// Upstream `_target_enum_cache`, keyed by the spec range (the class and
    /// slot are the keys above).
    pub(crate) enums: Vec<(Option<String>, Option<EnumIndex>)>,
}

/// `Some(multivalued)` of the target class's induced slot, `None` when absent.
fn resolve_target_slot_multivalued(
    sv: &dyn SchemaProvider,
    class_name: &str,
    slot_name: &str,
) -> Option<bool> {
    sv.induced_slot(slot_name, class_name)
        .ok()
        .map(|s| s.multivalued)
}

impl TargetCoercions {
    /// Resolve every slot derivation of `spec` (nested class derivations
    /// included) against the target schema.
    ///
    /// The derivations resolved are those the engine runs: each top-level one
    /// merged with its `is_a` / `mixins` ancestors, so inherited slots are in
    /// the table too.
    pub fn build(spec: &TransformationSpecification, target: &dyn SchemaProvider) -> Self {
        let mut out = Self::default();
        for cd in super::effective_class_derivations(spec) {
            out.add_class_derivation(&cd, target);
        }
        out
    }

    fn add_class_derivation(&mut self, cd: &ClassDerivation, target: &dyn SchemaProvider) {
        for (_, sd) in cd.slot_derivations.iter().flatten() {
            let slot = self
                .slots
                .entry(cd.name.clone())
                .or_default()
                .entry(sd.name.clone())
                .or_insert_with(|| TargetSlot {
                    datatype: resolve_target_slot_datatype(target, &cd.name, &sd.name),
                    multivalued: resolve_target_slot_multivalued(target, &cd.name, &sd.name),
                    enums: Vec::new(),
                });
            if !slot.enums.iter().any(|(range, _)| *range == sd.range) {
                slot.enums.push((
                    sd.range.clone(),
                    resolve_target_enum_index(target, &cd.name, sd),
                ));
            }
            // The nested derivation's own name is the target class for its slots.
            for nested in sd.class_derivations.iter().flatten().map(|(_, v)| v) {
                self.add_class_derivation(nested, target);
            }
        }
    }

    pub(crate) fn slot(&self, class_name: &str, slot_name: &str) -> Option<&TargetSlot> {
        self.slots.get(class_name)?.get(slot_name)
    }

    /// Mirror of `Transformer._target_slot_datatype`.
    pub fn datatype(
        &self,
        target: &dyn SchemaProvider,
        class_name: &str,
        slot_name: &str,
    ) -> Option<&'static str> {
        match self.slot(class_name, slot_name) {
            Some(found) => found.datatype,
            None => resolve_target_slot_datatype(target, class_name, slot_name),
        }
    }

    /// Whether the target schema's induced slot `slot_name` of `class_name` is
    /// multivalued; `None` when the target class has no such slot.
    pub fn multivalued(
        &self,
        target: &dyn SchemaProvider,
        class_name: &str,
        slot_name: &str,
    ) -> Option<bool> {
        match self.slot(class_name, slot_name) {
            Some(found) => found.multivalued,
            None => resolve_target_slot_multivalued(target, class_name, slot_name),
        }
    }
}

impl TargetCoercions {
    /// Mirror of `Transformer._target_enum_index`.
    pub fn enum_index(
        &self,
        target: &dyn SchemaProvider,
        class_name: &str,
        sd: &SlotDerivation,
    ) -> Option<Cow<'_, EnumIndex>> {
        let cached = self
            .slot(class_name, &sd.name)
            .and_then(|slot| slot.enums.iter().find(|(range, _)| *range == sd.range));
        match cached {
            Some((_, found)) => found.as_ref().map(Cow::Borrowed),
            None => resolve_target_enum_index(target, class_name, sd).map(Cow::Owned),
        }
    }

    /// Mirror of `Transformer._resolve_enum_value`: replace `v` with the key of
    /// the permissible value it spells.
    ///
    /// A list is resolved element-wise and a dict value-wise (keys untouched).
    /// A value matching no permissible value, or more than one, errors in
    /// `strict` mode; otherwise it passes through with a warning, logged once
    /// per `(enum, value)` for the whole run.
    ///
    /// # Errors
    /// [`Error::EnumValue`] carrying the upstream problem text (strict only).
    pub fn resolve_enum_value(
        &self,
        v: Value,
        index: &EnumIndex,
        target: &dyn SchemaProvider,
        strict: bool,
    ) -> Result<Value> {
        match v {
            Value::List(items) => items
                .into_iter()
                .map(|i| self.resolve_enum_value(i, index, target, strict))
                .collect::<Result<Vec<_>>>()
                .map(Value::List),
            Value::Map(m) => m
                .into_iter()
                .map(|(k, i)| {
                    self.resolve_enum_value(i, index, target, strict)
                        .map(|r| (k, r))
                })
                .collect::<Result<_>>()
                .map(Value::Map),
            scalar => {
                // Python `str(v)`: `True` is 'True', `1.5` is '1.5', `None` is 'None'.
                let text = py_str(&scalar);
                match index.resolve(&text, target) {
                    Ok(key) => Ok(Value::Str(key)),
                    Err(problem) if strict => Err(Error::EnumValue(problem)),
                    Err(problem) => {
                        let first_time = self
                            .warned_enum_values
                            .lock()
                            .map(|mut seen| seen.insert((index.enum_name.clone(), text)))
                            .unwrap_or(false);
                        if first_time {
                            log::warn!("{problem}");
                        }
                        Ok(scalar)
                    }
                }
            }
        }
    }

    /// How many distinct `(enum, value)` problems have been warned about
    /// (non-strict mode). For tests and diagnostics.
    pub fn warned_enum_value_count(&self) -> usize {
        self.warned_enum_values.lock().map_or(0, |seen| seen.len())
    }
}

/// Mirror of upstream `_to_decimal`: an exact, finite decimal.
///
/// A [`Value::Decimal`] is kept as is; anything else goes through Python
/// `Decimal(str(v))`, so a float keeps its printed digits rather than its
/// binary expansion. The messages are upstream's: `Cannot convert 'abc' to
/// decimal`, and `... to decimal: not a finite number` for NaN / infinities.
///
/// PARITY: transformer.py::_to_decimal. `True` fails (`Decimal("True")`).
pub(crate) fn to_decimal(v: &Value) -> std::result::Result<Decimal, String> {
    if let Value::Decimal(d) = v {
        return Ok(d.clone());
    }
    match Decimal::parse_python(&py_str(v)) {
        Ok(d) => Ok(d),
        Err(DecimalError::Invalid) => Err(format!("Cannot convert {} to decimal", py_repr(v))),
        Err(DecimalError::NotFinite) => Err(format!(
            "Cannot convert {} to decimal: not a finite number",
            py_repr(v)
        )),
    }
}

/// Mirror of upstream `_to_bool`: strings are parsed (`true|false|1|0`,
/// case-insensitive, whitespace-stripped), anything else uses Python truthiness.
///
/// A string outside that set raises (`Cannot convert 'maybe' to boolean`).
pub(crate) fn to_bool(v: &Value) -> std::result::Result<bool, String> {
    let Value::Str(s) = v else {
        return Ok(v.is_truthy());
    };
    match s.trim().to_lowercase().as_str() {
        "true" | "1" => Ok(true),
        "false" | "0" => Ok(false),
        _ => Err(format!("Cannot convert {} to boolean", py_repr_str(s))),
    }
}

/// Mirror of Python `Transformer._coerce_datatype`.
///
/// `None`-equivalent targets are handled by the caller. A list is coerced
/// element-wise and a dict value-wise with the same target. `integer`,
/// `float`, `string` follow `isinstance(v, cls) -> v, else cls(v)`; `boolean`
/// goes through [`to_bool`]. Any other name (a class, an enum, a type with no
/// conversion such as `date`) passes the value through.
///
/// # Errors
/// A value Python's constructor would reject (`int("abc")`, `float("x")`,
/// `_to_bool("maybe")`) raises, which fails the row like upstream's
/// `TransformationError`.
pub(crate) fn coerce_datatype(v: Value, target_range: &str) -> Result<Value> {
    match v {
        Value::List(items) => items
            .into_iter()
            .map(|i| coerce_datatype(i, target_range))
            .collect::<Result<Vec<_>>>()
            .map(Value::List),
        Value::Map(m) => m
            .into_iter()
            .map(|(k, i)| coerce_datatype(i, target_range).map(|c| (k, c)))
            .collect::<Result<_>>()
            .map(Value::Map),
        scalar => coerce_scalar(scalar, target_range),
    }
}

fn coerce_scalar(v: Value, target_range: &str) -> Result<Value> {
    let fail = |v: &Value, cause: String| Error::Coercion {
        value: py_repr(v),
        target: target_range.to_owned(),
        cause,
    };
    match target_range {
        "decimal" => to_decimal(&v)
            .map(Value::Decimal)
            .map_err(|cause| fail(&v, cause)),
        "boolean" => match to_bool(&v) {
            Ok(b) => Ok(Value::Bool(b)),
            Err(cause) => Err(fail(&v, cause)),
        },
        // `isinstance(True, int)` holds, so a bool stays a bool.
        "integer" => match &v {
            Value::Int(_) | Value::Bool(_) => Ok(v),
            // `int(Decimal)` truncates toward zero.
            Value::Decimal(d) => {
                let fixed = d.to_fixed_string();
                let int_part = fixed.split_once('.').map_or(fixed.as_str(), |(i, _)| i);
                int_part
                    .parse::<i64>()
                    .map(Value::Int)
                    .map_err(|e| fail(&v, e.to_string()))
            }
            Value::Float(f) if f.is_nan() => {
                Err(fail(&v, "cannot convert float NaN to integer".to_owned()))
            }
            Value::Float(f) if f.is_infinite() => Err(fail(
                &v,
                "cannot convert float infinity to integer".to_owned(),
            )),
            _ => py_int(&v).map_err(|e| fail(&v, expr_cause(e))),
        },
        "float" => match &v {
            Value::Float(_) => Ok(v),
            Value::Decimal(d) => Ok(Value::Float(d.to_f64())),
            _ => py_float(&v).map_err(|e| fail(&v, expr_cause(e))),
        },
        "string" => match &v {
            Value::Str(_) => Ok(v),
            other => Ok(Value::Str(py_str(other))),
        },
        // PARITY: transformer.py::_coerce_datatype passes an unknown range
        // through silently since f299f3a (it warned at 0469da7).
        _ => Ok(v),
    }
}

/// The message of an evaluator error, without the `eval error:` prefix.
fn expr_cause(e: ExprError) -> String {
    match e {
        ExprError::Eval(m) | ExprError::Uncaught(m) | ExprError::Lex(m) | ExprError::Parse(m) => m,
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        datamodel::TransformationSpecification,
        engine::ObjectTransformer,
        schema::{ClassDef, InMemorySchemaBuilder, RangeKind, SlotDef},
    };
    use indexmap::IndexMap;

    fn s(v: &str) -> Value {
        Value::Str(v.into())
    }

    fn msg(r: Result<Value>) -> String {
        match r {
            Err(Error::Coercion { cause, .. }) => cause,
            other => panic!("expected a coercion error, got {other:?}"),
        }
    }

    // ── coerce_datatype ───────────────────────────────────────────────────────

    #[test]
    fn integer_coercion() {
        assert_eq!(coerce_datatype(s("42"), "integer").unwrap(), Value::Int(42));
        assert_eq!(coerce_datatype(s(" 7 "), "integer").unwrap(), Value::Int(7));
        assert_eq!(
            coerce_datatype(Value::Float(3.7), "integer").unwrap(),
            Value::Int(3)
        );
        assert_eq!(
            coerce_datatype(Value::Int(5), "integer").unwrap(),
            Value::Int(5)
        );
        // `isinstance(True, int)`: a bool stays a bool.
        assert_eq!(
            coerce_datatype(Value::Bool(true), "integer").unwrap(),
            Value::Bool(true)
        );
    }

    #[test]
    fn integer_coercion_raises_like_python() {
        assert_eq!(
            msg(coerce_datatype(s("7.5"), "integer")),
            "invalid literal for int() with base 10: '7.5'"
        );
        assert_eq!(
            msg(coerce_datatype(s("abc"), "integer")),
            "invalid literal for int() with base 10: 'abc'"
        );
        assert_eq!(
            msg(coerce_datatype(Value::Float(f64::NAN), "integer")),
            "cannot convert float NaN to integer"
        );
    }

    #[test]
    fn float_coercion() {
        assert_eq!(
            coerce_datatype(s("2.75"), "float").unwrap(),
            Value::Float(2.75)
        );
        assert_eq!(
            coerce_datatype(Value::Int(2), "float").unwrap(),
            Value::Float(2.0)
        );
        assert_eq!(
            coerce_datatype(Value::Bool(true), "float").unwrap(),
            Value::Float(1.0)
        );
        assert_eq!(
            msg(coerce_datatype(s("x"), "float")),
            "could not convert string to float: 'x'"
        );
    }

    #[test]
    fn string_coercion_is_python_str() {
        assert_eq!(coerce_datatype(Value::Int(99), "string").unwrap(), s("99"));
        assert_eq!(
            coerce_datatype(Value::Float(5.0), "string").unwrap(),
            s("5.0")
        );
        assert_eq!(
            coerce_datatype(Value::Bool(true), "string").unwrap(),
            s("True")
        );
        assert_eq!(coerce_datatype(Value::Null, "string").unwrap(), s("None"));
    }

    #[test]
    fn boolean_coercion_parses_strings() {
        for (src, want) in [
            ("true", true),
            ("FALSE", false),
            ("1", true),
            ("0", false),
            (" True ", true),
        ] {
            assert_eq!(
                coerce_datatype(s(src), "boolean").unwrap(),
                Value::Bool(want),
                "{src:?}"
            );
        }
        assert_eq!(
            coerce_datatype(Value::Int(0), "boolean").unwrap(),
            Value::Bool(false)
        );
        assert_eq!(
            coerce_datatype(Value::Float(0.0), "boolean").unwrap(),
            Value::Bool(false)
        );
        assert_eq!(
            coerce_datatype(Value::Null, "boolean").unwrap(),
            Value::Bool(false)
        );
        assert_eq!(
            msg(coerce_datatype(s("maybe"), "boolean")),
            "Cannot convert 'maybe' to boolean"
        );
        // PARITY: `yes` is not in `_BOOLEAN_STRINGS`; it raises upstream.
        assert!(coerce_datatype(s("yes"), "boolean").is_err());
    }

    #[test]
    fn unknown_range_names_pass_through() {
        // PARITY: the extra aliases the pre-0469da7 port accepted (`int`, `str`,
        // `bool`, `double`) are not upstream ranges, so they pass through.
        for r in ["date", "Site", "Color", "int", "str", "bool", "double"] {
            assert_eq!(coerce_datatype(s("x"), r).unwrap(), s("x"), "{r}");
        }
    }

    #[test]
    fn lists_coerce_elementwise_and_maps_value_wise() {
        let list = Value::List(vec![s("1"), s("2")]);
        assert_eq!(
            coerce_datatype(list, "integer").unwrap(),
            Value::List(vec![Value::Int(1), Value::Int(2)])
        );
        let mut m = IndexMap::new();
        m.insert("a".to_owned(), Value::Int(1));
        m.insert("b".to_owned(), Value::List(vec![Value::Float(2.5)]));
        let mut want = IndexMap::new();
        want.insert("a".to_owned(), s("1"));
        want.insert("b".to_owned(), Value::List(vec![s("2.5")]));
        assert_eq!(
            coerce_datatype(Value::Map(m), "string").unwrap(),
            Value::Map(want)
        );
        // A bad element fails the whole value.
        assert!(coerce_datatype(Value::List(vec![s("1"), s("x")]), "integer").is_err());
    }

    // ── decimal (upstream 72c0c28) ────────────────────────────────────────────

    fn sci(v: &Value) -> String {
        match v {
            Value::Decimal(d) => d.to_sci_string(),
            other => panic!("expected a decimal, got {other:?}"),
        }
    }

    // Upstream: test_decimal_range.py::test_decimal_range_coerces_exactly
    #[test]
    fn decimal_range_coerces_exactly() {
        for (src, want) in [
            (s("52.3"), "52.3"),
            (s("52.30"), "52.30"),
            (s("52"), "52"),
            (s("-0.05"), "-0.05"),
            (s("0.123456789012345678901"), "0.123456789012345678901"),
            (Value::Float(0.1), "0.1"),
            (Value::Int(7), "7"),
        ] {
            let out = coerce_datatype(src.clone(), "decimal").unwrap();
            assert_eq!(sci(&out), want, "{src:?}");
        }
    }

    // Upstream: test_decimal_range_coerces_each_item
    #[test]
    fn decimal_range_coerces_each_item() {
        let out = coerce_datatype(Value::List(vec![s("1"), s("2.50")]), "decimal").unwrap();
        let Value::List(items) = out else {
            panic!("expected a list")
        };
        assert_eq!(items.iter().map(sci).collect::<Vec<_>>(), ["1", "2.50"]);
    }

    // Upstream: test_decimal_range_rejects_non_numbers
    #[test]
    fn decimal_range_rejects_non_numbers() {
        assert_eq!(
            msg(coerce_datatype(s("abc"), "decimal")),
            "Cannot convert 'abc' to decimal"
        );
        assert_eq!(
            msg(coerce_datatype(Value::Bool(true), "decimal")),
            "Cannot convert True to decimal"
        );
        assert_eq!(
            msg(coerce_datatype(s("NaN"), "decimal")),
            "Cannot convert 'NaN' to decimal: not a finite number"
        );
        assert_eq!(
            msg(coerce_datatype(s("Infinity"), "decimal")),
            "Cannot convert 'Infinity' to decimal: not a finite number"
        );
        assert_eq!(
            msg(coerce_datatype(Value::Float(f64::INFINITY), "decimal")),
            "Cannot convert inf to decimal: not a finite number"
        );
        for bad in ["", "  ", "0x10", "True"] {
            assert!(coerce_datatype(s(bad), "decimal").is_err(), "{bad:?}");
        }
    }

    #[test]
    fn decimal_edge_cases() {
        for (src, sci_want) in [
            (" 5 ", "5"),
            ("1_000", "1000"),
            ("5.", "5"),
            (".5", "0.5"),
            ("1E-7", "1E-7"),
            ("00012.50", "12.50"),
        ] {
            assert_eq!(
                sci(&coerce_datatype(s(src), "decimal").unwrap()),
                sci_want,
                "{src:?}"
            );
        }
        // Floats keep Python repr digits.
        assert_eq!(
            sci(&coerce_datatype(Value::Float(1.0), "decimal").unwrap()),
            "1.0"
        );
        assert_eq!(
            sci(&coerce_datatype(Value::Float(1e16), "decimal").unwrap()),
            "1E+16"
        );
        // An existing Decimal passes through untouched.
        let d = coerce_datatype(s("52.30"), "decimal").unwrap();
        assert_eq!(sci(&coerce_datatype(d, "decimal").unwrap()), "52.30");
    }

    #[test]
    fn decimal_converts_to_other_builtins() {
        let d = coerce_datatype(s("52.70"), "decimal").unwrap();
        assert_eq!(
            coerce_datatype(d.clone(), "integer").unwrap(),
            Value::Int(52)
        );
        assert_eq!(
            coerce_datatype(d.clone(), "float").unwrap(),
            Value::Float(52.7)
        );
        assert_eq!(coerce_datatype(d.clone(), "string").unwrap(), s("52.70"));
        assert_eq!(coerce_datatype(d, "boolean").unwrap(), Value::Bool(true));
        let zero = coerce_datatype(s("0.00"), "decimal").unwrap();
        assert_eq!(
            coerce_datatype(zero, "boolean").unwrap(),
            Value::Bool(false)
        );
    }

    #[test]
    fn decimal_range_through_the_engine_keeps_exact_digits() {
        let src = source_schema();
        let engine = ObjectTransformer::new(
            spec(&[("ratio", Some("decimal")), ("counts", Some("decimal"))]),
            Some(&src),
            None,
        );
        let out = engine
            .map_object(
                &row(&[
                    ("ratio", s("52.30")),
                    ("counts", Value::List(vec![s("1"), s("2.50")])),
                ]),
                Some("Row"),
            )
            .unwrap();
        assert_eq!(sci(&get(&out, "ratio")), "52.30");
        let Value::List(items) = get(&out, "counts") else {
            panic!("expected a list")
        };
        assert_eq!(items.iter().map(sci).collect::<Vec<_>>(), ["1", "2.50"]);
        // A non-number fails the row, with the slot context attached.
        let err = engine
            .map_object(&row(&[("ratio", s("abc"))]), Some("Row"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("to decimal"), "{err}");
    }

    // Upstream: ObjectTransformer._multivalued_to_single stringification
    // (dumps_json / dump_yaml keep decimals exact).
    #[test]
    fn multivalued_stringification_keeps_decimals_exact() {
        let list = Value::List(vec![
            coerce_datatype(s("1E-7"), "decimal").unwrap(),
            coerce_datatype(s("2.50"), "decimal").unwrap(),
        ]);
        // JSON: `str(Decimal)`; YAML flow: `format(d, 'f')`.
        assert_eq!(super::super::py_json(&list), "[1E-7, 2.50]");
        assert_eq!(super::super::yaml_flow(&list), "[0.0000001, 2.50]");
    }

    // ── target schema lookups ─────────────────────────────────────────────────

    fn slot(name: &str, range: RangeKind, multivalued: bool) -> SlotDef {
        SlotDef {
            name: name.into(),
            range,
            multivalued,
            inlined: false,
            inlined_as_list: false,
            required: false,
            identifier: false,
            key: false,
            unit: None,
            any_of_enums: vec![],
        }
    }

    fn ty(name: &str) -> RangeKind {
        RangeKind::Type(name.into())
    }

    fn target_schema(default_range: Option<&str>) -> crate::schema::InMemorySchema {
        let mut b = InMemorySchemaBuilder::new()
            .add_type("string")
            .add_type("integer")
            .add_type("float")
            .add_type("boolean")
            .add_type("decimal")
            .add_type("double")
            .add_type("date")
            .add_type_with_parent("Score", "decimal")
            .add_type_with_parent("Grade", "Score")
            .add_class(ClassDef {
                name: "Out".into(),
                tree_root: true,
                is_a: None,
                mixins: vec![],
            })
            .add_class(ClassDef {
                name: "Site".into(),
                tree_root: false,
                is_a: None,
                mixins: vec![],
            })
            .add_enum(crate::schema::EnumDef {
                name: "Color".into(),
                permissible_values: vec![],
                ..Default::default()
            });
        if let Some(r) = default_range {
            b = b.default_range(r);
        }
        for (name, range, mv) in [
            ("count", ty("integer"), false),
            ("ratio", ty("float"), false),
            ("flag", ty("boolean"), false),
            ("score", ty("Score"), false),
            ("grade", ty("Grade"), false),
            ("wide", ty("double"), false),
            ("when", ty("date"), false),
            ("color", RangeKind::Enum("Color".into()), false),
            ("site", RangeKind::Class("Site".into()), false),
            ("note", RangeKind::None, false),
            ("counts", ty("integer"), true),
        ] {
            b = b.add_slot("Out", slot(name, range, mv));
        }
        b.build()
    }

    #[test]
    fn coercible_datatype_resolves_typeof_chains() {
        let sv = target_schema(Some("string"));
        for (range, want) in [
            ("integer", Some("integer")),
            ("float", Some("float")),
            ("decimal", Some("decimal")),
            ("string", Some("string")),
            ("boolean", Some("boolean")),
            ("Score", Some("decimal")),
            // Two hops, with the final parent never defined as a loaded type.
            ("Grade", Some("decimal")),
            // PARITY: `double` has no `typeof: float`, so it is not coerced.
            ("double", None),
            ("date", None),
            ("Color", None),
            ("Site", None),
        ] {
            assert_eq!(coercible_datatype(&sv, Some(range)), want, "{range}");
        }
    }

    #[test]
    fn target_slot_datatype_uses_default_range_only_when_declared() {
        let with_default = target_schema(Some("string"));
        let without = target_schema(None);
        let d = |sv: &dyn SchemaProvider, slot: &str| resolve_target_slot_datatype(sv, "Out", slot);
        assert_eq!(d(&with_default, "note"), Some("string"));
        // RangeKind::None must NOT be treated as `string` (measurements guard).
        assert_eq!(d(&without, "note"), None);
        assert_eq!(d(&with_default, "count"), Some("integer"));
        assert_eq!(d(&with_default, "counts"), Some("integer"));
        assert_eq!(d(&with_default, "grade"), Some("decimal"));
        assert_eq!(d(&with_default, "wide"), None);
        assert_eq!(d(&with_default, "color"), None);
        assert_eq!(d(&with_default, "site"), None);
        // An unknown slot is tolerated rather than raising.
        assert_eq!(d(&with_default, "ghost"), None);
    }

    // ── engine integration ────────────────────────────────────────────────────

    fn source_schema() -> crate::schema::InMemorySchema {
        let mut b = InMemorySchemaBuilder::new()
            .add_type("string")
            .add_class(ClassDef {
                name: "Row".into(),
                tree_root: true,
                is_a: None,
                mixins: vec![],
            });
        for (name, mv) in [
            ("count", false),
            ("ratio", false),
            ("flag", false),
            ("score", false),
            ("note", false),
            ("color", false),
            ("site", false),
            ("counts", true),
        ] {
            b = b.add_slot("Row", slot(name, ty("string"), mv));
        }
        b.build()
    }

    fn spec(slots: &[(&str, Option<&str>)]) -> TransformationSpecification {
        let mut sds = serde_json::Map::new();
        for (name, range) in slots {
            let mut sd = serde_json::json!({ "name": name, "populated_from": name });
            if let Some(r) = range {
                sd["range"] = (*r).into();
            }
            sds.insert((*name).into(), sd);
        }
        serde_json::from_value(serde_json::json!({
            "class_derivations": [{
                "name": "Out", "populated_from": "Row", "slot_derivations": sds,
            }]
        }))
        .unwrap()
    }

    fn row(pairs: &[(&str, Value)]) -> Value {
        Value::Map(
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), v.clone()))
                .collect(),
        )
    }

    fn get(v: &Value, key: &str) -> Value {
        match v {
            Value::Map(m) => m.get(key).cloned().unwrap_or(Value::Null),
            _ => panic!("not a map"),
        }
    }

    // Upstream: test_target_schema_range.py::test_values_take_the_target_schema_range
    #[test]
    fn values_take_the_target_schema_range() {
        let src = source_schema();
        let tgt = target_schema(Some("string"));
        let sp = spec(&[
            ("count", None),
            ("ratio", None),
            ("flag", None),
            ("note", None),
            ("counts", None),
            ("color", None),
            ("site", None),
        ]);
        let engine = ObjectTransformer::new(sp, Some(&src), Some(&tgt));
        let out = engine
            .map_object(
                &row(&[
                    ("count", s("7")),
                    ("ratio", s("1.5")),
                    ("flag", s("false")),
                    ("note", Value::Int(42)),
                    ("counts", Value::List(vec![s("1"), s("2")])),
                    ("color", s("red")),
                    ("site", s("S1")),
                ]),
                Some("Row"),
            )
            .unwrap();
        assert_eq!(get(&out, "count"), Value::Int(7));
        assert_eq!(get(&out, "ratio"), Value::Float(1.5));
        assert_eq!(get(&out, "flag"), Value::Bool(false));
        assert_eq!(get(&out, "note"), s("42"));
        assert_eq!(
            get(&out, "counts"),
            Value::List(vec![Value::Int(1), Value::Int(2)])
        );
        // Class- and enum-ranged slots pass through (test_class_and_enum_ranges_pass_through).
        assert_eq!(get(&out, "color"), s("red"));
        assert_eq!(get(&out, "site"), s("S1"));
    }

    // Upstream: test_spec_range_overrides_target_schema
    #[test]
    fn spec_range_overrides_target_schema() {
        let src = source_schema();
        let tgt = target_schema(Some("string"));
        let engine =
            ObjectTransformer::new(spec(&[("ratio", Some("string"))]), Some(&src), Some(&tgt));
        let out = engine
            .map_object(&row(&[("ratio", s("1.5"))]), Some("Row"))
            .unwrap();
        assert_eq!(get(&out, "ratio"), s("1.5"));
    }

    // Upstream: test_no_target_schema_leaves_values_alone
    #[test]
    fn no_target_schema_leaves_values_alone() {
        let src = source_schema();
        let sp = spec(&[
            ("count", None),
            ("flag", None),
            ("note", None),
            ("ratio", Some("string")),
        ]);
        let engine = ObjectTransformer::new(sp, Some(&src), None);
        let out = engine
            .map_object(
                &row(&[
                    ("count", s("7")),
                    ("flag", s("false")),
                    ("note", Value::Int(42)),
                    ("ratio", Value::Float(1.5)),
                ]),
                Some("Row"),
            )
            .unwrap();
        assert_eq!(get(&out, "count"), s("7"));
        assert_eq!(get(&out, "flag"), s("false"));
        assert_eq!(get(&out, "note"), Value::Int(42));
        assert_eq!(get(&out, "ratio"), s("1.5"));
    }

    // Rust-specific: the measurements guard (no default_range, untyped slot).
    #[test]
    fn untyped_slot_without_default_range_is_left_alone() {
        let src = source_schema();
        let tgt = target_schema(None);
        let engine = ObjectTransformer::new(spec(&[("note", None)]), Some(&src), Some(&tgt));
        let out = engine
            .map_object(&row(&[("note", Value::Float(172.0))]), Some("Row"))
            .unwrap();
        assert_eq!(get(&out, "note"), Value::Float(172.0));
    }

    // Upstream: test_boolean_strings_are_parsed
    #[test]
    fn boolean_strings_are_parsed() {
        let src = source_schema();
        let tgt = target_schema(Some("string"));
        for (input, want) in [
            (s("true"), true),
            (s("FALSE"), false),
            (s("1"), true),
            (s("0"), false),
            (Value::Bool(true), true),
            (Value::Int(0), false),
        ] {
            let engine = ObjectTransformer::new(spec(&[("flag", None)]), Some(&src), Some(&tgt));
            let out = engine
                .map_object(&row(&[("flag", input.clone())]), Some("Row"))
                .unwrap();
            assert_eq!(get(&out, "flag"), Value::Bool(want), "{input:?}");
        }
    }

    // Upstream: test_unparseable_boolean_fails
    #[test]
    fn unparseable_boolean_fails() {
        let src = source_schema();
        let tgt = target_schema(Some("string"));
        let engine = ObjectTransformer::new(spec(&[("flag", None)]), Some(&src), Some(&tgt));
        let err = engine
            .map_object(&row(&[("flag", s("maybe"))]), Some("Row"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("to boolean"), "{err}");
        assert!(err.contains("Out.flag") || err.contains("flag"), "{err}");
    }

    #[test]
    fn integer_of_non_integral_string_fails_the_row() {
        let src = source_schema();
        let tgt = target_schema(Some("string"));
        let engine = ObjectTransformer::new(spec(&[("count", None)]), Some(&src), Some(&tgt));
        assert!(
            engine
                .map_object(&row(&[("count", s("7.5"))]), Some("Row"))
                .is_err()
        );
    }

    #[test]
    fn shared_table_matches_the_lazy_fallback() {
        let src = source_schema();
        let tgt = target_schema(Some("string"));
        let sp = spec(&[("count", None), ("note", None)]);
        let table = TargetCoercions::build(&sp, &tgt);
        let engine = ObjectTransformer::new_borrowed(&sp, Some(&src), Some(&tgt))
            .with_target_coercions(&table);
        let out = engine
            .map_object(
                &row(&[("count", s("7")), ("note", Value::Int(1))]),
                Some("Row"),
            )
            .unwrap();
        assert_eq!(get(&out, "count"), Value::Int(7));
        assert_eq!(get(&out, "note"), s("1"));
    }
}
