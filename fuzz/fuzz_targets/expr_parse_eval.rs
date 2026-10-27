#![no_main]
//! Expression lexer + parser + evaluator over arbitrary text and bindings.
//!
//! Input layout: the expression text, a NUL byte, then arbitrary bytes that decide the
//! variable bindings. A seed corpus of plain expression files therefore works as is.

use fuzz_support::{FuzzValue, MAX_EXPR_LEN, UNGUARDED, has_repetition};
use libfuzzer_sys::{
    arbitrary::{Arbitrary, Unstructured},
    fuzz_target,
};
use linkml_map_core::expr::lexer::lex;
use linkml_map_core::expr::parser::parse;
use linkml_map_core::expr::stmt::parse_program;
use linkml_map_core::expr::{
    Bindings, EvalMode, eval_enum_parsed, eval_expr_with_mapping, eval_expr_with_mapping_strict,
    eval_parsed_gated, expression_asts, parse_expr,
};
use linkml_map_core::value::Value;

fuzz_target!(|data: &[u8]| {
    let (text, rest) = match data.iter().position(|&b| b == 0) {
        Some(i) => (&data[..i], &data[i + 1..]),
        None => (data, &data[..0]),
    };
    let Ok(src) = std::str::from_utf8(text) else {
        return;
    };
    if !UNGUARDED && (src.len() > MAX_EXPR_LEN || has_repetition(src)) {
        return;
    }
    let mut u = Unstructured::new(rest);
    let vars: Bindings = Vec::<(String, FuzzValue)>::arbitrary(&mut u)
        .unwrap_or_default()
        .into_iter()
        .map(|(k, v)| (k, v.into_value()))
        .collect();
    let _ = lex(src);
    let _ = parse(src);
    let _ = parse_program(src);
    let _ = expression_asts(src);
    // The restricted/unrestricted gate, slot and enum entry points, lax and strict.
    if let Ok(parsed) = parse_expr(src) {
        let enum_src = Value::Map(vars.clone());
        for strict in [false, true] {
            for unrestricted in [false, true] {
                let mode = EvalMode { strict, unrestricted };
                let a = eval_parsed_gated(&parsed, &vars, mode);
                assert_eq!(format!("{a:?}"), format!("{:?}", eval_parsed_gated(&parsed, &vars, mode)));
                let _ = eval_enum_parsed(&parsed, &vars, &enum_src, mode);
            }
        }
    }
    let lax = eval_expr_with_mapping(src, &vars);
    let strict = eval_expr_with_mapping_strict(src, &vars);
    // Strict mode only ever turns a success into an error, never into another value.
    if let (Ok(a), Ok(b)) = (&lax, &strict) {
        assert_eq!(format!("{a:?}"), format!("{b:?}"));
    }
    assert_eq!(format!("{lax:?}"), format!("{:?}", eval_expr_with_mapping(src, &vars)));
});
