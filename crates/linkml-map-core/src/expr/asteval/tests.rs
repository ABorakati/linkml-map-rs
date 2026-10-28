//! Unit tests for the `asteval` port. Expected values come from running
//! upstream `asteval` 1.0.10 on the same text.

use indexmap::IndexMap;

use super::*;
use crate::value::Value;

fn run(text: &str) -> Result<Value, ExecError> {
    run_with(text, IndexMap::new())
}

fn run_with(text: &str, src: IndexMap<String, Value>) -> Result<Value, ExecError> {
    let Ok(parsed) = compile(text) else {
        return Ok(Value::Null);
    };
    let src = build_src(src.iter(), None);
    execute(&parsed, text, src, None, true)
}

fn int(i: i64) -> Value {
    Value::Int(i)
}

fn s(x: &str) -> Value {
    Value::Str(x.to_string())
}

#[test]
fn basics() {
    assert_eq!(run("target = 1").unwrap(), int(1));
    assert_eq!(run("target = 1 + 2 * 3").unwrap(), int(7));
    assert_eq!(run("x = 1\ntarget = x + 1").unwrap(), int(2));
    assert_eq!(run("pass").unwrap(), Value::Null);
    assert_eq!(run("target = 1\npass").unwrap(), int(1));
}

#[test]
fn syntax_errors_are_swallowed() {
    for text in [
        "1 +",
        "x = ",
        "if :",
        "))",
        "'a",
        "1.id",
        "target = (",
        "target = 1 2",
        "@",
        "target = $x",
        "if 1:\ntarget = 1",
        "  target = 1",
        "target = 1\n  target = 2",
        "else: 1",
        "target = [1,",
        "global target\ntarget = 1x",
    ] {
        assert_eq!(run(text).unwrap(), Value::Null, "{text:?}");
    }
}

#[test]
fn statements() {
    assert_eq!(run("target = 1\ntarget += 1").unwrap(), int(2));
    assert_eq!(run("target = 5\ntarget -= 2").unwrap(), int(3));
    assert_eq!(
        run("target = [1]\ntarget *= 2").unwrap(),
        Value::List(vec![int(1), int(1)])
    );
    assert_eq!(run("tmp = 1\ndel tmp\ntarget = 1").unwrap(), int(1));
    assert_eq!(run("a, b = 1, 2\ntarget = a + b").unwrap(), int(3));
    assert_eq!(run("tmp, target = [1, 2]").unwrap(), int(2));
    assert_eq!(
        run("x = [1, 2]\nfor i in x:\n    target = i").unwrap(),
        int(2)
    );
    assert_eq!(
        run("target = 0\nfor i in [1, 2, 3]:\n    target += i").unwrap(),
        int(6)
    );
    assert_eq!(
        run("target = 0\nwhile target < 3:\n    target += 1").unwrap(),
        int(3)
    );
    assert_eq!(
        run("if 0:\n    target = 1\nelif 1:\n    target = 2\nelse:\n    target = 3").unwrap(),
        int(2)
    );
    assert_eq!(
        run("if not 0: target = 1\nelse: target = 2").unwrap(),
        int(1)
    );
    assert_eq!(
        run("try:\n    target = 1 / 0\nexcept ZeroDivisionError:\n    target = 'z'").unwrap(),
        s("z")
    );
    assert_eq!(
        run("def f(x):\n    return x * 2\ntarget = f(3)").unwrap(),
        int(6)
    );
    assert_eq!(run("target = (lambda a, b=2: a * b)(3)").unwrap(), int(6));
    assert_eq!(run("target: int = 3").unwrap(), Value::Null);
    assert_eq!(run("assert 1 == 1\ntarget = 'ok'").unwrap(), s("ok"));
    assert_eq!(run("target = x = 5").unwrap(), int(5));
    assert_eq!(run("import math\ntarget = 1").unwrap(), Value::Null);
    assert_eq!(run("target = 'a' 'b'").unwrap(), s("ab"));
    assert_eq!(run("target = 0x10").unwrap(), int(16));
    assert_eq!(run("global target\ntarget = 1").unwrap(), Value::Null);
}

#[test]
fn namespace() {
    assert_eq!(
        run("target = list(range(3))").unwrap(),
        Value::List(vec![int(0), int(1), int(2)])
    );
    assert_eq!(
        run("target = pi").unwrap(),
        Value::Float(std::f64::consts::PI)
    );
    assert_eq!(run("target = sqrt(4)").unwrap(), Value::Float(2.0));
    assert_eq!(run("target = pow(2, 3)").unwrap(), Value::Float(8.0));
    assert_eq!(run("target = type(1)").unwrap(), s("int"));
    assert_eq!(
        run("target = isinstance(1, int)").unwrap(),
        Value::Bool(true)
    );
    assert_eq!(run("target = chr(65)").unwrap(), s("A"));
    assert_eq!(run("target = repr('a')").unwrap(), s("'a'"));
    assert_eq!(
        run("target = divmod(7, 2)").unwrap(),
        Value::Tuple(vec![int(3), int(1)])
    );
    assert_eq!(
        run("target = enumerate([5])").unwrap(),
        Value::List(vec![Value::Tuple(vec![int(0), int(5)])])
    );
    for f in [
        "upper('a')",
        "strlen('abc')",
        "coalesce(None, 1)",
        "slugify('A b')",
        "uuid()",
        "foo(1)",
        "getattr(src, 'a')",
    ] {
        let r = run(&format!("target = {f}"));
        assert!(matches!(r, Err(ExecError::Undefined { .. })), "{f}: {r:?}");
    }
}

#[test]
fn error_policy() {
    // The block stops at the error and keeps `target`.
    assert_eq!(run("target = 0\ntarget = None.a").unwrap(), int(0));
    assert_eq!(
        run("target = 1\ntarget = 1 / 0\ntarget = 2").unwrap(),
        int(1)
    );
    assert_eq!(
        run("target = 1\ntarget = [1][5]\ntarget = 2").unwrap(),
        int(1)
    );
    // `del target` makes the final read a KeyError.
    assert!(matches!(run("del target"), Err(ExecError::Uncaught(_))));
    // break at top level silently skips the rest.
    assert_eq!(run("break\ntarget = 1").unwrap(), Value::Null);
}

#[test]
fn quirks() {
    // A `raise` directly in a try body has no exc_info: a typed handler fails.
    assert_eq!(
        run("try:\n  raise ValueError('x')\nexcept ValueError:\n  target = 1").unwrap(),
        Value::Null
    );
    // ... but a bare except catches it.
    assert_eq!(
        run("try:\n  raise ValueError('x')\nexcept:\n  target = 1").unwrap(),
        int(1)
    );
    // `del` of a missing name inside try is silently swallowed (no error recorded).
    assert_eq!(
        run("try:\n  del nothere\nexcept KeyError:\n  target = 1\ntarget = 2").unwrap(),
        int(2)
    );
    // Functions see and restore the flat symbol table.
    assert_eq!(run("def f():\n  target = 5\nf()\n").unwrap(), Value::Null);
    assert_eq!(
        run("def f(n):\n  return 0 if n == 0 else 1 + f(n - 1)\ntarget = f(20)").unwrap(),
        int(20)
    );
    assert_eq!(
        run("def f(n):\n  return 0 if n == 0 else 1 + f(n - 1)\ntarget = f(200)").unwrap(),
        Value::Null
    );
}

#[test]
fn src_access() {
    let mut src = IndexMap::new();
    src.insert("a".to_string(), int(1));
    src.insert("name".to_string(), s("bob"));
    src.insert(
        "events".to_string(),
        Value::List(vec![Value::Map(
            [("year".to_string(), int(2000))].into_iter().collect(),
        )]),
    );
    assert_eq!(run_with("target = src.a + 1", src.clone()).unwrap(), int(2));
    assert_eq!(
        run_with("target = src.missing", src.clone()).unwrap(),
        Value::Null
    );
    assert_eq!(
        run_with("target = src.name.upper()", src.clone()).unwrap(),
        s("BOB")
    );
    assert_eq!(
        run_with("target = [e.year for e in src.events]", src.clone()).unwrap(),
        Value::List(vec![int(2000)])
    );
    assert_eq!(
        run_with("target = a", src).unwrap_err(),
        run("target = a").unwrap_err()
    );
}

fn list(v: &[Value]) -> Value {
    Value::List(v.to_vec())
}

#[test]
fn interrupt_flags_survive_a_handled_error() {
    // Verified against asteval 1.0.10: `raise_exception` leaves `_interrupt` set, so
    // the statements after a handled error in a loop body are skipped.
    let t = "target = 0\nfor i in range(3):\n    try:\n        x = 1 / 0\n    except ZeroDivisionError:\n        target += 1\n    target += 10";
    assert_eq!(run(t).unwrap(), int(3));
    // `continue` in a `while` skips the next test, so this would never end; the
    // step limit turns the upstream hang into an error.
    let t = "i = 0\nwhile i < 3:\n    i += 1\n    continue\ntarget = i";
    assert!(matches!(run(t), Err(ExecError::Uncaught(_))));
}

#[test]
fn error_list_decides_whether_a_typed_except_matches() {
    // One level of nesting leaves an error with `exc_info`: the handler runs.
    assert_eq!(
        run("try:\n    x = unknown\nexcept NameError:\n    target = 'ne'").unwrap(),
        s("ne")
    );
    // Deeper nesting is deduplicated to the innermost error, which has none:
    // `None()` is called and the block aborts.
    assert!(matches!(
        run("try:\n    x = 1 + (2 + unknown)\nexcept NameError:\n    target = 'ne'"),
        Err(ExecError::Undefined { .. })
    ));
    // `except <not a Python builtin>` catches everything; `except <builtin non-class>` fails.
    assert_eq!(
        run("try:\n    x = 1 / 0\nexcept sqrt:\n    target = 's'").unwrap(),
        s("s")
    );
    assert_eq!(
        run("try:\n    x = 1 / 0\nexcept len:\n    target = 'l'").unwrap(),
        Value::Null
    );
    assert_eq!(
        run("try:\n    x = 1 / 0\nexcept object:\n    target = 'o'").unwrap(),
        s("o")
    );
}

#[test]
fn flat_symbol_table_and_calls() {
    assert_eq!(
        run("x = 1\ndef f():\n    x = 10\n    return x\nf()\ntarget = x").unwrap(),
        int(1)
    );
    assert_eq!(
        run("x = 1\ndef f():\n    return x\nx = 2\ntarget = f()").unwrap(),
        int(2)
    );
    assert_eq!(
        run("def f(a, b=2, *c, **d):\n    return [a, b, len(c), len(d)]\ntarget = f(1, 5, 6, 7, k=1)")
            .unwrap(),
        list(&[int(1), int(2), int(3), int(1)])
    );
    assert_eq!(
        run("def f(a):\n    return a\ntarget = f()").unwrap(),
        Value::Null
    );
    // `i` is gone once the comprehension ends: the call raises a NameError.
    assert!(matches!(
        run("target = [lambda: i for i in range(2)][0]()"),
        Err(ExecError::Undefined { .. })
    ));
}

#[test]
fn comprehension_variables_leak_only_on_error() {
    assert!(matches!(
        run("y = [z for z in range(3)]\ntarget = z"),
        Err(ExecError::Undefined { .. })
    ));
    assert_eq!(
        run("try:\n    y = [zz for zz in range(3) if 1 / 0]\nexcept ZeroDivisionError:\n    pass\ntarget = zz")
            .unwrap(),
        int(0)
    );
}

#[test]
fn aliasing_and_in_place_methods() {
    assert_eq!(
        run("a = b = []\na.append(1)\ntarget = b").unwrap(),
        list(&[int(1)])
    );
    assert_eq!(
        run("x = [1, 2]\ny = x\ny += [3]\ntarget = x").unwrap(),
        list(&[int(1), int(2)])
    );
    assert_eq!(
        run("x = {'a': [1]}\ny = dict(x)\ny['a'].append(2)\ntarget = x['a']").unwrap(),
        list(&[int(1), int(2)])
    );
}

#[test]
fn formatting() {
    assert_eq!(
        run("target = f'{1234567.891:,.2f}|{255:#x}|{\"a\":*^5}|{1:03d}'").unwrap(),
        s("1,234,567.89|0xff|**a**|001")
    );
    assert_eq!(
        run("target = '%5.2f|%-4s|%03d' % (3.14159, 'a', 7)").unwrap(),
        s(" 3.14|a   |007")
    );
    assert_eq!(run("target = f'{100:.3}'").unwrap(), Value::Null);
}

#[test]
fn numeric_semantics() {
    assert_eq!(
        run("target = [7 // -2, -7 % 3, 5.5 % 2, round(2.675, 2), round(2.5), 2 ** -1]").unwrap(),
        list(&[
            int(-4),
            int(2),
            Value::Float(1.5),
            Value::Float(2.67),
            int(2),
            Value::Float(0.5)
        ])
    );
    assert_eq!(run("target = sum([0.1] * 10)").unwrap(), Value::Float(1.0));
    assert_eq!(run("target = 2 ** 10001").unwrap(), Value::Null);
    assert_eq!(run("target = 1 << 1001").unwrap(), Value::Null);
}

#[test]
fn string_length_limit() {
    // asteval's MAX_STR_LEN is 2 << 17.
    assert_eq!(run("target = len('a' * 262144)").unwrap(), int(262144));
    assert_eq!(run("target = 'a' * 262145").unwrap(), Value::Null);
    assert_eq!(
        run("x = 'a' * 262144\ntarget = len(x + 'b')").unwrap(),
        Value::Null
    );
}

/// A `datetime.date` PyYAML built from the data stays a date in a statement
/// block (corpus: examples sweep `personinfo-to-agent.transform.yaml`, whose
/// `driving_since` block picks a date out of `src.has_important_life_events`).
#[test]
fn dates_from_the_source_stay_dates() {
    use crate::temporal::Temporal;
    let date = |t: &str| Value::Temporal(Temporal::from_yaml_timestamp(t).unwrap());
    let mut event = IndexMap::new();
    event.insert("when".to_string(), date("2014-03-31"));
    let mut src = IndexMap::new();
    src.insert("born".to_string(), date("1990-01-01"));
    src.insert("events".to_string(), Value::List(vec![Value::Map(event)]));
    let r = |text: &str| run_with(text, src.clone()).unwrap();
    assert_eq!(r("target = src.born"), date("1990-01-01"));
    assert_eq!(
        r("d = [e.when for e in src.events]\nif len(d):\n    target = d[0]"),
        date("2014-03-31")
    );
    assert_eq!(r("target = str(src.born)"), s("1990-01-01"));
    assert_eq!(r("target = repr(src.born)"), s("datetime.date(1990, 1, 1)"));
    assert_eq!(r("target = src.born == src.born"), Value::Bool(true));
    assert_eq!(r("target = src.born == '1990-01-01'"), Value::Bool(false));
    assert_eq!(
        r("target = src.born < src.events[0].when"),
        Value::Bool(true)
    );
}
