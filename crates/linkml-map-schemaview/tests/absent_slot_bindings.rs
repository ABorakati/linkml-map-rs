//! A slot the source class declares but the row lacks reads as null inside an
//! `expr:`, in every evaluation mode, strict included.
//!
//! Port of upstream `tests/test_transformer/test_expr_strict_mode.py::
//! test_schema_slot_absent_from_row_resolves_to_none` (`Bindings._collect_schema_slots`),
//! widened to the evaluator's other name-resolution paths. Expected values come from
//! upstream `linkml-map` v0.6.0-rc1 run on the same schema, expressions and row
//! (`tests/differential/corpus/*` replays the CLI cases).

use linkml_map_core::{
    datamodel::{TransformationSpecification, normalise_spec_json},
    engine::{ObjectTransformer, SourceSlots},
    schema::SchemaProvider,
    value::Value,
};
use linkml_map_schemaview::SchemaViewProvider;

const SOURCE_SCHEMA: &str = r"
id: https://example.org/absent-source
name: absent_source
prefixes: {linkml: 'https://w3id.org/linkml/'}
default_prefix: absent_source
default_range: string
imports: [linkml:types]
classes:
  Address:
    attributes:
      street: {}
  Org:
    attributes:
      id: {identifier: true}
      size: {}
  Person:
    attributes:
      id: {identifier: true}
      name: {}
      nick: {}
      age: {range: integer}
      max: {range: integer}
      tags: {multivalued: true}
      addr: {range: Address, inlined: true}
  Student:
    is_a: Person
    attributes:
      school: {}
";

#[derive(Clone, Copy)]
enum Mode {
    Lax,
    Strict,
    StrictUnrestricted,
}

fn s(v: &str) -> Value {
    Value::Str(v.into())
}

fn row() -> Value {
    Value::Map(
        [("id".to_owned(), s("p1")), ("name".to_owned(), s("a"))]
            .into_iter()
            .collect(),
    )
}

fn spec(expr: &str, source_class: &str) -> TransformationSpecification {
    let mut obj = serde_json::json!({
        "id": "absent",
        "class_derivations": {
            "Agent": {
                "populated_from": source_class,
                "slot_derivations": {
                    "id": {"populated_from": "id"},
                    "out": {"expr": expr},
                }
            }
        }
    });
    normalise_spec_json(&mut obj);
    serde_json::from_value(obj).unwrap()
}

fn engine<'s>(
    spec: TransformationSpecification,
    source: &'s SchemaViewProvider,
    mode: Mode,
) -> ObjectTransformer<'s> {
    let (strict, unrestricted) = match mode {
        Mode::Lax => (false, false),
        Mode::Strict => (true, false),
        Mode::StrictUnrestricted => (true, true),
    };
    ObjectTransformer::new(spec, Some(source as &dyn SchemaProvider), None)
        .with_strict_exprs(strict)
        .with_unrestricted_eval(unrestricted)
}

/// `out` of the transformed `Person` row (`Value::Null` when the key is absent).
fn out(expr: &str, mode: Mode) -> Result<Value, String> {
    let source = SchemaViewProvider::from_yaml_str(SOURCE_SCHEMA).unwrap();
    let result = engine(spec(expr, "Person"), &source, mode)
        .map_object(&row(), Some("Person"))
        .map_err(|e| e.to_string())?;
    let Value::Map(m) = result else {
        panic!("expected a map")
    };
    Ok(m.get("out").cloned().unwrap_or(Value::Null))
}

const ALL_MODES: [Mode; 3] = [Mode::Lax, Mode::Strict, Mode::StrictUnrestricted];

#[test]
fn absent_declared_slot_is_null_in_every_mode() {
    for expr in [
        "{age} * 2", // null propagates through arithmetic
        "age",
        "age + 1",
        "str(age)",
        "{addr.street}", // and through attribute access
        "addr.street",
        "tags + ['x']",
        "'x' in tags",
        "max", // a slot named like a built-in function: names win over functions
    ] {
        for mode in ALL_MODES {
            assert_eq!(out(expr, mode), Ok(Value::Null), "{expr}");
        }
    }
}

#[test]
fn absent_declared_slot_takes_part_in_null_aware_functions() {
    for mode in ALL_MODES {
        assert_eq!(
            out("case(({age} == None, 'none'), (True, 'x'))", mode),
            Ok(s("none"))
        );
        assert_eq!(out("coalesce(age, 5)", mode), Ok(Value::Int(5)));
        assert_eq!(out("age is None", mode), Ok(Value::Bool(true)));
        assert_eq!(out("len(tags) if tags else 0", mode), Ok(Value::Int(0)));
    }
}

#[test]
fn absent_declared_slot_is_the_outer_binding_of_a_comprehension_variable() {
    // upstream resolves `age` in `age.foo` outside the comprehension scope, where it is a
    // declared slot that is null, so the attribute access is null for each item.
    for mode in ALL_MODES {
        assert_eq!(
            out("[age.foo for age in [1, 2]]", mode),
            Ok(Value::List(vec![Value::Null, Value::Null]))
        );
    }
}

#[test]
fn iterating_an_absent_declared_slot_fails_like_any_null() {
    // `[x for x in None]` is a `TypeError`: an error restricted, null once the
    // unrestricted evaluator is allowed to take over.
    for mode in [Mode::Lax, Mode::Strict] {
        assert!(out("[x for x in tags]", mode).is_err());
    }
    assert_eq!(
        out("[x for x in tags]", Mode::StrictUnrestricted),
        Ok(Value::Null)
    );
}

#[test]
fn undeclared_names_still_warn_or_raise() {
    // Lax: null (upstream logs a warning once); strict: an error.
    assert_eq!(out("nosuch", Mode::Lax), Ok(Value::Null));
    assert_eq!(out("{nosuch} + 1", Mode::Lax), Ok(Value::Null));
    assert!(out("nosuch", Mode::Strict).is_err());
    assert!(out("{nosuch} + 1", Mode::Strict).is_err());
    // A qualified reference to an unknown root fails even when lax.
    for mode in ALL_MODES {
        assert!(out("nosuch.x", mode).is_err(), "nosuch.x");
    }
}

#[test]
fn a_slot_of_another_class_is_not_declared() {
    // `Org.size` is no slot of `Person`: only the source class's own slots count.
    assert_eq!(out("{size}", Mode::Lax), Ok(Value::Null));
    assert!(out("{size}", Mode::Strict).is_err());
}

#[test]
fn inherited_slots_are_declared() {
    // `Student` is_a `Person`: `age` is induced from the parent.
    let source = SchemaViewProvider::from_yaml_str(SOURCE_SCHEMA).unwrap();
    let result = engine(spec("{age} + 1", "Student"), &source, Mode::Strict)
        .map_object(&row(), Some("Student"))
        .unwrap();
    let Value::Map(m) = result else {
        panic!("expected a map")
    };
    assert_eq!(m.get("out"), Some(&Value::Null));
}

#[test]
fn present_slots_are_unaffected() {
    for mode in ALL_MODES {
        assert_eq!(out("name + '!'", mode), Ok(s("a!")));
    }
}

#[test]
fn the_source_type_names_the_row() {
    // upstream `Bindings.__getitem__`: `Person.name` reads the row of a `Person`; a slot the
    // row lacks (declared or not) is null.
    for mode in ALL_MODES {
        assert_eq!(out("Person.name", mode), Ok(s("a")));
        assert_eq!(out("{Person.age}", mode), Ok(Value::Null));
        assert_eq!(out("Person.nick", mode), Ok(Value::Null));
    }
}

#[test]
fn statement_blocks_read_slots_as_src() {
    // The block sees only the row's own keys as `src` (a missing attribute is null), and never
    // the bare slot names, declared or not.
    let mode = Mode::StrictUnrestricted;
    assert_eq!(out("target = src.age or 'dflt'", mode), Ok(s("dflt")));
    assert_eq!(
        out("target = 'a' if src.age is None else 'b'", mode),
        Ok(s("a"))
    );
    assert_eq!(out("if True:\n    target = src.age", mode), Ok(Value::Null));
    assert_eq!(out("x = src.name\ntarget = x", mode), Ok(s("a")));
    let err = out("target = age", mode).unwrap_err();
    assert!(err.contains("age"), "unexpected error: {err}");
}

#[test]
fn nested_class_derivations_use_their_own_source_class() {
    let source = SchemaViewProvider::from_yaml_str(SOURCE_SCHEMA).unwrap();
    let mut obj = serde_json::json!({
        "id": "nested",
        "class_derivations": {
            "Agent": {
                "populated_from": "Person",
                "slot_derivations": {
                    "id": {"populated_from": "id"},
                    "inner": {"class_derivations": {"Inner": {
                        "populated_from": "Person",
                        "slot_derivations": {"out": {"expr": "{age} + 1"}}
                    }}}
                }
            }
        }
    });
    normalise_spec_json(&mut obj);
    let spec: TransformationSpecification = serde_json::from_value(obj).unwrap();
    let target = SchemaViewProvider::from_yaml_str(
        r"
id: https://example.org/absent-target
name: absent_target
prefixes: {linkml: 'https://w3id.org/linkml/'}
default_prefix: absent_target
default_range: string
imports: [linkml:types]
classes:
  Inner:
    attributes:
      out: {range: integer}
  Agent:
    attributes:
      id: {identifier: true}
      inner: {range: Inner, inlined: true}
",
    )
    .unwrap();
    let result = ObjectTransformer::new(
        spec,
        Some(&source as &dyn SchemaProvider),
        Some(&target as &dyn SchemaProvider),
    )
    .with_strict_exprs(true)
    .map_object(&row(), Some("Person"))
    .unwrap();
    let Value::Map(m) = result else {
        panic!("expected a map")
    };
    let Some(Value::Map(inner)) = m.get("inner") else {
        panic!("expected an inner object, got {m:?}")
    };
    assert_eq!(inner.get("out"), Some(&Value::Null));
}

#[test]
fn a_shared_table_gives_the_same_result_as_the_lazy_one() {
    let source = SchemaViewProvider::from_yaml_str(SOURCE_SCHEMA).unwrap();
    let table = SourceSlots::build(&source);
    let slots = table.slots_of("Student").expect("Student is a class");
    for name in ["id", "name", "age", "tags", "addr", "school"] {
        assert!(slots.contains(name), "{name}");
    }
    assert!(table.slots_of("Nobody").is_none());

    let shared = ObjectTransformer::new(
        spec("{age} + 1", "Person"),
        Some(&source as &dyn SchemaProvider),
        None,
    )
    .with_strict_exprs(true)
    .with_source_slots(&table)
    .map_object(&row(), Some("Person"))
    .unwrap();
    let lazy = engine(spec("{age} + 1", "Person"), &source, Mode::Strict)
        .map_object(&row(), Some("Person"))
        .unwrap();
    assert_eq!(shared, lazy);
}
