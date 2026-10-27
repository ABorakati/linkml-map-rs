//! Tests for the conditional expression, dict/set displays, comprehensions,
//! generator expressions, keyword arguments and the `dict()`/`set()`
//! constructors (`parser.rs`, `compound.rs` and the matching `eval.rs` arms).
//!
//! Ports of the upstream pytest cases in `tests/test_utils/test_eval_utils.py`
//! (named after them), a table whose expected values were produced by running the
//! same expressions through upstream `linkml-map` v0.6.0-rc1 itself (`RESTRICTED_CASES`:
//! `eval_expr_with_mapping`, simpleeval; `UNRESTRICTED_CASES`: `asteval`; sets are
//! compared sorted because Python iterates them in hash order), and cases for the places this
//! evaluator deliberately differs (each noted `PARITY`).

use serde_json::{Value as Json, json};

use super::error::ExprError;
use super::eval::{Bindings, eval_expr_with_mapping};
use super::with_restricted_syntax;
use crate::value::Value;

fn bindings(vars: &Json) -> Bindings {
    vars.as_object()
        .expect("bindings must be a JSON object")
        .iter()
        .map(|(k, v)| (k.clone(), Value::from(v)))
        .collect()
}

fn eval(expr: &str, vars: Json) -> Result<Value, ExprError> {
    eval_expr_with_mapping(expr, &bindings(&vars))
}

#[track_caller]
fn expect(expr: &str, vars: Json, expected: Json) {
    let got = eval(expr, vars).unwrap_or_else(|e| panic!("{expr}: {e}"));
    assert_eq!(got, Value::from(&expected), "{expr}");
}

#[track_caller]
fn expect_err(expr: &str, vars: Json, fragment: &str) {
    match eval(expr, vars) {
        Ok(v) => panic!("{expr}: expected an error containing {fragment:?}, got {v:?}"),
        Err(e) => assert!(
            e.to_string().contains(fragment),
            "{expr}: {e} does not contain {fragment:?}"
        ),
    }
}

/// `(expression, bindings, kind, result)` from upstream's restricted evaluator
/// (`eval_expr_with_mapping`): kind `ok`, `set` (result sorted) or `err` (the
/// exception).
const RESTRICTED_CASES: &[(&str, &str, &str, &str)] = &[
    (
        r##"'yes' if x else 'no'"##,
        r##"{"x": 1}"##,
        r##"ok"##,
        r##""yes""##,
    ),
    (
        r##"'yes' if x else 'no'"##,
        r##"{"x": 0}"##,
        r##"ok"##,
        r##""no""##,
    ),
    (
        r##"'yes' if x else 'no'"##,
        r##"{"x": null}"##,
        r##"ok"##,
        r##""no""##,
    ),
    (
        r##"'yes' if x else 'no'"##,
        r##"{"x": ""}"##,
        r##"ok"##,
        r##""no""##,
    ),
    (
        r##"'yes' if x else 'no'"##,
        r##"{"x": "s"}"##,
        r##"ok"##,
        r##""yes""##,
    ),
    (
        r##"'yes' if x else 'no'"##,
        r##"{"x": []}"##,
        r##"ok"##,
        r##""no""##,
    ),
    (
        r##"'yes' if x else 'no'"##,
        r##"{"x": [0]}"##,
        r##"ok"##,
        r##""yes""##,
    ),
    (
        r##"'yes' if x else 'no'"##,
        r##"{"x": {}}"##,
        r##"ok"##,
        r##""no""##,
    ),
    (
        r##"'yes' if x else 'no'"##,
        r##"{"x": {"a": 1}}"##,
        r##"ok"##,
        r##""yes""##,
    ),
    (
        r##"'yes' if x else 'no'"##,
        r##"{"x": 0.0}"##,
        r##"ok"##,
        r##""no""##,
    ),
    (
        r##"'yes' if x else 'no'"##,
        r##"{"x": 2.5}"##,
        r##"ok"##,
        r##""yes""##,
    ),
    (
        r##"'yes' if x else 'no'"##,
        r##"{"x": false}"##,
        r##"ok"##,
        r##""no""##,
    ),
    (
        r##"'yes' if x else 'no'"##,
        r##"{"x": true}"##,
        r##"ok"##,
        r##""yes""##,
    ),
    (
        r##"'neg' if x < 0 else 'zero' if x == 0 else 'pos'"##,
        r##"{"x": -5}"##,
        r##"ok"##,
        r##""neg""##,
    ),
    (
        r##"x if x > 0 else -x"##,
        r##"{"x": -5}"##,
        r##"ok"##,
        r##"5"##,
    ),
    (
        r##"x + 1 if x else 0"##,
        r##"{"x": -5}"##,
        r##"ok"##,
        r##"-4"##,
    ),
    (
        r##"'neg' if x < 0 else 'zero' if x == 0 else 'pos'"##,
        r##"{"x": 0}"##,
        r##"ok"##,
        r##""zero""##,
    ),
    (
        r##"x if x > 0 else -x"##,
        r##"{"x": 0}"##,
        r##"ok"##,
        r##"0"##,
    ),
    (
        r##"x + 1 if x else 0"##,
        r##"{"x": 0}"##,
        r##"ok"##,
        r##"0"##,
    ),
    (
        r##"'neg' if x < 0 else 'zero' if x == 0 else 'pos'"##,
        r##"{"x": 5}"##,
        r##"ok"##,
        r##""pos""##,
    ),
    (
        r##"x if x > 0 else -x"##,
        r##"{"x": 5}"##,
        r##"ok"##,
        r##"5"##,
    ),
    (
        r##"x + 1 if x else 0"##,
        r##"{"x": 5}"##,
        r##"ok"##,
        r##"6"##,
    ),
    (
        r##"'ok' if True else boom(1)"##,
        r##"{}"##,
        r##"ok"##,
        r##""ok""##,
    ),
    (
        r##"'ok' if False else 'fine'"##,
        r##"{}"##,
        r##"ok"##,
        r##""fine""##,
    ),
    (
        r##"boom(1) if False else 'lazy'"##,
        r##"{}"##,
        r##"ok"##,
        r##""lazy""##,
    ),
    (
        r##"(1 if a else 2) if b else 3"##,
        r##"{"a": true, "b": true}"##,
        r##"ok"##,
        r##"1"##,
    ),
    (
        r##"(1 if a else 2) if b else 3"##,
        r##"{"a": false, "b": true}"##,
        r##"ok"##,
        r##"2"##,
    ),
    (
        r##"(1 if a else 2) if b else 3"##,
        r##"{"a": false, "b": false}"##,
        r##"ok"##,
        r##"3"##,
    ),
    (
        r##"1 if x else 2 + 3"##,
        r##"{"x": 0}"##,
        r##"ok"##,
        r##"5"##,
    ),
    (
        r##"1 if x else 2 + 3"##,
        r##"{"x": 1}"##,
        r##"ok"##,
        r##"1"##,
    ),
    (
        r##"not x if y else z"##,
        r##"{"x": 0, "y": 1, "z": "zz"}"##,
        r##"ok"##,
        r##"true"##,
    ),
    (
        r##"not x if y else z"##,
        r##"{"x": 0, "y": 0, "z": "zz"}"##,
        r##"ok"##,
        r##""zz""##,
    ),
    (
        r##"a or b if c else d"##,
        r##"{"a": 0, "b": "B", "c": 1, "d": "D"}"##,
        r##"ok"##,
        r##""B""##,
    ),
    (
        r##"a or b if c else d"##,
        r##"{"a": 0, "b": "B", "c": 0, "d": "D"}"##,
        r##"ok"##,
        r##""D""##,
    ),
    (
        r##"a if a else b if b else 'none'"##,
        r##"{"a": 0, "b": 0}"##,
        r##"ok"##,
        r##""none""##,
    ),
    (
        r##"a if a else b if b else 'none'"##,
        r##"{"a": 0, "b": "B"}"##,
        r##"ok"##,
        r##""B""##,
    ),
    (
        r##"a if a else b if b else 'none'"##,
        r##"{"a": "A", "b": "B"}"##,
        r##"ok"##,
        r##""A""##,
    ),
    (
        r##"len(x) if x else 0"##,
        r##"{"x": [1, 2, 3]}"##,
        r##"ok"##,
        r##"3"##,
    ),
    (
        r##"len(x) if x else 0"##,
        r##"{"x": []}"##,
        r##"ok"##,
        r##"0"##,
    ),
    (
        r##"[a if a > 1 else 0 for a in xs]"##,
        r##"{"xs": [0, 1, 2, 3]}"##,
        r##"ok"##,
        r##"[0, 0, 2, 3]"##,
    ),
    (
        r##"[a for a in xs if (a if a else 0)]"##,
        r##"{"xs": [0, 1, 2]}"##,
        r##"ok"##,
        r##"[1, 2]"##,
    ),
    (
        r##"{'k': 1 if x else 2}"##,
        r##"{"x": 0}"##,
        r##"ok"##,
        r##"{"k": 2}"##,
    ),
    (
        r##"{'k': 1 if x else 2}"##,
        r##"{"x": 1}"##,
        r##"ok"##,
        r##"{"k": 1}"##,
    ),
    (
        r##"x if y > 0 and y < 10 else 'out'"##,
        r##"{"x": "in", "y": 5}"##,
        r##"ok"##,
        r##""in""##,
    ),
    (
        r##"x if y > 0 and y < 10 else 'out'"##,
        r##"{"x": "in", "y": 50}"##,
        r##"ok"##,
        r##""out""##,
    ),
    (
        r##"'EQ' if x == y else 'NEQ'"##,
        r##"{"x": 1, "y": 1}"##,
        r##"ok"##,
        r##""EQ""##,
    ),
    (
        r##"'EQ' if x == y else 'NEQ'"##,
        r##"{"x": 1, "y": 2}"##,
        r##"ok"##,
        r##""NEQ""##,
    ),
    (
        r##"'a' if x in ['a', 'b'] else 'c'"##,
        r##"{"x": "b"}"##,
        r##"ok"##,
        r##""a""##,
    ),
    (
        r##"'a' if x not in ['a', 'b'] else 'c'"##,
        r##"{"x": "b"}"##,
        r##"ok"##,
        r##""c""##,
    ),
    (
        r##"'big' if x >= 10 else 'small'"##,
        r##"{"x": 10}"##,
        r##"ok"##,
        r##""big""##,
    ),
    (r##"{}"##, r##"{}"##, r##"ok"##, r##"{}"##),
    (r##"{ }"##, r##"{}"##, r##"ok"##, r##"{}"##),
    (r##"{'a': 1}"##, r##"{}"##, r##"ok"##, r##"{"a": 1}"##),
    (r##"{'a': 1,}"##, r##"{}"##, r##"ok"##, r##"{"a": 1}"##),
    (
        r##"{ 'a' : 1 , 'b' : 2 }"##,
        r##"{}"##,
        r##"ok"##,
        r##"{"a": 1, "b": 2}"##,
    ),
    (
        r##"{'a': 1, 'b': [1, 2], 'c': {'d': 3}}"##,
        r##"{}"##,
        r##"ok"##,
        r##"{"a": 1, "b": [1, 2], "c": {"d": 3}}"##,
    ),
    (r##"{'a': x}"##, r##"{"x": 7}"##, r##"ok"##, r##"{"a": 7}"##),
    (r##"{x: 1}"##, r##"{"x": "k"}"##, r##"ok"##, r##"{"k": 1}"##),
    (
        r##"{x: y}"##,
        r##"{"x": "k", "y": [1, 2]}"##,
        r##"ok"##,
        r##"{"k": [1, 2]}"##,
    ),
    (
        r##"{'a': 1, 'a': 2}"##,
        r##"{}"##,
        r##"ok"##,
        r##"{"a": 2}"##,
    ),
    (
        r##"{'a': 1, 'b': 2, 'a': 3}"##,
        r##"{}"##,
        r##"ok"##,
        r##"{"a": 3, "b": 2}"##,
    ),
    (r##"{'a': 1}['a']"##, r##"{}"##, r##"ok"##, r##"1"##),
    (
        r##"{'a': {'b': 2}}['a']['b']"##,
        r##"{}"##,
        r##"ok"##,
        r##"2"##,
    ),
    (r##"len({'a': 1, 'b': 2})"##, r##"{}"##, r##"ok"##, r##"2"##),
    (r##"'a' in {'a': 1}"##, r##"{}"##, r##"ok"##, r##"true"##),
    (r##"'z' in {'a': 1}"##, r##"{}"##, r##"ok"##, r##"false"##),
    (
        r##"'z' not in {'a': 1}"##,
        r##"{}"##,
        r##"ok"##,
        r##"true"##,
    ),
    (r##"len({})"##, r##"{}"##, r##"ok"##, r##"0"##),
    (r##"{} == {}"##, r##"{}"##, r##"ok"##, r##"true"##),
    (
        r##"{'a': 1} == {'a': 1}"##,
        r##"{}"##,
        r##"ok"##,
        r##"true"##,
    ),
    (
        r##"{'a': 1} == {'a': 2}"##,
        r##"{}"##,
        r##"ok"##,
        r##"false"##,
    ),
    (r##"{1: 'a'}"##, r##"{}"##, r##"ok"##, r##"{"1": "a"}"##),
    (
        r##"{1: 'a', 2: 'b'}"##,
        r##"{}"##,
        r##"ok"##,
        r##"{"1": "a", "2": "b"}"##,
    ),
    (r##"{1.5: 'a'}"##, r##"{}"##, r##"ok"##, r##"{"1.5": "a"}"##),
    (
        r##"{'a' + 'b': 1}"##,
        r##"{}"##,
        r##"ok"##,
        r##"{"ab": 1}"##,
    ),
    (
        r##"{'a': 1 + 2, 'b': 'x' + 'y'}"##,
        r##"{}"##,
        r##"ok"##,
        r##"{"a": 3, "b": "xy"}"##,
    ),
    (
        r##"[{'id': x} for x in xs]"##,
        r##"{"xs": ["a", "b"]}"##,
        r##"ok"##,
        r##"[{"id": "a"}, {"id": "b"}]"##,
    ),
    (
        r##"{'xs': [x * 2 for x in xs]}"##,
        r##"{"xs": [1, 2]}"##,
        r##"ok"##,
        r##"{"xs": [2, 4]}"##,
    ),
    (r##"x or {}"##, r##"{"x": null}"##, r##"ok"##, r##"{}"##),
    (
        r##"x or {}"##,
        r##"{"x": {"a": 1}}"##,
        r##"ok"##,
        r##"{"a": 1}"##,
    ),
    (
        r##"{'a': 1} if x else {}"##,
        r##"{"x": 1}"##,
        r##"ok"##,
        r##"{"a": 1}"##,
    ),
    (
        r##"{'a': 1} if x else {}"##,
        r##"{"x": 0}"##,
        r##"ok"##,
        r##"{}"##,
    ),
    (r##"bool({})"##, r##"{}"##, r##"ok"##, r##"false"##),
    (r##"bool({'a': 1})"##, r##"{}"##, r##"ok"##, r##"true"##),
    (r##"{'k': x}['k']"##, r##"{"x": 3}"##, r##"ok"##, r##"3"##),
    (r##"{'a': None}"##, r##"{}"##, r##"ok"##, r##"{"a": null}"##),
    (
        r##"{'a': True, 'b': False, 'c': None}"##,
        r##"{}"##,
        r##"ok"##,
        r##"{"a": true, "b": false, "c": null}"##,
    ),
    (r##"dict()"##, r##"{}"##, r##"ok"##, r##"{}"##),
    (r##"dict(a=1)"##, r##"{}"##, r##"ok"##, r##"{"a": 1}"##),
    (
        r##"dict(a=1, b=2)"##,
        r##"{}"##,
        r##"ok"##,
        r##"{"a": 1, "b": 2}"##,
    ),
    (r##"dict(a=1)['a']"##, r##"{}"##, r##"ok"##, r##"1"##),
    (
        r##"dict([('a', 1), ('b', 2)])"##,
        r##"{}"##,
        r##"ok"##,
        r##"{"a": 1, "b": 2}"##,
    ),
    (
        r##"dict([['a', 1], ['b', 2]])"##,
        r##"{}"##,
        r##"ok"##,
        r##"{"a": 1, "b": 2}"##,
    ),
    (
        r##"dict([('a', 1), ('a', 2)])"##,
        r##"{}"##,
        r##"ok"##,
        r##"{"a": 2}"##,
    ),
    (r##"dict({'a': 1})"##, r##"{}"##, r##"ok"##, r##"{"a": 1}"##),
    (
        r##"dict({'a': 1}, b=2)"##,
        r##"{}"##,
        r##"ok"##,
        r##"{"a": 1, "b": 2}"##,
    ),
    (
        r##"dict({'a': 1}, a=2)"##,
        r##"{}"##,
        r##"ok"##,
        r##"{"a": 2}"##,
    ),
    (
        r##"dict([('a', 1)], a=2)"##,
        r##"{}"##,
        r##"ok"##,
        r##"{"a": 2}"##,
    ),
    (
        r##"dict(x)"##,
        r##"{"x": {"a": 1, "b": [1]}}"##,
        r##"ok"##,
        r##"{"a": 1, "b": [1]}"##,
    ),
    (
        r##"dict(x, c=3)"##,
        r##"{"x": {"a": 1}}"##,
        r##"ok"##,
        r##"{"a": 1, "c": 3}"##,
    ),
    (
        r##"dict(pairs)"##,
        r##"{"pairs": [["a", 1], ["b", 2]]}"##,
        r##"ok"##,
        r##"{"a": 1, "b": 2}"##,
    ),
    (
        r##"dict([(1, 'x')])"##,
        r##"{}"##,
        r##"ok"##,
        r##"{"1": "x"}"##,
    ),
    (
        r##"dict(a=1 if x else 2)"##,
        r##"{"x": 0}"##,
        r##"ok"##,
        r##"{"a": 2}"##,
    ),
    (
        r##"dict(a=[1, 2], b={'c': 3})"##,
        r##"{}"##,
        r##"ok"##,
        r##"{"a": [1, 2], "b": {"c": 3}}"##,
    ),
    (r##"dict([])"##, r##"{}"##, r##"ok"##, r##"{}"##),
    (
        r##"dict(['ab', 'cd'])"##,
        r##"{}"##,
        r##"ok"##,
        r##"{"a": "b", "c": "d"}"##,
    ),
    (r##"len(dict(a=1, b=2))"##, r##"{}"##, r##"ok"##, r##"2"##),
    (
        r##"dict((k, v) for k, v in pairs)"##,
        r##"{"pairs": [["a", 1], ["b", 2]]}"##,
        r##"ok"##,
        r##"{"a": 1, "b": 2}"##,
    ),
    (
        r##"dict([(x, len(x)) for x in xs])"##,
        r##"{"xs": ["aa", "b"]}"##,
        r##"ok"##,
        r##"{"aa": 2, "b": 1}"##,
    ),
    (
        r##"dict(zero=0)"##,
        r##"{}"##,
        r##"ok"##,
        r##"{"zero": 0}"##,
    ),
    (r##"'a' in dict(a=1)"##, r##"{}"##, r##"ok"##, r##"true"##),
    (r##"len(set())"##, r##"{}"##, r##"ok"##, r##"0"##),
    (
        r##"sorted(set([3, 1, 2, 3]))"##,
        r##"{}"##,
        r##"ok"##,
        r##"[1, 2, 3]"##,
    ),
    (
        r##"len(set([3, 1, 2, 3]))"##,
        r##"{}"##,
        r##"ok"##,
        r##"3"##,
    ),
    (
        r##"len(set([1, 1.0, True]))"##,
        r##"{}"##,
        r##"ok"##,
        r##"1"##,
    ),
    (r##"len(set('hello'))"##, r##"{}"##, r##"ok"##, r##"4"##),
    (
        r##"sorted(set('hello'))"##,
        r##"{}"##,
        r##"ok"##,
        r##"["e", "h", "l", "o"]"##,
    ),
    (
        r##"sorted(set(['b', 'a', 'b']))"##,
        r##"{}"##,
        r##"ok"##,
        r##"["a", "b"]"##,
    ),
    (r##"2 in set([1, 2])"##, r##"{}"##, r##"ok"##, r##"true"##),
    (r##"5 in set([1, 2])"##, r##"{}"##, r##"ok"##, r##"false"##),
    (
        r##"5 not in set([1, 2])"##,
        r##"{}"##,
        r##"ok"##,
        r##"true"##,
    ),
    (
        r##"sorted(set(x))"##,
        r##"{"x": {"b": 1, "a": 2}}"##,
        r##"ok"##,
        r##"["a", "b"]"##,
    ),
    (
        r##"len(set(x))"##,
        r##"{"x": ["a", "a", "b"]}"##,
        r##"ok"##,
        r##"2"##,
    ),
    (r##"list(set())"##, r##"{}"##, r##"ok"##, r##"[]"##),
    (
        r##"sorted(set(set([2, 1])))"##,
        r##"{}"##,
        r##"ok"##,
        r##"[1, 2]"##,
    ),
    (
        r##"len(set([None, None, 1]))"##,
        r##"{}"##,
        r##"ok"##,
        r##"2"##,
    ),
    (
        r##"len(set([0, False, 0.0]))"##,
        r##"{}"##,
        r##"ok"##,
        r##"1"##,
    ),
    (r##"len(set([1, 2]) )"##, r##"{}"##, r##"ok"##, r##"2"##),
    (r##"sum(set([1, 1, 2]))"##, r##"{}"##, r##"ok"##, r##"3"##),
    (r##"max(set([1, 5, 2]))"##, r##"{}"##, r##"ok"##, r##"5"##),
    (
        r##"sorted(set([x, y]))"##,
        r##"{"x": "b", "y": "a"}"##,
        r##"ok"##,
        r##"["a", "b"]"##,
    ),
    (r##"len(set(['a', 'A']))"##, r##"{}"##, r##"ok"##, r##"2"##),
    (r##"bool(set())"##, r##"{}"##, r##"ok"##, r##"false"##),
    (r##"bool(set([0]))"##, r##"{}"##, r##"ok"##, r##"true"##),
    (
        r##"len(set([float(1), 1]))"##,
        r##"{}"##,
        r##"ok"##,
        r##"1"##,
    ),
    (
        r##"sorted(set([1.5, 1.5, 2.5]))"##,
        r##"{}"##,
        r##"ok"##,
        r##"[1.5, 2.5]"##,
    ),
    (
        r##"sorted({3, 1, 2})"##,
        r##"{}"##,
        r##"err"##,
        r##"ValueError: The {} must enclose a single variable"##,
    ),
    (
        r##"sorted({1, 2, 2, 3})"##,
        r##"{}"##,
        r##"err"##,
        r##"ValueError: The {} must enclose a single variable"##,
    ),
    (
        r##"len({1, 1.0, True})"##,
        r##"{}"##,
        r##"err"##,
        r##"ValueError: The {} must enclose a single variable"##,
    ),
    (
        r##"sorted({x, y})"##,
        r##"{"x": "b", "y": "a"}"##,
        r##"err"##,
        r##"ValueError: The {} must enclose a single variable"##,
    ),
    (
        r##"len({1, 2} )"##,
        r##"{}"##,
        r##"err"##,
        r##"ValueError: The {} must enclose a single variable"##,
    ),
    (
        r##"3 in {1, 2, 3}"##,
        r##"{}"##,
        r##"err"##,
        r##"ValueError: The {} must enclose a single variable"##,
    ),
    (
        r##"4 in {1, 2, 3}"##,
        r##"{}"##,
        r##"err"##,
        r##"ValueError: The {} must enclose a single variable"##,
    ),
    (
        r##"sorted({a % 2 for a in xs})"##,
        r##"{"xs": [1, 2, 3, 4]}"##,
        r##"err"##,
        r##"FeatureNotAvailable: Sorry, SetComp is not available in this evaluator"##,
    ),
    (
        r##"len({a for a in xs})"##,
        r##"{"xs": [1, 1, 2]}"##,
        r##"err"##,
        r##"FeatureNotAvailable: Sorry, SetComp is not available in this evaluator"##,
    ),
    (
        r##"sorted({a + 1 for a in xs if a > 1})"##,
        r##"{"xs": [1, 2, 3, 3]}"##,
        r##"err"##,
        r##"FeatureNotAvailable: Sorry, SetComp is not available in this evaluator"##,
    ),
    (
        r##"sorted({1 + 2})"##,
        r##"{}"##,
        r##"err"##,
        r##"TypeError: The {} must enclose a variable"##,
    ),
    (
        r##"sorted({'a'})"##,
        r##"{}"##,
        r##"err"##,
        r##"TypeError: The {} must enclose a variable"##,
    ),
    (
        r##"sorted({c for c in 'hello'})"##,
        r##"{}"##,
        r##"err"##,
        r##"FeatureNotAvailable: Sorry, SetComp is not available in this evaluator"##,
    ),
    (
        r##"len({x, x})"##,
        r##"{"x": 1}"##,
        r##"err"##,
        r##"ValueError: The {} must enclose a single variable"##,
    ),
    (
        r##"bool({0})"##,
        r##"{}"##,
        r##"err"##,
        r##"TypeError: The {} must enclose a variable"##,
    ),
    (
        r##"[x * 2 for x in xs]"##,
        r##"{"xs": [1, 2, 3]}"##,
        r##"ok"##,
        r##"[2, 4, 6]"##,
    ),
    (
        r##"{x: len(x) for x in xs}"##,
        r##"{"xs": ["a", "bb"]}"##,
        r##"ok"##,
        r##"{"a": 1, "bb": 2}"##,
    ),
    (
        r##"{k: v for k, v in pairs}"##,
        r##"{"pairs": [["a", 1], ["b", 2]]}"##,
        r##"ok"##,
        r##"{"a": 1, "b": 2}"##,
    ),
    (
        r##"{k: v for k, v in pairs if v > 1}"##,
        r##"{"pairs": [["a", 1], ["b", 2]]}"##,
        r##"ok"##,
        r##"{"b": 2}"##,
    ),
    (
        r##"{x: [y for y in ys if y != x] for x in ys}"##,
        r##"{"ys": ["p", "q"]}"##,
        r##"ok"##,
        r##"{"p": ["q"], "q": ["p"]}"##,
    ),
    (
        r##"[a + b for a in xs for b in ys]"##,
        r##"{"xs": ["a", "b"], "ys": ["1", "2"]}"##,
        r##"ok"##,
        r##"["a1", "a2", "b1", "b2"]"##,
    ),
    (
        r##"[a for a in xs if a > 1 if a < 4]"##,
        r##"{"xs": [0, 1, 2, 3, 4, 5]}"##,
        r##"ok"##,
        r##"[2, 3]"##,
    ),
    (
        r##"[(a, b) for a in xs for b in ys if a != b]"##,
        r##"{"xs": [1, 2], "ys": [1, 2]}"##,
        r##"ok"##,
        r##"[[1, 2], [2, 1]]"##,
    ),
    (
        r##"[b for a in xss for b in a]"##,
        r##"{"xss": [[1, 2], [3]]}"##,
        r##"ok"##,
        r##"[1, 2, 3]"##,
    ),
    (
        r##"[c for c in 'abc']"##,
        r##"{}"##,
        r##"ok"##,
        r##"["a", "b", "c"]"##,
    ),
    (
        r##"{c: 1 for c in 'abc'}"##,
        r##"{}"##,
        r##"ok"##,
        r##"{"a": 1, "b": 1, "c": 1}"##,
    ),
    (
        r##"sum(x for x in xs)"##,
        r##"{"xs": [1, 2, 3]}"##,
        r##"ok"##,
        r##"6"##,
    ),
    (
        r##"sum(x * x for x in xs if x > 1)"##,
        r##"{"xs": [1, 2, 3]}"##,
        r##"ok"##,
        r##"13"##,
    ),
    (
        r##"sorted(x for x in xs if x)"##,
        r##"{"xs": [3, 0, 1, 2]}"##,
        r##"ok"##,
        r##"[1, 2, 3]"##,
    ),
    (
        r##"max(len(x) for x in xs)"##,
        r##"{"xs": ["a", "bbb", "cc"]}"##,
        r##"ok"##,
        r##"3"##,
    ),
    (
        r##"list(x * 2 for x in xs)"##,
        r##"{"xs": [1, 2]}"##,
        r##"ok"##,
        r##"[2, 4]"##,
    ),
    (
        r##"any(x > 2 for x in xs)"##,
        r##"{"xs": [1, 2, 3]}"##,
        r##"ok"##,
        r##"true"##,
    ),
    (
        r##"all(x > 2 for x in xs)"##,
        r##"{"xs": [1, 2, 3]}"##,
        r##"ok"##,
        r##"false"##,
    ),
    (
        r##"len(list(x for x in xs))"##,
        r##"{"xs": [1, 2, 3]}"##,
        r##"ok"##,
        r##"3"##,
    ),
    (
        r##"[x for x in xs for y in ys]"##,
        r##"{"xs": [1, 2], "ys": [0, 0]}"##,
        r##"ok"##,
        r##"[1, 1, 2, 2]"##,
    ),
    (
        r##"[[y * x for y in ys] for x in xs]"##,
        r##"{"xs": [1, 2], "ys": [1, 2, 3]}"##,
        r##"ok"##,
        r##"[[1, 2, 3], [2, 4, 6]]"##,
    ),
    (
        r##"[x for x in xs if x in ys]"##,
        r##"{"xs": [1, 2, 3], "ys": [2, 3, 4]}"##,
        r##"ok"##,
        r##"[2, 3]"##,
    ),
    (
        r##"[x.n for x in xs]"##,
        r##"{"xs": [{"n": 1}, {"n": 2}]}"##,
        r##"err"##,
        r##"NameError: Attribute access on comprehension-bound 'x' in 'x.n' is not supported. Building records in an expression bypasses the range, cardinality, and datatype coercion applied to derived objects; declare a class_derivation keyed on the source range class and reduce the slot to populated_from instead."##,
    ),
    (
        r##"{x.n: x.m for x in xs}"##,
        r##"{"xs": [{"n": "a", "m": 1}, {"n": "b", "m": 2}]}"##,
        r##"err"##,
        r##"NameError: Attribute access on comprehension-bound 'x' in 'x.m' is not supported. Building records in an expression bypasses the range, cardinality, and datatype coercion applied to derived objects; declare a class_derivation keyed on the source range class and reduce the slot to populated_from instead."##,
    ),
    (
        r##"[k for k in d]"##,
        r##"{"d": {"a": 1, "b": 2}}"##,
        r##"ok"##,
        r##"["a", "b"]"##,
    ),
    (
        r##"[k + '!' for k in d if d[k] > 1]"##,
        r##"{"d": {"a": 1, "b": 2}}"##,
        r##"ok"##,
        r##"["b!"]"##,
    ),
    (
        r##"(x for x in xs) if False else 1"##,
        r##"{"xs": [1]}"##,
        r##"ok"##,
        r##"1"##,
    ),
    (
        r##"{'name': source_schema.name}"##,
        r##"{"source_schema": null}"##,
        r##"ok"##,
        r##"{"name": null}"##,
    ),
    (
        r##"[x.n for x in xs]"##,
        r##"{"xs": [{"n": 1}, {"n": 2}]}"##,
        r##"err"##,
        r##"NameError: Attribute access on comprehension-bound 'x' in 'x.n' is not supported. Building records in an expression bypasses the range, cardinality, and datatype coercion applied to derived objects; declare a class_derivation keyed on the source range class and reduce the slot to populated_from instead."##,
    ),
    (
        r##"{x.n: x.m for x in xs}"##,
        r##"{"xs": [{"n": "a", "m": 1}, {"n": "b", "m": 2}]}"##,
        r##"err"##,
        r##"NameError: Attribute access on comprehension-bound 'x' in 'x.m' is not supported. Building records in an expression bypasses the range, cardinality, and datatype coercion applied to derived objects; declare a class_derivation keyed on the source range class and reduce the slot to populated_from instead."##,
    ),
    (
        r##"'NOT_NULL' if x else 'NULL'"##,
        r##"{"x": null}"##,
        r##"ok"##,
        r##""NULL""##,
    ),
    (
        r##"upper(x) if is_str(x) else str(x)"##,
        r##"{"x": "hello"}"##,
        r##"ok"##,
        r##""HELLO""##,
    ),
    (
        r##"upper(x) if is_str(x) else str(x)"##,
        r##"{"x": 42}"##,
        r##"ok"##,
        r##""42""##,
    ),
    (
        r##"x + 1 if x else 0"##,
        r##"{"x": null}"##,
        r##"ok"##,
        r##"0"##,
    ),
    (
        r##"1 if x < 5 else 2"##,
        r##"{"x": null}"##,
        r##"ok"##,
        r##"2"##,
    ),
    (
        r##"{'a': x + 1}"##,
        r##"{"x": null}"##,
        r##"ok"##,
        r##"{"a": null}"##,
    ),
    (r##"{'a': 1}[k]"##, r##"{"k": "a"}"##, r##"ok"##, r##"1"##),
    (
        r##"dict(a=x)"##,
        r##"{"x": null}"##,
        r##"ok"##,
        r##"{"a": null}"##,
    ),
    (
        r##"set(x)"##,
        r##"{"x": null}"##,
        r##"err"##,
        r##"TypeError: 'NoneType' object is not iterable"##,
    ),
    (
        r##"dict(x)"##,
        r##"{"x": null}"##,
        r##"err"##,
        r##"TypeError: 'NoneType' object is not iterable"##,
    ),
    (
        r##"sorted(set(x))"##,
        r##"{"x": ["b", "a"]}"##,
        r##"ok"##,
        r##"["a", "b"]"##,
    ),
    (
        r##"len(set(strip(x)))"##,
        r##"{"x": [" a", "a ", "b"]}"##,
        r##"ok"##,
        r##"2"##,
    ),
    (
        r##"case((x > 1, 'big'), (True, 'small'))"##,
        r##"{"x": 2}"##,
        r##"ok"##,
        r##""big""##,
    ),
    (
        r##"coalesce(x, {})"##,
        r##"{"x": null}"##,
        r##"ok"##,
        r##"{}"##,
    ),
    (
        r##"max(set(x)) if x else None"##,
        r##"{"x": [1, 3, 3]}"##,
        r##"ok"##,
        r##"3"##,
    ),
    (r##"{x}"##, r##"{"x": 5}"##, r##"ok"##, r##"5"##),
    (r##"{ x }"##, r##"{"x": 5}"##, r##"ok"##, r##"5"##),
    (r##"{x,}"##, r##"{"x": 5}"##, r##"ok"##, r##"5"##),
    (
        r##"{x} if x else {y}"##,
        r##"{"x": 0, "y": 9}"##,
        r##"ok"##,
        r##"9"##,
    ),
    (
        r##"{x: 1 for x in xs}"##,
        r##"{"xs": ["a", "b"]}"##,
        r##"ok"##,
        r##"{"a": 1, "b": 1}"##,
    ),
    (r##"dict(zip=1)"##, r##"{}"##, r##"ok"##, r##"{"zip": 1}"##),
    (
        r##"dict(1)"##,
        r##"{}"##,
        r##"err"##,
        r##"TypeError: 'int' object is not iterable"##,
    ),
    (
        r##"dict([1])"##,
        r##"{}"##,
        r##"err"##,
        r##"TypeError: cannot convert dictionary update sequence element #0 to a sequence"##,
    ),
    (
        r##"dict([(1, 2, 3)])"##,
        r##"{}"##,
        r##"err"##,
        r##"ValueError: dictionary update sequence element #0 has length 3; 2 is required"##,
    ),
    (
        r##"dict(None)"##,
        r##"{}"##,
        r##"err"##,
        r##"TypeError: 'NoneType' object is not iterable"##,
    ),
    (
        r##"dict([('a', 1)], [('b', 2)])"##,
        r##"{}"##,
        r##"err"##,
        r##"TypeError: dict expected at most 1 argument, got 2"##,
    ),
    (
        r##"set(1)"##,
        r##"{}"##,
        r##"err"##,
        r##"TypeError: 'int' object is not iterable"##,
    ),
    (
        r##"set(None)"##,
        r##"{}"##,
        r##"err"##,
        r##"TypeError: 'NoneType' object is not iterable"##,
    ),
    (
        r##"set([[1]])"##,
        r##"{}"##,
        r##"err"##,
        r##"TypeError: unhashable type: 'list'"##,
    ),
    (
        r##"set([1], [2])"##,
        r##"{}"##,
        r##"err"##,
        r##"TypeError: set expected at most 1 argument, got 2"##,
    ),
    (
        r##"{[1]: 2}"##,
        r##"{}"##,
        r##"err"##,
        r##"TypeError: unhashable type: 'list'"##,
    ),
    (
        r##"{{}: 1}"##,
        r##"{}"##,
        r##"err"##,
        r##"TypeError: unhashable type: 'dict'"##,
    ),
    (
        r##"list(a=1)"##,
        r##"{}"##,
        r##"err"##,
        r##"TypeError: list() takes no keyword arguments"##,
    ),
    (
        r##"sorted([2,1], reverse=True)"##,
        r##"{}"##,
        r##"err"##,
        r##"TypeError: _null_safe.<locals>.wrapper() got an unexpected keyword argument 'reverse'"##,
    ),
    (
        r##"len(a=[1])"##,
        r##"{}"##,
        r##"err"##,
        r##"TypeError: _null_safe.<locals>.wrapper() got an unexpected keyword argument 'a'"##,
    ),
    (
        r##"tuple(a=1)"##,
        r##"{}"##,
        r##"err"##,
        r##"TypeError: tuple() takes no keyword arguments"##,
    ),
    (
        r##"set(a=1)"##,
        r##"{}"##,
        r##"err"##,
        r##"TypeError: set() takes no keyword arguments"##,
    ),
    (
        r##"1 if True"##,
        r##"{}"##,
        r##"err"##,
        r##"SyntaxError: expected 'else' after 'if' expression (<unknown>, line 1)"##,
    ),
    (
        r##"[x for x in 5]"##,
        r##"{}"##,
        r##"err"##,
        r##"TypeError: 'int' object is not iterable"##,
    ),
    (
        r##"{x: 1 for x in 5}"##,
        r##"{}"##,
        r##"err"##,
        r##"TypeError: 'int' object is not iterable"##,
    ),
    (
        r##"{1, 2}"##,
        r##"{}"##,
        r##"err"##,
        r##"ValueError: The {} must enclose a single variable"##,
    ),
    (
        r##"{1}"##,
        r##"{}"##,
        r##"err"##,
        r##"TypeError: The {} must enclose a variable"##,
    ),
    (
        r##"{x, y}"##,
        r##"{"x": 1, "y": 2}"##,
        r##"err"##,
        r##"ValueError: The {} must enclose a single variable"##,
    ),
    (
        r##"{x + 1}"##,
        r##"{"x": 1}"##,
        r##"err"##,
        r##"TypeError: The {} must enclose a variable"##,
    ),
    (
        r##"{'a'}"##,
        r##"{}"##,
        r##"err"##,
        r##"TypeError: The {} must enclose a variable"##,
    ),
    (r##"{}"##, r##"{}"##, r##"ok"##, r##"{}"##),
    (
        r##"{x for x in xs}"##,
        r##"{"xs": [1]}"##,
        r##"err"##,
        r##"FeatureNotAvailable: Sorry, SetComp is not available in this evaluator"##,
    ),
    (
        r##"1 if {1, 2} else 2"##,
        r##"{}"##,
        r##"err"##,
        r##"ValueError: The {} must enclose a single variable"##,
    ),
    (r##"1 if True else {1, 2}"##, r##"{}"##, r##"ok"##, r##"1"##),
    (
        r##"{1, 2} if False else 3"##,
        r##"{}"##,
        r##"ok"##,
        r##"3"##,
    ),
];

/// `(expression, kind, result)` from upstream's unrestricted `asteval`
/// interpreter, running `target = <expression>` with no variables. An
/// evaluation error leaves `target` as `None` (`err`).
const UNRESTRICTED_CASES: &[(&str, &str, &str)] = &[
    (r##"'ok' if True else boom(1)"##, r##"ok"##, r##""ok""##),
    (r##"'ok' if False else 'fine'"##, r##"ok"##, r##""fine""##),
    (
        r##"boom(1) if False else 'lazy'"##,
        r##"ok"##,
        r##""lazy""##,
    ),
    (r##"{}"##, r##"ok"##, r##"{}"##),
    (r##"{ }"##, r##"ok"##, r##"{}"##),
    (r##"{'a': 1}"##, r##"ok"##, r##"{"a": 1}"##),
    (r##"{'a': 1,}"##, r##"ok"##, r##"{"a": 1}"##),
    (
        r##"{ 'a' : 1 , 'b' : 2 }"##,
        r##"ok"##,
        r##"{"a": 1, "b": 2}"##,
    ),
    (
        r##"{'a': 1, 'b': [1, 2], 'c': {'d': 3}}"##,
        r##"ok"##,
        r##"{"a": 1, "b": [1, 2], "c": {"d": 3}}"##,
    ),
    (r##"{'a': 1, 'a': 2}"##, r##"ok"##, r##"{"a": 2}"##),
    (
        r##"{'a': 1, 'b': 2, 'a': 3}"##,
        r##"ok"##,
        r##"{"a": 3, "b": 2}"##,
    ),
    (r##"{'a': 1}['a']"##, r##"ok"##, r##"1"##),
    (r##"{'a': {'b': 2}}['a']['b']"##, r##"ok"##, r##"2"##),
    (r##"len({'a': 1, 'b': 2})"##, r##"ok"##, r##"2"##),
    (r##"'a' in {'a': 1}"##, r##"ok"##, r##"true"##),
    (r##"'z' in {'a': 1}"##, r##"ok"##, r##"false"##),
    (r##"'z' not in {'a': 1}"##, r##"ok"##, r##"true"##),
    (r##"len({})"##, r##"ok"##, r##"0"##),
    (r##"{} == {}"##, r##"ok"##, r##"true"##),
    (r##"{'a': 1} == {'a': 1}"##, r##"ok"##, r##"true"##),
    (r##"{'a': 1} == {'a': 2}"##, r##"ok"##, r##"false"##),
    (r##"{1: 'a'}"##, r##"ok"##, r##"{"1": "a"}"##),
    (
        r##"{1: 'a', 2: 'b'}"##,
        r##"ok"##,
        r##"{"1": "a", "2": "b"}"##,
    ),
    (r##"{1.5: 'a'}"##, r##"ok"##, r##"{"1.5": "a"}"##),
    (r##"{'a' + 'b': 1}"##, r##"ok"##, r##"{"ab": 1}"##),
    (
        r##"{'a': 1 + 2, 'b': 'x' + 'y'}"##,
        r##"ok"##,
        r##"{"a": 3, "b": "xy"}"##,
    ),
    (r##"bool({})"##, r##"ok"##, r##"false"##),
    (r##"bool({'a': 1})"##, r##"ok"##, r##"true"##),
    (r##"{'a': None}"##, r##"ok"##, r##"{"a": null}"##),
    (
        r##"{'a': True, 'b': False, 'c': None}"##,
        r##"ok"##,
        r##"{"a": true, "b": false, "c": null}"##,
    ),
    (r##"dict()"##, r##"ok"##, r##"{}"##),
    (r##"dict(a=1)"##, r##"ok"##, r##"{"a": 1}"##),
    (r##"dict(a=1, b=2)"##, r##"ok"##, r##"{"a": 1, "b": 2}"##),
    (r##"dict(a=1)['a']"##, r##"ok"##, r##"1"##),
    (
        r##"dict([('a', 1), ('b', 2)])"##,
        r##"ok"##,
        r##"{"a": 1, "b": 2}"##,
    ),
    (
        r##"dict([['a', 1], ['b', 2]])"##,
        r##"ok"##,
        r##"{"a": 1, "b": 2}"##,
    ),
    (
        r##"dict([('a', 1), ('a', 2)])"##,
        r##"ok"##,
        r##"{"a": 2}"##,
    ),
    (r##"dict({'a': 1})"##, r##"ok"##, r##"{"a": 1}"##),
    (
        r##"dict({'a': 1}, b=2)"##,
        r##"ok"##,
        r##"{"a": 1, "b": 2}"##,
    ),
    (r##"dict({'a': 1}, a=2)"##, r##"ok"##, r##"{"a": 2}"##),
    (r##"dict([('a', 1)], a=2)"##, r##"ok"##, r##"{"a": 2}"##),
    (r##"dict([(1, 'x')])"##, r##"ok"##, r##"{"1": "x"}"##),
    (
        r##"dict(a=[1, 2], b={'c': 3})"##,
        r##"ok"##,
        r##"{"a": [1, 2], "b": {"c": 3}}"##,
    ),
    (r##"dict([])"##, r##"ok"##, r##"{}"##),
    (
        r##"dict(['ab', 'cd'])"##,
        r##"ok"##,
        r##"{"a": "b", "c": "d"}"##,
    ),
    (r##"len(dict(a=1, b=2))"##, r##"ok"##, r##"2"##),
    (r##"dict(zero=0)"##, r##"ok"##, r##"{"zero": 0}"##),
    (r##"'a' in dict(a=1)"##, r##"ok"##, r##"true"##),
    (r##"len(set())"##, r##"ok"##, r##"0"##),
    (
        r##"sorted(set([3, 1, 2, 3]))"##,
        r##"ok"##,
        r##"[1, 2, 3]"##,
    ),
    (r##"len(set([3, 1, 2, 3]))"##, r##"ok"##, r##"3"##),
    (r##"len(set([1, 1.0, True]))"##, r##"ok"##, r##"1"##),
    (r##"len(set('hello'))"##, r##"ok"##, r##"4"##),
    (
        r##"sorted(set('hello'))"##,
        r##"ok"##,
        r##"["e", "h", "l", "o"]"##,
    ),
    (
        r##"sorted(set(['b', 'a', 'b']))"##,
        r##"ok"##,
        r##"["a", "b"]"##,
    ),
    (r##"2 in set([1, 2])"##, r##"ok"##, r##"true"##),
    (r##"5 in set([1, 2])"##, r##"ok"##, r##"false"##),
    (r##"5 not in set([1, 2])"##, r##"ok"##, r##"true"##),
    (r##"list(set())"##, r##"ok"##, r##"[]"##),
    (r##"sorted(set(set([2, 1])))"##, r##"ok"##, r##"[1, 2]"##),
    (r##"len(set([None, None, 1]))"##, r##"ok"##, r##"2"##),
    (r##"len(set([0, False, 0.0]))"##, r##"ok"##, r##"1"##),
    (r##"len(set([1, 2]) )"##, r##"ok"##, r##"2"##),
    (r##"sum(set([1, 1, 2]))"##, r##"ok"##, r##"3"##),
    (r##"max(set([1, 5, 2]))"##, r##"ok"##, r##"5"##),
    (r##"len(set(['a', 'A']))"##, r##"ok"##, r##"2"##),
    (r##"bool(set())"##, r##"ok"##, r##"false"##),
    (r##"bool(set([0]))"##, r##"ok"##, r##"true"##),
    (r##"len(set([float(1), 1]))"##, r##"ok"##, r##"1"##),
    (
        r##"sorted(set([1.5, 1.5, 2.5]))"##,
        r##"ok"##,
        r##"[1.5, 2.5]"##,
    ),
    (r##"sorted({3, 1, 2})"##, r##"ok"##, r##"[1, 2, 3]"##),
    (r##"sorted({1, 2, 2, 3})"##, r##"ok"##, r##"[1, 2, 3]"##),
    (r##"len({1, 1.0, True})"##, r##"ok"##, r##"1"##),
    (r##"len({1, 2} )"##, r##"ok"##, r##"2"##),
    (r##"3 in {1, 2, 3}"##, r##"ok"##, r##"true"##),
    (r##"4 in {1, 2, 3}"##, r##"ok"##, r##"false"##),
    (r##"sorted({1 + 2})"##, r##"ok"##, r##"[3]"##),
    (r##"sorted({'a'})"##, r##"ok"##, r##"["a"]"##),
    (
        r##"sorted({c for c in 'hello'})"##,
        r##"ok"##,
        r##"["e", "h", "l", "o"]"##,
    ),
    (r##"bool({0})"##, r##"ok"##, r##"true"##),
    (r##"[c for c in 'abc']"##, r##"ok"##, r##"["a", "b", "c"]"##),
    (
        r##"{c: 1 for c in 'abc'}"##,
        r##"ok"##,
        r##"{"a": 1, "b": 1, "c": 1}"##,
    ),
    (r##"dict(zip=1)"##, r##"ok"##, r##"{"zip": 1}"##),
    (r##"dict(1)"##, r##"err"##, r##"TypeError"##),
    (r##"dict([1])"##, r##"err"##, r##"TypeError"##),
    (r##"dict([(1, 2, 3)])"##, r##"err"##, r##"ValueError"##),
    (r##"dict(None)"##, r##"err"##, r##"TypeError"##),
    (
        r##"dict([('a', 1)], [('b', 2)])"##,
        r##"err"##,
        r##"TypeError"##,
    ),
    (r##"set(1)"##, r##"err"##, r##"TypeError"##),
    (r##"set(None)"##, r##"err"##, r##"TypeError"##),
    (r##"set([[1]])"##, r##"err"##, r##"TypeError"##),
    (r##"set([1], [2])"##, r##"err"##, r##"TypeError"##),
    (r##"{[1]: 2}"##, r##"err"##, r##"TypeError"##),
    (r##"{{}: 1}"##, r##"err"##, r##"TypeError"##),
    (r##"list(a=1)"##, r##"err"##, r##"TypeError"##),
    (r##"sorted([2,1], reverse=True)"##, r##"ok"##, r##"[2, 1]"##),
    (r##"len(a=[1])"##, r##"err"##, r##"TypeError"##),
    (r##"tuple(a=1)"##, r##"err"##, r##"TypeError"##),
    (r##"set(a=1)"##, r##"err"##, r##"TypeError"##),
    (r##"1 if True"##, r##"err"##, r##"SyntaxError"##),
    (r##"[x for x in 5]"##, r##"err"##, r##"TypeError"##),
    (r##"{x: 1 for x in 5}"##, r##"err"##, r##"TypeError"##),
    (r##"{1, 2}"##, r##"set"##, r##"[1, 2]"##),
    (r##"{1}"##, r##"set"##, r##"[1]"##),
    (r##"{'a'}"##, r##"set"##, r##"["a"]"##),
    (r##"1 if {1, 2} else 2"##, r##"ok"##, r##"1"##),
    (r##"1 if True else {1, 2}"##, r##"ok"##, r##"1"##),
    (r##"{1, 2} if False else 3"##, r##"ok"##, r##"3"##),
];

fn sorted_json(v: &Value) -> Json {
    let Json::Array(mut items) = Json::from(v) else {
        panic!("a set is a list here, got {v:?}");
    };
    items.sort_by_key(Json::to_string);
    Json::Array(items)
}

/// Check one oracle result; `Some(message)` when it differs.
fn check(got: &Result<Value, ExprError>, kind: &str, payload: &str) -> Option<String> {
    match (kind, got) {
        ("err", Err(_)) => None,
        ("ok", Ok(v)) => {
            let want: Json = serde_json::from_str(payload).unwrap();
            (*v != Value::from(&want)).then(|| format!("want {want}, got {v:?}"))
        }
        ("set", Ok(v)) => {
            let mut want: Json = serde_json::from_str(payload).unwrap();
            want.as_array_mut().unwrap().sort_by_key(Json::to_string);
            (!matches!(v, Value::List(_)) || sorted_json(v) != want)
                .then(|| format!("want set {want}, got {v:?}"))
        }
        _ => Some(format!("want {kind} {payload}, got {got:?}")),
    }
}

/// Differences from upstream that follow from [`Value`] having no set or tuple
/// (see `compound.rs`): `(expression, what this evaluator gives instead)`.
const KNOWN_DIVERGENCES: &[(&str, &str)] = &[
    // PARITY: a set is a list here, so the distributing built-ins (`bool`, `str`,
    // ...) map over it where Python sees one set.
    ("bool(set())", "[] (distributes over the empty list)"),
    ("bool(set([0]))", "[False]"),
    ("bool({0})", "[False]"),
    // Not about sets: upstream's restricted evaluator refuses attribute access
    // on a comprehension target (`_eval_attribute`, "comprehension-bound"); this
    // evaluator reads the attribute, as `asteval` does.
    ("[x.n for x in xs]", "[1, 2]"),
    ("{x.n: x.m for x in xs}", "{'a': 1, 'b': 2}"),
    // Not about syntax: asteval has all of Python's built-ins, this evaluator the
    // LinkML function table, which takes no keyword arguments.
    ("sorted([2,1], reverse=True)", "error"),
];

#[test]
fn restricted_expressions_match_upstream() {
    let mut failures = Vec::new();
    for &(expr, vars, kind, payload) in RESTRICTED_CASES {
        let vars: Json = serde_json::from_str(vars).unwrap();
        let got = with_restricted_syntax(true, || eval(expr, vars.clone()));
        if let Some(why) = check(&got, kind, payload)
            && !KNOWN_DIVERGENCES.iter().any(|(e, _)| *e == expr)
        {
            failures.push(format!("{expr} {vars}: {why}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn unrestricted_expressions_match_asteval() {
    let mut failures = Vec::new();
    for &(expr, kind, payload) in UNRESTRICTED_CASES {
        if KNOWN_DIVERGENCES.iter().any(|(e, _)| *e == expr) {
            continue;
        }
        // A statement block: asteval records an evaluation error and leaves
        // `target` as None, where a syntax error fails up front.
        let got = eval(&format!("target = {expr}"), json!({}));
        let (kind, payload) = match (kind, payload) {
            ("err", p) if p.contains("SyntaxError") => ("err", p),
            ("err", _) => ("ok", "null"),
            other => other,
        };
        if let Some(why) = check(&got, kind, payload) {
            failures.push(format!("{expr}: {why}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

// === ports of upstream tests/test_utils/test_eval_utils.py ====================

#[test]
fn test_ternary() {
    // ids: ternary_eq, ternary_neq, ternary_and_true, ternary_and_false
    expect(
        "'EQ' if {x} == {y} else 'NEQ'",
        json!({"x": 1, "y": 1}),
        json!("EQ"),
    );
    expect(
        "'EQ' if {x} == {y} else 'NEQ'",
        json!({"x": 1, "y": 2}),
        json!("NEQ"),
    );
    expect(
        "'yes' if x > 0 and x < 10 else 'no'",
        json!({"x": 5}),
        json!("yes"),
    );
    expect(
        "'yes' if x > 0 and x < 10 else 'no'",
        json!({"x": 15}),
        json!("no"),
    );
}

#[test]
fn test_null_in_conditional() {
    expect(
        "'NOT_NULL' if x else 'NULL'",
        json!({"x": null}),
        json!("NULL"),
    );
    expect(
        "'NOT_NULL' if x else 'NULL'",
        json!({"x": 1}),
        json!("NOT_NULL"),
    );
}

#[test]
fn test_dict_literal() {
    expect("{'a': 1}", json!({}), json!({"a": 1}));
}

#[test]
fn test_type_predicates_in_case_if_else_is_lazy() {
    expect(
        "upper(x) if is_str(x) else str(x)",
        json!({"x": "hello"}),
        json!("HELLO"),
    );
    expect(
        "upper(x) if is_str(x) else str(x)",
        json!({"x": 42}),
        json!("42"),
    );
}

// === the spec expression that motivated this =================================

#[test]
fn spec_dict_literal_with_attribute_value() {
    // tests/golden/fair_mappings_metadata: `{'name': source_schema.name}`.
    let vars = json!({"source_schema": {"name": "S", "version": "1"}, "target_schema": null});
    expect(
        "{'name': source_schema.name}",
        vars.clone(),
        json!({"name": "S"}),
    );
    // The golden fixture's own data and expected output.
    let vars = json!({
        "source_schema": {"name": "legacy_gene_expression.yaml"},
        "target_schema": {"name": "biolink_model.yaml"},
    });
    expect(
        "{'name': source_schema.name}",
        vars.clone(),
        json!({"name": "legacy_gene_expression.yaml"}),
    );
    expect(
        "{'name': target_schema.name}",
        vars,
        json!({"name": "biolink_model.yaml"}),
    );
    // An absent schema reference gives a null name, not an error.
    let vars = json!({"source_schema": {"name": "S"}, "target_schema": null});
    expect("{'name': target_schema.name}", vars, json!({"name": null}));
}

// === brace disambiguation (upstream: Python's parser, then `_eval_set`) =======

#[test]
fn braces_are_references_dicts_or_sets_by_syntax() {
    use super::parser::{Ast, parse};
    let kind = |src: &str| match parse(src).unwrap() {
        Ast::Brace(_) => "reference",
        Ast::Set(_) => "set",
        Ast::Dict(_) => "dict",
        Ast::Comp(_) => "comprehension",
        _ => "other",
    };
    for reference in [
        "{x}",
        "{ x }",
        "{x,}",
        "{Table.col}",
        "{ a.b.c }",
        "{\tx\n}",
    ] {
        assert_eq!(kind(reference), "reference", "{reference}");
    }
    for set in [
        "{1}", "{x, y}", "{'a'}", "{x + 1}", "{x[0]}", "{f(x)}", "{x.y, z}",
    ] {
        assert_eq!(kind(set), "set", "{set}");
    }
    for dict in [
        "{}",
        "{ }",
        "{'a': 1}",
        "{a: 1}",
        "{a: 1,}",
        "{'a': {'b': {}}}",
    ] {
        assert_eq!(kind(dict), "dict", "{dict}");
    }
    for comp in ["{x for x in xs}", "{x: 1 for x in xs}"] {
        assert_eq!(kind(comp), "comprehension", "{comp}");
    }
}

#[test]
fn braces_inside_strings_are_text() {
    expect("'{x}'", json!({"x": 1}), json!("{x}"));
    expect("{'{': '}'}", json!({}), json!({"{": "}"}));
    expect("{'a': '{b}'}", json!({"b": 2}), json!({"a": "{b}"}));
    expect("'}{' + {x}", json!({"x": "!"}), json!("}{!"));
    expect("{x: '{y}'}", json!({"x": "k", "y": 1}), json!({"k": "{y}"}));
}

#[test]
fn references_still_resolve_inside_dicts_and_calls() {
    expect("{'k': {x}}", json!({"x": 1}), json!({"k": 1}));
    expect("{'k': {t.col}}", json!({"t": {"col": 5}}), json!({"k": 5}));
    expect("{{x}: 1}", json!({"x": "key"}), json!({"key": 1}));
    expect("dict(a={x})", json!({"x": 3}), json!({"a": 3}));
    expect("[{x}, {y}]", json!({"x": 1, "y": 2}), json!([1, 2]));
    expect(
        "{x} if {y} else {z}",
        json!({"x": 1, "y": 0, "z": 9}),
        json!(9),
    );
}

// === restricted syntax ========================================================

#[test]
fn restricted_syntax_rejects_exactly_what_upstream_rejects() {
    let restricted = |expr: &str| with_restricted_syntax(true, || eval(expr, json!({"x": 1})));
    for rejected in [
        "{1, 2}",
        "{1}",
        "{x + 1}",
        "{'a'}",
        "{x, x}",
        "{x for x in [1]}",
    ] {
        assert!(restricted(rejected).is_err(), "{rejected}");
    }
    for accepted in [
        "{x}",
        "{}",
        "{'a': 1}",
        "{x: 1}",
        "{k: v for k, v in [[1, 2]]}",
        "[x for x in [1]]",
        "(x for x in [1])",
        "dict(a=1)",
        "set([1])",
        "1 if x else 2",
    ] {
        assert!(restricted(accepted).is_ok(), "{accepted}");
    }
    // Like upstream, the rejection is dynamic: a branch not taken is not checked.
    assert!(restricted("1 if True else {1, 2}").is_ok());
    assert!(restricted("{1, 2} if True else 1").is_err());
    // And the default setting accepts them.
    assert!(eval("{1, 2}", json!({})).is_ok());
}

#[test]
fn restricted_syntax_nests_and_restores() {
    with_restricted_syntax(true, || {
        with_restricted_syntax(false, || assert!(eval("{1, 2}", json!({})).is_ok()));
        assert!(eval("{1, 2}", json!({})).is_err());
    });
    assert!(eval("{1, 2}", json!({})).is_ok());
}

#[test]
fn statement_blocks_are_never_restricted() {
    // asteval: `{x}` is a one-element set, whatever the restriction.
    let got = with_restricted_syntax(true, || eval("target = {1, 2}", json!({}))).unwrap();
    assert_eq!(got, Value::from(&json!([1, 2])));
}

// === statement blocks (asteval) ===============================================

fn run_block(src: &str, slots: Json) -> Value {
    eval(src, json!({"src": slots})).unwrap()
}

#[test]
fn block_brace_is_a_one_element_set() {
    // asteval has no reference syntax: `{src.x}` is `{src.x}` the set.
    assert_eq!(
        run_block("target = {src.x}", json!({"x": 4})),
        Value::from(&json!([4]))
    );
    // An unhashable member is a TypeError, which asteval records: target stays None.
    assert_eq!(
        run_block("target = {src.x}", json!({"x": [1]})),
        Value::Null
    );
}

#[test]
fn block_ternary_dict_and_constructors() {
    let slots = json!({"x": 0, "y": "why", "ids": ["a", "b", "a"]});
    assert_eq!(
        run_block("target = 'yes' if src.x else 'no'", slots.clone()),
        Value::from(&json!("no"))
    );
    assert_eq!(
        run_block(
            "target = {'k': src.y, 'n': len(set(src.ids))}",
            slots.clone()
        ),
        Value::from(&json!({"k": "why", "n": 2}))
    );
    assert_eq!(
        run_block(
            "if src.y:\n    target = dict(a=src.y) if not src.x else 1",
            slots
        ),
        Value::from(&json!({"a": "why"}))
    );
}

// === errors ===================================================================

#[test]
fn constructor_errors_follow_python() {
    expect_err("dict(1)", json!({}), "'int' object is not iterable");
    expect_err(
        "dict([1])",
        json!({}),
        "cannot convert dictionary update sequence element #0",
    );
    expect_err(
        "dict([(1, 2, 3)])",
        json!({}),
        "has length 3; 2 is required",
    );
    expect_err(
        "dict([], [])",
        json!({}),
        "dict expected at most 1 argument, got 2",
    );
    expect_err("dict(None)", json!({}), "'NoneType' object is not iterable");
    expect_err("set([[1]])", json!({}), "unhashable type: 'list'");
    expect_err("set(None)", json!({}), "'NoneType' object is not iterable");
    expect_err("set(1, 2)", json!({}), "set expected at most 1 argument");
    expect_err("set(a=1)", json!({}), "set() takes no keyword arguments");
    expect_err("list(a=1)", json!({}), "list() takes no keyword arguments");
    expect_err("len(a=[1])", json!({}), "unexpected keyword argument 'a'");
    expect_err("nosuch(a=1)", json!({}), "Unknown function 'nosuch'");
    expect_err("{[1]: 2}", json!({}), "unhashable type: 'list'");
    expect_err("{{}: 1}", json!({}), "unhashable type: 'dict'");
}

#[test]
fn syntax_errors() {
    use super::parser::parse;
    for bad in [
        "1 if x",
        "1 if else 2",
        "if",
        "x else y",
        "{1: }",
        "{1: 2, 3}",
        "{1, 2: 3}",
        "[x for in xs]",
        "{x for}",
        "f(a=1, 2)",
        "f(a=)",
        "(x for x in)",
    ] {
        assert!(parse(bad).is_err(), "{bad}");
    }
}

// === PARITY: the set representation ===========================================

#[test]
fn a_set_is_a_deduplicated_list_in_insertion_order() {
    // PARITY: Python iterates `set([3, 1, 2])` as {1, 2, 3} (small ints hash to
    // themselves) and a string set in per-process hash order; this evaluator
    // keeps first-insertion order. Membership, length, `sorted` and `list`
    // agree with Python.
    expect("set([3, 1, 2, 3, 1])", json!({}), json!([3, 1, 2]));
    expect("list(set(['b', 'a', 'b']))", json!({}), json!(["b", "a"]));
    expect(
        "{c for c in 'hello'}",
        json!({}),
        json!(["h", "e", "l", "o"]),
    );
    // PARITY: a tuple is a list here, so it is unhashable (Python hashes tuples).
    expect_err("set([(1, 2)])", json!({}), "unhashable type: 'list'");
    // PARITY: no set type to distinguish, so set equality is list equality.
    expect("set([1, 2]) == set([2, 1])", json!({}), json!(false));
}

#[test]
fn dict_keys_are_stringified() {
    // PARITY: Python keeps `1` as an int key (JSON output still shows "1").
    expect("{1: 'a'}", json!({}), json!({"1": "a"}));
    expect("{1: 'a'}[1]", json!({}), json!("a"));
    // ... which also merges keys Python keeps apart.
    expect("len({1: 'a', '1': 'b'})", json!({}), json!(1));
    // PARITY: a non-string needle never matches a map key (Python: True).
    expect("1 in {1: 'a'}", json!({}), json!(false));
}

#[test]
fn comprehension_tuple_targets_zip_like_simpleeval() {
    // simpleeval zips the target names with the item, so a surplus element (or a
    // missing one) is ignored; upstream's `test_comprehension_bound_root_...`
    // depends on `for k, v in codes` binding k, v to the first two characters of
    // each dict key.
    expect(
        "[k + v for k, v in ['abc', 'de']]",
        json!({}),
        json!(["ab", "de"]),
    );
    expect("[k for k, v in [[1], [2, 3]]]", json!({}), json!([1, 2]));
}
