//! Statement-block (`asteval`-style) `expr:` evaluation through the engine.
//!
//! Ports upstream `tests/test_transformer/test_quiet_mapping.py` (f299f3a) and
//! the dispatch rule `_is_statement_block` (`transformer/object_transformer.py`).

use linkml_map_core::datamodel::TransformationSpecification;
use linkml_map_core::engine::ObjectTransformer;
use linkml_map_core::error::Error;
use linkml_map_core::value::Value;

/// Map `person` through a one-slot spec whose slot `v` is derived from `expr`.
fn derive(expr: &str, person: serde_json::Value) -> Result<Value, Error> {
    let spec: TransformationSpecification = serde_json::from_value(serde_json::json!({
        "class_derivations": [{
            "name": "P",
            "populated_from": "Person",
            "slot_derivations": { "v": { "name": "v", "expr": expr } },
        }],
    }))
    .expect("spec");
    let engine = ObjectTransformer::new(spec, None, None);
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
