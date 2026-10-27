//! Statement-block (`asteval`-style) `expr:` evaluation through the engine.
//!
//! Ports upstream `tests/test_transformer/test_quiet_mapping.py` (f299f3a) and
//! the dispatch rule `_is_statement_block` (`transformer/object_transformer.py`).
//!
//! Statement blocks run only with `unrestricted_eval`, as upstream's tests do
//! (`ObjectTransformer(..., unrestricted_eval=True)`); the restricted-mode
//! counterparts at the end pin what the default evaluator does with them. The
//! expected values were checked against upstream v0.6.0-rc1 run both ways.

use linkml_map_core::datamodel::TransformationSpecification;
use linkml_map_core::engine::ObjectTransformer;
use linkml_map_core::error::Error;
use linkml_map_core::value::Value;

/// Map `person` through a one-slot spec whose slot `v` is derived from `expr`,
/// with `unrestricted_eval` on.
fn derive(expr: &str, person: serde_json::Value) -> Result<Value, Error> {
    derive_in_mode(expr, person, true)
}

/// As [`derive`], with `unrestricted_eval` set to `unrestricted`.
fn derive_in_mode(
    expr: &str,
    person: serde_json::Value,
    unrestricted: bool,
) -> Result<Value, Error> {
    let spec: TransformationSpecification = serde_json::from_value(serde_json::json!({
        "class_derivations": [{
            "name": "P",
            "populated_from": "Person",
            "slot_derivations": { "v": { "name": "v", "expr": expr } },
        }],
    }))
    .expect("spec");
    let engine = ObjectTransformer::new(spec, None, None).with_unrestricted_eval(unrestricted);
    let out = engine.map_object(&Value::from(&person), Some("Person"))?;
    match out {
        Value::Map(m) => Ok(m.get("v").cloned().unwrap_or(Value::Null)),
        other => panic!("expected a map, got {other:?}"),
    }
}

fn err_text(r: Result<Value, Error>) -> String {
    r.expect_err("expected an error").to_string()
}

// PARITY: test_statement_block_runs_every_statement
#[test]
fn statement_block_runs_every_statement() {
    let v = derive(
        "x = src.age * 2\ntarget = x + 1",
        serde_json::json!({"age": 10}),
    );
    assert_eq!(v.unwrap(), Value::Int(21));
}

// PARITY: test_statement_block_undefined_name_raises (three parametrised exprs)
#[test]
fn statement_block_undefined_name_raises() {
    for expr in [
        "x = {age} * 2\ntarget = x + 1",
        "target = {age} * 2",
        "target = age * 2",
    ] {
        let msg = err_text(derive(expr, serde_json::json!({"age": 10})));
        assert!(
            msg.contains("Unrestricted expression uses an undefined name: ")
                && msg.contains("NameError: name 'age' is not defined")
                && msg.ends_with("Statements read slots as src.<slot>."),
            "{expr:?} gave {msg}"
        );
    }
}

#[test]
fn undefined_name_message_quotes_the_failing_line_and_column() {
    // asteval's ExceptionHolder.get_error(): the source line, a caret line at
    // the name's column, then `NameError: ...`.
    let msg = err_text(derive(
        "x = src.age\ntarget = age * 2\n",
        serde_json::json!({"age": 10}),
    ));
    assert!(
        msg.ends_with(
            "undefined name: target = age * 2\n         ^^^^\n\
             NameError: name 'age' is not defined. Statements read slots as src.<slot>."
        ),
        "{msg}"
    );
    // Column 0 draws no caret line.
    let msg = err_text(derive("target = 1\nage\n", serde_json::json!({})));
    assert!(
        msg.ends_with(
            "undefined name: age\nNameError: name 'age' is not defined. \
             Statements read slots as src.<slot>."
        ),
        "{msg}"
    );
}

// PARITY: test_statement_block_missing_data_yields_none
#[test]
fn statement_block_missing_data_yields_none() {
    let v = derive(
        "target = [e for e in src.events][0]",
        serde_json::json!({"age": 10}),
    );
    assert_eq!(v.unwrap(), Value::Null);
}

#[test]
fn error_keeps_target_assigned_before_it() {
    // asteval stops at the first error and `target` keeps its value.
    let v = derive(
        "target = 1\nx = [e for e in src.events][0]\ntarget = 2",
        serde_json::json!({"age": 10}),
    );
    assert_eq!(v.unwrap(), Value::Int(1));
}

#[test]
fn undefined_name_after_assigned_target_still_raises() {
    let msg = err_text(derive(
        "target = 1\ntarget = nope",
        serde_json::json!({"age": 10}),
    ));
    assert!(msg.contains("name 'nope' is not defined"), "{msg}");
}

#[test]
fn undefined_name_in_if_condition_raises() {
    let msg = err_text(derive(
        "if age:\n    target = 1",
        serde_json::json!({"age": 10}),
    ));
    assert!(msg.contains("name 'age' is not defined"), "{msg}");
}

#[test]
fn undefined_attribute_root_raises_in_a_block() {
    let msg = err_text(derive("target = age.x", serde_json::json!({"age": 10})));
    assert!(msg.contains("name 'age' is not defined"), "{msg}");
}

#[test]
fn null_is_not_a_name_in_a_block() {
    // Single expressions see `NULL`; the asteval namespace has no such symbol.
    let msg = err_text(derive("target = NULL", serde_json::json!({})));
    assert!(msg.contains("name 'NULL' is not defined"), "{msg}");
}

#[test]
fn locals_and_comprehension_variables_stay_visible() {
    let v = derive(
        "years = [e.year for e in src.events if e.kind == \"start\"]\nif years:\n    target = years[0]",
        serde_json::json!({"events": [
            {"kind": "start", "year": 2001}, {"kind": "stop", "year": 2009}]}),
    );
    assert_eq!(v.unwrap(), Value::Int(2001));
}

// PARITY: _is_statement_block: only `src` and `target` are bare names; a bare
// slot name works in a single expression.
#[test]
fn single_expression_still_reads_bare_slots() {
    let v = derive("{age} * 2", serde_json::json!({"age": 10}));
    assert_eq!(v.unwrap(), Value::Int(20));
    let v = derive("age * 2", serde_json::json!({"age": 10}));
    assert_eq!(v.unwrap(), Value::Int(20));
}

// Pre-existing bug fixed alongside: a YAML `|` block adds a trailing newline.
#[test]
fn single_expression_with_trailing_newline_is_not_a_block() {
    let v = derive("{age} * 2\n", serde_json::json!({"age": 10}));
    assert_eq!(v.unwrap(), Value::Int(20));
}

#[test]
fn bracketed_multi_line_expression_is_one_expression() {
    let expr = "case(\n    ({age} > 5, \"big\"),\n    (True, \"small\"),\n)\n";
    assert_eq!(
        derive(expr, serde_json::json!({"age": 10})).unwrap(),
        Value::Str("big".into())
    );
    assert_eq!(
        derive(expr, serde_json::json!({"age": 1})).unwrap(),
        Value::Str("small".into())
    );
}

#[test]
fn comment_after_an_expression_does_not_make_it_a_block() {
    let v = derive("{age} * 2  # double it\n", serde_json::json!({"age": 10}));
    assert_eq!(v.unwrap(), Value::Int(20));
}

#[test]
fn comments_and_continuations_in_a_block() {
    let v = derive(
        "# header\nxs = [\n    e.year  # the year\n    for e in src.events\n]\ntarget = len(xs)\n",
        serde_json::json!({"events": [{"year": 1}, {"year": 2}]}),
    );
    assert_eq!(v.unwrap(), Value::Int(2));
}

// === restricted mode (the default) ========================================
//
// Upstream's `Bindings` hold the source slots only: `src` is not bound, and
// `simpleeval` evaluates the first statement of a block.

fn restricted(expr: &str) -> Result<Value, Error> {
    derive_in_mode(
        expr,
        serde_json::json!({"id": "1", "age": 10, "name": "Al", "events": ["a", "b"]}),
        false,
    )
}

fn unrestricted(expr: &str) -> Result<Value, Error> {
    derive_in_mode(
        expr,
        serde_json::json!({"id": "1", "age": 10, "name": "Al", "events": ["a", "b"]}),
        true,
    )
}

// PARITY: upstream test_statement_block_runs_every_statement's docstring: "the
// restricted evaluator used to run only the first and return 20".
#[test]
fn restricted_runs_only_the_first_statement() {
    assert_eq!(
        restricted("x = {age}*2\ntarget = x+1").unwrap(),
        Value::Int(20)
    );
    // `src` is unbound there, so `src.age * 2` is null.
    assert_eq!(
        restricted("x = src.age*2\ntarget = x+1").unwrap(),
        Value::Null
    );
    assert_eq!(restricted("target = 1").unwrap(), Value::Int(1));
}

#[test]
fn restricted_has_no_src_even_in_a_single_expression() {
    for mode in [restricted, unrestricted] {
        assert_eq!(mode("src.name").unwrap(), Value::Null);
        assert_eq!(mode("src.age + 1").unwrap(), Value::Null);
    }
}

#[test]
fn restricted_if_block_is_not_available() {
    let msg = err_text(restricted("if {age}:\n    target = 1"));
    assert!(
        msg.contains("Sorry, If is not available in this evaluator"),
        "{msg}"
    );
}

// Upstream test_derive_from_expr_restricted_raises / `_unrestricted_fallback`:
// a rejection (TypeError, ValueError, InvalidExpression) is re-raised in
// restricted mode and falls back to asteval, which yields null, with the opt-in.
#[test]
fn rejections_raise_when_restricted_and_become_null_when_unrestricted() {
    for expr in ["int(name)", "len(5)", "unknownfn(1)"] {
        assert!(restricted(expr).is_err(), "{expr}");
        assert_eq!(unrestricted(expr).unwrap(), Value::Null, "{expr}");
    }
}

// IndexError, AttributeError and NameError are not in `_eval_expr`'s catch
// tuple, so they propagate in both modes.
#[test]
fn other_errors_propagate_in_both_modes() {
    for expr in ["events[5]", "name.foo", "[e.x for e in events]"] {
        assert!(restricted(expr).is_err(), "{expr}");
        assert!(unrestricted(expr).is_err(), "{expr}");
    }
}

#[test]
fn comprehension_attribute_message_in_restricted_mode() {
    let msg = err_text(restricted("[e.x for e in events]"));
    assert!(
        msg.contains("Attribute access on comprehension-bound 'e' in 'e.x' is not supported."),
        "{msg}"
    );
}

#[test]
fn single_expressions_are_unaffected_by_the_mode() {
    for mode in [restricted, unrestricted] {
        assert_eq!(mode("{age} * 2").unwrap(), Value::Int(20));
    }
}

// Upstream tests/test_transformer/test_range_override.py
// `test_parse_expr_malformed_input_raises` (restricted) and its unrestricted
// counterpart, whose docstring "malformed parse inputs yield None" is the
// asteval fallback at work. Upstream's expression calls the `str.split` method,
// which this evaluator has no syntax for, so the `split()` function stands in.
const PARSE_DEPTH: &str =
    r#"{"has_numeric_value": float(split(depth, " ")[0]), "has_unit": split(depth, " ")[1]}"#;

#[test]
fn malformed_parse_input_raises_when_restricted_and_is_null_when_unrestricted() {
    for depth in ["", "five m"] {
        let person = serde_json::json!({ "depth": depth });
        assert!(
            derive_in_mode(PARSE_DEPTH, person.clone(), false).is_err(),
            "{depth:?}"
        );
        assert_eq!(
            derive_in_mode(PARSE_DEPTH, person, true).unwrap(),
            Value::Null,
            "{depth:?}"
        );
    }
    // `"5".split(" ")[1]` is an IndexError, which `_eval_expr` never catches.
    let person = serde_json::json!({ "depth": "5" });
    assert!(derive_in_mode(PARSE_DEPTH, person.clone(), false).is_err());
    assert!(derive_in_mode(PARSE_DEPTH, person, true).is_err());
    let ok = derive_in_mode(PARSE_DEPTH, serde_json::json!({"depth": "5 m"}), false).unwrap();
    assert_eq!(
        ok,
        Value::from(&serde_json::json!({"has_numeric_value": 5.0, "has_unit": "m"}))
    );
}
