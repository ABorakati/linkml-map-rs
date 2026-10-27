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
    Bindings, eval_expr_with_mapping, eval_expr_with_mapping_strict, eval_parsed,
    eval_parsed_strict, expression_asts, parse_expr,
};
use linkml_map_core::value::Value;
use proptest::prelude::*;

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
deep_case!(
    #[ignore = "bug: parser stack overflow (SIGABRT) at ~600 nested parentheses in release (2 MiB stack)"]
    deep_parentheses,
    nest("(", "1", ")", DEPTH)
);
deep_case!(
    #[ignore = "bug: parser stack overflow at ~600 nested brackets in release (2 MiB stack)"]
    deep_brackets,
    nest("[", "1", "]", DEPTH)
);
deep_case!(
    #[ignore = "bug: parser stack overflow at ~600 nested calls in release (2 MiB stack)"]
    deep_nested_calls,
    nest("len(", "x", ")", DEPTH)
);
deep_case!(
    #[ignore = "bug: parser stack overflow at ~600 nested braces in release (2 MiB stack)"]
    deep_braces,
    nest("{", "x", "}", DEPTH)
);
deep_case!(
    #[ignore = "bug: stack overflow at ~1700 chained unary minus (eval) / ~11k (parse) in release"]
    deep_unary_minus,
    format!("{}1", "-".repeat(DEPTH))
);
deep_case!(
    #[ignore = "bug: stack overflow at ~1700 chained `not` (eval) / ~4.7k (parse) in release"]
    deep_not,
    format!("{}x", "not ".repeat(DEPTH))
);
deep_case!(
    #[ignore = "bug: eval stack overflow at ~1700 chained subscripts in release (parse: ~33k)"]
    deep_subscripts,
    format!("x{}", "[0]".repeat(DEPTH))
);
deep_case!(
    #[ignore = "bug: eval stack overflow at ~1700 chained attributes in release (parse: ~33k)"]
    deep_attributes,
    format!("x{}", ".a".repeat(DEPTH))
);
deep_case!(
    #[ignore = "bug: eval stack overflow at ~1700 terms of a flat `1 + 1 + ...` in release (parse: ~33k)"]
    long_addition_chain,
    vec!["1"; DEPTH].join(" + ")
);
deep_case!(
    #[ignore = "bug: stack overflow at ~2700 terms of `1 ** 1 ** ...` (parse), ~1700 (eval) in release"]
    long_power_chain,
    vec!["1"; DEPTH].join(" ** ")
);
deep_case!(
    #[ignore = "bug: statement parser stack overflow on deeply nested `if` blocks"]
    deep_if_blocks,
    {
        let mut s = String::new();
        for i in 0..DEPTH / 10 {
            s.push_str(&" ".repeat(i));
            s.push_str("if x:\n");
        }
        s.push_str(&" ".repeat(DEPTH / 10));
        s.push_str("target = 1\n");
        s
    }
);

// ── Allocation hazards ───────────────────────────────────────────────────────

#[test]
#[ignore = "bug: `[1, 2] * 9223372036854775807` panics with `capacity overflow` (eval.rs list repetition: items.len() * n); CPython raises MemoryError"]
fn list_repetition_overflow_is_an_error() {
    let r = std::panic::catch_unwind(|| {
        eval_expr_with_mapping("[1, 2] * 9223372036854775807", &Bindings::new())
    });
    assert!(matches!(r, Ok(Err(_))), "expected an Err, got {r:?}");
}

#[test]
#[ignore = "bug: `[1] * 1000000000000` aborts the process (memory allocation of 72 TB failed); CPython raises MemoryError"]
fn huge_list_repetition_is_an_error() {
    crash_isolated("huge_list_repetition_is_an_error", || {
        let r = eval_expr_with_mapping("[1] * 1000000000000", &Bindings::new());
        assert!(r.is_err(), "expected an Err, got {r:?}");
    });
}
