//! Property tests for the expression lexer, parser and evaluator.
//!
//! The contract under test is "errors are fine, panics are not": whatever text and
//! whatever bindings arrive, the pipeline must return `Ok` or `Err`, never unwind,
//! overflow the stack or abort. Evaluation must also be deterministic and the
//! pre-parsed and string-in entry points must agree.
//!
//! Default case counts keep `cargo test` quick; set `PROPTEST_CASES=100000` for a long run.

mod common;

use common::*;
use linkml_map_core::expr::lexer::lex;
use linkml_map_core::expr::parser::parse;
use linkml_map_core::expr::stmt::{is_statement_block, parse_program};
use linkml_map_core::expr::{
    Bindings, EvalMode, ExprError, eval_enum_parsed, eval_expr_with_mapping,
    eval_expr_with_mapping_strict, eval_parsed, eval_parsed_gated, eval_parsed_strict,
    expression_asts, parse_expr,
};
use linkml_map_core::value::Value;
use proptest::prelude::*;

/// Every evaluation mode of the gate (restricted/unrestricted x lax/strict), for slot and
/// enum expressions. Must not panic whatever the text.
fn eval_gated_everywhere(src: &str, vars: &Bindings) {
    let Ok(parsed) = parse_expr(src) else { return };
    let enum_src = Value::Map(vars.clone());
    for strict in [false, true] {
        for unrestricted in [false, true] {
            let mode = EvalMode {
                strict,
                unrestricted,
            };
            let _ = eval_parsed_gated(&parsed, vars, mode);
            let _ = eval_enum_parsed(&parsed, vars, &enum_src, mode);
        }
    }
}

/// `Debug` text of a result: structural equality that treats `NaN == NaN`.
fn shown<T: std::fmt::Debug>(r: &T) -> String {
    format!("{r:?}")
}

proptest! {
    #![proptest_config(config(256))]

    /// The lexer, the single-expression parser and the statement parser never panic.
    #[test]
    fn lex_and_parse_never_panic(src in arb_expr_source(false)) {
        let _ = lex(&src);
        let _ = parse(&src);
        let _ = is_statement_block(&src);
        let _ = parse_program(&src);
        let _ = parse_expr(&src);
        let _ = expression_asts(&src);
    }

    /// Parsing is deterministic and `parse_expr` agrees with the lower-level entry points.
    #[test]
    fn parse_is_deterministic(src in arb_expr_source(false)) {
        prop_assert_eq!(shown(&parse_expr(&src)), shown(&parse_expr(&src)));
        prop_assert_eq!(shown(&lex(&src)), shown(&lex(&src)));
    }

    /// Evaluation never panics for any text (boundary integers, no `*`) and any
    /// bindings, lax or strict.
    #[test]
    fn eval_never_panics(src in arb_expr_source(true), vars in arb_bindings(true)) {
        let _ = eval_expr_with_mapping(&src, &vars);
        let _ = eval_expr_with_mapping_strict(&src, &vars);
        eval_gated_everywhere(&src, &vars);
    }

    /// The same with `*` allowed (so list/string repetition is exercised), over
    /// small integers only.
    #[test]
    fn eval_never_panics_with_multiplication(
        src in arb_expr_source(false),
        vars in arb_bindings(false),
    ) {
        let _ = eval_expr_with_mapping(&src, &vars);
        let _ = eval_expr_with_mapping_strict(&src, &vars);
        eval_gated_everywhere(&src, &vars);
    }

    /// Grammar-built expressions reach the evaluator; `eval` is a pure function of
    /// (source, bindings), and the cached-AST path equals the string path.
    #[test]
    fn eval_is_pure_and_consistent(
        (src, vars) in prop_oneof![
            (arb_expr_tree(true), arb_bindings(true)),
            (arb_expr_tree(false), arb_bindings(false)),
        ]
    ) {
        let before = vars.clone();
        let direct = eval_expr_with_mapping(&src, &vars);
        let again = eval_expr_with_mapping(&src, &vars);
        prop_assert_eq!(shown(&direct), shown(&again));
        prop_assert_eq!(shown(&vars), shown(&before), "evaluation mutated its bindings");

        if let Ok(parsed) = parse_expr(&src) {
            prop_assert_eq!(shown(&eval_parsed(&parsed, &vars)), shown(&direct));
            let strict = eval_parsed_strict(&parsed, &vars);
            prop_assert_eq!(
                shown(&strict),
                shown(&eval_expr_with_mapping_strict(&src, &vars))
            );
            // Strict mode only ever turns a lax success into an error, never
            // changes a successful value into a different one.
            if let (Ok(lax), Ok(strict)) = (&direct, &strict) {
                prop_assert_eq!(shown(lax), shown(strict));
            }
        } else {
            prop_assert!(direct.is_err(), "string path evaluated what parse_expr rejected");
        }
    }

    /// Multi-statement programs (assignments, `if`, comprehensions) never panic.
    #[test]
    fn programs_never_panic(
        (src, vars) in prop_oneof![
            (arb_program(true), arb_bindings(true)),
            (arb_program(false), arb_bindings(false)),
        ]
    ) {
        let _ = eval_expr_with_mapping(&src, &vars);
        let _ = eval_expr_with_mapping_strict(&src, &vars);
        let _ = expression_asts(&src);
        eval_gated_everywhere(&src, &vars);
    }

    /// The gate is deterministic in every mode (restricted/unrestricted x lax/strict).
    #[test]
    fn gate_is_deterministic(
        (src, vars) in prop_oneof![
            (arb_expr_tree(true), arb_bindings(true)),
            (arb_program(true), arb_bindings(true)),
        ]
    ) {
        if let Ok(parsed) = parse_expr(&src) {
            for strict in [false, true] {
                for unrestricted in [false, true] {
                    let mode = EvalMode { strict, unrestricted };
                    prop_assert_eq!(
                        shown(&eval_parsed_gated(&parsed, &vars, mode)),
                        shown(&eval_parsed_gated(&parsed, &vars, mode))
                    );
                }
            }
        }
    }

    /// With no bindings at all, every name is null in lax mode and an error in strict mode.
    #[test]
    fn unbound_names_are_null_when_lax(name in prop::sample::select(EXPR_NAMES.to_vec())) {
        let none = Bindings::new();
        prop_assert_eq!(eval_expr_with_mapping(name, &none), Ok(Value::Null));
        prop_assert!(eval_expr_with_mapping_strict(name, &none).is_err());
    }

    /// Integer literals evaluate to themselves (or fail to lex when out of range).
    #[test]
    fn int_literals_round_trip(i in any::<i64>()) {
        let none = Bindings::new();
        let src = i.to_string();
        match eval_expr_with_mapping(&src, &none) {
            Ok(v) => prop_assert_eq!(v, Value::Int(i)),
            // `-9223372036854775808` is `-(9223372036854775808)` and may not fit.
            Err(_) => prop_assert_eq!(i, i64::MIN),
        }
    }
}

// ── Deep nesting ─────────────────────────────────────────────────────────────
//
// A stack overflow aborts the process and cannot be caught by proptest, so each
// shape runs in a child process (see `common::crash_isolated`) on a 2 MiB thread.
// Python's own parser rejects such input with `SyntaxError`/`RecursionError`, so a
// graceful `Err` is the expected result.

const DEPTH: usize = 10_000;

fn nest(open: &str, inner: &str, close: &str, depth: usize) -> String {
    format!("{}{inner}{}", open.repeat(depth), close.repeat(depth))
}

/// Lex, parse and evaluate `src` with every public entry point.
fn survive(src: String) {
    let vars = Bindings::new();
    let _ = lex(&src);
    let _ = parse(&src);
    let _ = parse_program(&src);
    let _ = expression_asts(&src);
    let _ = eval_expr_with_mapping(&src, &vars);
    let _ = eval_expr_with_mapping_strict(&src, &vars);
}

macro_rules! deep_case {
    ($(#[$attr:meta])* $name:ident, $src:expr) => {
        $(#[$attr])*
        #[test]
        fn $name() {
            crash_isolated(stringify!($name), || survive($src));
        }
    };
}

// Moderate nesting must always work, including in a debug build (whose frames are
// roughly ten times larger than release ones).
deep_case!(moderate_parentheses, nest("(", "1", ")", 40));
deep_case!(moderate_brackets, nest("[", "1", "]", 40));
deep_case!(moderate_unary, format!("{}1", "-".repeat(40)));
deep_case!(moderate_calls, nest("len(", "x", ")", 40));
deep_case!(moderate_addition_chain, vec!["1"; 200].join(" + "));

// Flat chains: left-associative and `and`/comparison chains stay shallow.
deep_case!(long_boolean_chain, vec!["x"; DEPTH].join(" and "));
deep_case!(long_comparison_chain, vec!["1"; DEPTH].join(" < "));

// Pathological nesting. The thresholds below are for a *release* build on a 2 MiB
// thread (a tokio/rayon worker): the recursive-descent parser and the recursive
// evaluator both recurse once per nesting level and neither has a depth limit.
// CPython rejects this with SyntaxError ("too many nested parentheses") or
// RecursionError, so the parity behaviour is a graceful `Err`.
deep_case!(deep_parentheses, nest("(", "1", ")", DEPTH));
deep_case!(deep_brackets, nest("[", "1", "]", DEPTH));
deep_case!(deep_nested_calls, nest("len(", "x", ")", DEPTH));
deep_case!(deep_braces, nest("{", "x", "}", DEPTH));
deep_case!(deep_unary_minus, format!("{}1", "-".repeat(DEPTH)));
deep_case!(deep_not, format!("{}x", "not ".repeat(DEPTH)));
deep_case!(deep_subscripts, format!("x{}", "[0]".repeat(DEPTH)));
deep_case!(deep_attributes, format!("x{}", ".a".repeat(DEPTH)));
deep_case!(long_addition_chain, vec!["1"; DEPTH].join(" + "));
deep_case!(long_power_chain, vec!["1"; DEPTH].join(" ** "));
deep_case!(
    deep_ternary_chain,
    format!("{}1", "1 if x else ".repeat(DEPTH))
);
deep_case!(deep_dict_nesting, nest("{1: ", "1", "}", DEPTH));
deep_case!(deep_set_nesting, nest("{", "1", "}", DEPTH));
deep_case!(deep_keyword_calls, nest("dict(a=", "1", ")", DEPTH));
deep_case!(deep_generators, nest("(1 for q in ", "x", ")", DEPTH));
deep_case!(deep_if_blocks, {
    let mut s = String::new();
    for i in 0..DEPTH / 10 {
        s.push_str(&" ".repeat(i));
        s.push_str("if x:\n");
    }
    s.push_str(&" ".repeat(DEPTH / 10));
    s.push_str("target = 1\n");
    s
});

// ── The depth limit ──────────────────────────────────────────────────────────
//
// Mirrors CPython: more than 200 open brackets is a `SyntaxError` (tokenizer
// `MAXLEVEL`), and a tree deeper than about 496 levels exceeds the recursion limit
// of upstream's evaluator (measured against v0.6.0-rc1: 495 chained `-` evaluate,
// 496 raise `RecursionError`). What is accepted must also fit a 2 MiB thread, in a
// debug build too.

/// Evaluate `src` (parse errors included) with the evaluator's own error text.
fn eval_text(src: &str) -> Result<Value, String> {
    eval_expr_with_mapping(src, &Bindings::new()).map_err(|e| e.to_string())
}

#[test]
fn accepted_depth_fits_a_small_stack() {
    // 2 MiB is a tokio/rayon worker's stack. A debug build's frames are about ten
    // times larger, so it gets 32 MiB; `cargo test --release` checks the real figure.
    let stack = if cfg!(debug_assertions) { 32 } else { 2 } * 1024 * 1024;
    crash_isolated_with_stack("accepted_depth_fits_a_small_stack", stack, || {
        let chains: Vec<(&str, String)> = vec![
            ("unary", format!("{}1", "-".repeat(495))),
            ("not", format!("{}1", "not ".repeat(495))),
            ("add", vec!["1"; 496].join(" + ")),
            ("pow", vec!["1"; 496].join(" ** ")),
            ("ternary", format!("{}1", "1 if 0 else ".repeat(495))),
            ("subscript", format!("[0]{}", "[0]".repeat(494))),
            ("brackets", nest("[", "1", "]", 200)),
            ("parentheses", nest("(", "1", ")", 200)),
            ("parenthesised_unary", nest("(-", "1", ")", 200)),
            ("dicts", nest("{1: ", "1", "}", 200)),
            ("calls", nest("len(", "[1]", ")", 199)),
            ("mixed", format!("{}1{}", "(-".repeat(150), ")".repeat(150))),
        ];
        for (name, src) in chains {
            let parsed = parse_expr(&src);
            assert!(parsed.is_ok(), "{name}: {:?}", parsed.err());
            // Evaluating must not overflow; the result may be an error (`len(1)`).
            let _ = eval_text(&src);
        }
    });
}

#[test]
fn depth_just_past_the_limit_is_an_error() {
    crash_isolated("depth_just_past_the_limit_is_an_error", || {
        let past: Vec<(&str, String, &str)> = vec![
            ("unary", format!("{}1", "-".repeat(496)), "recursion"),
            ("not", format!("{}1", "not ".repeat(496)), "recursion"),
            ("add", vec!["1"; 497].join(" + "), "recursion"),
            ("pow", vec!["1"; 497].join(" ** "), "recursion"),
            (
                "ternary",
                format!("{}1", "1 if 0 else ".repeat(496)),
                "recursion",
            ),
            (
                "brackets",
                nest("[", "1", "]", 201),
                "too many nested parentheses",
            ),
            (
                "parentheses",
                nest("(", "1", ")", 201),
                "too many nested parentheses",
            ),
            (
                "braces",
                nest("{", "x", "}", 201),
                "too many nested parentheses",
            ),
        ];
        for (name, src, expected) in past {
            let err = eval_text(&src).expect_err(name);
            assert!(err.contains(expected), "{name}: {err}");
        }
        // Deep nesting is an uncaught `RecursionError`, a bracket overflow a `SyntaxError`.
        let recursion = eval_expr_with_mapping(&format!("{}1", "-".repeat(500)), &Bindings::new());
        assert!(
            matches!(recursion, Err(ExprError::Uncaught(_))),
            "{recursion:?}"
        );
    });
}

#[test]
fn nested_blocks_stop_at_cpython_indentation_limit() {
    let nested = |levels: usize| {
        let mut src = String::new();
        for level in 0..levels {
            src.push_str(&" ".repeat(level));
            src.push_str("if x:\n");
        }
        src.push_str(&" ".repeat(levels));
        src.push_str("target = 1\n");
        src
    };
    // `ast.parse` accepts 99 indented levels and raises `IndentationError` for 100.
    assert!(parse_program(&nested(99)).is_ok());
    let err = parse_program(&nested(100)).expect_err("100 levels");
    assert!(
        err.to_string().contains("too many levels of indentation"),
        "{err}"
    );
}

// ── Allocation hazards ───────────────────────────────────────────────────────

#[test]

fn list_repetition_overflow_is_an_error() {
    let r = std::panic::catch_unwind(|| {
        eval_expr_with_mapping("[1, 2] * 9223372036854775807", &Bindings::new())
    });
    assert!(matches!(r, Ok(Err(_))), "expected an Err, got {r:?}");
}

#[test]

fn huge_list_repetition_is_an_error() {
    crash_isolated("huge_list_repetition_is_an_error", || {
        let r = eval_expr_with_mapping("[1] * 1000000000000", &Bindings::new());
        assert!(r.is_err(), "expected an Err, got {r:?}");
    });
}
