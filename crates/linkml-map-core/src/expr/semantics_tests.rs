//! Value-semantics cases checked against upstream `linkml-map` v0.6.0-rc1
//! (`eval_expr_with_mapping`, bindings built as `DynObj`s): zero division, `in` and
//! ordering `TypeError` retries, map attributes, CPython numeric-string parsing,
//! `repr`/case mapping from CPython 3.13's Unicode 15.1 data, and `sorted`.
//! Expected values were produced by running the upstream evaluator.

use serde_json::Value as Json;

use super::error::ExprError;
use super::eval::{Bindings, eval_expr_with_mapping};
use crate::value::Value;

/// What an expression gives upstream.
enum Expect {
    /// A value (JSON-shaped).
    Json(&'static str),
    /// An exception `_eval_expr` does not catch (`ZeroDivisionError`).
    Uncaught,
    /// A `TypeError`/`ValueError`: rejected by the restricted evaluator.
    Rejected,
}

fn bindings(vars: &str) -> Bindings {
    let vars: Json = serde_json::from_str(vars).expect("bindings JSON");
    vars.as_object()
        .expect("bindings must be a JSON object")
        .iter()
        .map(|(k, v)| (k.clone(), Value::from(v)))
        .collect()
}

const CASES: &[(&str, &str, Expect)] = &[
    ("1/0", "{}", Expect::Uncaught),
    ("1//0", "{}", Expect::Uncaught),
    ("1%0", "{}", Expect::Uncaught),
    ("1.0/0", "{}", Expect::Uncaught),
    ("1 % 0.0", "{}", Expect::Uncaught),
    (
        "a/x",
        "{\u{22}a\u{22}: 1, \u{22}x\u{22}: 0}",
        Expect::Uncaught,
    ),
    ("'5'/0", "{}", Expect::Uncaught),
    ("'a' in 5", "{}", Expect::Json("null")),
    ("True not in True", "{}", Expect::Json("null")),
    ("1 in 5", "{}", Expect::Rejected),
    ("1 in '1'", "{}", Expect::Rejected),
    (
        "[1] in {'a': 1}",
        "{\u{22}m\u{22}: {\u{22}a\u{22}: 1}}",
        Expect::Json("null"),
    ),
    (
        "m.nope",
        "{\u{22}m\u{22}: {\u{22}a\u{22}: 1}}",
        Expect::Json("null"),
    ),
    ("m.nope", "{\u{22}m\u{22}: {}}", Expect::Json("null")),
    (
        "l.a",
        "{\u{22}l\u{22}: [{\u{22}a\u{22}: 1}, {\u{22}b\u{22}: 2}]}",
        Expect::Json("[1, null]"),
    ),
    (
        "x.a",
        "{\u{22}x\u{22}: {\u{22}k\u{22}: {\u{22}a\u{22}: 5}}}",
        Expect::Json("null"),
    ),
    (
        "m.a",
        "{\u{22}m\u{22}: {\u{22}a\u{22}: 1}}",
        Expect::Json("1"),
    ),
    ("[1] < [2]", "{}", Expect::Json("true")),
    ("a < a", "{\u{22}a\u{22}: []}", Expect::Json("false")),
    ("[1, 2] <= [1, 3]", "{}", Expect::Json("true")),
    ("[1, 'a'] < [1, 2]", "{}", Expect::Json("null")),
    ("[1] < 'a'", "{}", Expect::Json("null")),
    ("[2] > [1, 5]", "{}", Expect::Json("true")),
    ("'\u{661}' + 1", "{}", Expect::Json("2")),
    ("'\u{663}' * 2", "{}", Expect::Json("6")),
    ("'\u{661}' == 1", "{}", Expect::Json("true")),
    ("'1_0' + 1", "{}", Expect::Json("11")),
    ("'1__0' + 1", "{}", Expect::Json("null")),
    ("'\u{1c}1' + 1", "{}", Expect::Json("null")),
    (
        "is_numeric(x)",
        "{\u{22}x\u{22}: \u{22}\u{5c}u001c5\u{22}}",
        Expect::Json("false"),
    ),
    (
        "is_numeric(x)",
        "{\u{22}x\u{22}: \u{22}\u{a0}5\u{2003}\u{22}}",
        Expect::Json("true"),
    ),
    (
        "int(x)",
        "{\u{22}x\u{22}: \u{22}\u{ff12}\u{ff13}\u{22}}",
        Expect::Json("23"),
    ),
    (
        "int(x)",
        "{\u{22}x\u{22}: \u{22}\u{5c}u001f4\u{22}}",
        Expect::Rejected,
    ),
    (
        "float(x)",
        "{\u{22}x\u{22}: \u{22}4\u{5c}u001c\u{22}}",
        Expect::Rejected,
    ),
    (
        "str(x)",
        "{\u{22}x\u{22}: [\u{22}\u{a0}\u{22}, \u{22}a\u{2028}\u{22}, \u{22}\u{ad}\u{22}]}",
        Expect::Json("[\u{22}\u{a0}\u{22}, \u{22}a\u{2028}\u{22}, \u{22}\u{ad}\u{22}]"),
    ),
    (
        "str(x)",
        "{\u{22}x\u{22}: {\u{22}k\u{80}\u{22}: \u{22}\u{1bef9}\u{22}}}",
        Expect::Json("\u{22}{'k\u{5c}\u{5c}x80': '\u{5c}\u{5c}U0001bef9'}\u{22}"),
    ),
    (
        "str(x)",
        "{\u{22}x\u{22}: [\u{22}\u{e9}\u{4e2d}\u{1f600} \u{22}]}",
        Expect::Json("[\u{22}\u{e9}\u{4e2d}\u{1f600} \u{22}]"),
    ),
    (
        "upper(x)",
        "{\u{22}x\u{22}: \u{22}\u{1c89}a\u{22}}",
        Expect::Json("\u{22}\u{1c89}A\u{22}"),
    ),
    (
        "lower(x)",
        "{\u{22}x\u{22}: \u{22}\u{1c89}\u{22}}",
        Expect::Json("\u{22}\u{1c89}\u{22}"),
    ),
    (
        "upper(x)",
        "{\u{22}x\u{22}: \u{22}\u{df}\u{149}\u{fb03}\u{22}}",
        Expect::Json("\u{22}SS\u{2bc}NFFI\u{22}"),
    ),
    (
        "lower(x)",
        "{\u{22}x\u{22}: \u{22}\u{130}AB\u{3a3}\u{22}}",
        Expect::Json("\u{22}i\u{307}ab\u{3c2}\u{22}"),
    ),
    (
        "lower(x)",
        "{\u{22}x\u{22}: \u{22}\u{3a3}A\u{3a3}\u{22}}",
        Expect::Json("\u{22}\u{3c3}a\u{3c2}\u{22}"),
    ),
    (
        "title(x)",
        "{\u{22}x\u{22}: \u{22}hello \u{1c6}world \u{1f80}a\u{22}}",
        Expect::Json("\u{22}Hello \u{1c5}world \u{1f88}a\u{22}"),
    ),
    (
        "capitalize(x)",
        "{\u{22}x\u{22}: \u{22}\u{1c6}dz\u{22}}",
        Expect::Json("\u{22}\u{1c5}dz\u{22}"),
    ),
    (
        "to_snake(x)",
        "{\u{22}x\u{22}: \u{22}HTTP\u{c9}Response\u{22}}",
        Expect::Json("\u{22}http\u{e9}response\u{22}"),
    ),
    (
        "to_pascal(x)",
        "{\u{22}x\u{22}: \u{22}a_\u{e9}b\u{22}}",
        Expect::Json("\u{22}A\u{c9}b\u{22}"),
    ),
    (
        "sorted(x)",
        "{\u{22}x\u{22}: [3, 1, 2]}",
        Expect::Json("[1, 2, 3]"),
    ),
    (
        "sorted(x)",
        "{\u{22}x\u{22}: [1, \u{22}a\u{22}]}",
        Expect::Rejected,
    ),
    (
        "sorted(x)",
        "{\u{22}x\u{22}: [[2, 1], [1, 5], [1]]}",
        Expect::Json("[[1], [1, 5], [2, 1]]"),
    ),
];

#[test]
fn upstream_value_semantics() {
    let mut failures = Vec::new();
    for (expr, vars, expect) in CASES {
        let got = eval_expr_with_mapping(expr, &bindings(vars));
        let ok = match (&got, expect) {
            (Ok(v), Expect::Json(want)) => {
                let want: Json = serde_json::from_str(want).expect("expected JSON");
                *v == Value::from(&want)
            }
            (Err(ExprError::Uncaught(_)), Expect::Uncaught) => true,
            (Err(ExprError::Eval(_)), Expect::Rejected) => true,
            _ => false,
        };
        if !ok {
            failures.push(format!("{expr} with {vars}: got {got:?}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Results with NaNs or floats, compared through `str()`.
#[test]
fn upstream_value_semantics_repr() {
    let cases: &[(&str, &str)] = &[
        // `NaN` counts as equal to anything in `count_run`, as in CPython.
        (
            "join('|', str(sorted([True, -13, float('nan')])))",
            "-13|nan|True",
        ),
        (
            "join('|', str(sorted([float('nan'), 2.5, 2.5, 2.5, 0, 2, 0, 3])))",
            "0|nan|0|2|2.5|2.5|2.5|3",
        ),
        ("str(float('\u{663}.\u{665}'))", "3.5"),
        ("str(float('1_0.5_5'))", "10.55"),
        // `str` distributes over a list, so wrap the list to take its `repr`.
        (
            "str([['\u{a0}', 'a\u{2028}', '\u{ad}']])[0]",
            "['\\xa0', 'a\\u2028', '\\xad']",
        ),
        ("str({'k\u{80}': '\u{1bef9}'})", "{'k\\x80': '\\U0001bef9'}"),
    ];
    for (expr, want) in cases {
        let got = eval_expr_with_mapping(expr, &Bindings::new());
        assert_eq!(got, Ok(Value::Str((*want).to_string())), "{expr}");
    }
}

/// `Bindings.__getitem__` answers null for a declared slot the row lacks: only while
/// the declared set is in force, and never for a name outside it.
#[test]
fn declared_slots_resolve_to_null_only_within_their_scope() {
    use super::eval::{DeclaredSlots, eval_expr_with_mapping_strict, with_declared_slots};
    use std::sync::Arc;

    let declared: Arc<DeclaredSlots> =
        Arc::new(["age", "max"].map(str::to_owned).into_iter().collect());
    let vars = bindings("{}");
    let in_scope = |expr: &str, strict: bool| {
        with_declared_slots(Some(&declared), || {
            if strict {
                eval_expr_with_mapping_strict(expr, &vars)
            } else {
                eval_expr_with_mapping(expr, &vars)
            }
        })
    };

    assert_eq!(in_scope("age", true).unwrap(), Value::Null);
    assert_eq!(in_scope("{age}.x", true).unwrap(), Value::Null);
    // Names win over functions: a declared slot called `max` is null, not the function.
    assert_eq!(in_scope("max", true).unwrap(), Value::Null);
    // An undeclared name is still an error when strict, null when lax.
    assert!(in_scope("nosuch", true).is_err());
    assert_eq!(in_scope("nosuch", false).unwrap(), Value::Null);
    // Out of scope nothing is declared.
    assert!(eval_expr_with_mapping_strict("age", &vars).is_err());
    // Scopes nest and restore.
    with_declared_slots(Some(&declared), || {
        with_declared_slots(None, || {
            assert!(eval_expr_with_mapping_strict("age", &vars).is_err());
        });
        assert_eq!(
            eval_expr_with_mapping_strict("age", &vars).unwrap(),
            Value::Null
        );
    });
    assert!(eval_expr_with_mapping_strict("age", &vars).is_err());
}
