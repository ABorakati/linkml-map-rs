//! Evaluation-time behaviour of the Python syntax beyond the basic operators,
//! including the places that deliberately differ from upstream (each `PARITY`).
//! Expected values were produced by running upstream v0.6.0-rc1 (simpleeval for
//! the restricted evaluator, asteval for statement blocks) under CPython 3.13.

use serde_json::{Value as Json, json};

use super::error::ExprError;
use super::eval::{Bindings, eval_expr_with_mapping, parse_expr};
use crate::value::Value;

fn bindings(vars: &Json) -> Bindings {
    vars.as_object()
        .expect("an object")
        .iter()
        .map(|(k, v)| (k.clone(), Value::from(v)))
        .collect()
}

fn eval(expr: &str, vars: Json) -> Result<Value, ExprError> {
    eval_expr_with_mapping(expr, &bindings(&vars))
}

fn block(expr: &str, vars: Json) -> Result<Value, ExprError> {
    parse_expr(expr)?.eval_unrestricted(&bindings(&vars))
}

#[track_caller]
fn expect(expr: &str, vars: Json, expected: Json) {
    let got = eval(expr, vars).unwrap_or_else(|e| panic!("{expr}: {e}"));
    assert_eq!(got, Value::from(&expected), "{expr}");
}

#[track_caller]
fn expect_block(expr: &str, vars: Json, expected: Json) {
    let got = block(expr, vars).unwrap_or_else(|e| panic!("{expr}: {e}"));
    assert_eq!(got, Value::from(&expected), "{expr}");
}

#[test]
fn slices_follow_cpython_index_clamping() {
    let vars = json!({"l": [1, 2, 3], "s": "héllo"});
    expect("l[1:]", vars.clone(), json!([2, 3]));
    expect("l[::-1]", vars.clone(), json!([3, 2, 1]));
    expect("l[-9:9]", vars.clone(), json!([1, 2, 3]));
    expect("l[3:0:-1]", vars.clone(), json!([3, 2]));
    expect("l[5:]", vars.clone(), json!([]));
    expect("s[1:3]", vars.clone(), json!("él"));
    expect("s[::-2]", vars.clone(), json!("olh"));
    expect("l[True:None]", vars.clone(), json!([2, 3]));
    for bad in [
        "l[::0]",
        "l['a':]",
        "l[1.5:]",
        "l[1:2, 3]",
        "None[1:]",
        "5[1:]",
    ] {
        assert!(eval(bad, vars.clone()).is_err(), "{bad}");
    }
}

#[test]
fn a_rejected_construct_is_refused_only_when_evaluated() {
    // `FeatureNotAvailable` / `OperatorNotDefined` at evaluation, so an untaken
    // branch is fine and the error is a rejection the unrestricted fallback retries.
    expect("1 if True else 1 @ 2", json!({}), json!(1));
    expect("1 or (lambda: 1)", json!({}), json!(1));
    expect("0 if 1 else (x := 1)", json!({}), json!(0));
    for refused in [
        "1 @ 2",
        "(lambda: 1)()",
        "(x := 1)",
        "max(*[1, 2])",
        "(*[1],)",
        "(1)(2)",
        "max(1)(2)",
    ] {
        let err = eval(refused, json!({})).unwrap_err();
        assert!(err.is_restricted_rejection(), "{refused}: {err}");
    }
}

#[test]
fn starred_and_double_starred_elements() {
    expect("[*[1, 2], 3]", json!({}), json!([1, 2, 3]));
    expect("[*'ab']", json!({}), json!(["a", "b"]));
    expect("{**{'a': 1}, 'b': 2}", json!({}), json!({"a": 1, "b": 2}));
    assert!(eval("max(**{'a': 1})", json!({})).is_err());
    assert!(eval("{**1}", json!({})).is_err());
}

#[test]
fn identity_is_modelled_on_cpython_singletons() {
    let vars = json!({"a": 1000, "s": "hello", "n": null, "l": [1]});
    expect("1 is 1", vars.clone(), json!(true));
    expect("1000 is 1000", vars.clone(), json!(false));
    expect("a is a", vars.clone(), json!(true));
    expect("'ab' is 'ab'", vars.clone(), json!(false));
    expect("'' is ''", vars.clone(), json!(true));
    expect("() is ()", vars.clone(), json!(true));
    expect("[] is []", vars.clone(), json!(false));
    expect("l is l", vars.clone(), json!(true));
    expect("n is None", vars.clone(), json!(true));
    expect("s is not None", vars.clone(), json!(true));
    expect("1 is 1.0", vars, json!(false));
}

#[test]
fn methods_and_attributes_of_scalars() {
    let vars = json!({"s": "Hello World", "n": 255, "f": 2.5});
    expect("s.upper()", vars.clone(), json!("HELLO WORLD"));
    expect("s.split(' ', 1)", vars.clone(), json!(["Hello", "World"]));
    expect("s.center(15, '*')", vars.clone(), json!("**Hello World**"));
    expect("s.format()", vars.clone(), json!("Hello World"));
    expect(
        "'{0:>5}|{x}'.format(7, x=s)",
        vars.clone(),
        json!("    7|Hello World"),
    );
    expect("n.bit_length()", vars.clone(), json!(8));
    expect("n.real + n.imag", vars.clone(), json!(255));
    expect("f.hex()", vars.clone(), json!("0x1.4000000000000p+1"));
    expect("f.is_integer()", vars.clone(), json!(false));
    expect("s.startswith(('he', 'He'))", vars.clone(), json!(true));
    // A list is not a tuple of affixes.
    assert!(eval("s.startswith(['He'])", vars.clone()).is_err());
    // Attributes of a list distribute over its items, so a list's methods are unreachable.
    assert!(eval("[1].append(2)", vars.clone()).is_err());
    assert!(eval("s.nope()", vars.clone()).is_err());
    assert!(eval("s.__class__", vars).is_err());
}

#[test]
fn a_bound_method_or_function_has_no_value() {
    for expr in ["s.upper", "upper", "b'x'", "1j", "..."] {
        let err = eval(expr, json!({"s": "x"})).unwrap_err();
        assert!(err.is_unrepresentable(), "{expr}: {err}");
    }
}

#[test]
fn fstrings_ignore_conversions_under_simpleeval_but_not_asteval() {
    let vars = json!({"s": "hi", "x": 3.14159});
    // PARITY: simpleeval's `_eval_formattedvalue` drops `!r`.
    expect("f'{s!r}'", vars.clone(), json!("hi"));
    expect("f'{s=}'", vars.clone(), json!("s=hi"));
    expect(
        "f'{x:.2f}|{s:>5}|{x:{5}.{3}}'",
        vars.clone(),
        json!("3.14|   hi| 3.14"),
    );
    expect_block("target = f'{src.s!r}'", vars, json!("'hi'"));
}

#[test]
fn dict_displays_evaluate_the_value_before_the_key() {
    // `result[self._eval(key)] = self._eval(value)`: a failing value wins over an
    // unhashable key.
    let err = eval("{k: a.id for k in [{}]}", json!({"a": 0})).unwrap_err();
    assert!(matches!(err, ExprError::Uncaught(_)), "{err}");
    let err = eval("{[]: 1}", json!({})).unwrap_err();
    assert!(err.is_restricted_rejection(), "{err}");
    let err = eval("{'a': 1}[[]]", json!({})).unwrap_err();
    assert!(err.is_restricted_rejection(), "{err}");
}

#[test]
fn statement_blocks_follow_asteval() {
    let vars = json!({"l": [1, 2, 1], "s": "hi", "m": {"a": 1}});
    // A list keeps its own attributes (no distribution); asteval's `getattr(.., None)`.
    expect_block("target = src.l.count(1)", vars.clone(), json!(2));
    expect_block("target = src.l.index(2)", vars.clone(), json!(1));
    expect_block("target = src.l.real", vars.clone(), json!(null));
    // `str.format` is an unsafe attribute; `*` unpacking is not supported at all.
    expect_block("target = src.s.format()", vars.clone(), json!(null));
    expect_block("target = [*src.l]", vars.clone(), json!(null));
    expect_block("target = max(*src.l)", vars.clone(), json!(null));
    // `**` in a dict display is a `None` key, which no map here can hold.
    let err = block("target = {**src.m}", vars.clone()).unwrap_err();
    assert!(err.is_unrepresentable(), "{err}");
    expect_block("target = (lambda x, y=2: x + y)(1)", vars.clone(), json!(3));
    expect_block("target = src.s.upper().lower()", vars, json!("hi"));
}

#[test]
fn deeply_nested_lambdas_are_refused_not_overflowed() {
    let src = format!("{}1", "lambda: ".repeat(10_000));
    assert!(parse_expr(&src).is_err());
}
