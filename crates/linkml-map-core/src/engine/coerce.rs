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

use std::collections::HashMap;

use crate::{
    datamodel::{ClassDerivation, TransformationSpecification},
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

/// Read-only, shareable table of target-schema lookups, keyed by
/// `(target class, slot)`.
///
/// Built once (see [`TargetCoercions::build`]) and borrowed by every engine, so
/// a pipeline that builds an engine per row/chunk does no repeated schema work.
/// A key missing from the table (a spec the table was not built from) is
/// resolved on demand, uncached.
#[derive(Debug, Default)]
pub struct TargetCoercions {
    datatypes: HashMap<(String, String), Option<&'static str>>,
}

impl TargetCoercions {
    /// Resolve every slot derivation of `spec` (nested class derivations
    /// included) against the target schema.
    pub fn build(spec: &TransformationSpecification, target: &dyn SchemaProvider) -> Self {
        let mut out = Self::default();
        for cd in spec.class_derivations.iter().flatten() {
            out.add_class_derivation(cd, target);
        }
        out
    }

    fn add_class_derivation(&mut self, cd: &ClassDerivation, target: &dyn SchemaProvider) {
        for (_, sd) in cd.slot_derivations.iter().flatten() {
            self.datatypes
                .entry((cd.name.clone(), sd.name.clone()))
                .or_insert_with(|| resolve_target_slot_datatype(target, &cd.name, &sd.name));
            // The nested derivation's own name is the target class for its slots.
            for nested in sd.class_derivations.iter().flatten().map(|(_, v)| v) {
                self.add_class_derivation(nested, target);
            }
        }
    }

    /// Mirror of `Transformer._target_slot_datatype`.
    pub fn datatype(
        &self,
        target: &dyn SchemaProvider,
        class_name: &str,
        slot_name: &str,
    ) -> Option<&'static str> {
        match self
            .datatypes
            .get(&(class_name.to_owned(), slot_name.to_owned()))
        {
            Some(found) => *found,
            None => resolve_target_slot_datatype(target, class_name, slot_name),
        }
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
        // TODO(72c0c28): "decimal" becomes an exact `Decimal` coercion.
        "boolean" => match to_bool(&v) {
            Ok(b) => Ok(Value::Bool(b)),
            Err(cause) => Err(fail(&v, cause)),
        },
        // `isinstance(True, int)` holds, so a bool stays a bool.
        "integer" => match &v {
            Value::Int(_) | Value::Bool(_) => Ok(v),
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
        ExprError::Eval(m) | ExprError::Lex(m) | ExprError::Parse(m) => m,
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
