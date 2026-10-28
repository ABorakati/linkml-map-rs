//! Evaluator micro-benchmark: parse once, evaluate many times.
//!
//! `cargo run --release -p linkml-map-core --example expr_bench [ITERATIONS]`
//! prints nanoseconds per evaluation for a set of typical expressions, so
//! changes to the numeric tower can be checked for a regression on the `i64`
//! fast path.

use std::hint::black_box;
use std::time::Instant;

use indexmap::IndexMap;
use linkml_map_core::expr::{Bindings, eval_parsed, parse_expr};
use linkml_map_core::value::Value;

const CASES: &[(&str, &str)] = &[
    ("int add", "a + b"),
    ("int mul chain", "a * b + a * 3 - b * 2"),
    ("int floordiv/mod", "(a // 7) + (b % 5)"),
    ("int pow", "a ** 2 + b ** 3"),
    ("int shift/bit", "(a << 3) | (b & 255) ^ 5"),
    ("int compare", "a < b and b >= a and a != b"),
    ("float arith", "x * 2.5 + y / 3.0 - x ** 2"),
    ("int/float mix", "a * x + b / y"),
    ("true div", "a / b"),
    ("unary", "-a + +b - (~a)"),
    ("abs/round/int", "abs(a) + round(x) + int(y)"),
    ("sum list", "sum([a, b, 1, 2, 3, 4])"),
    ("str(int)", "str(a) + str(b)"),
    ("case", "case((a > 100, 'big'), (a > 10, 'mid'), (True, 'small'))"),
    ("numeric strings", "s * 3 + t"),
];

fn main() {
    let iterations: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(2_000_000);
    let mut vars: Bindings = IndexMap::new();
    vars.insert("a".into(), Value::Int(123_456));
    vars.insert("b".into(), Value::Int(789));
    vars.insert("x".into(), Value::Float(2.5));
    vars.insert("y".into(), Value::Float(7.25));
    vars.insert("s".into(), Value::Str("71".into()));
    vars.insert("t".into(), Value::Int(4));
    for (name, expr) in CASES {
        let parsed = parse_expr(expr).expect("parse");
        // Warm up, and fail loudly on an expression that stopped evaluating.
        eval_parsed(&parsed, &vars).expect("eval");
        let start = Instant::now();
        for _ in 0..iterations {
            black_box(eval_parsed(black_box(&parsed), black_box(&vars)).ok());
        }
        let per = start.elapsed().as_nanos() as f64 / iterations as f64;
        println!("{name:<20} {per:>9.1} ns   {expr}");
    }
}
