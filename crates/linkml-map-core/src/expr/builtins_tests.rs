//! Tests for the built-in function table (`super::builtins`).
//!
//! Two kinds: ports of the upstream pytest cases in
//! `tests/test_utils/test_eval_utils.py` (named after them), and a large set of
//! cases whose expected results come from running the upstream function tables
//! (`FUNCTIONS` in `utils/eval_utils.py`, with `inflection` 0.5.1) under CPython
//! 3.13, so the Rust results are checked against Python itself.

// PARITY: the 3.14 literals mirror the upstream test inputs.
#![allow(clippy::approx_constant)]

use serde_json::{Value as Json, json};

use super::eval::{Bindings, eval_expr_with_mapping};
use crate::value::Value;

fn bindings(vars: &Json) -> Bindings {
    vars.as_object()
        .expect("bindings must be a JSON object")
        .iter()
        .map(|(k, v)| (k.clone(), Value::from(v)))
        .collect()
}

fn eval(expr: &str, vars: Json) -> Result<Value, super::error::ExprError> {
    eval_expr_with_mapping(expr, &bindings(&vars))
}

/// Evaluate and compare with the expected JSON-shaped value.
#[track_caller]
fn expect(expr: &str, vars: Json, expected: Json) {
    let got = eval(expr, vars).unwrap_or_else(|e| panic!("{expr}: {e}"));
    assert_eq!(got, Value::from(&expected), "{expr}");
}

#[track_caller]
fn expect_null(expr: &str, vars: Json) {
    expect(expr, vars, Json::Null);
}

#[track_caller]
fn expect_error(expr: &str, vars: Json) {
    assert!(eval(expr, vars).is_err(), "{expr} should raise");
}

/// Collects every mismatch so one run reports them all.
#[derive(Default)]
struct Checks {
    failures: Vec<String>,
}

impl Checks {
    fn ok(&mut self, expr: &str, vars: Json, expected: Json) {
        match eval(expr, vars.clone()) {
            Ok(got) if got == Value::from(&expected) => {}
            Ok(got) => self.failures.push(format!(
                "{expr} with {vars}: expected {expected}, got {got:?}"
            )),
            Err(e) => self.failures.push(format!(
                "{expr} with {vars}: expected {expected}, got error {e}"
            )),
        }
    }

    fn err(&mut self, expr: &str, vars: Json) {
        if let Ok(got) = eval(expr, vars.clone()) {
            self.failures.push(format!(
                "{expr} with {vars}: expected an error, got {got:?}"
            ));
        }
    }

    fn finish(self) {
        assert!(
            self.failures.is_empty(),
            "{} mismatches:\n{}",
            self.failures.len(),
            self.failures.join("\n")
        );
    }
}

// ---- ports of upstream test_eval_utils.py ----

#[test]
fn test_functions() {
    // test_functions (the cases not already covered by the evaluator tests)
    expect("max([1, 5, 2])", json!({}), json!(5));
    expect("max({x})", json!({"x": [1, 5, 2]}), json!(5));
    expect("min([3, 1, 2])", json!({}), json!(1));
    expect("len([1, 2, 3])", json!({}), json!(3));
    expect(r#"strlen("abc")"#, json!({}), json!(3));
    expect(r#"strlen("a" + "bc")"#, json!({}), json!(3));
    expect("str(42)", json!({}), json!("42"));
    expect(r#"int("42")"#, json!({}), json!(42));
    expect(r#"float("3.14")"#, json!({}), json!(3.14));
    expect("bool(1)", json!({}), json!(true));
    expect("bool(0)", json!({}), json!(false));
    expect("abs(-5)", json!({}), json!(5));
    expect("round(3.7)", json!({}), json!(4));
}

#[test]
fn test_case_function() {
    let case = "case(({x} < 25, 'LOW'), ({x} > 75, 'HIGH'), (True, 'MEDIUM'))";
    expect(case, json!({"x": 10}), json!("LOW"));
    expect(case, json!({"x": 100}), json!("HIGH"));
    expect(case, json!({"x": 50}), json!("MEDIUM"));
}

#[test]
fn test_function_distributes_over_lists() {
    expect(
        "strlen(items)",
        json!({"items": ["ab", "cde", "f"]}),
        json!([2, 3, 1]),
    );
}

#[test]
fn test_unknown_function_raises() {
    let e = eval("my_func(1)", json!({})).unwrap_err().to_string();
    assert!(e.contains("Unknown function 'my_func'"), "{e}");
    assert!(e.contains("--functions"), "{e}");
}

#[test]
fn test_unknown_function_suggests_close_names() {
    // upstream suggest_for_unknown_name: difflib.get_close_matches(n=3, cutoff=0.6)
    let e = eval("uper('a')", json!({})).unwrap_err().to_string();
    assert!(
        e.contains("Unknown function 'uper'. Did you mean: 'upper'"),
        "{e}"
    );
    let e = eval("strp(' a ')", json!({})).unwrap_err().to_string();
    assert!(e.contains("Did you mean: 'strip'"), "{e}");
    let e = eval("xyzzy(1)", json!({})).unwrap_err().to_string();
    assert_eq!(
        e,
        "eval error: Unknown function 'xyzzy'. (If this is a custom function, pass it via --functions <path>.)"
    );
}

#[test]
fn test_unknown_underscore_function_omits_extensions_hint() {
    let e = eval("_hidden(1)", json!({})).unwrap_err().to_string();
    assert!(e.contains("Unknown function '_hidden'"), "{e}");
    assert!(!e.contains("--functions"), "{e}");
}

#[test]
fn test_import_call_reports_unsupported() {
    let e = eval("__import__('os')", json!({})).unwrap_err().to_string();
    assert!(e.contains("Import expressions are not supported"), "{e}");
    assert!(!e.contains("--functions"), "{e}");
}

#[test]
fn test_null_in_function_call() {
    expect_null("float(x)", json!({"x": null}));
    expect_null("strlen(x)", json!({"x": null}));
    expect_null("abs(x)", json!({"x": null}));
}

#[test]
fn test_list_functions_do_not_distribute() {
    expect("max(items)", json!({"items": [1, 5, 2]}), json!(5));
    expect("min(items)", json!({"items": [3, 1, 2]}), json!(1));
    expect("len(items)", json!({"items": ["a", "b", "c"]}), json!(3));
}

#[test]
fn test_scalar_functions_distribute_over_lists() {
    expect(
        "str(items)",
        json!({"items": [1, 2, 3]}),
        json!(["1", "2", "3"]),
    );
    expect(
        "abs(items)",
        json!({"items": [-1, 2, -3]}),
        json!([1, 2, 3]),
    );
    expect(
        "int(items)",
        json!({"items": ["1", "2", "3"]}),
        json!([1, 2, 3]),
    );
}

#[test]
fn test_is_numeric() {
    expect("is_numeric(x)", json!({"x": "3.14"}), json!(true));
    expect("is_numeric(x)", json!({"x": "abc"}), json!(false));
    expect("is_numeric(x)", json!({"x": 5}), json!(true));
    expect("is_numeric(x)", json!({"x": ""}), json!(false));
    expect("is_numeric(x)", json!({"x": null}), json!(false));
    expect("is_numeric(x)", json!({"x": "0"}), json!(true));
}

#[test]
fn test_is_numeric_guard_pattern() {
    let expr = "case((is_numeric(x), x * 2.54), (True, None))";
    expect(expr, json!({"x": "5"}), json!(12.7));
    expect_null(expr, json!({"x": "abc"}));
    expect_null(expr, json!({"x": ""}));
    expect_null(expr, json!({"x": null}));
}

#[test]
fn test_list_functions_new() {
    expect("sum([1, 2, 3])", json!({}), json!(6));
    expect("sum(items)", json!({"items": [10, 20, 30]}), json!(60));
    expect("sorted([3, 1, 2])", json!({}), json!([1, 2, 3]));
    expect(
        "sorted(items)",
        json!({"items": ["c", "a", "b"]}),
        json!(["a", "b", "c"]),
    );
    expect("any([False, True, False])", json!({}), json!(true));
    expect("any([False, False])", json!({}), json!(false));
    expect("all([True, True])", json!({}), json!(true));
    expect("all([True, False])", json!({}), json!(false));
    expect("reversed([1, 2, 3])", json!({}), json!([3, 2, 1]));
    expect(
        "reversed(items)",
        json!({"items": ["a", "b", "c"]}),
        json!(["c", "b", "a"]),
    );
}

#[test]
fn test_new_list_functions_null_safe() {
    for f in ["sum", "sorted", "any", "all", "reversed"] {
        expect_null(&format!("{f}(x)"), json!({"x": null}));
    }
}

#[test]
fn test_new_list_functions_do_not_distribute() {
    expect("sum(items)", json!({"items": [1, 2, 3]}), json!(6));
    expect(
        "sorted(items)",
        json!({"items": [3, 1, 2]}),
        json!([1, 2, 3]),
    );
}

#[test]
fn test_string_functions() {
    let cases: &[(&str, Json)] = &[
        (r#"upper("hello")"#, json!("HELLO")),
        (r#"lower("HELLO")"#, json!("hello")),
        (r#"title("hello world")"#, json!("Hello World")),
        (r#"capitalize("hello world")"#, json!("Hello world")),
        (r#"strip("  hello  ")"#, json!("hello")),
        (r#"lstrip("  hello  ")"#, json!("hello  ")),
        (r#"rstrip("  hello  ")"#, json!("  hello")),
        (r#"replace("hello", "l", "r")"#, json!("herro")),
        (r#"startswith("hello", "hell")"#, json!(true)),
        (r#"startswith("hello", "world")"#, json!(false)),
        (r#"endswith("hello", "llo")"#, json!(true)),
        (r#"endswith("hello", "xyz")"#, json!(false)),
        (r#"split("a,b,c", ",")"#, json!(["a", "b", "c"])),
        (r#"join(",", ["a", "b", "c"])"#, json!("a,b,c")),
    ];
    for (expr, expected) in cases {
        expect(expr, json!({}), expected.clone());
    }
}

#[test]
fn test_string_functions_with_variables() {
    expect("upper(name)", json!({"name": "alice"}), json!("ALICE"));
    expect("lower(name)", json!({"name": "BOB"}), json!("bob"));
    expect("strip(name)", json!({"name": "  alice  "}), json!("alice"));
    expect(
        r#"replace(name, " ", "_")"#,
        json!({"name": "hello world"}),
        json!("hello_world"),
    );
}

#[test]
fn test_string_functions_distribute_over_lists() {
    expect(
        "upper(names)",
        json!({"names": ["alice", "bob"]}),
        json!(["ALICE", "BOB"]),
    );
    expect(
        "lower(names)",
        json!({"names": ["ALICE", "BOB"]}),
        json!(["alice", "bob"]),
    );
    expect(
        "strip(names)",
        json!({"names": ["  a  ", "  b  "]}),
        json!(["a", "b"]),
    );
    expect(
        "title(names)",
        json!({"names": ["hello world", "foo bar"]}),
        json!(["Hello World", "Foo Bar"]),
    );
    expect(
        r#"replace(names, " ", "_")"#,
        json!({"names": ["hello world", "foo bar"]}),
        json!(["hello_world", "foo_bar"]),
    );
}

#[test]
fn test_string_functions_null_propagation() {
    expect_null("upper(x)", json!({"x": null}));
    expect_null("lower(x)", json!({"x": null}));
    expect_null("strip(x)", json!({"x": null}));
    expect_null("replace(x, 'a', 'b')", json!({"x": null}));
}

#[test]
fn test_string_functions_null_in_list() {
    expect(
        "upper(names)",
        json!({"names": ["alice", null, "bob"]}),
        json!(["ALICE", null, "BOB"]),
    );
}

#[test]
fn test_split_and_join_roundtrip() {
    expect(
        r#"join(",", split(x, ","))"#,
        json!({"x": "a,b,c"}),
        json!("a,b,c"),
    );
}

#[test]
fn test_type_predicates() {
    let cases: &[(&str, Json, bool)] = &[
        (r#"is_str("hello")"#, json!({}), true),
        ("is_str(42)", json!({}), false),
        ("is_str(x)", json!({"x": null}), false),
        ("is_int(42)", json!({}), true),
        ("is_int(3.14)", json!({}), false),
        (r#"is_int("42")"#, json!({}), false),
        ("is_int(True)", json!({}), false),
        ("is_float(3.14)", json!({}), true),
        ("is_float(42)", json!({}), false),
        ("is_bool(True)", json!({}), true),
        ("is_bool(False)", json!({}), true),
        ("is_bool(1)", json!({}), false),
        ("is_list([1, 2])", json!({}), true),
        (r#"is_list("hello")"#, json!({}), false),
        ("is_list(x)", json!({"x": null}), false),
    ];
    for (expr, vars, expected) in cases {
        expect(expr, vars.clone(), json!(expected));
    }
}

#[test]
fn test_type_predicates_in_case() {
    // The upstream test also checks `upper(x) if is_str(x) else str(x)`; the
    // conditional expression is not part of the Rust expression grammar.
    expect(
        r#"case((is_str(x), upper(x)), (True, "other"))"#,
        json!({"x": "hello"}),
        json!("HELLO"),
    );
    // case() evaluates every branch value, so `upper(x)` on a non-string raises
    // (upstream: "the value expression must be safe for all inputs").
    expect_error(
        r#"case((is_str(x), upper(x)), (True, "other"))"#,
        json!({"x": 42}),
    );
}

#[test]
fn test_contains() {
    expect(
        r#"contains("hello world", "world")"#,
        json!({}),
        json!(true),
    );
    expect(r#"contains("hello world", "xyz")"#, json!({}), json!(false));
    expect(r#"contains(x, "ell")"#, json!({"x": "hello"}), json!(true));
    expect(r#"contains(x, "ell")"#, json!({"x": "world"}), json!(false));
    expect(
        r#"contains(names, "li")"#,
        json!({"names": ["alice", "bob", "charlie"]}),
        json!([true, false, true]),
    );
    expect_null(r#"contains(x, "a")"#, json!({"x": null}));
    expect(
        r#"contains(names, "a")"#,
        json!({"names": ["alice", null, "bob"]}),
        json!([true, null, false]),
    );
}

#[test]
fn test_coalesce() {
    let cases: &[(&str, Json, Json)] = &[
        (
            r#"coalesce(None, "fallback")"#,
            json!({}),
            json!("fallback"),
        ),
        (
            r#"coalesce("hello", "fallback")"#,
            json!({}),
            json!("hello"),
        ),
        (r#"coalesce(None, None, "last")"#, json!({}), json!("last")),
        ("coalesce(None, None)", json!({}), Json::Null),
        (
            r#"coalesce(x, "default")"#,
            json!({"x": "value"}),
            json!("value"),
        ),
        (
            r#"coalesce(x, "default")"#,
            json!({"x": null}),
            json!("default"),
        ),
        ("coalesce(x, y)", json!({"x": null, "y": 42}), json!(42)),
    ];
    for (expr, vars, expected) in cases {
        expect(expr, vars.clone(), expected.clone());
    }
}

#[test]
fn test_coalesce_preserves_falsy_values() {
    expect("coalesce(x, 99)", json!({"x": 0}), json!(0));
    expect(r#"coalesce(x, "default")"#, json!({"x": ""}), json!(""));
    expect("coalesce(x, True)", json!({"x": false}), json!(false));
    expect(r#"coalesce(x, "default")"#, json!({"x": []}), json!([]));
}

#[test]
fn test_coalesce_with_expression_chains() {
    expect(
        r#"upper(coalesce(x, "unknown"))"#,
        json!({"x": null}),
        json!("UNKNOWN"),
    );
    expect(
        r#"upper(coalesce(x, "unknown"))"#,
        json!({"x": "alice"}),
        json!("ALICE"),
    );
}

#[test]
fn test_first_last() {
    expect("first([10, 20, 30])", json!({}), json!(10));
    expect(
        "first(items)",
        json!({"items": ["a", "b", "c"]}),
        json!("a"),
    );
    expect_null("first(items)", json!({"items": []}));
    expect("last([10, 20, 30])", json!({}), json!(30));
    expect("last(items)", json!({"items": ["a", "b", "c"]}), json!("c"));
    expect_null("last(items)", json!({"items": []}));
    expect_null("first(x)", json!({"x": null}));
    expect_null("last(x)", json!({"x": null}));
    expect(
        "first(split(x, \"=\"))",
        json!({"x": "key=value"}),
        json!("key"),
    );
    expect(
        "last(split(x, \"=\"))",
        json!({"x": "key=value"}),
        json!("value"),
    );
}

#[test]
fn test_floor_ceil() {
    expect("floor(3.7)", json!({}), json!(3));
    expect("floor(3.2)", json!({}), json!(3));
    expect("floor(-1.5)", json!({}), json!(-2));
    expect("floor(x)", json!({"x": 4.9}), json!(4));
    expect("ceil(3.2)", json!({}), json!(4));
    expect("ceil(3.7)", json!({}), json!(4));
    expect("ceil(-1.5)", json!({}), json!(-1));
    expect("ceil(x)", json!({"x": 4.1}), json!(5));
    expect_null("floor(x)", json!({"x": null}));
    expect_null("ceil(x)", json!({"x": null}));
    expect(
        "floor(vals)",
        json!({"vals": [1.1, 2.9, 3.5]}),
        json!([1, 2, 3]),
    );
    expect(
        "ceil(vals)",
        json!({"vals": [1.1, 2.9, 3.5]}),
        json!([2, 3, 4]),
    );
}

#[test]
fn test_substr() {
    expect(r#"substr("abcdef", 0, 3)"#, json!({}), json!("abc"));
    expect(r#"substr("abcdef", 2)"#, json!({}), json!("cdef"));
    expect(r#"substr("20240101", 0, 4)"#, json!({}), json!("2024"));
    expect(r#"substr("20240101", 4, 6)"#, json!({}), json!("01"));
    expect("substr(x, 0, 4)", json!({"x": "20240101"}), json!("2024"));
    expect_null("substr(x, 0, 3)", json!({"x": null}));
    expect(
        "substr(dates, 0, 4)",
        json!({"dates": ["20240101", "20230615"]}),
        json!(["2024", "2023"]),
    );
    expect(
        "substr(dates, 0, 4)",
        json!({"dates": ["20240101", null, "20230615"]}),
        json!(["2024", null, "2023"]),
    );
}

#[test]
fn test_case_converters() {
    expect(r#"to_snake("CamelCase")"#, json!({}), json!("camel_case"));
    expect(
        r#"to_snake("HTTPResponse")"#,
        json!({}),
        json!("http_response"),
    );
    expect(r#"to_pascal("snake_case")"#, json!({}), json!("SnakeCase"));
    expect(
        r#"to_pascal("http_response")"#,
        json!({}),
        json!("HttpResponse"),
    );
    expect(r#"to_camel("snake_case")"#, json!({}), json!("snakeCase"));
    expect(
        r#"to_camel("http_response")"#,
        json!({}),
        json!("httpResponse"),
    );
}

#[test]
fn test_case_converters_distribute_and_propagate_none() {
    expect_null("to_snake(x)", json!({"x": null}));
    expect(
        "to_pascal(x)",
        json!({"x": ["snake_case", "http_response"]}),
        json!(["SnakeCase", "HttpResponse"]),
    );
}

#[test]
fn test_uuid5_matches_stdlib() {
    // test_uuid5_*: uuid5(uuid5(NAMESPACE_URL, ns), name)
    expect(
        r#"uuid5("https://example.org/ns", "example")"#,
        json!({}),
        json!("579c5560-7d2f-5b42-aa37-f82032facbea"),
    );
}

// ---- table wiring ----

#[test]
fn function_names_match_the_dispatch_table() {
    let mut table: Vec<&str> = super::builtins::function_names().collect();
    table.push("slot");
    let mut listed: Vec<&str> = super::eval::FUNCTION_NAMES.to_vec();
    table.sort_unstable();
    listed.sort_unstable();
    assert_eq!(table, listed);
    let mut deduped = table.clone();
    deduped.dedup();
    assert_eq!(deduped, table, "a function name is listed twice");
}

// ---- hand-written edge cases (Python behaviour checked against CPython 3.13) ----

#[test]
fn nan_and_infinity() {
    let eval_with = |expr: &str, x: f64| {
        let mut b = Bindings::new();
        b.insert("x".into(), Value::Float(x));
        eval_expr_with_mapping(expr, &b)
    };
    // int/floor/ceil/round of NaN raise ValueError, of infinity OverflowError.
    for f in ["int", "floor", "ceil", "round"] {
        for x in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(eval_with(&format!("{f}(x)"), x).is_err(), "{f}({x})");
        }
    }
    // round(x, ndigits) returns NaN and infinity unchanged.
    assert_eq!(
        eval_with("round(x, 2)", f64::INFINITY).unwrap(),
        Value::Float(f64::INFINITY)
    );
    assert!(matches!(eval_with("round(x, 2)", f64::NAN).unwrap(), Value::Float(f) if f.is_nan()));
    // float() accepts the inf/nan spellings.
    assert_eq!(
        eval("float('-inf')", json!({})).unwrap(),
        Value::Float(f64::NEG_INFINITY)
    );
    assert_eq!(
        eval("float(' Infinity ')", json!({})).unwrap(),
        Value::Float(f64::INFINITY)
    );
    assert!(matches!(eval("float('nan')", json!({})).unwrap(), Value::Float(f) if f.is_nan()));
    // max/min and sorted with NaN: comparisons are all false, so the first item stays.
    assert!(matches!(eval_with("max([x, 1.0])", f64::NAN).unwrap(), Value::Float(f) if f.is_nan()));
    assert_eq!(
        eval_with("min([1.0, x])", f64::NAN).unwrap(),
        Value::Float(1.0)
    );
}

#[test]
fn python_whitespace_set() {
    // str.strip() and str.split() treat U+001C..U+001F and U+0085 as whitespace.
    expect(
        "strip(x)",
        json!({"x": "\u{1c}a\u{1d}\u{85}\u{a0}\u{2003}\u{3000}"}),
        json!("a"),
    );
    expect(
        "split(x)",
        json!({"x": "a\u{1f}b\u{2028}c\u{200b}d"}),
        json!(["a", "b", "c\u{200b}d"]),
    );
    // U+200B and U+FEFF are not whitespace to Python.
    expect(
        "strip(x)",
        json!({"x": "\u{200b}a\u{feff}"}),
        json!("\u{200b}a\u{feff}"),
    );
    expect("is_numeric(x)", json!({"x": "\u{1c}5\u{1f}"}), json!(true));
}

#[test]
fn startswith_and_endswith_take_a_list_for_python_tuples() {
    // PARITY: Python takes a tuple of prefixes; `Value` has no tuple (a tuple
    // literal evaluates to a list), so a list of strings is accepted too.
    expect("startswith('hello', ('he', 'zz'))", json!({}), json!(true));
    expect("startswith('hello', ['zz'])", json!({}), json!(false));
    expect("startswith('hello', [])", json!({}), json!(false));
    expect("endswith('hello', ('zz', 'lo'))", json!({}), json!(true));
    expect("endswith('hello', ('he', 'zz'))", json!({}), json!(false));
    expect_error("endswith('hello', ('lo', 5))", json!({}));
}

#[test]
fn python_parity_cases() {
    let mut c = Checks::default();
    // BEGIN GENERATED
    // --- case
    c.ok(r#"upper('hello world')"#, json!({}), json!("HELLO WORLD"));
    c.ok(r#"lower('hello world')"#, json!({}), json!("hello world"));
    c.ok(r#"title('hello world')"#, json!({}), json!("Hello World"));
    c.ok(
        r#"capitalize('hello world')"#,
        json!({}),
        json!("Hello world"),
    );
    c.ok(
        r#"upper("they're bill's friends")"#,
        json!({}),
        json!("THEY'RE BILL'S FRIENDS"),
    );
    c.ok(
        r#"lower("they're bill's friends")"#,
        json!({}),
        json!("they're bill's friends"),
    );
    c.ok(
        r#"title("they're bill's friends")"#,
        json!({}),
        json!("They'Re Bill'S Friends"),
    );
    c.ok(
        r#"capitalize("they're bill's friends")"#,
        json!({}),
        json!("They're bill's friends"),
    );
    c.ok(r#"upper('a1b')"#, json!({}), json!("A1B"));
    c.ok(r#"lower('a1b')"#, json!({}), json!("a1b"));
    c.ok(r#"title('a1b')"#, json!({}), json!("A1B"));
    c.ok(r#"capitalize('a1b')"#, json!({}), json!("A1b"));
    c.ok(r#"upper('ǆemal')"#, json!({}), json!("ǄEMAL"));
    c.ok(r#"lower('ǆemal')"#, json!({}), json!("ǆemal"));
    c.ok(r#"title('ǆemal')"#, json!({}), json!("ǅemal"));
    c.ok(r#"capitalize('ǆemal')"#, json!({}), json!("ǅemal"));
    c.ok(r#"upper('straße')"#, json!({}), json!("STRASSE"));
    c.ok(r#"lower('straße')"#, json!({}), json!("straße"));
    c.ok(r#"title('straße')"#, json!({}), json!("Straße"));
    c.ok(r#"capitalize('straße')"#, json!({}), json!("Straße"));
    c.ok(r#"upper('ﬁsh ﬂow')"#, json!({}), json!("FISH FLOW"));
    c.ok(r#"lower('ﬁsh ﬂow')"#, json!({}), json!("ﬁsh ﬂow"));
    c.ok(r#"title('ﬁsh ﬂow')"#, json!({}), json!("Fish Flow"));
    c.ok(r#"capitalize('ﬁsh ﬂow')"#, json!({}), json!("Fish ﬂow"));
    c.ok(r#"upper('ΟΔΟΣ ΣΟΦΟΣ')"#, json!({}), json!("ΟΔΟΣ ΣΟΦΟΣ"));
    c.ok(r#"lower('ΟΔΟΣ ΣΟΦΟΣ')"#, json!({}), json!("οδος σοφος"));
    c.ok(r#"title('ΟΔΟΣ ΣΟΦΟΣ')"#, json!({}), json!("Οδος Σοφος"));
    c.ok(
        r#"capitalize('ΟΔΟΣ ΣΟΦΟΣ')"#,
        json!({}),
        json!("Οδος σοφος"),
    );
    c.ok(r#"upper('ὈΔΥΣΣΕΎΣ')"#, json!({}), json!("ὈΔΥΣΣΕΎΣ"));
    c.ok(r#"lower('ὈΔΥΣΣΕΎΣ')"#, json!({}), json!("ὀδυσσεύς"));
    c.ok(r#"title('ὈΔΥΣΣΕΎΣ')"#, json!({}), json!("Ὀδυσσεύς"));
    c.ok(r#"capitalize('ὈΔΥΣΣΕΎΣ')"#, json!({}), json!("Ὀδυσσεύς"));
    c.ok(r#"upper('ǅ')"#, json!({}), json!("Ǆ"));
    c.ok(r#"lower('ǅ')"#, json!({}), json!("ǆ"));
    c.ok(r#"title('ǅ')"#, json!({}), json!("ǅ"));
    c.ok(r#"capitalize('ǅ')"#, json!({}), json!("ǅ"));
    c.ok(r#"upper('İstanbul')"#, json!({}), json!("İSTANBUL"));
    c.ok(r#"lower('İstanbul')"#, json!({}), json!("i̇stanbul"));
    c.ok(r#"title('İstanbul')"#, json!({}), json!("İstanbul"));
    c.ok(r#"capitalize('İstanbul')"#, json!({}), json!("İstanbul"));
    c.ok(r#"upper('x-y_z')"#, json!({}), json!("X-Y_Z"));
    c.ok(r#"lower('x-y_z')"#, json!({}), json!("x-y_z"));
    c.ok(r#"title('x-y_z')"#, json!({}), json!("X-Y_Z"));
    c.ok(r#"capitalize('x-y_z')"#, json!({}), json!("X-y_z"));
    c.ok(r#"upper("O'NEIL")"#, json!({}), json!("O'NEIL"));
    c.ok(r#"lower("O'NEIL")"#, json!({}), json!("o'neil"));
    c.ok(r#"title("O'NEIL")"#, json!({}), json!("O'Neil"));
    c.ok(r#"capitalize("O'NEIL")"#, json!({}), json!("O'neil"));
    c.ok(r#"upper('McDONALD')"#, json!({}), json!("MCDONALD"));
    c.ok(r#"lower('McDONALD')"#, json!({}), json!("mcdonald"));
    c.ok(r#"title('McDONALD')"#, json!({}), json!("Mcdonald"));
    c.ok(r#"capitalize('McDONALD')"#, json!({}), json!("Mcdonald"));
    c.ok(r#"upper('漢a b')"#, json!({}), json!("漢A B"));
    c.ok(r#"lower('漢a b')"#, json!({}), json!("漢a b"));
    c.ok(r#"title('漢a b')"#, json!({}), json!("漢A B"));
    c.ok(r#"capitalize('漢a b')"#, json!({}), json!("漢a b"));
    c.ok(r#"upper('ა ბ')"#, json!({}), json!("Ა Ბ"));
    c.ok(r#"lower('ა ბ')"#, json!({}), json!("ა ბ"));
    c.ok(r#"title('ა ბ')"#, json!({}), json!("ა ბ"));
    c.ok(r#"capitalize('ა ბ')"#, json!({}), json!("ა ბ"));
    c.ok(r#"upper('ᾳ')"#, json!({}), json!("ΑΙ"));
    c.ok(r#"lower('ᾳ')"#, json!({}), json!("ᾳ"));
    c.ok(r#"title('ᾳ')"#, json!({}), json!("ᾼ"));
    c.ok(r#"capitalize('ᾳ')"#, json!({}), json!("ᾼ"));
    c.ok(r#"upper('ŉx')"#, json!({}), json!("ʼNX"));
    c.ok(r#"lower('ŉx')"#, json!({}), json!("ŉx"));
    c.ok(r#"title('ŉx')"#, json!({}), json!("ʼNx"));
    c.ok(r#"capitalize('ŉx')"#, json!({}), json!("ʼNx"));
    c.ok(r#"upper('')"#, json!({}), json!(""));
    c.ok(r#"lower('')"#, json!({}), json!(""));
    c.ok(r#"title('')"#, json!({}), json!(""));
    c.ok(r#"capitalize('')"#, json!({}), json!(""));
    c.ok(r#"upper('ǰ')"#, json!({}), json!("J̌"));
    c.ok(r#"lower('ǰ')"#, json!({}), json!("ǰ"));
    c.ok(r#"title('ǰ')"#, json!({}), json!("J̌"));
    c.ok(r#"capitalize('ǰ')"#, json!({}), json!("J̌"));
    c.ok(r#"upper('ΑΣ.')"#, json!({}), json!("ΑΣ."));
    c.ok(r#"lower('ΑΣ.')"#, json!({}), json!("ας."));
    c.ok(r#"title('ΑΣ.')"#, json!({}), json!("Ας."));
    c.ok(r#"capitalize('ΑΣ.')"#, json!({}), json!("Ας."));
    c.ok(r#"upper('ΑΣ.Β')"#, json!({}), json!("ΑΣ.Β"));
    c.ok(r#"lower('ΑΣ.Β')"#, json!({}), json!("ασ.β"));
    c.ok(r#"title('ΑΣ.Β')"#, json!({}), json!("Ασ.Β"));
    c.ok(r#"capitalize('ΑΣ.Β')"#, json!({}), json!("Ασ.β"));
    c.ok(r#"upper('ΑΣ b')"#, json!({}), json!("ΑΣ B"));
    c.ok(r#"lower('ΑΣ b')"#, json!({}), json!("ας b"));
    c.ok(r#"title('ΑΣ b')"#, json!({}), json!("Ας B"));
    c.ok(r#"capitalize('ΑΣ b')"#, json!({}), json!("Ας b"));
    c.ok(r#"upper('Σ')"#, json!({}), json!("Σ"));
    c.ok(r#"lower('Σ')"#, json!({}), json!("σ"));
    c.ok(r#"title('Σ')"#, json!({}), json!("Σ"));
    c.ok(r#"capitalize('Σ')"#, json!({}), json!("Σ"));
    c.ok(r#"upper('aΣ')"#, json!({}), json!("AΣ"));
    c.ok(r#"lower('aΣ')"#, json!({}), json!("aς"));
    c.ok(r#"title('aΣ')"#, json!({}), json!("Aς"));
    c.ok(r#"capitalize('aΣ')"#, json!({}), json!("Aς"));
    c.ok(r#"upper('1st 2nd')"#, json!({}), json!("1ST 2ND"));
    c.ok(r#"lower('1st 2nd')"#, json!({}), json!("1st 2nd"));
    c.ok(r#"title('1st 2nd')"#, json!({}), json!("1St 2Nd"));
    c.ok(r#"capitalize('1st 2nd')"#, json!({}), json!("1st 2nd"));
    c.ok(r#"upper('ÉCOLE élève')"#, json!({}), json!("ÉCOLE ÉLÈVE"));
    c.ok(r#"lower('ÉCOLE élève')"#, json!({}), json!("école élève"));
    c.ok(r#"title('ÉCOLE élève')"#, json!({}), json!("École Élève"));
    c.ok(
        r#"capitalize('ÉCOLE élève')"#,
        json!({}),
        json!("École élève"),
    );
    c.ok(r#"upper('ʰa ʰA')"#, json!({}), json!("ʰA ʰA"));
    c.ok(r#"lower('ʰa ʰA')"#, json!({}), json!("ʰa ʰa"));
    c.ok(r#"title('ʰa ʰA')"#, json!({}), json!("ʰa ʰa"));
    c.ok(r#"capitalize('ʰa ʰA')"#, json!({}), json!("ʰa ʰa"));
    c.ok(r#"upper('ΐ')"#, json!({}), json!("Ϊ́"));
    c.ok(r#"lower('ΐ')"#, json!({}), json!("ΐ"));
    c.ok(r#"title('ΐ')"#, json!({}), json!("Ϊ́"));
    c.ok(r#"capitalize('ΐ')"#, json!({}), json!("Ϊ́"));
    c.ok(r#"upper('և')"#, json!({}), json!("ԵՒ"));
    c.ok(r#"lower('և')"#, json!({}), json!("և"));
    c.ok(r#"title('և')"#, json!({}), json!("Եւ"));
    c.ok(r#"capitalize('և')"#, json!({}), json!("Եւ"));
    c.ok(r#"case((True, 1))"#, json!({}), json!(1));
    c.ok(r#"case((False, 1), (True, 2))"#, json!({}), json!(2));
    c.ok(r#"case((False, 1))"#, json!({}), json!(null));
    c.ok(r#"case()"#, json!({}), json!(null));
    c.ok(
        r#"case((None, 1), (0, 2), ('', 3), ([], 4), (1, 5))"#,
        json!({}),
        json!(5),
    );
    c.err(r#"case((True,))"#, json!({})); // ValueError
    c.err(r#"case((True, 1, 2))"#, json!({})); // ValueError
    c.err(r#"case(5)"#, json!({})); // TypeError
    c.ok(r#"case('ab')"#, json!({}), json!("b"));
    c.err(r#"case('a')"#, json!({})); // ValueError
    c.err(r#"case('abc')"#, json!({})); // ValueError
    c.err(r#"case('')"#, json!({})); // ValueError
    c.err(r#"case((False, 1), 5)"#, json!({})); // TypeError
    c.ok(r#"case((True, 1), 5)"#, json!({}), json!(1));
    c.err(r#"case(None)"#, json!({})); // TypeError
    c.ok(r#"case([True, 'yes'])"#, json!({}), json!("yes"));
    c.ok(r#"case(([], 1), ([0], 2))"#, json!({}), json!(2));
    c.ok(r#"case((False, 1), ('', 2))"#, json!({}), json!(null));
    // --- strip
    c.ok(r#"strip('xxhixx', 'x')"#, json!({}), json!("hi"));
    c.ok(r#"lstrip('xxhixx', 'x')"#, json!({}), json!("hixx"));
    c.ok(r#"rstrip('xxhixx', 'x')"#, json!({}), json!("xxhi"));
    c.ok(r#"strip('abcba', 'ab')"#, json!({}), json!("c"));
    c.ok(r#"strip('abc', '')"#, json!({}), json!("abc"));
    c.ok(r#"strip('  a  ')"#, json!({}), json!("a"));
    c.ok(r#"strip('abc', None)"#, json!({}), json!(null));
    c.err(r#"strip('abc', 1)"#, json!({})); // TypeError
    c.err(r#"strip(5)"#, json!({})); // TypeError
    c.err(r#"strip()"#, json!({})); // TypeError
    c.err(r#"strip('a', 'b', 'c')"#, json!({})); // TypeError
    c.ok(
        r#"strip(names)"#,
        json!({"names": ["  a ", null, " b"]}),
        json!(["a", null, "b"]),
    );
    // --- split
    c.ok(
        r#"split('a b  c')"#,
        json!({"names": ["a,b", "c"]}),
        json!(["a", "b", "c"]),
    );
    c.ok(
        r#"split('  a b  c  ')"#,
        json!({"names": ["a,b", "c"]}),
        json!(["a", "b", "c"]),
    );
    c.ok(r#"split('')"#, json!({"names": ["a,b", "c"]}), json!([]));
    c.ok(r#"split('   ')"#, json!({"names": ["a,b", "c"]}), json!([]));
    c.ok(
        r#"split('a,b,,c', ',')"#,
        json!({"names": ["a,b", "c"]}),
        json!(["a", "b", "", "c"]),
    );
    c.ok(
        r#"split('', ',')"#,
        json!({"names": ["a,b", "c"]}),
        json!([""]),
    );
    c.ok(
        r#"split(',', ',')"#,
        json!({"names": ["a,b", "c"]}),
        json!(["", ""]),
    );
    c.ok(
        r#"split('a,b,c', ',', 1)"#,
        json!({"names": ["a,b", "c"]}),
        json!(["a", "b,c"]),
    );
    c.ok(
        r#"split('a,b,c', ',', 0)"#,
        json!({"names": ["a,b", "c"]}),
        json!(["a,b,c"]),
    );
    c.ok(
        r#"split('a,b,c', ',', -1)"#,
        json!({"names": ["a,b", "c"]}),
        json!(["a", "b", "c"]),
    );
    c.ok(
        r#"split('  a  b  c  ', None)"#,
        json!({"names": ["a,b", "c"]}),
        json!(null),
    );
    c.ok(
        r#"split('a b c', ' ', 1)"#,
        json!({"names": ["a,b", "c"]}),
        json!(["a", "b c"]),
    );
    c.ok(
        r#"split('  a  b  c  ', ' ', 1)"#,
        json!({"names": ["a,b", "c"]}),
        json!(["", " a  b  c  "]),
    );
    c.ok(
        r#"split('aaa', 'aa')"#,
        json!({"names": ["a,b", "c"]}),
        json!(["", "a"]),
    );
    c.err(r#"split('a,b', '')"#, json!({"names": ["a,b", "c"]})); // ValueError
    c.err(r#"split('a,b', 5)"#, json!({"names": ["a,b", "c"]})); // TypeError
    c.err(r#"split(5)"#, json!({"names": ["a,b", "c"]})); // TypeError
    c.err(r#"split('a b c', 1)"#, json!({"names": ["a,b", "c"]})); // TypeError
    c.ok(
        r#"split(' a b c ', ' ')"#,
        json!({"names": ["a,b", "c"]}),
        json!(["", "a", "b", "c", ""]),
    );
    c.ok(
        r#"split('abc', 'abc')"#,
        json!({"names": ["a,b", "c"]}),
        json!(["", ""]),
    );
    c.ok(
        r#"split('a1b22c', '22')"#,
        json!({"names": ["a,b", "c"]}),
        json!(["a1b", "c"]),
    );
    c.ok(
        r#"split(names, ',')"#,
        json!({"names": ["a,b", "c"]}),
        json!([["a", "b"], ["c"]]),
    );
    c.err(
        r#"split('a b c', ' ', 1.5)"#,
        json!({"names": ["a,b", "c"]}),
    ); // TypeError
    // --- join
    c.ok(r#"join(',', ['a', 'b', 'c'])"#, json!({}), json!("a,b,c"));
    c.ok(r#"join('', [])"#, json!({}), json!(""));
    c.ok(r#"join(',', 'abc')"#, json!({}), json!("a,b,c"));
    c.err(r#"join(',', ['a', 1])"#, json!({})); // TypeError
    c.err(r#"join(',', 5)"#, json!({})); // TypeError
    c.err(r#"join(5, ['a'])"#, json!({})); // TypeError
    c.err(r#"join(',', ['a'], 'x')"#, json!({})); // TypeError
    c.ok(
        r#"join(['-', '+'], ['a', 'b'])"#,
        json!({}),
        json!(["a-b", "a+b"]),
    );
    c.ok(r#"join(',', None)"#, json!({}), json!(null));
    c.ok(r#"join(None, ['a'])"#, json!({}), json!(null));
    c.err(r#"join(',', [['a']])"#, json!({})); // TypeError
    c.ok(
        r#"join(', ', names)"#,
        json!({"names": ["x", "y"]}),
        json!("x, y"),
    );
    // --- replace
    c.ok(r#"replace('hello', 'l', 'r')"#, json!({}), json!("herro"));
    c.ok(
        r#"replace('hello', 'l', 'r', 1)"#,
        json!({}),
        json!("herlo"),
    );
    c.ok(
        r#"replace('hello', 'l', 'r', 0)"#,
        json!({}),
        json!("hello"),
    );
    c.ok(
        r#"replace('hello', 'l', 'r', -1)"#,
        json!({}),
        json!("herro"),
    );
    c.ok(r#"replace('abc', '', '-')"#, json!({}), json!("-a-b-c-"));
    c.ok(r#"replace('abc', '', '-', 2)"#, json!({}), json!("-a-bc"));
    c.ok(r#"replace('aaa', 'aa', 'b')"#, json!({}), json!("ba"));
    c.err(r#"replace('hello', 'l')"#, json!({})); // TypeError
    c.err(r#"replace('hello', 1, 'r')"#, json!({})); // TypeError
    c.err(r#"replace('hello', 'l', 2)"#, json!({})); // TypeError
    c.err(r#"replace('hello', 'l', 'r', 1.0)"#, json!({})); // TypeError
    c.ok(
        r#"replace('hello', 'l', 'r', True)"#,
        json!({}),
        json!("herlo"),
    );
    c.ok(
        r#"replace(['ab', 'cb'], 'b', 'x')"#,
        json!({}),
        json!(["ax", "cx"]),
    );
    c.ok(r#"replace('ab', 'b', None)"#, json!({}), json!(null));
    c.err(r#"replace(5, 'a', 'b')"#, json!({})); // TypeError
    // --- startswith
    c.ok(r#"startswith('hello', 'he')"#, json!({}), json!(true));
    c.ok(r#"startswith('hello', 'lo')"#, json!({}), json!(false));
    c.ok(r#"startswith('hello', '')"#, json!({}), json!(true));
    c.ok(r#"startswith('', '')"#, json!({}), json!(true));
    c.ok(r#"startswith('hello', 'hello!')"#, json!({}), json!(false));
    c.ok(r#"startswith('hello', 'l', 2)"#, json!({}), json!(true));
    c.ok(r#"startswith('hello', 'l', 3)"#, json!({}), json!(true));
    c.ok(r#"startswith('hello', 'h', 1)"#, json!({}), json!(false));
    c.ok(r#"startswith('hello', 'e', 1, 3)"#, json!({}), json!(true));
    c.ok(r#"startswith('hello', 'o', 0, 5)"#, json!({}), json!(false));
    c.ok(r#"startswith('hello', 'o', 0, 4)"#, json!({}), json!(false));
    c.ok(r#"startswith('hello', '', 5)"#, json!({}), json!(true));
    c.ok(r#"startswith('hello', '', 6)"#, json!({}), json!(false));
    c.ok(r#"startswith('hello', 'o', -1)"#, json!({}), json!(true));
    c.ok(
        r#"startswith('hello', 'l', -3, -1)"#,
        json!({}),
        json!(true),
    );
    c.ok(r#"startswith('hello', 'lo', 3, 5)"#, json!({}), json!(true));
    c.ok(
        r#"startswith('hello', 'ell', 1, 4)"#,
        json!({}),
        json!(true),
    );
    c.ok(r#"startswith('hello', 'l', 99)"#, json!({}), json!(false));
    c.ok(
        r#"startswith('hello', 'o', 0, 99)"#,
        json!({}),
        json!(false),
    );
    c.ok(r#"startswith('hello', 'h', -99)"#, json!({}), json!(true));
    c.err(r#"startswith('hello', 5)"#, json!({})); // TypeError
    c.err(r#"startswith('hello', ['h', 5])"#, json!({})); // TypeError
    c.err(r#"startswith(5, 'a')"#, json!({})); // TypeError
    c.err(r#"startswith('a')"#, json!({})); // TypeError
    c.err(r#"startswith('hello', 'h', 1.0)"#, json!({})); // TypeError
    // --- endswith
    c.ok(r#"endswith('hello', 'he')"#, json!({}), json!(false));
    c.ok(r#"endswith('hello', 'lo')"#, json!({}), json!(true));
    c.ok(r#"endswith('hello', '')"#, json!({}), json!(true));
    c.ok(r#"endswith('', '')"#, json!({}), json!(true));
    c.ok(r#"endswith('hello', 'hello!')"#, json!({}), json!(false));
    c.ok(r#"endswith('hello', 'l', 2)"#, json!({}), json!(false));
    c.ok(r#"endswith('hello', 'l', 3)"#, json!({}), json!(false));
    c.ok(r#"endswith('hello', 'h', 1)"#, json!({}), json!(false));
    c.ok(r#"endswith('hello', 'e', 1, 3)"#, json!({}), json!(false));
    c.ok(r#"endswith('hello', 'o', 0, 5)"#, json!({}), json!(true));
    c.ok(r#"endswith('hello', 'o', 0, 4)"#, json!({}), json!(false));
    c.ok(r#"endswith('hello', '', 5)"#, json!({}), json!(true));
    c.ok(r#"endswith('hello', '', 6)"#, json!({}), json!(false));
    c.ok(r#"endswith('hello', 'o', -1)"#, json!({}), json!(true));
    c.ok(r#"endswith('hello', 'l', -3, -1)"#, json!({}), json!(true));
    c.ok(r#"endswith('hello', 'lo', 3, 5)"#, json!({}), json!(true));
    c.ok(r#"endswith('hello', 'ell', 1, 4)"#, json!({}), json!(true));
    c.ok(r#"endswith('hello', 'l', 99)"#, json!({}), json!(false));
    c.ok(r#"endswith('hello', 'o', 0, 99)"#, json!({}), json!(true));
    c.ok(r#"endswith('hello', 'h', -99)"#, json!({}), json!(false));
    c.err(r#"endswith('hello', 5)"#, json!({})); // TypeError
    c.err(r#"endswith('hello', ['h', 5])"#, json!({})); // TypeError
    c.err(r#"endswith(5, 'a')"#, json!({})); // TypeError
    c.err(r#"endswith('a')"#, json!({})); // TypeError
    c.err(r#"endswith('hello', 'h', 1.0)"#, json!({})); // TypeError
    // --- contains
    c.ok(
        r#"contains('hello', 'ell')"#,
        json!({"names": ["ab", null, "c"]}),
        json!(true),
    );
    c.ok(
        r#"contains('hello', '')"#,
        json!({"names": ["ab", null, "c"]}),
        json!(true),
    );
    c.ok(
        r#"contains('', '')"#,
        json!({"names": ["ab", null, "c"]}),
        json!(true),
    );
    c.ok(
        r#"contains('hello', 'x')"#,
        json!({"names": ["ab", null, "c"]}),
        json!(false),
    );
    c.ok(
        r#"contains(['a', 'b'], 'a')"#,
        json!({"names": ["ab", null, "c"]}),
        json!([true, false]),
    );
    c.err(
        r#"contains('hello', 1)"#,
        json!({"names": ["ab", null, "c"]}),
    ); // TypeError
    c.err(r#"contains(5, 1)"#, json!({"names": ["ab", null, "c"]})); // TypeError
    c.ok(
        r#"contains('hello', None)"#,
        json!({"names": ["ab", null, "c"]}),
        json!(null),
    );
    c.ok(
        r#"contains(['ab', 'cd'], 'b')"#,
        json!({"names": ["ab", null, "c"]}),
        json!([true, false]),
    );
    c.ok(
        r#"contains([['a', 'b']], 'a')"#,
        json!({"names": ["ab", null, "c"]}),
        json!([true]),
    );
    c.ok(
        r#"contains(names, 'a')"#,
        json!({"names": ["ab", null, "c"]}),
        json!([true, null, false]),
    );
    c.err(r#"contains('a')"#, json!({"names": ["ab", null, "c"]})); // TypeError
    // --- substr
    c.ok(
        r#"substr('abcdef', 1, 3)"#,
        json!({"names": ["abcdef", null, "xy"]}),
        json!("bc"),
    );
    c.ok(
        r#"substr('abcdef', 2)"#,
        json!({"names": ["abcdef", null, "xy"]}),
        json!("cdef"),
    );
    c.ok(
        r#"substr('abcdef', -2)"#,
        json!({"names": ["abcdef", null, "xy"]}),
        json!("ef"),
    );
    c.ok(
        r#"substr('abcdef', 0, -1)"#,
        json!({"names": ["abcdef", null, "xy"]}),
        json!("abcde"),
    );
    c.ok(
        r#"substr('abcdef', -100, 100)"#,
        json!({"names": ["abcdef", null, "xy"]}),
        json!("abcdef"),
    );
    c.ok(
        r#"substr('abcdef', 4, 2)"#,
        json!({"names": ["abcdef", null, "xy"]}),
        json!(""),
    );
    c.ok(
        r#"substr('abcdef', 10)"#,
        json!({"names": ["abcdef", null, "xy"]}),
        json!(""),
    );
    c.ok(
        r#"substr('abcdef', 0, 0)"#,
        json!({"names": ["abcdef", null, "xy"]}),
        json!(""),
    );
    c.ok(
        r#"substr('héllo', 1, 3)"#,
        json!({"names": ["abcdef", null, "xy"]}),
        json!("él"),
    );
    c.ok(
        r#"substr('日本語', 1)"#,
        json!({"names": ["abcdef", null, "xy"]}),
        json!("本語"),
    );
    c.err(
        r#"substr('abc', 1.0)"#,
        json!({"names": ["abcdef", null, "xy"]}),
    ); // TypeError
    c.err(
        r#"substr('abc', 'a')"#,
        json!({"names": ["abcdef", null, "xy"]}),
    ); // TypeError
    c.err(
        r#"substr('abc', 0, 'a')"#,
        json!({"names": ["abcdef", null, "xy"]}),
    ); // TypeError
    c.ok(
        r#"substr('abc', True)"#,
        json!({"names": ["abcdef", null, "xy"]}),
        json!("bc"),
    );
    c.ok(
        r#"substr(['a', 'b', 'c'], 1)"#,
        json!({"names": ["abcdef", null, "xy"]}),
        json!(["", "", ""]),
    );
    c.ok(
        r#"substr([['abc', 'def']], 1)"#,
        json!({"names": ["abcdef", null, "xy"]}),
        json!([["def"]]),
    );
    c.err(r#"substr(5, 1)"#, json!({"names": ["abcdef", null, "xy"]})); // TypeError
    c.err(r#"substr('abc')"#, json!({"names": ["abcdef", null, "xy"]})); // TypeError
    c.err(
        r#"substr('abc', 0, 1, 2)"#,
        json!({"names": ["abcdef", null, "xy"]}),
    ); // TypeError
    c.ok(
        r#"substr('abc', None)"#,
        json!({"names": ["abcdef", null, "xy"]}),
        json!(null),
    );
    c.ok(
        r#"substr(None, 1)"#,
        json!({"names": ["abcdef", null, "xy"]}),
        json!(null),
    );
    c.ok(
        r#"substr(names, 1, 3)"#,
        json!({"names": ["abcdef", null, "xy"]}),
        json!(["bc", null, "y"]),
    );
    // --- floor
    c.ok(r#"floor(3.7)"#, json!({"names": [1.2, -1.2]}), json!(3));
    c.ok(r#"floor(-3.7)"#, json!({"names": [1.2, -1.2]}), json!(-4));
    c.ok(r#"floor(3.0)"#, json!({"names": [1.2, -1.2]}), json!(3));
    c.ok(r#"floor(-0.5)"#, json!({"names": [1.2, -1.2]}), json!(-1));
    c.ok(r#"floor(0.5)"#, json!({"names": [1.2, -1.2]}), json!(0));
    c.ok(r#"floor(0)"#, json!({"names": [1.2, -1.2]}), json!(0));
    c.ok(r#"floor(5)"#, json!({"names": [1.2, -1.2]}), json!(5));
    c.ok(r#"floor(True)"#, json!({"names": [1.2, -1.2]}), json!(1));
    c.err(r#"floor('3.5')"#, json!({"names": [1.2, -1.2]})); // TypeError
    c.ok(
        r#"floor(1e15)"#,
        json!({"names": [1.2, -1.2]}),
        json!(1000000000000000_i64),
    );
    c.ok(
        r#"floor(-1e15)"#,
        json!({"names": [1.2, -1.2]}),
        json!(-1000000000000000_i64),
    );
    c.ok(r#"floor(2.5)"#, json!({"names": [1.2, -1.2]}), json!(2));
    c.ok(r#"floor(-2.5)"#, json!({"names": [1.2, -1.2]}), json!(-3));
    c.err(r#"floor(1e19)"#, json!({"names": [1.2, -1.2]})); // Python returns a big integer
    c.err(r#"floor('a')"#, json!({"names": [1.2, -1.2]})); // TypeError
    c.ok(
        r#"floor([1.5, 2.5, None])"#,
        json!({"names": [1.2, -1.2]}),
        json!([1, 2, null]),
    );
    c.ok(
        r#"floor(names)"#,
        json!({"names": [1.2, -1.2]}),
        json!([1, -2]),
    );
    c.err(r#"floor()"#, json!({})); // TypeError
    c.err(r#"floor(1, 2)"#, json!({})); // TypeError
    // --- ceil
    c.ok(r#"ceil(3.7)"#, json!({"names": [1.2, -1.2]}), json!(4));
    c.ok(r#"ceil(-3.7)"#, json!({"names": [1.2, -1.2]}), json!(-3));
    c.ok(r#"ceil(3.0)"#, json!({"names": [1.2, -1.2]}), json!(3));
    c.ok(r#"ceil(-0.5)"#, json!({"names": [1.2, -1.2]}), json!(0));
    c.ok(r#"ceil(0.5)"#, json!({"names": [1.2, -1.2]}), json!(1));
    c.ok(r#"ceil(0)"#, json!({"names": [1.2, -1.2]}), json!(0));
    c.ok(r#"ceil(5)"#, json!({"names": [1.2, -1.2]}), json!(5));
    c.ok(r#"ceil(True)"#, json!({"names": [1.2, -1.2]}), json!(1));
    c.err(r#"ceil('3.5')"#, json!({"names": [1.2, -1.2]})); // TypeError
    c.ok(
        r#"ceil(1e15)"#,
        json!({"names": [1.2, -1.2]}),
        json!(1000000000000000_i64),
    );
    c.ok(
        r#"ceil(-1e15)"#,
        json!({"names": [1.2, -1.2]}),
        json!(-1000000000000000_i64),
    );
    c.ok(r#"ceil(2.5)"#, json!({"names": [1.2, -1.2]}), json!(3));
    c.ok(r#"ceil(-2.5)"#, json!({"names": [1.2, -1.2]}), json!(-2));
    c.err(r#"ceil(1e19)"#, json!({"names": [1.2, -1.2]})); // Python returns a big integer
    c.err(r#"ceil('a')"#, json!({"names": [1.2, -1.2]})); // TypeError
    c.ok(
        r#"ceil([1.5, 2.5, None])"#,
        json!({"names": [1.2, -1.2]}),
        json!([2, 3, null]),
    );
    c.ok(
        r#"ceil(names)"#,
        json!({"names": [1.2, -1.2]}),
        json!([2, -1]),
    );
    c.err(r#"ceil()"#, json!({})); // TypeError
    c.err(r#"ceil(1, 2)"#, json!({})); // TypeError
    // --- round
    c.ok(r#"round(2.5)"#, json!({}), json!(2));
    c.ok(r#"round(3.5)"#, json!({}), json!(4));
    c.ok(r#"round(-2.5)"#, json!({}), json!(-2));
    c.ok(r#"round(0.5)"#, json!({}), json!(0));
    c.ok(r#"round(1.5)"#, json!({}), json!(2));
    c.ok(r#"round(2.675)"#, json!({}), json!(3));
    c.ok(r#"round(1.005)"#, json!({}), json!(1));
    c.ok(r#"round(0.285)"#, json!({}), json!(0));
    c.ok(r#"round(5.015)"#, json!({}), json!(5));
    c.ok(r#"round(-0.4)"#, json!({}), json!(0));
    c.ok(r#"round(7)"#, json!({}), json!(7));
    c.ok(r#"round(True)"#, json!({}), json!(1));
    c.ok(r#"round(1e15)"#, json!({}), json!(1000000000000000_i64));
    c.err(r#"round(2.5e300)"#, json!({})); // Python returns a big integer
    c.err(r#"round('1.5')"#, json!({})); // TypeError
    c.ok(r#"round([1.5, 2.5])"#, json!({}), json!([2, 2]));
    c.ok(r#"round(2.675, 2)"#, json!({}), json!(2.67));
    c.ok(r#"round(1.005, 2)"#, json!({}), json!(1.0));
    c.ok(r#"round(0.285, 2)"#, json!({}), json!(0.28));
    c.ok(r#"round(5.015, 2)"#, json!({}), json!(5.01));
    c.ok(r#"round(2.5, 0)"#, json!({}), json!(2.0));
    c.ok(r#"round(3.5, 0)"#, json!({}), json!(4.0));
    c.ok(r#"round(-0.4, 0)"#, json!({}), json!(-0.0));
    c.ok(r#"round(1234.5678, 2)"#, json!({}), json!(1234.57));
    c.ok(r#"round(1234.5678, -2)"#, json!({}), json!(1200.0));
    c.ok(r#"round(1250.0, -2)"#, json!({}), json!(1200.0));
    c.ok(r#"round(1350.0, -2)"#, json!({}), json!(1400.0));
    c.ok(r#"round(-1250.0, -2)"#, json!({}), json!(-1200.0));
    c.ok(r#"round(0.5, -1)"#, json!({}), json!(0.0));
    c.ok(r#"round(5.0, -1)"#, json!({}), json!(0.0));
    c.ok(r#"round(15.0, -1)"#, json!({}), json!(20.0));
    c.ok(r#"round(25.0, -1)"#, json!({}), json!(20.0));
    c.ok(r#"round(25.000001, -1)"#, json!({}), json!(30.0));
    c.ok(r#"round(-4.0, -1)"#, json!({}), json!(-0.0));
    c.ok(r#"round(4.0, -1)"#, json!({}), json!(0.0));
    c.ok(r#"round(1e22, -5)"#, json!({}), json!(1e+22));
    c.ok(r#"round(1.5, 400)"#, json!({}), json!(1.5));
    c.ok(r#"round(1.5, -400)"#, json!({}), json!(0.0));
    c.ok(r#"round(0.1, 20)"#, json!({}), json!(0.1));
    c.ok(
        r#"round(123456789.123456789, 5)"#,
        json!({}),
        json!(123456789.12346),
    );
    c.ok(r#"round(1e300, -299)"#, json!({}), json!(1e+300));
    c.ok(
        r#"round(1.7976931348623157e308, -300)"#,
        json!({}),
        json!(1.79769313e+308),
    );
    c.ok(r#"round(15, -1)"#, json!({}), json!(20));
    c.ok(r#"round(25, -1)"#, json!({}), json!(20));
    c.ok(r#"round(35, -1)"#, json!({}), json!(40));
    c.ok(r#"round(-25, -1)"#, json!({}), json!(-20));
    c.ok(r#"round(1234, 2)"#, json!({}), json!(1234));
    c.ok(r#"round(1234, -2)"#, json!({}), json!(1200));
    c.ok(r#"round(1250, -2)"#, json!({}), json!(1200));
    c.ok(r#"round(1350, -2)"#, json!({}), json!(1400));
    c.ok(r#"round(5, -1)"#, json!({}), json!(0));
    c.ok(r#"round(4, -1)"#, json!({}), json!(0));
    c.ok(r#"round(True, 1)"#, json!({}), json!(1));
    c.ok(r#"round(True, -1)"#, json!({}), json!(0));
    c.ok(r#"round(99, -2)"#, json!({}), json!(100));
    c.ok(r#"round(-5, -1)"#, json!({}), json!(0));
    c.err(r#"round(6000000000000000000, -19)"#, json!({})); // Python returns a big integer
    c.ok(r#"round(5000000000000000000, -19)"#, json!({}), json!(0));
    c.ok(r#"round(7, -30)"#, json!({}), json!(0));
    c.ok(r#"round(3.14159, 3)"#, json!({}), json!(3.142));
    c.ok(r#"round(2.5, True)"#, json!({}), json!(2.5));
    c.err(r#"round(2.5, 1.0)"#, json!({})); // TypeError
    c.err(r#"round('2.5', 1)"#, json!({})); // TypeError
    c.ok(r#"round([1.234, 5.678], 1)"#, json!({}), json!([1.2, 5.7]));
    c.err(r#"round()"#, json!({})); // TypeError
    c.err(r#"round(1, 2, 3)"#, json!({})); // TypeError
    c.ok(r#"round(1.5, None)"#, json!({}), json!(null));
    c.ok(r#"round(None, 1)"#, json!({}), json!(null));
    c.ok(r#"round(1.2345, vals)"#, json!({"vals": 1}), json!(1.2));
    // --- int
    c.ok(r#"int('42')"#, json!({}), json!(42));
    c.ok(r#"int(' 42 ')"#, json!({}), json!(42));
    c.ok(r#"int('-7')"#, json!({}), json!(-7));
    c.ok(r#"int('+7')"#, json!({}), json!(7));
    c.ok(r#"int('1_000')"#, json!({}), json!(1000));
    c.err(r#"int('1__0')"#, json!({})); // ValueError
    c.err(r#"int('_1')"#, json!({})); // ValueError
    c.err(r#"int('1_')"#, json!({})); // ValueError
    c.err(r#"int('4.2')"#, json!({})); // ValueError
    c.err(r#"int('')"#, json!({})); // ValueError
    c.err(r#"int('abc')"#, json!({})); // ValueError
    c.ok(r#"int(3.9)"#, json!({}), json!(3));
    c.ok(r#"int(-3.9)"#, json!({}), json!(-3));
    c.ok(r#"int(True)"#, json!({}), json!(1));
    c.ok(r#"int(None)"#, json!({}), json!(null));
    c.ok(r#"int([1])"#, json!({}), json!([1]));
    c.err(r#"int('0x10')"#, json!({})); // ValueError
    c.ok(r#"int('１２')"#, json!({}), json!(12));
    c.err(r#"int(1e19)"#, json!({})); // Python returns a big integer
    c.err(r#"int('99999999999999999999')"#, json!({})); // Python returns a big integer
    c.ok(r#"int([' 1', '2', 3.7])"#, json!({}), json!([1, 2, 3]));
    c.ok(r#"int('ff', 16)"#, json!({}), json!(255));
    c.ok(r#"int('0xff', 16)"#, json!({}), json!(255));
    c.ok(r#"int('FF', 16)"#, json!({}), json!(255));
    c.ok(r#"int('0xff', 0)"#, json!({}), json!(255));
    c.ok(r#"int('0b101', 0)"#, json!({}), json!(5));
    c.ok(r#"int('0o17', 0)"#, json!({}), json!(15));
    c.err(r#"int('017', 0)"#, json!({})); // ValueError
    c.ok(r#"int('0', 0)"#, json!({}), json!(0));
    c.ok(r#"int('00', 0)"#, json!({}), json!(0));
    c.ok(r#"int('101', 2)"#, json!({}), json!(5));
    c.ok(r#"int('z', 36)"#, json!({}), json!(35));
    c.err(r#"int('z', 10)"#, json!({})); // ValueError
    c.err(r#"int('12', 1)"#, json!({})); // ValueError
    c.err(r#"int('12', 37)"#, json!({})); // ValueError
    c.err(r#"int(12, 10)"#, json!({})); // TypeError
    c.ok(r#"int('0x_f', 16)"#, json!({}), json!(15));
    c.err(r#"int('0x', 16)"#, json!({})); // ValueError
    c.ok(r#"int('-0xf', 16)"#, json!({}), json!(-15));
    c.ok(r#"int(' 7 ', 8)"#, json!({}), json!(7));
    c.err(r#"int('8', 8)"#, json!({})); // ValueError
    c.ok(r#"int('1_0', 0)"#, json!({}), json!(10));
    c.ok(r#"int('1_0', 2)"#, json!({}), json!(2));
    c.err(r#"int('0_7', 0)"#, json!({})); // ValueError
    c.err(r#"int('10', 1.0)"#, json!({})); // TypeError
    c.ok(r#"int()"#, json!({}), json!(0));
    c.err(r#"int(1, 2, 3)"#, json!({})); // TypeError
    // --- float
    c.ok(r#"float()"#, json!({}), json!(0.0));
    c.ok(r#"float('3.14')"#, json!({}), json!(3.14));
    c.ok(r#"float(' 3.14 ')"#, json!({}), json!(3.14));
    c.ok(r#"float('1e3')"#, json!({}), json!(1000.0));
    c.ok(r#"float('1_000.5')"#, json!({}), json!(1000.5));
    c.err(r#"float('1__0')"#, json!({})); // ValueError
    c.err(r#"float('_1')"#, json!({})); // ValueError
    c.err(r#"float('1_')"#, json!({})); // ValueError
    c.err(r#"float('1_e5')"#, json!({})); // ValueError
    c.ok(r#"float('+.5')"#, json!({}), json!(0.5));
    c.ok(r#"float('5.')"#, json!({}), json!(5.0));
    c.err(r#"float('.')"#, json!({})); // ValueError
    c.err(r#"float('')"#, json!({})); // ValueError
    c.err(r#"float('abc')"#, json!({})); // ValueError
    c.err(r#"float('0x10')"#, json!({})); // ValueError
    c.err(r#"float('1e')"#, json!({})); // ValueError
    c.ok(r#"float(3)"#, json!({}), json!(3.0));
    c.ok(r#"float(True)"#, json!({}), json!(1.0));
    c.ok(r#"float(None)"#, json!({}), json!(null));
    c.ok(r#"float([1])"#, json!({}), json!([1.0]));
    c.err(r#"float('1,5')"#, json!({})); // ValueError
    // --- str
    c.ok(r#"str()"#, json!({}), json!(""));
    c.err(r#"str(1, 2)"#, json!({})); // TypeError
    c.ok(r#"str(1)"#, json!({}), json!("1"));
    c.ok(r#"str(1.5)"#, json!({}), json!("1.5"));
    c.ok(r#"str(True)"#, json!({}), json!("True"));
    c.ok(r#"str(None)"#, json!({}), json!(null));
    c.ok(r#"str('a')"#, json!({}), json!("a"));
    c.ok(r#"str([1, None, 'a'])"#, json!({}), json!(["1", null, "a"]));
    c.ok(r#"str([1, [2]])"#, json!({}), json!(["1", "[2]"]));
    // --- bool
    c.ok(r#"bool()"#, json!({}), json!(false));
    c.err(r#"bool(1, 2)"#, json!({})); // TypeError
    c.ok(r#"bool(0)"#, json!({}), json!(false));
    c.ok(r#"bool(1)"#, json!({}), json!(true));
    c.ok(r#"bool('')"#, json!({}), json!(false));
    c.ok(r#"bool('a')"#, json!({}), json!(true));
    c.ok(r#"bool([])"#, json!({}), json!([]));
    c.ok(r#"bool([0])"#, json!({}), json!([false]));
    c.ok(r#"bool(None)"#, json!({}), json!(null));
    c.ok(r#"bool(0.0)"#, json!({}), json!(false));
    c.ok(r#"bool('0')"#, json!({}), json!(true));
    c.ok(r#"bool(True)"#, json!({}), json!(true));
    c.ok(r#"bool([[]])"#, json!({}), json!([false]));
    // --- abs
    c.ok(r#"abs(-5)"#, json!({}), json!(5));
    c.ok(r#"abs(5)"#, json!({}), json!(5));
    c.ok(r#"abs(-5.5)"#, json!({}), json!(5.5));
    c.ok(r#"abs(True)"#, json!({}), json!(1));
    c.err(r#"abs('a')"#, json!({})); // TypeError
    c.ok(r#"abs([-1, 2.5, True])"#, json!({}), json!([1, 2.5, 1]));
    c.ok(r#"abs(None)"#, json!({}), json!(null));
    c.ok(r#"abs([None, -1])"#, json!({}), json!([null, 1]));
    c.ok(
        r#"abs(-9223372036854775807)"#,
        json!({}),
        json!(9223372036854775807_i64),
    );
    c.err(r#"abs()"#, json!({})); // TypeError
    c.err(r#"abs(1, 2)"#, json!({})); // TypeError
    // --- len
    c.ok(
        r#"len('héllo')"#,
        json!({"names": ["a", null, "bcd"]}),
        json!(5),
    );
    c.ok(
        r#"len([1, 2])"#,
        json!({"names": ["a", null, "bcd"]}),
        json!(2),
    );
    c.ok(r#"len([])"#, json!({"names": ["a", null, "bcd"]}), json!(0));
    c.ok(r#"len('')"#, json!({"names": ["a", null, "bcd"]}), json!(0));
    c.err(r#"len(5)"#, json!({"names": ["a", null, "bcd"]})); // TypeError
    c.err(r#"len(True)"#, json!({"names": ["a", null, "bcd"]})); // TypeError
    c.ok(
        r#"len([[1, 2], 'ab'])"#,
        json!({"names": ["a", null, "bcd"]}),
        json!(2),
    );
    c.ok(
        r#"len(None)"#,
        json!({"names": ["a", null, "bcd"]}),
        json!(null),
    );
    c.ok(
        r#"len(names)"#,
        json!({"names": ["a", null, "bcd"]}),
        json!(3),
    );
    c.err(r#"len()"#, json!({})); // TypeError
    c.err(r#"len('a', 'b')"#, json!({})); // TypeError
    c.ok(r#"len(None, 'b')"#, json!({}), json!(null));
    // --- strlen
    c.ok(
        r#"strlen('héllo')"#,
        json!({"names": ["a", null, "bcd"]}),
        json!(5),
    );
    c.err(r#"strlen([1, 2])"#, json!({"names": ["a", null, "bcd"]})); // TypeError
    c.ok(
        r#"strlen([])"#,
        json!({"names": ["a", null, "bcd"]}),
        json!([]),
    );
    c.ok(
        r#"strlen('')"#,
        json!({"names": ["a", null, "bcd"]}),
        json!(0),
    );
    c.err(r#"strlen(5)"#, json!({"names": ["a", null, "bcd"]})); // TypeError
    c.err(r#"strlen(True)"#, json!({"names": ["a", null, "bcd"]})); // TypeError
    c.ok(
        r#"strlen([[1, 2], 'ab'])"#,
        json!({"names": ["a", null, "bcd"]}),
        json!([2, 2]),
    );
    c.ok(
        r#"strlen(None)"#,
        json!({"names": ["a", null, "bcd"]}),
        json!(null),
    );
    c.ok(
        r#"strlen(names)"#,
        json!({"names": ["a", null, "bcd"]}),
        json!([1, null, 3]),
    );
    c.err(r#"strlen()"#, json!({})); // TypeError
    c.err(r#"strlen('a', 'b')"#, json!({})); // TypeError
    c.ok(r#"strlen(None, 'b')"#, json!({}), json!(null));
    // --- minmax
    c.ok(r#"max([1, 5, 2])"#, json!({}), json!(5));
    c.ok(r#"max([1, 5.0, 5])"#, json!({}), json!(5.0));
    c.ok(r#"max([5, 5.0])"#, json!({}), json!(5));
    c.ok(r#"max([5.0, 5])"#, json!({}), json!(5.0));
    c.ok(r#"max([1.5, 2])"#, json!({}), json!(2));
    c.ok(r#"max([True, 0])"#, json!({}), json!(true));
    c.ok(r#"max(['a', 'b', 'B'])"#, json!({}), json!("b"));
    c.ok(r#"max([[1, 2], [1, 3]])"#, json!({}), json!([1, 3]));
    c.ok(r#"max([[1, 2], [1]])"#, json!({}), json!([1, 2]));
    c.err(r#"max([])"#, json!({})); // ValueError
    c.err(r#"max([1, 'a'])"#, json!({})); // TypeError
    c.err(r#"max(['10', 9])"#, json!({})); // TypeError
    c.err(r#"max([1, None])"#, json!({})); // TypeError
    c.ok(r#"max([None])"#, json!({}), json!(null));
    c.err(r#"max(5)"#, json!({})); // TypeError
    c.ok(r#"max('abc')"#, json!({}), json!("c"));
    c.ok(r#"max(1, 2)"#, json!({}), json!(2));
    c.ok(r#"max(1, 5, 3)"#, json!({}), json!(5));
    c.err(r#"max(1, 'a')"#, json!({})); // TypeError
    c.err(r#"max()"#, json!({})); // TypeError
    c.err(r#"max([1], 2)"#, json!({})); // TypeError
    c.ok(r#"max(None)"#, json!({}), json!(null));
    c.ok(r#"max(1, None)"#, json!({}), json!(null));
    c.ok(r#"min([3, 1, 2])"#, json!({}), json!(1));
    c.ok(r#"min([1, 1.0])"#, json!({}), json!(1));
    c.ok(r#"min([1.0, 1])"#, json!({}), json!(1.0));
    c.ok(r#"min(['b', 'a'])"#, json!({}), json!("a"));
    c.err(r#"min([])"#, json!({})); // ValueError
    c.ok(r#"min(2, 1)"#, json!({}), json!(1));
    c.ok(r#"min([[2], [1, 5]])"#, json!({}), json!([1, 5]));
    c.err(r#"min([[1, 'a'], [1, 2]])"#, json!({})); // TypeError
    c.ok(r#"min('hello')"#, json!({}), json!("e"));
    c.ok(
        r#"max([9223372036854775807, 9.3e18])"#,
        json!({}),
        json!(9.3e+18),
    );
    // `2**63` is a big integer in Python (so `2**63 - 1` is the largest i64); here it
    // is an error rather than a wrapped number.
    c.err(r#"max([2**63 - 1, 9.3e18])"#, json!({}));
    c.ok(
        r#"min([2**62, 4.611686018427388e18])"#,
        json!({}),
        json!(4611686018427387904_i64),
    );
    c.ok(
        r#"max([9007199254740993, 9007199254740992.0])"#,
        json!({}),
        json!(9007199254740993_i64),
    );
    c.ok(
        r#"min([9007199254740993, 9007199254740992.0])"#,
        json!({}),
        json!(9007199254740992.0),
    );
    // --- sum
    c.ok(
        r#"sum([1, 2, 3])"#,
        json!({"names": [1.5, 2.5, 3.0]}),
        json!(6),
    );
    c.ok(r#"sum([])"#, json!({"names": [1.5, 2.5, 3.0]}), json!(0));
    c.ok(
        r#"sum([1.5, 2])"#,
        json!({"names": [1.5, 2.5, 3.0]}),
        json!(3.5),
    );
    c.ok(
        r#"sum([0.1] * 10)"#,
        json!({"names": [1.5, 2.5, 3.0]}),
        json!(1.0),
    );
    c.ok(
        r#"sum([0.1, 0.2, 0.3])"#,
        json!({"names": [1.5, 2.5, 3.0]}),
        json!(0.6),
    );
    c.ok(
        r#"sum([1, 2.5, 3])"#,
        json!({"names": [1.5, 2.5, 3.0]}),
        json!(6.5),
    );
    c.ok(
        r#"sum([True, True])"#,
        json!({"names": [1.5, 2.5, 3.0]}),
        json!(2),
    );
    c.ok(
        r#"sum([True, 0.5])"#,
        json!({"names": [1.5, 2.5, 3.0]}),
        json!(1.5),
    );
    c.ok(
        r#"sum([1e100, 1.0, -1e100])"#,
        json!({"names": [1.5, 2.5, 3.0]}),
        json!(1.0),
    );
    c.ok(
        r#"sum([1e100, 1, -1e100])"#,
        json!({"names": [1.5, 2.5, 3.0]}),
        json!(0.0),
    );
    c.ok(
        r#"sum([1, 1e100, 1, -1e100])"#,
        json!({"names": [1.5, 2.5, 3.0]}),
        json!(0.0),
    );
    c.ok(
        r#"sum([0.1, 0.2, 0.3, True, 0.4])"#,
        json!({"names": [1.5, 2.5, 3.0]}),
        json!(2.0),
    );
    c.err(r#"sum(['a', 'b'])"#, json!({"names": [1.5, 2.5, 3.0]})); // TypeError
    c.err(r#"sum([[1], [2]])"#, json!({"names": [1.5, 2.5, 3.0]})); // TypeError
    c.ok(
        r#"sum([[1], [2]], [])"#,
        json!({"names": [1.5, 2.5, 3.0]}),
        json!([1, 2]),
    );
    c.ok(
        r#"sum([1, 2], 10)"#,
        json!({"names": [1.5, 2.5, 3.0]}),
        json!(13),
    );
    c.ok(
        r#"sum([1, 2], 0.5)"#,
        json!({"names": [1.5, 2.5, 3.0]}),
        json!(3.5),
    );
    c.ok(
        r#"sum([0.1, 0.2], 0.3)"#,
        json!({"names": [1.5, 2.5, 3.0]}),
        json!(0.6),
    );
    c.err(r#"sum([1, 'a'])"#, json!({"names": [1.5, 2.5, 3.0]})); // TypeError
    c.err(r#"sum([1, None])"#, json!({"names": [1.5, 2.5, 3.0]})); // TypeError
    c.err(r#"sum(5)"#, json!({"names": [1.5, 2.5, 3.0]})); // TypeError
    c.err(r#"sum('abc')"#, json!({"names": [1.5, 2.5, 3.0]})); // TypeError
    c.err(r#"sum(['a'], 'x')"#, json!({"names": [1.5, 2.5, 3.0]})); // TypeError
    c.err(r#"sum([1], [1])"#, json!({"names": [1.5, 2.5, 3.0]})); // TypeError
    c.err(r#"sum([[1]], 0)"#, json!({"names": [1.5, 2.5, 3.0]})); // TypeError
    c.err(r#"sum([1.5, 'a'])"#, json!({"names": [1.5, 2.5, 3.0]})); // TypeError
    c.err(
        r#"sum([9223372036854775807, 1])"#,
        json!({"names": [1.5, 2.5, 3.0]}),
    ); // Python returns a big integer
    c.ok(
        r#"sum([9223372036854775807, 1.0])"#,
        json!({"names": [1.5, 2.5, 3.0]}),
        json!(9.223372036854776e+18),
    );
    c.ok(
        r#"sum([1, 2], None)"#,
        json!({"names": [1.5, 2.5, 3.0]}),
        json!(null),
    );
    c.err(r#"sum()"#, json!({"names": [1.5, 2.5, 3.0]})); // TypeError
    c.err(r#"sum([1], 2, 3)"#, json!({"names": [1.5, 2.5, 3.0]})); // TypeError
    c.ok(
        r#"sum([0.5, 0.25, 1])"#,
        json!({"names": [1.5, 2.5, 3.0]}),
        json!(1.75),
    );
    c.ok(
        r#"sum([1, 2**40])"#,
        json!({"names": [1.5, 2.5, 3.0]}),
        json!(1099511627777_i64),
    );
    c.ok(
        r#"sum([2.5, True, 1])"#,
        json!({"names": [1.5, 2.5, 3.0]}),
        json!(4.5),
    );
    c.ok(
        r#"sum(names)"#,
        json!({"names": [1.5, 2.5, 3.0]}),
        json!(7.0),
    );
    c.ok(
        r#"sum([[1, 2], [3]][0])"#,
        json!({"names": [1.5, 2.5, 3.0]}),
        json!(3),
    );
    // --- sorted
    c.ok(
        r#"sorted([3, 1, 2])"#,
        json!({"names": ["b", "a"]}),
        json!([1, 2, 3]),
    );
    c.ok(r#"sorted([])"#, json!({"names": ["b", "a"]}), json!([]));
    c.ok(r#"sorted([1])"#, json!({"names": ["b", "a"]}), json!([1]));
    c.ok(
        r#"sorted(['b', 'a', 'B'])"#,
        json!({"names": ["b", "a"]}),
        json!(["B", "a", "b"]),
    );
    c.ok(
        r#"sorted([1, 2.5, True, 0])"#,
        json!({"names": ["b", "a"]}),
        json!([0, 1, true, 2.5]),
    );
    c.ok(
        r#"sorted([3, 1.5, 2])"#,
        json!({"names": ["b", "a"]}),
        json!([1.5, 2, 3]),
    );
    c.ok(
        r#"sorted([[2, 1], [1, 5], [1]])"#,
        json!({"names": ["b", "a"]}),
        json!([[1], [1, 5], [2, 1]]),
    );
    c.err(r#"sorted([1, 'a'])"#, json!({"names": ["b", "a"]})); // TypeError
    c.ok(
        r#"sorted([None])"#,
        json!({"names": ["b", "a"]}),
        json!([null]),
    );
    c.err(r#"sorted([None, None])"#, json!({"names": ["b", "a"]})); // TypeError
    c.err(r#"sorted([None, 1])"#, json!({"names": ["b", "a"]})); // TypeError
    c.ok(
        r#"sorted('cab')"#,
        json!({"names": ["b", "a"]}),
        json!(["a", "b", "c"]),
    );
    c.err(r#"sorted(5)"#, json!({"names": ["b", "a"]})); // TypeError
    c.err(r#"sorted([1, 2], [3])"#, json!({"names": ["b", "a"]})); // TypeError
    c.err(r#"sorted()"#, json!({"names": ["b", "a"]})); // TypeError
    c.ok(
        r#"sorted([2.0, 2, 1, 1.0])"#,
        json!({"names": ["b", "a"]}),
        json!([1, 1.0, 2.0, 2]),
    );
    c.ok(
        r#"sorted(['é', 'e', 'f', 'z'])"#,
        json!({"names": ["b", "a"]}),
        json!(["e", "f", "z", "é"]),
    );
    c.err(r#"sorted([3, 'a', 1])"#, json!({"names": ["b", "a"]})); // TypeError
    c.err(r#"sorted([['a'], [1]])"#, json!({"names": ["b", "a"]})); // TypeError
    c.err(r#"sorted([1, [2]])"#, json!({"names": ["b", "a"]})); // TypeError
    c.ok(
        r#"sorted(names)"#,
        json!({"names": ["b", "a"]}),
        json!(["a", "b"]),
    );
    // --- anyall
    c.ok(r#"any([0, '', None])"#, json!({}), json!(false));
    c.ok(r#"any([0, 'x'])"#, json!({}), json!(true));
    c.ok(r#"any([])"#, json!({}), json!(false));
    c.ok(r#"all([])"#, json!({}), json!(true));
    c.ok(r#"all([1, 'a'])"#, json!({}), json!(true));
    c.ok(r#"all([1, 0])"#, json!({}), json!(false));
    c.ok(r#"any('')"#, json!({}), json!(false));
    c.ok(r#"all('ab')"#, json!({}), json!(true));
    c.err(r#"any(5)"#, json!({})); // TypeError
    c.ok(r#"all(None)"#, json!({}), json!(null));
    c.ok(r#"any([[], [0]])"#, json!({}), json!(true));
    c.ok(r#"all([[1], [0]])"#, json!({}), json!(true));
    c.err(r#"any(1, 2)"#, json!({})); // TypeError
    c.err(r#"any()"#, json!({})); // TypeError
    c.ok(r#"any([0.0, 0])"#, json!({}), json!(false));
    c.ok(r#"all([0.1, True])"#, json!({}), json!(true));
    // --- reversed
    c.ok(
        r#"reversed([1, 2, 3])"#,
        json!({"names": [1, "a"]}),
        json!([3, 2, 1]),
    );
    c.ok(r#"reversed([])"#, json!({"names": [1, "a"]}), json!([]));
    c.ok(
        r#"reversed('abc')"#,
        json!({"names": [1, "a"]}),
        json!(["c", "b", "a"]),
    );
    c.err(r#"reversed(5)"#, json!({"names": [1, "a"]})); // TypeError
    c.ok(
        r#"reversed([[1, 2], [3]])"#,
        json!({"names": [1, "a"]}),
        json!([[3], [1, 2]]),
    );
    c.err(r#"reversed()"#, json!({"names": [1, "a"]})); // TypeError
    c.err(r#"reversed([1], 2)"#, json!({"names": [1, "a"]})); // TypeError
    c.ok(r#"reversed(None)"#, json!({"names": [1, "a"]}), json!(null));
    c.ok(
        r#"reversed(names)"#,
        json!({"names": [1, "a"]}),
        json!(["a", 1]),
    );
    // --- first
    c.ok(
        r#"first([10, 20, 30])"#,
        json!({"names": [1, 2]}),
        json!(10),
    );
    c.ok(r#"first([])"#, json!({"names": [1, 2]}), json!(null));
    c.ok(r#"first('abc')"#, json!({"names": [1, 2]}), json!("a"));
    c.ok(r#"first('')"#, json!({"names": [1, 2]}), json!(null));
    c.ok(r#"first(0)"#, json!({"names": [1, 2]}), json!(null));
    c.err(r#"first(5)"#, json!({"names": [1, 2]})); // TypeError
    c.ok(r#"first(0.0)"#, json!({"names": [1, 2]}), json!(null));
    c.err(r#"first(1.5)"#, json!({"names": [1, 2]})); // TypeError
    c.ok(r#"first(False)"#, json!({"names": [1, 2]}), json!(null));
    c.err(r#"first(True)"#, json!({"names": [1, 2]})); // TypeError
    c.ok(r#"first(None)"#, json!({"names": [1, 2]}), json!(null));
    c.ok(r#"first([None])"#, json!({"names": [1, 2]}), json!(null));
    c.ok(r#"first([[1], 2])"#, json!({"names": [1, 2]}), json!([1]));
    c.ok(r#"first(names)"#, json!({"names": [1, 2]}), json!(1));
    c.ok(r#"first([0])"#, json!({"names": [1, 2]}), json!(0));
    c.err(r#"first()"#, json!({})); // TypeError
    c.err(r#"first([1], [2])"#, json!({})); // TypeError
    // --- last
    c.ok(r#"last([10, 20, 30])"#, json!({"names": [1, 2]}), json!(30));
    c.ok(r#"last([])"#, json!({"names": [1, 2]}), json!(null));
    c.ok(r#"last('abc')"#, json!({"names": [1, 2]}), json!("c"));
    c.ok(r#"last('')"#, json!({"names": [1, 2]}), json!(null));
    c.ok(r#"last(0)"#, json!({"names": [1, 2]}), json!(null));
    c.err(r#"last(5)"#, json!({"names": [1, 2]})); // TypeError
    c.ok(r#"last(0.0)"#, json!({"names": [1, 2]}), json!(null));
    c.err(r#"last(1.5)"#, json!({"names": [1, 2]})); // TypeError
    c.ok(r#"last(False)"#, json!({"names": [1, 2]}), json!(null));
    c.err(r#"last(True)"#, json!({"names": [1, 2]})); // TypeError
    c.ok(r#"last(None)"#, json!({"names": [1, 2]}), json!(null));
    c.ok(r#"last([None])"#, json!({"names": [1, 2]}), json!(null));
    c.ok(r#"last([[1], 2])"#, json!({"names": [1, 2]}), json!(2));
    c.ok(r#"last(names)"#, json!({"names": [1, 2]}), json!(2));
    c.ok(r#"last([0])"#, json!({"names": [1, 2]}), json!(0));
    c.err(r#"last()"#, json!({})); // TypeError
    c.err(r#"last([1], [2])"#, json!({})); // TypeError
    // --- coalesce
    c.ok(r#"coalesce()"#, json!({"x": null, "y": null}), json!(null));
    c.ok(
        r#"coalesce(None)"#,
        json!({"x": null, "y": null}),
        json!(null),
    );
    c.ok(
        r#"coalesce(None, 0)"#,
        json!({"x": null, "y": null}),
        json!(0),
    );
    c.ok(
        r#"coalesce('', 1)"#,
        json!({"x": null, "y": null}),
        json!(""),
    );
    c.ok(
        r#"coalesce(None, None, [])"#,
        json!({"x": null, "y": null}),
        json!([]),
    );
    c.ok(
        r#"coalesce(1, 2, 3)"#,
        json!({"x": null, "y": null}),
        json!(1),
    );
    c.ok(
        r#"coalesce(x, y, 'z')"#,
        json!({"x": null, "y": null}),
        json!("z"),
    );
    c.ok(
        r#"coalesce(False, 1)"#,
        json!({"x": null, "y": null}),
        json!(false),
    );
    // --- is_numeric
    c.ok(r#"is_numeric('3.14')"#, json!({}), json!(true));
    c.ok(r#"is_numeric('abc')"#, json!({}), json!(false));
    c.ok(r#"is_numeric(5)"#, json!({}), json!(true));
    c.ok(r#"is_numeric('')"#, json!({}), json!(false));
    c.ok(r#"is_numeric(None)"#, json!({}), json!(false));
    c.ok(r#"is_numeric(True)"#, json!({}), json!(false));
    c.ok(r#"is_numeric(False)"#, json!({}), json!(false));
    c.ok(r#"is_numeric('0')"#, json!({}), json!(true));
    c.ok(r#"is_numeric(' 7 ')"#, json!({}), json!(true));
    c.ok(r#"is_numeric('1e5')"#, json!({}), json!(true));
    c.ok(r#"is_numeric('1_0')"#, json!({}), json!(true));
    c.ok(r#"is_numeric('inf')"#, json!({}), json!(true));
    c.ok(r#"is_numeric('nan')"#, json!({}), json!(true));
    c.ok(r#"is_numeric(1.5)"#, json!({}), json!(true));
    c.ok(r#"is_numeric([1])"#, json!({}), json!(false));
    c.ok(r#"is_numeric('1,5')"#, json!({}), json!(false));
    c.ok(r#"is_numeric('.5')"#, json!({}), json!(true));
    c.ok(r#"is_numeric('+5')"#, json!({}), json!(true));
    c.ok(r#"is_numeric('--5')"#, json!({}), json!(false));
    c.ok(r#"is_numeric('5 5')"#, json!({}), json!(false));
    c.err(r#"is_numeric()"#, json!({})); // TypeError
    c.err(r#"is_numeric(1, 2)"#, json!({})); // TypeError
    // --- is_str
    c.ok(r#"is_str('a')"#, json!({"names": ["a"]}), json!(true));
    c.ok(r#"is_str(1)"#, json!({"names": ["a"]}), json!(false));
    c.ok(r#"is_str(1.5)"#, json!({"names": ["a"]}), json!(false));
    c.ok(r#"is_str(True)"#, json!({"names": ["a"]}), json!(false));
    c.ok(r#"is_str(None)"#, json!({"names": ["a"]}), json!(false));
    c.ok(r#"is_str([1])"#, json!({"names": ["a"]}), json!(false));
    c.ok(r#"is_str([])"#, json!({"names": ["a"]}), json!(false));
    c.ok(r#"is_str('')"#, json!({"names": ["a"]}), json!(true));
    c.ok(r#"is_str(0)"#, json!({"names": ["a"]}), json!(false));
    c.ok(r#"is_str(False)"#, json!({"names": ["a"]}), json!(false));
    c.ok(r#"is_str(names)"#, json!({"names": ["a"]}), json!(false));
    c.err(r#"is_str()"#, json!({})); // TypeError
    c.err(r#"is_str(1, 2)"#, json!({})); // TypeError
    // --- is_int
    c.ok(r#"is_int('a')"#, json!({"names": ["a"]}), json!(false));
    c.ok(r#"is_int(1)"#, json!({"names": ["a"]}), json!(true));
    c.ok(r#"is_int(1.5)"#, json!({"names": ["a"]}), json!(false));
    c.ok(r#"is_int(True)"#, json!({"names": ["a"]}), json!(false));
    c.ok(r#"is_int(None)"#, json!({"names": ["a"]}), json!(false));
    c.ok(r#"is_int([1])"#, json!({"names": ["a"]}), json!(false));
    c.ok(r#"is_int([])"#, json!({"names": ["a"]}), json!(false));
    c.ok(r#"is_int('')"#, json!({"names": ["a"]}), json!(false));
    c.ok(r#"is_int(0)"#, json!({"names": ["a"]}), json!(true));
    c.ok(r#"is_int(False)"#, json!({"names": ["a"]}), json!(false));
    c.ok(r#"is_int(names)"#, json!({"names": ["a"]}), json!(false));
    c.err(r#"is_int()"#, json!({})); // TypeError
    c.err(r#"is_int(1, 2)"#, json!({})); // TypeError
    // --- is_float
    c.ok(r#"is_float('a')"#, json!({"names": ["a"]}), json!(false));
    c.ok(r#"is_float(1)"#, json!({"names": ["a"]}), json!(false));
    c.ok(r#"is_float(1.5)"#, json!({"names": ["a"]}), json!(true));
    c.ok(r#"is_float(True)"#, json!({"names": ["a"]}), json!(false));
    c.ok(r#"is_float(None)"#, json!({"names": ["a"]}), json!(false));
    c.ok(r#"is_float([1])"#, json!({"names": ["a"]}), json!(false));
    c.ok(r#"is_float([])"#, json!({"names": ["a"]}), json!(false));
    c.ok(r#"is_float('')"#, json!({"names": ["a"]}), json!(false));
    c.ok(r#"is_float(0)"#, json!({"names": ["a"]}), json!(false));
    c.ok(r#"is_float(False)"#, json!({"names": ["a"]}), json!(false));
    c.ok(r#"is_float(names)"#, json!({"names": ["a"]}), json!(false));
    c.err(r#"is_float()"#, json!({})); // TypeError
    c.err(r#"is_float(1, 2)"#, json!({})); // TypeError
    // --- is_bool
    c.ok(r#"is_bool('a')"#, json!({"names": ["a"]}), json!(false));
    c.ok(r#"is_bool(1)"#, json!({"names": ["a"]}), json!(false));
    c.ok(r#"is_bool(1.5)"#, json!({"names": ["a"]}), json!(false));
    c.ok(r#"is_bool(True)"#, json!({"names": ["a"]}), json!(true));
    c.ok(r#"is_bool(None)"#, json!({"names": ["a"]}), json!(false));
    c.ok(r#"is_bool([1])"#, json!({"names": ["a"]}), json!(false));
    c.ok(r#"is_bool([])"#, json!({"names": ["a"]}), json!(false));
    c.ok(r#"is_bool('')"#, json!({"names": ["a"]}), json!(false));
    c.ok(r#"is_bool(0)"#, json!({"names": ["a"]}), json!(false));
    c.ok(r#"is_bool(False)"#, json!({"names": ["a"]}), json!(true));
    c.ok(r#"is_bool(names)"#, json!({"names": ["a"]}), json!(false));
    c.err(r#"is_bool()"#, json!({})); // TypeError
    c.err(r#"is_bool(1, 2)"#, json!({})); // TypeError
    // --- is_list
    c.ok(r#"is_list('a')"#, json!({"names": ["a"]}), json!(false));
    c.ok(r#"is_list(1)"#, json!({"names": ["a"]}), json!(false));
    c.ok(r#"is_list(1.5)"#, json!({"names": ["a"]}), json!(false));
    c.ok(r#"is_list(True)"#, json!({"names": ["a"]}), json!(false));
    c.ok(r#"is_list(None)"#, json!({"names": ["a"]}), json!(false));
    c.ok(r#"is_list([1])"#, json!({"names": ["a"]}), json!(true));
    c.ok(r#"is_list([])"#, json!({"names": ["a"]}), json!(true));
    c.ok(r#"is_list('')"#, json!({"names": ["a"]}), json!(false));
    c.ok(r#"is_list(0)"#, json!({"names": ["a"]}), json!(false));
    c.ok(r#"is_list(False)"#, json!({"names": ["a"]}), json!(false));
    c.ok(r#"is_list(names)"#, json!({"names": ["a"]}), json!(true));
    c.err(r#"is_list()"#, json!({})); // TypeError
    c.err(r#"is_list(1, 2)"#, json!({})); // TypeError
    // --- uuid5
    c.ok(
        r#"uuid5('https://example.org/ns', 'example')"#,
        json!({}),
        json!("579c5560-7d2f-5b42-aa37-f82032facbea"),
    );
    c.ok(
        r#"uuid5('', '')"#,
        json!({}),
        json!("99454d6a-8efb-576b-96f4-fae2f0a37cde"),
    );
    c.err(r#"uuid5('ns', 5)"#, json!({})); // TypeError
    c.err(r#"uuid5(5, 'a')"#, json!({})); // TypeError
    c.err(r#"uuid5('ns')"#, json!({})); // TypeError
    c.ok(
        r#"uuid5('ns', 'é')"#,
        json!({}),
        json!("3bc58cdf-b086-52d0-be18-fafe883d40bc"),
    );
    c.ok(
        r#"uuid5(['a', 'b'], 'x')"#,
        json!({}),
        json!([
            "6154a907-ce7b-5809-a51c-fc42073ada9f",
            "047fda66-7dd8-592c-9e76-ab827b06ba6c"
        ]),
    );
    c.ok(r#"uuid5('ns', None)"#, json!({}), json!(null));
    c.ok(r#"uuid5(None, 'x')"#, json!({}), json!(null));
    c.ok(
        r#"uuid5(['a', None], 'x')"#,
        json!({}),
        json!(["6154a907-ce7b-5809-a51c-fc42073ada9f", null]),
    );
    c.err(r#"uuid5('ns', ['a'])"#, json!({})); // TypeError
    c.err(r#"uuid5(['a'], 5)"#, json!({})); // TypeError
    // --- to_snake
    c.ok(r#"to_snake('CamelCase')"#, json!({}), json!("camel_case"));
    c.ok(
        r#"to_snake('HTTPResponse')"#,
        json!({}),
        json!("http_response"),
    );
    c.ok(r#"to_snake('snake_case')"#, json!({}), json!("snake_case"));
    c.ok(
        r#"to_snake('http_response')"#,
        json!({}),
        json!("http_response"),
    );
    c.ok(
        r#"to_snake('already_snake')"#,
        json!({}),
        json!("already_snake"),
    );
    c.ok(r#"to_snake('ABC')"#, json!({}), json!("abc"));
    c.ok(r#"to_snake('ABc')"#, json!({}), json!("a_bc"));
    c.ok(r#"to_snake('aBC')"#, json!({}), json!("a_bc"));
    c.ok(r#"to_snake('ABCDef')"#, json!({}), json!("abc_def"));
    c.ok(
        r#"to_snake('getHTTPResponseCode')"#,
        json!({}),
        json!("get_http_response_code"),
    );
    c.ok(
        r#"to_snake('XMLHttpRequest2Go')"#,
        json!({}),
        json!("xml_http_request2_go"),
    );
    c.ok(r#"to_snake('a1B')"#, json!({}), json!("a1_b"));
    c.ok(r#"to_snake('1A')"#, json!({}), json!("1_a"));
    c.ok(r#"to_snake('a-b-c')"#, json!({}), json!("a_b_c"));
    c.ok(
        r#"to_snake('Foo-Bar_baz')"#,
        json!({}),
        json!("foo_bar_baz"),
    );
    c.ok(r#"to_snake('_leading')"#, json!({}), json!("_leading"));
    c.ok(r#"to_snake('trailing_')"#, json!({}), json!("trailing_"));
    c.ok(r#"to_snake('__dunder__')"#, json!({}), json!("__dunder__"));
    c.ok(r#"to_snake('a__b')"#, json!({}), json!("a__b"));
    c.ok(r#"to_snake('a_')"#, json!({}), json!("a_"));
    c.ok(r#"to_snake('_')"#, json!({}), json!("_"));
    c.ok(r#"to_snake('x')"#, json!({}), json!("x"));
    c.ok(r#"to_snake('X')"#, json!({}), json!("x"));
    c.ok(r#"to_snake('')"#, json!({}), json!(""));
    c.ok(r#"to_snake('two words')"#, json!({}), json!("two words"));
    c.ok(r#"to_snake('Two Words')"#, json!({}), json!("two words"));
    c.ok(r#"to_snake('ÉcoleÀB')"#, json!({}), json!("écoleàb"));
    c.ok(
        r#"to_snake('école_élève')"#,
        json!({}),
        json!("école_élève"),
    );
    c.ok(
        r#"to_snake('snake_case_with_Number1Two')"#,
        json!({}),
        json!("snake_case_with_number1_two"),
    );
    c.ok(r#"to_snake('IOError')"#, json!({}), json!("io_error"));
    c.ok(r#"to_snake('iOS')"#, json!({}), json!("i_os"));
    c.ok(r#"to_snake('ß_x')"#, json!({}), json!("ß_x"));
    c.ok(r#"to_snake('i̇_x')"#, json!({}), json!("i̇_x"));
    c.ok(r#"to_snake('Σ_Σ')"#, json!({}), json!("σ_σ"));
    c.ok(r#"to_snake('TÉST')"#, json!({}), json!("tést"));
    c.ok(r#"to_snake('aÉ')"#, json!({}), json!("aé"));
    c.ok(r#"to_snake('a٣B')"#, json!({}), json!("a٣_b"));
    c.ok(r#"to_snake('a²B')"#, json!({}), json!("a²b"));
    c.ok(r#"to_snake('a_b-c d')"#, json!({}), json!("a_b_c d"));
    c.ok(r#"to_snake('a_é')"#, json!({}), json!("a_é"));
    c.ok(r#"to_snake('ǆ_x')"#, json!({}), json!("ǆ_x"));
    c.ok(
        r#"to_snake('hello_world_')"#,
        json!({}),
        json!("hello_world_"),
    );
    c.ok(
        r#"to_snake('HELLO_WORLD')"#,
        json!({}),
        json!("hello_world"),
    );
    c.err(r#"to_snake(5)"#, json!({"names": ["foo_bar", "BazQux"]})); // TypeError
    c.err(r#"to_snake()"#, json!({"names": ["foo_bar", "BazQux"]})); // TypeError
    c.err(
        r#"to_snake('a', 'b')"#,
        json!({"names": ["foo_bar", "BazQux"]}),
    ); // TypeError
    c.ok(
        r#"to_snake(None)"#,
        json!({"names": ["foo_bar", "BazQux"]}),
        json!(null),
    );
    c.ok(
        r#"to_snake(['snake_case', None, 'a_b'])"#,
        json!({"names": ["foo_bar", "BazQux"]}),
        json!(["snake_case", null, "a_b"]),
    );
    c.ok(
        r#"to_snake(names)"#,
        json!({"names": ["foo_bar", "BazQux"]}),
        json!(["foo_bar", "baz_qux"]),
    );
    c.err(
        r#"to_snake(['a_b', 5])"#,
        json!({"names": ["foo_bar", "BazQux"]}),
    ); // TypeError
    // --- to_camel
    c.ok(r#"to_camel('CamelCase')"#, json!({}), json!("camelCase"));
    c.ok(
        r#"to_camel('HTTPResponse')"#,
        json!({}),
        json!("hTTPResponse"),
    );
    c.ok(r#"to_camel('snake_case')"#, json!({}), json!("snakeCase"));
    c.ok(
        r#"to_camel('http_response')"#,
        json!({}),
        json!("httpResponse"),
    );
    c.ok(
        r#"to_camel('already_snake')"#,
        json!({}),
        json!("alreadySnake"),
    );
    c.ok(r#"to_camel('ABC')"#, json!({}), json!("aBC"));
    c.ok(r#"to_camel('ABc')"#, json!({}), json!("aBc"));
    c.ok(r#"to_camel('aBC')"#, json!({}), json!("aBC"));
    c.ok(r#"to_camel('ABCDef')"#, json!({}), json!("aBCDef"));
    c.ok(
        r#"to_camel('getHTTPResponseCode')"#,
        json!({}),
        json!("getHTTPResponseCode"),
    );
    c.ok(
        r#"to_camel('XMLHttpRequest2Go')"#,
        json!({}),
        json!("xMLHttpRequest2Go"),
    );
    c.ok(r#"to_camel('a1B')"#, json!({}), json!("a1B"));
    c.ok(r#"to_camel('1A')"#, json!({}), json!("1A"));
    c.ok(r#"to_camel('a-b-c')"#, json!({}), json!("a-b-c"));
    c.ok(r#"to_camel('Foo-Bar_baz')"#, json!({}), json!("foo-BarBaz"));
    c.ok(r#"to_camel('_leading')"#, json!({}), json!("_leading"));
    c.ok(r#"to_camel('trailing_')"#, json!({}), json!("trailing_"));
    c.ok(r#"to_camel('__dunder__')"#, json!({}), json!("_Dunder_"));
    c.ok(r#"to_camel('a__b')"#, json!({}), json!("a_b"));
    c.ok(r#"to_camel('a_')"#, json!({}), json!("a_"));
    c.ok(r#"to_camel('_')"#, json!({}), json!("_"));
    c.ok(r#"to_camel('x')"#, json!({}), json!("x"));
    c.ok(r#"to_camel('X')"#, json!({}), json!("x"));
    c.err(r#"to_camel('')"#, json!({})); // IndexError
    c.ok(r#"to_camel('two words')"#, json!({}), json!("two words"));
    c.ok(r#"to_camel('Two Words')"#, json!({}), json!("two Words"));
    c.ok(r#"to_camel('ÉcoleÀB')"#, json!({}), json!("écoleÀB"));
    c.ok(r#"to_camel('école_élève')"#, json!({}), json!("écoleÉlève"));
    c.ok(
        r#"to_camel('snake_case_with_Number1Two')"#,
        json!({}),
        json!("snakeCaseWithNumber1Two"),
    );
    c.ok(r#"to_camel('IOError')"#, json!({}), json!("iOError"));
    c.ok(r#"to_camel('iOS')"#, json!({}), json!("iOS"));
    c.ok(r#"to_camel('ß_x')"#, json!({}), json!("ßSX"));
    c.ok(r#"to_camel('i̇_x')"#, json!({}), json!("i̇X"));
    c.ok(r#"to_camel('Σ_Σ')"#, json!({}), json!("σΣ"));
    c.ok(r#"to_camel('TÉST')"#, json!({}), json!("tÉST"));
    c.ok(r#"to_camel('aÉ')"#, json!({}), json!("aÉ"));
    c.ok(r#"to_camel('a٣B')"#, json!({}), json!("a٣B"));
    c.ok(r#"to_camel('a²B')"#, json!({}), json!("a²B"));
    c.ok(r#"to_camel('a_b-c d')"#, json!({}), json!("aB-c d"));
    c.ok(r#"to_camel('a_é')"#, json!({}), json!("aÉ"));
    c.ok(r#"to_camel('ǆ_x')"#, json!({}), json!("ǆX"));
    c.ok(
        r#"to_camel('hello_world_')"#,
        json!({}),
        json!("helloWorld_"),
    );
    c.ok(r#"to_camel('HELLO_WORLD')"#, json!({}), json!("hELLOWORLD"));
    c.err(r#"to_camel(5)"#, json!({"names": ["foo_bar", "BazQux"]})); // TypeError
    c.err(r#"to_camel()"#, json!({"names": ["foo_bar", "BazQux"]})); // TypeError
    c.err(
        r#"to_camel('a', 'b')"#,
        json!({"names": ["foo_bar", "BazQux"]}),
    ); // TypeError
    c.ok(
        r#"to_camel(None)"#,
        json!({"names": ["foo_bar", "BazQux"]}),
        json!(null),
    );
    c.ok(
        r#"to_camel(['snake_case', None, 'a_b'])"#,
        json!({"names": ["foo_bar", "BazQux"]}),
        json!(["snakeCase", null, "aB"]),
    );
    c.ok(
        r#"to_camel(names)"#,
        json!({"names": ["foo_bar", "BazQux"]}),
        json!(["fooBar", "bazQux"]),
    );
    c.err(
        r#"to_camel(['a_b', 5])"#,
        json!({"names": ["foo_bar", "BazQux"]}),
    ); // TypeError
    // --- to_pascal
    c.ok(r#"to_pascal('CamelCase')"#, json!({}), json!("CamelCase"));
    c.ok(
        r#"to_pascal('HTTPResponse')"#,
        json!({}),
        json!("HTTPResponse"),
    );
    c.ok(r#"to_pascal('snake_case')"#, json!({}), json!("SnakeCase"));
    c.ok(
        r#"to_pascal('http_response')"#,
        json!({}),
        json!("HttpResponse"),
    );
    c.ok(
        r#"to_pascal('already_snake')"#,
        json!({}),
        json!("AlreadySnake"),
    );
    c.ok(r#"to_pascal('ABC')"#, json!({}), json!("ABC"));
    c.ok(r#"to_pascal('ABc')"#, json!({}), json!("ABc"));
    c.ok(r#"to_pascal('aBC')"#, json!({}), json!("ABC"));
    c.ok(r#"to_pascal('ABCDef')"#, json!({}), json!("ABCDef"));
    c.ok(
        r#"to_pascal('getHTTPResponseCode')"#,
        json!({}),
        json!("GetHTTPResponseCode"),
    );
    c.ok(
        r#"to_pascal('XMLHttpRequest2Go')"#,
        json!({}),
        json!("XMLHttpRequest2Go"),
    );
    c.ok(r#"to_pascal('a1B')"#, json!({}), json!("A1B"));
    c.ok(r#"to_pascal('1A')"#, json!({}), json!("1A"));
    c.ok(r#"to_pascal('a-b-c')"#, json!({}), json!("A-b-c"));
    c.ok(
        r#"to_pascal('Foo-Bar_baz')"#,
        json!({}),
        json!("Foo-BarBaz"),
    );
    c.ok(r#"to_pascal('_leading')"#, json!({}), json!("_leading"));
    c.ok(r#"to_pascal('trailing_')"#, json!({}), json!("Trailing_"));
    c.ok(r#"to_pascal('__dunder__')"#, json!({}), json!("_Dunder_"));
    c.ok(r#"to_pascal('a__b')"#, json!({}), json!("A_b"));
    c.ok(r#"to_pascal('a_')"#, json!({}), json!("A_"));
    c.ok(r#"to_pascal('_')"#, json!({}), json!("_"));
    c.ok(r#"to_pascal('x')"#, json!({}), json!("X"));
    c.ok(r#"to_pascal('X')"#, json!({}), json!("X"));
    c.ok(r#"to_pascal('')"#, json!({}), json!(""));
    c.ok(r#"to_pascal('two words')"#, json!({}), json!("Two words"));
    c.ok(r#"to_pascal('Two Words')"#, json!({}), json!("Two Words"));
    c.ok(r#"to_pascal('ÉcoleÀB')"#, json!({}), json!("ÉcoleÀB"));
    c.ok(
        r#"to_pascal('école_élève')"#,
        json!({}),
        json!("ÉcoleÉlève"),
    );
    c.ok(
        r#"to_pascal('snake_case_with_Number1Two')"#,
        json!({}),
        json!("SnakeCaseWithNumber1Two"),
    );
    c.ok(r#"to_pascal('IOError')"#, json!({}), json!("IOError"));
    c.ok(r#"to_pascal('iOS')"#, json!({}), json!("IOS"));
    c.ok(r#"to_pascal('ß_x')"#, json!({}), json!("SSX"));
    c.ok(r#"to_pascal('i̇_x')"#, json!({}), json!("İX"));
    c.ok(r#"to_pascal('Σ_Σ')"#, json!({}), json!("ΣΣ"));
    c.ok(r#"to_pascal('TÉST')"#, json!({}), json!("TÉST"));
    c.ok(r#"to_pascal('aÉ')"#, json!({}), json!("AÉ"));
    c.ok(r#"to_pascal('a٣B')"#, json!({}), json!("A٣B"));
    c.ok(r#"to_pascal('a²B')"#, json!({}), json!("A²B"));
    c.ok(r#"to_pascal('a_b-c d')"#, json!({}), json!("AB-c d"));
    c.ok(r#"to_pascal('a_é')"#, json!({}), json!("AÉ"));
    c.ok(r#"to_pascal('ǆ_x')"#, json!({}), json!("ǄX"));
    c.ok(
        r#"to_pascal('hello_world_')"#,
        json!({}),
        json!("HelloWorld_"),
    );
    c.ok(
        r#"to_pascal('HELLO_WORLD')"#,
        json!({}),
        json!("HELLOWORLD"),
    );
    c.err(r#"to_pascal(5)"#, json!({"names": ["foo_bar", "BazQux"]})); // TypeError
    c.err(r#"to_pascal()"#, json!({"names": ["foo_bar", "BazQux"]})); // TypeError
    c.err(
        r#"to_pascal('a', 'b')"#,
        json!({"names": ["foo_bar", "BazQux"]}),
    ); // TypeError
    c.ok(
        r#"to_pascal(None)"#,
        json!({"names": ["foo_bar", "BazQux"]}),
        json!(null),
    );
    c.ok(
        r#"to_pascal(['snake_case', None, 'a_b'])"#,
        json!({"names": ["foo_bar", "BazQux"]}),
        json!(["SnakeCase", null, "AB"]),
    );
    c.ok(
        r#"to_pascal(names)"#,
        json!({"names": ["foo_bar", "BazQux"]}),
        json!(["FooBar", "BazQux"]),
    );
    c.err(
        r#"to_pascal(['a_b', 5])"#,
        json!({"names": ["foo_bar", "BazQux"]}),
    ); // TypeError
    // --- list
    c.ok(r#"list()"#, json!({}), json!([]));
    c.ok(r#"list([1, 2])"#, json!({}), json!([1, 2]));
    c.ok(r#"list('abc')"#, json!({}), json!(["a", "b", "c"]));
    c.err(r#"list(5)"#, json!({})); // TypeError
    c.err(r#"list(None)"#, json!({})); // TypeError
    c.err(r#"list([1], [2])"#, json!({})); // TypeError
    c.ok(r#"tuple([1, 2])"#, json!({}), json!([1, 2]));
    c.ok(r#"tuple('ab')"#, json!({}), json!(["a", "b"]));
    c.ok(r#"tuple()"#, json!({}), json!([]));
    // --- distribute
    c.ok(r#"str([1, None, 'a'])"#, json!({}), json!(["1", null, "a"]));
    c.ok(
        r#"upper(['a', None, 'b'])"#,
        json!({}),
        json!(["A", null, "B"]),
    );
    c.ok(r#"upper([])"#, json!({}), json!([]));
    c.ok(r#"upper([], 1, 2)"#, json!({}), json!([]));
    c.err(r#"upper(['a'], 1)"#, json!({})); // TypeError
    c.err(r#"upper([['a']])"#, json!({})); // TypeError
    c.ok(
        r#"replace(['ab', None], 'b', 'c')"#,
        json!({}),
        json!(["ac", null]),
    );
    c.ok(
        r#"replace(['ab', 'cb'], 'b', None)"#,
        json!({}),
        json!([null, null]),
    );
    c.ok(r#"replace([], 1)"#, json!({}), json!([]));
    c.ok(r#"len([[1], 'ab'])"#, json!({}), json!(2));
    c.ok(r#"strlen([[1, 2], 'ab', []])"#, json!({}), json!([2, 2, 0]));
    c.ok(r#"int(['1', None, '2'])"#, json!({}), json!([1, null, 2]));
    c.ok(r#"int(['ff', 'a'], 16)"#, json!({}), json!([255, 10]));
    c.ok(r#"round([1.234, 5.678], 1)"#, json!({}), json!([1.2, 5.7]));
    c.ok(r#"round([1.234, None], 1)"#, json!({}), json!([1.2, null]));
    c.ok(r#"round([1.234], None)"#, json!({}), json!([null]));
    c.ok(
        r#"substr(['abc', None], 1)"#,
        json!({}),
        json!(["bc", null]),
    );
    c.ok(
        r#"join([',', ';'], ['a', 'b'])"#,
        json!({}),
        json!(["a,b", "a;b"]),
    );
    c.ok(
        r#"to_snake(['AbC', None])"#,
        json!({}),
        json!(["ab_c", null]),
    );
    c.ok(
        r#"uuid5(['a', 'b'], 'x')"#,
        json!({}),
        json!([
            "6154a907-ce7b-5809-a51c-fc42073ada9f",
            "047fda66-7dd8-592c-9e76-ab827b06ba6c"
        ]),
    );
    c.ok(
        r#"contains(['abc', 'xyz'], 'b')"#,
        json!({}),
        json!([true, false]),
    );
    c.ok(
        r#"floor([1.5, None, -1.5])"#,
        json!({}),
        json!([1, null, -2]),
    );
    c.ok(r#"abs([-1, None, 2.5])"#, json!({}), json!([1, null, 2.5]));
    c.ok(
        r#"bool([0, 1, None, ''])"#,
        json!({}),
        json!([false, true, null, false]),
    );
    c.ok(
        r#"float(['1', 2, None])"#,
        json!({}),
        json!([1.0, 2.0, null]),
    );
    c.ok(
        r#"strip([' a ', '  b'], ' ')"#,
        json!({}),
        json!(["a", "b"]),
    );
    c.ok(
        r#"startswith(['ab', 'cb'], 'a')"#,
        json!({}),
        json!([true, false]),
    );
    c.ok(
        r#"split(['a b', 'c d e'])"#,
        json!({}),
        json!([["a", "b"], ["c", "d", "e"]]),
    );
    c.ok(r#"split(['a,b'], ',', 1)"#, json!({}), json!([["a", "b"]]));
    c.ok(
        r#"title(['ab cd', 'ef'])"#,
        json!({}),
        json!(["Ab Cd", "Ef"]),
    );
    // END GENERATED

    c.finish();
}
