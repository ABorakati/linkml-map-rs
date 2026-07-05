//! Restricted expression evaluator for LinkML transformation expressions.
//!
//! Native Rust port of `linkml_map.utils.eval_utils`. See [`eval`] for the
//! evaluator semantics and the module-level tests for the ported doctests.

pub mod error;
pub mod eval;
pub mod lexer;
pub mod parser;
pub mod stmt;

pub use error::{ExprError, ExprResult};
pub use eval::{
    eval_expr, eval_expr_with_mapping, eval_expr_with_mapping_strict, eval_parsed,
    eval_parsed_strict, parse_expr, Bindings, ParsedExpr,
};
pub use parser::Ast;

#[cfg(test)]
mod tests {
    // PARITY: the 3.14 test literals mirror upstream Python doctests
    // (_try_numeric / _maybe_coerce_numeric in utils/eval_utils.py); keep the exact
    // value and silence approx_constant rather than diverge from the reference.
    #![allow(clippy::approx_constant)]
    use super::eval::{eval_expr, eval_expr_with_mapping, eval_parsed, parse_expr, Bindings};
    use crate::value::Value;
    use indexmap::IndexMap;

    // --- helpers -----------------------------------------------------------

    /// Evaluate a no-binding expression, unwrapping the result.
    fn e(expr: &str) -> Value {
        eval_expr(expr).expect("eval failed")
    }

    /// Evaluate with bindings supplied as (name, value) pairs.
    fn ev(expr: &str, vars: &[(&str, Value)]) -> Value {
        let mut m: Bindings = IndexMap::new();
        for (k, v) in vars {
            m.insert((*k).to_string(), v.clone());
        }
        eval_expr_with_mapping(expr, &m).expect("eval failed")
    }

    fn s(x: &str) -> Value {
        Value::Str(x.to_string())
    }

    // === eval_expr doctests ===============================================

    #[test]
    fn arithmetic_doctests() {
        // >>> eval_expr('2^6') == 4   (^ is bitwise XOR per doctest)
        assert_eq!(e("2^6"), Value::Int(4));
        // >>> eval_expr('2**6') == 64
        assert_eq!(e("2**6"), Value::Int(64));
        // >>> eval_expr('1 + 2*3**(4^5) / (6 + -7)') == -5.0
        assert_eq!(e("1 + 2*3**(4^5) / (6 + -7)"), Value::Float(-5.0));
        // >>> eval_expr('10 % 3') == 1
        assert_eq!(e("10 % 3"), Value::Int(1));
        // >>> eval_expr('7 // 2') == 3
        assert_eq!(e("7 // 2"), Value::Int(3));
    }

    #[test]
    fn variable_doctests() {
        // >>> eval_expr('{x} + {y}', x=1, y=2) == 3
        assert_eq!(
            ev("{x} + {y}", &[("x", Value::Int(1)), ("y", Value::Int(2))]),
            Value::Int(3)
        );
        // >>> eval_expr('x + y', x=1, y=2) == 3
        assert_eq!(
            ev("x + y", &[("x", Value::Int(1)), ("y", Value::Int(2))]),
            Value::Int(3)
        );
    }

    #[test]
    fn null_propagation_doctests() {
        // >>> print(eval_expr('{x} + {y}', x=None, y=2)) -> None
        assert_eq!(
            ev("{x} + {y}", &[("x", Value::Null), ("y", Value::Int(2))]),
            Value::Null
        );
        // >>> print(eval_expr('x + 1', x=None)) -> None
        assert_eq!(ev("x + 1", &[("x", Value::Null)]), Value::Null);
        // >>> print(eval_expr('x <= 0', x=None)) -> None
        assert_eq!(ev("x <= 0", &[("x", Value::Null)]), Value::Null);
        // >>> print(eval_expr('float(x)', x=None)) -> None
        assert_eq!(ev("float(x)", &[("x", Value::Null)]), Value::Null);
    }

    #[test]
    fn case_doctests() {
        // >>> eval_expr('case((x == "1", "YES"), (True, "NO"))', x=None) == 'NO'
        assert_eq!(
            ev(
                "case((x == \"1\", \"YES\"), (True, \"NO\"))",
                &[("x", Value::Null)]
            ),
            s("NO")
        );
        // >>> eval_expr('case(({x} == "1", "YES"), (True, "NO"))', x=None) == 'NO'
        assert_eq!(
            ev(
                "case(({x} == \"1\", \"YES\"), (True, \"NO\"))",
                &[("x", Value::Null)]
            ),
            s("NO")
        );
    }

    #[test]
    fn comparison_doctests() {
        // >>> eval_expr('1 != 2') -> True
        assert_eq!(e("1 != 2"), Value::Bool(true));
        // >>> eval_expr('1 != 1') -> False
        assert_eq!(e("1 != 1"), Value::Bool(false));
        // >>> eval_expr('"a" in "abc"') -> True
        assert_eq!(e("\"a\" in \"abc\""), Value::Bool(true));
        // >>> eval_expr('1 not in [2, 3]') -> True
        assert_eq!(e("1 not in [2, 3]"), Value::Bool(true));
    }

    #[test]
    fn logical_doctests() {
        // >>> eval_expr('True and False') -> False
        assert_eq!(e("True and False"), Value::Bool(false));
        // >>> eval_expr('True or False') -> True
        assert_eq!(e("True or False"), Value::Bool(true));
        // >>> eval_expr('not True') -> False
        assert_eq!(e("not True"), Value::Bool(false));
    }

    #[test]
    fn function_doctests() {
        // >>> eval_expr('strlen("a" + "bc")') == 3
        assert_eq!(e("strlen(\"a\" + \"bc\")"), Value::Int(3));
        // >>> eval_expr('abs(-5)') == 5
        assert_eq!(e("abs(-5)"), Value::Int(5));
        // >>> eval_expr('int("42")') == 42
        assert_eq!(e("int(\"42\")"), Value::Int(42));
    }

    // === eval_conditional doctest =========================================

    #[test]
    fn eval_conditional_doctest() {
        // >>> x = 10
        // >>> eval_conditional((x < 25, 'low'), (x > 25, 'high'), (True, 'low')) == 'low'
        assert_eq!(
            ev(
                "case((x < 25, \"low\"), (x > 25, \"high\"), (True, \"low\"))",
                &[("x", Value::Int(10))]
            ),
            s("low")
        );
    }

    // === _uuid5 doctests ==================================================

    #[test]
    fn uuid5_doctests() {
        // >>> ns = "https://example.org/ns"
        // >>> _uuid5(ns, "example") == _uuid5(ns, "example") -> True
        let a = ev(
            "uuid5(ns, \"example\")",
            &[("ns", s("https://example.org/ns"))],
        );
        let b = ev(
            "uuid5(ns, \"example\")",
            &[("ns", s("https://example.org/ns"))],
        );
        assert_eq!(a, b);
        // >>> _uuid5(ns, "example") != _uuid5(ns, "different") -> True
        let c = ev(
            "uuid5(ns, \"different\")",
            &[("ns", s("https://example.org/ns"))],
        );
        assert_ne!(a, c);

        // Cross-check against the exact Python two-level algorithm:
        // ns = uuid5(NAMESPACE_URL, "https://example.org/ns"); uuid5(ns, "example")
        let derived = uuid::Uuid::new_v5(
            &uuid::Uuid::NAMESPACE_URL,
            "https://example.org/ns".as_bytes(),
        );
        let expected = uuid::Uuid::new_v5(&derived, "example".as_bytes()).to_string();
        assert_eq!(a, Value::Str(expected));
    }

    // === _try_numeric doctests ============================================

    #[test]
    fn try_numeric_doctests() {
        // >>> _try_numeric(5) -> 5
        assert_eq!(Value::Int(5).try_numeric(), Some(5.0));
        // >>> _try_numeric(3.14) -> 3.14
        assert_eq!(Value::Float(3.14).try_numeric(), Some(3.14));
        // >>> _try_numeric("3.14") -> 3.14
        assert_eq!(s("3.14").try_numeric(), Some(3.14));
        // >>> _try_numeric("abc") -> None
        assert_eq!(s("abc").try_numeric(), None);
        // >>> _try_numeric(None) -> None
        assert_eq!(Value::Null.try_numeric(), None);
        // >>> _try_numeric(True) -> None
        assert_eq!(Value::Bool(true).try_numeric(), None);
    }

    // === _is_numeric doctests =============================================

    #[test]
    fn is_numeric_doctests() {
        // >>> _is_numeric("3.14") -> True
        assert!(s("3.14").is_numeric());
        // >>> _is_numeric("abc") -> False
        assert!(!s("abc").is_numeric());
        // >>> _is_numeric(5) -> True
        assert!(Value::Int(5).is_numeric());
        // >>> _is_numeric("") -> False
        assert!(!s("").is_numeric());
        // >>> _is_numeric(None) -> False
        assert!(!Value::Null.is_numeric());
        // >>> _is_numeric(True) -> False
        assert!(!Value::Bool(true).is_numeric());

        // exercised via the expression-level is_numeric() too
        assert_eq!(e("is_numeric(\"3.14\")"), Value::Bool(true));
        assert_eq!(e("is_numeric(\"abc\")"), Value::Bool(false));
    }

    // === _maybe_coerce_numeric behaviour ==================================
    // Coercion is private to the evaluator; exercised via comparisons.

    #[test]
    fn maybe_coerce_numeric_behaviour() {
        // _maybe_coerce_numeric(1, '1') -> (1, 1): 1 == '1' True after coerce
        assert_eq!(
            ev("x == y", &[("x", Value::Int(1)), ("y", s("1"))]),
            Value::Bool(true)
        );
        // _maybe_coerce_numeric('3.14', 3.14) -> (3.14, 3.14)
        assert_eq!(
            ev("x == y", &[("x", s("3.14")), ("y", Value::Float(3.14))]),
            Value::Bool(true)
        );
        // numeric-string ordering coercion from the module docstring
        assert_eq!(
            ev("x < y", &[("x", s("3.14")), ("y", Value::Int(4))]),
            Value::Bool(true)
        );
        // _maybe_coerce_numeric(1, 'abc') -> (1, 'abc'): 1 == 'abc' is False
        assert_eq!(
            ev("x == y", &[("x", Value::Int(1)), ("y", s("abc"))]),
            Value::Bool(false)
        );
        // _maybe_coerce_numeric('a', 'b') -> ('a', 'b'): 'a' == 'b' is False
        assert_eq!(
            ev("x == y", &[("x", s("a")), ("y", s("b"))]),
            Value::Bool(false)
        );
        // _maybe_coerce_numeric(True, '0') -> (True, '0'): bool not numeric, no coerce
        assert_eq!(
            ev("x == y", &[("x", Value::Bool(true)), ("y", s("0"))]),
            Value::Bool(false)
        );
    }

    // === _distributed_getattr doctests ====================================

    #[test]
    fn distributed_getattr_doctests() {
        // >>> _distributed_getattr([P("Alice"), P("Bob")], "name") -> ['Alice', 'Bob']
        let alice = {
            let mut m = IndexMap::new();
            m.insert("name".to_string(), s("Alice"));
            Value::Map(m)
        };
        let bob = {
            let mut m = IndexMap::new();
            m.insert("name".to_string(), s("Bob"));
            Value::Map(m)
        };
        let people = Value::List(vec![alice, bob]);
        assert_eq!(
            ev("people.name", &[("people", people)]),
            Value::List(vec![s("Alice"), s("Bob")])
        );

        // >>> _distributed_getattr(None, "name") is None -> True
        assert_eq!(ev("x.name", &[("x", Value::Null)]), Value::Null);

        // >>> _distributed_getattr(P("Alice"), "_secret") -> NameError (private attr)
        let mut m = IndexMap::new();
        m.insert("name".to_string(), s("Alice"));
        let mut binds: Bindings = IndexMap::new();
        binds.insert("p".to_string(), Value::Map(m));
        let err = eval_expr_with_mapping("p._secret", &binds);
        assert!(err.is_err(), "private attribute access must error");
        let msg = format!("{}", err.unwrap_err());
        assert!(msg.contains("private attribute"), "got: {msg}");
    }

    // === eval_expr_with_mapping contract ==================================

    #[test]
    fn none_literal_short_circuit() {
        // eval_expr_with_mapping: if expr == "None": return None
        let vars: Bindings = IndexMap::new();
        assert_eq!(eval_expr_with_mapping("None", &vars).unwrap(), Value::Null);
    }

    // === parse_expr / eval_parsed (cached-AST API) ========================

    #[test]
    fn parse_once_eval_many_matches_string_path() {
        // A parsed AST evaluated against fresh bindings each time must give the
        // same answer as the string path on every iteration.
        let parsed = parse_expr("{x} + {y}").expect("parse failed");
        for (x, y, want) in [(1i64, 2i64, 3i64), (10, 20, 30), (-5, 5, 0)] {
            let mut vars: Bindings = IndexMap::new();
            vars.insert("x".into(), Value::Int(x));
            vars.insert("y".into(), Value::Int(y));
            let cached = eval_parsed(&parsed, &vars).unwrap();
            let string_path = eval_expr_with_mapping("{x} + {y}", &vars).unwrap();
            assert_eq!(cached, Value::Int(want));
            assert_eq!(cached, string_path);
        }
    }

    #[test]
    fn parsed_none_literal_short_circuit() {
        // parse_expr("None") must reproduce the "None" short-circuit.
        let parsed = parse_expr("None").unwrap();
        let vars: Bindings = IndexMap::new();
        assert_eq!(eval_parsed(&parsed, &vars).unwrap(), Value::Null);
        assert_eq!(parsed.eval(&vars).unwrap(), Value::Null);
    }

    #[test]
    fn parse_expr_propagates_parse_errors() {
        // A syntactically invalid expr must error at parse time.
        assert!(parse_expr("{1 + 2}").is_err());
    }

    #[test]
    fn parsed_expr_is_send_sync_clone() {
        // Compile-time assertion that ParsedExpr can cross thread boundaries
        // and be shared via Arc across the pipeline workers.
        fn assert_send_sync<T: Send + Sync + Clone>() {}
        assert_send_sync::<super::ParsedExpr>();
        let p = parse_expr("x + 1").unwrap();
        let _clone = p.clone();
    }

    #[test]
    fn unbound_name_is_null() {
        // Unbound names resolve to Null, not an error.
        let vars: Bindings = IndexMap::new();
        assert_eq!(
            eval_expr_with_mapping("missing", &vars).unwrap(),
            Value::Null
        );
    }

    // === extra invariant coverage =========================================

    #[test]
    fn equality_native_not_null_propagating() {
        // None == "x" -> False (native equality, NOT null)
        assert_eq!(ev("x == \"x\"", &[("x", Value::Null)]), Value::Bool(false));
        // None == None -> True
        assert_eq!(
            ev("x == y", &[("x", Value::Null), ("y", Value::Null)]),
            Value::Bool(true)
        );
        // None != "x" -> True
        assert_eq!(ev("x != \"x\"", &[("x", Value::Null)]), Value::Bool(true));
    }

    #[test]
    fn string_concatenation_stays_native() {
        // str + str is concatenation, not numeric coercion.
        assert_eq!(e("\"a\" + \"bc\""), s("abc"));
        // numeric strings + numeric strings still concatenate
        assert_eq!(e("\"1\" + \"2\""), s("12"));
    }

    #[test]
    fn arithmetic_coercion_retry() {
        // "3" * 1.5 -> not native (str*float TypeError) -> coerce -> 4.5
        assert_eq!(
            ev("x * y", &[("x", s("3")), ("y", Value::Float(1.5))]),
            Value::Float(4.5)
        );
        // non-numeric operand -> warn + None
        assert_eq!(
            ev("x - y", &[("x", s("abc")), ("y", Value::Int(2))]),
            Value::Null
        );
    }

    #[test]
    fn scalar_function_distribution() {
        // strlen distributes over a list of strings
        let names = Value::List(vec![s("Alice"), s("Bob")]);
        assert_eq!(
            ev("strlen(names)", &[("names", names)]),
            Value::List(vec![Value::Int(5), Value::Int(3)])
        );
    }

    #[test]
    fn list_functions_take_list_directly() {
        let nums = Value::List(vec![Value::Int(3), Value::Int(1), Value::Int(2)]);
        assert_eq!(ev("max(xs)", &[("xs", nums.clone())]), Value::Int(3));
        assert_eq!(ev("min(xs)", &[("xs", nums.clone())]), Value::Int(1));
        assert_eq!(ev("len(xs)", &[("xs", nums)]), Value::Int(3));
        // null-safe: None arg -> None
        assert_eq!(ev("max(x)", &[("x", Value::Null)]), Value::Null);
    }

    #[test]
    fn membership_null_propagation() {
        // None in [..] -> None (membership is null-propagating)
        assert_eq!(
            ev(
                "x in xs",
                &[("x", Value::Null), ("xs", Value::List(vec![Value::Int(1)]))]
            ),
            Value::Null
        );
    }

    #[test]
    fn brace_must_enclose_single_variable() {
        let vars: Bindings = IndexMap::new();
        // {1 + 2} is not a Name/Attribute -> error
        assert!(eval_expr_with_mapping("{1 + 2}", &vars).is_err());
        // {x, y} encloses two -> error
        assert!(eval_expr_with_mapping("{x, y}", &vars).is_err());
        // {x.y} attribute is allowed
        let mut binds: Bindings = IndexMap::new();
        let mut m = IndexMap::new();
        m.insert("y".to_string(), Value::Int(7));
        binds.insert("x".to_string(), Value::Map(m));
        assert_eq!(
            eval_expr_with_mapping("{x.y}", &binds).unwrap(),
            Value::Int(7)
        );
    }

    #[test]
    fn chained_attribute_paths() {
        // person.address.street
        let mut addr = IndexMap::new();
        addr.insert("street".to_string(), s("Main St"));
        let mut person = IndexMap::new();
        person.insert("address".to_string(), Value::Map(addr));
        assert_eq!(
            ev("person.address.street", &[("person", Value::Map(person))]),
            s("Main St")
        );
    }

    // === single-expression subscript + comprehension (fast path) ==========

    #[test]
    fn subscript_on_list_and_string() {
        let xs = Value::List(vec![s("a"), s("b"), s("c")]);
        assert_eq!(ev("xs[0]", &[("xs", xs.clone())]), s("a"));
        assert_eq!(ev("xs[2]", &[("xs", xs.clone())]), s("c"));
        // negative index
        assert_eq!(ev("xs[-1]", &[("xs", xs)]), s("c"));
        // string subscript
        assert_eq!(ev("name[0]", &[("name", s("fred"))]), s("f"));
    }

    #[test]
    fn list_comprehension_for_in_if() {
        // [x for x in xs if x > 2]  →  [3, 4]
        let xs = Value::List(vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
            Value::Int(4),
        ]);
        assert_eq!(
            ev("[x for x in xs if x > 2]", &[("xs", xs)]),
            Value::List(vec![Value::Int(3), Value::Int(4)])
        );
    }

    #[test]
    fn comprehension_attribute_is_per_element() {
        // events with event_name + date; pull dates where name matches.
        let mk = |name: &str, date: &str| {
            let mut m = IndexMap::new();
            m.insert("event_name".to_string(), s(name));
            m.insert("important_event_date".to_string(), s(date));
            Value::Map(m)
        };
        let events = Value::List(vec![
            mk("BIRTH", "1990-01-01"),
            mk("PASSED_DRIVING_TEST", "2014-03-31"),
        ]);
        assert_eq!(
            ev(
                "[x.important_event_date for x in events if str(x.event_name) == \"PASSED_DRIVING_TEST\"]",
                &[("events", events)]
            ),
            Value::List(vec![s("2014-03-31")])
        );
    }

    #[test]
    fn empty_comprehension_yields_empty_list() {
        let xs = Value::List(vec![Value::Int(1)]);
        assert_eq!(
            ev("[x for x in xs if x > 99]", &[("xs", xs)]),
            Value::List(vec![])
        );
        // comprehension over Null iterable → empty list
        assert_eq!(
            ev("[x for x in xs if x]", &[("xs", Value::Null)]),
            Value::List(vec![])
        );
    }

    // === multi-statement programs (asteval-style `target` contract) =======

    /// Build the personinfo-style `src` object with a list of life events.
    fn src_with_events(events: Vec<(&str, &str)>) -> Bindings {
        let mut binds: Bindings = IndexMap::new();
        let evs: Vec<Value> = events
            .into_iter()
            .map(|(name, date)| {
                let mut m = IndexMap::new();
                m.insert("event_name".to_string(), s(name));
                m.insert("important_event_date".to_string(), s(date));
                Value::Map(m)
            })
            .collect();
        let mut src = IndexMap::new();
        src.insert("has_important_life_events".to_string(), Value::List(evs));
        binds.insert("src".to_string(), Value::Map(src));
        binds
    }

    #[test]
    fn program_assignment_then_target() {
        // `target` symbol is the result; assignment binds it.
        let vars: Bindings = IndexMap::new();
        let parsed = parse_expr("d = 5\ntarget = d + 1").unwrap();
        assert!(parsed.is_program(), "should route to the program path");
        assert_eq!(parsed.eval(&vars).unwrap(), Value::Int(6));
    }

    #[test]
    fn program_target_unset_returns_null() {
        // If `target` is never assigned (guard did not fire), result is Null.
        let vars = src_with_events(vec![("BIRTH", "1990-01-01")]);
        let expr = "d_test = [x.important_event_date for x in src.has_important_life_events if str(x.event_name) == \"DEATH\"]\nif len(d_test):\n    target = d_test[0]";
        let parsed = parse_expr(expr).unwrap();
        assert!(parsed.is_program());
        assert_eq!(parsed.eval(&vars).unwrap(), Value::Null);
    }

    #[test]
    fn program_driving_since_block() {
        // The full personinfo `driving_since` block: comprehension + len guard
        // + subscript [0].
        let vars = src_with_events(vec![
            ("BIRTH", "1990-01-01"),
            ("PASSED_DRIVING_TEST", "2014-03-31"),
            ("FIRST_CAR_ACCIDENT", "2015-01-25"),
        ]);
        let expr = "d_test = [x.important_event_date for x in src.has_important_life_events if str(x.event_name) == \"PASSED_DRIVING_TEST\"]\nif len(d_test):\n    target = d_test[0]";
        let parsed = parse_expr(expr).unwrap();
        assert_eq!(parsed.eval(&vars).unwrap(), s("2014-03-31"));
    }

    #[test]
    fn program_single_line_if_first_event() {
        // `first_known_event`: indented single-statement body referencing a
        // subscripted attribute path.
        let vars = src_with_events(vec![("BIRTH", "1990-01-01")]);
        let expr =
            "if src.has_important_life_events:\n  target = src.has_important_life_events[0].important_event_date";
        let parsed = parse_expr(expr).unwrap();
        assert_eq!(parsed.eval(&vars).unwrap(), s("1990-01-01"));
    }

    #[test]
    fn program_missing_src_attr_is_null_not_distributed() {
        // In object mode, `src.<missing>` returns Null (DynObj semantics),
        // never distributing over the map's values into a list.
        let mut binds: Bindings = IndexMap::new();
        let mut src = IndexMap::new();
        src.insert("id".to_string(), s("P:002"));
        src.insert("name".to_string(), s("Alison"));
        binds.insert("src".to_string(), Value::Map(src));
        let expr = "death_dates = [x.important_event_date for x in src.has_important_life_events if str(x.event_name) == \"DEATH\"]\nif len(death_dates):\n  target = death_dates[0]";
        let parsed = parse_expr(expr).unwrap();
        // No life-events key → comprehension over Null → [] → target unset → Null.
        assert_eq!(parsed.eval(&binds).unwrap(), Value::Null);
    }

    #[test]
    fn single_expr_still_takes_fast_path() {
        // A plain expression must NOT be classified as a program.
        let parsed = parse_expr("str({age_in_years}) + ' years'").unwrap();
        assert!(!parsed.is_program(), "single expr must keep the fast path");
        let mut vars: Bindings = IndexMap::new();
        vars.insert("age_in_years".to_string(), Value::Int(33));
        assert_eq!(parsed.eval(&vars).unwrap(), s("33 years"));
    }
}
