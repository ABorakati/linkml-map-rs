#!/usr/bin/env python
"""Hand-written probes, one group per divergence class (or feature), run against both sides.

``python probes.py``            print agree/diverge per probe
``python probes.py --write F``  write corpus entries (status pass/divergence) to F
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))

from campaign import enc_bindings, py_pair  # noqa: E402
from codec import show  # noqa: E402
from oracle import Oracle  # noqa: E402

U = "unrestricted"
B_ALL = {"a": 1, "b": 2.5, "s": "hello", "n": None, "l": [1, 2, 3], "m": {"a": 1, "b": {"c": 2}}, "one": "1", "ws": " 2 ", "e3": "1e3", "inf": "inf", "digits": "١٢", "us": "1_0", "x": 0, "f": 0.0}

# class -> (reason, [probes]); a probe is an expression, or (expression, bindings[, mode]).
GROUPS: dict[str, tuple[str, list]] = {
    "div-zero": ("upstream lets ZeroDivisionError escape `_null_propagating_arithmetic` (only TypeError/ValueError are caught); Rust returns null", ["1/0", "1//0", "1%0", "1.0/0", ("a/x", B_ALL), ("a//f", B_ALL), ("a%x", B_ALL), "1/(1-1)", "divmod_(1)"]),
    "resource-limits": ("simpleeval caps results (MAX_STRING_LENGTH 100000, MAX_POWER 4e6, MAX_SHIFT 1e4, iterable length); Rust has no caps", ['"a" * 100001', '"ab" * 60000', "[1] * 100001", "2 ** 4000001", "1 << 10001", "10 ** 10 ** 10", "9 ** 9 ** 9", ("0 ** b", {"b": 9007199254740992.0}), ("0 >> z", {"z": "inf"}), '("a" * 99999) + "bb"', "[0] * 5000 + [0] * 5001 + [0]*100000"]),
    "bigint": ("arbitrary-precision int in Python; Rust Int is i64 (literals above i64 become floats, overflow is an error)", ["9223372036854775808", "-9223372036854775809", "9223372036854775807 + 1", "-9223372036854775808 - 1", "10 ** 20", "2 ** 64", "99999999999999999999 // 3", "~-100000000000000000000", "abs(-9223372036854775808)", "int(1e22)", "floor(1e30)", "round(1e22)", "sum([9223372036854775807, 1])", "3037000500 * 3037000500", "2 ** 63 - 1", "2 ** 100 % 7"]),
    "bitwise-on-float": ("float operands of | & ^ << >> ~ raise TypeError upstream (retry with floats fails too); Rust returns null/other", [("a | 0.0", B_ALL), "1.5 | 1", "1.5 & 1", "1.5 ^ 1", "1.5 << 1", "1 >> 1.5", "~1.5", "2.0 ** 2 | 1", ("b | a", B_ALL)]),
    "int-literal-forms": ("hex/octal/binary/underscore/exponent-int literal forms are not lexed", ["0x1f", "0b101", "0o17", "1_000", "0XFF", "1_0.5", "00", "0_0"]),
    "slicing": ("subscript slices `a[x:y:z]` are not lexed/parsed", ['"hello"[1:3]', "[1, 2, 3][:2]", "[1, 2, 3][::-1]", ("l[1:]", B_ALL), ("s[-3:]", B_ALL), ("s[::2]", B_ALL), "[1,2,3][5:]", ("n[1:2]", B_ALL)]),
    "string-literal-forms": ("triple-quoted, raw, u-prefixed and adjacent string literals are not lexed", ['"""abc"""', "r'a\\nb'", "u'x'", "'a' 'b'", "'''x'''", 'b"x"', "R'a'"]),
    "fstring": ("f-strings (ast.JoinedStr) are not supported", ["f\"{1}\"", ("f'{a}x'", B_ALL), ("f'{b:.2f}'", B_ALL), ("f'{s!r}'", B_ALL), "f'{1+1}'"]),
    "lambda-and-methods": ("lambda and method calls on values (`s.upper()`) are supported by simpleeval", ["(lambda q: q)(1)", ("s.upper()", B_ALL), ("s.split('l')", B_ALL), ("'-'.join(['a','b'])", B_ALL), ("s.startswith('h')", B_ALL), ("m.keys()", B_ALL), ("l.count(1)", B_ALL), ("s.format()", B_ALL), ("s.zfill(8)", B_ALL), ("s.upper", B_ALL)]),
    "matmul": ("`@` is lexed upstream and refused at evaluation (FeatureNotAvailable); Rust rejects at lex time, even in an untaken branch", ["1 @ 2", "0 if 1 else 0 @ 0", "1 or 2 @ 3"]),
    "in-not-iterable": ("`x in y` with a non-container y: upstream returns null after a warning (TypeError fallthrough); Rust raises", ["1 in 2", '"a" in 5', "True not in True", ("a in b", B_ALL), "None in 1", "1 in None", "[1] in 3"]),
    "comparison-typeerror": ("ordering across incomparable types: upstream returns null after a warning; check Rust", ["1 < 'a'", "'a' < 1", "[1] < 2", "None < 1", "[1] < [2]", "(1,) < (2,)", "[1, 'a'] < [1, 2]", "True < 'a'"]),
    "numeric-string-coercion": ("numeric strings coerce against numbers (upstream #285)", ['"1" + 1', '1 + "1"', '"1" * 3', '"1.5" + 1', '1 + " 2 "', '"1e3" * 2', '"١" + 1', '"inf" + 1', '"nan" + 1', '"1_0" + 1', '"0x10" + 1', '"1" + "1"', '"1" - "1"', '"1" * "3"', '"a" * 3', '3 * "a"', 'True + "1"', '"1" == 1', '"1" < 2', '"1.0" == 1', '"1" == 1.0', '" 1 " == 1', '"١" == 1', "'a' == 1", '1 == "1_0"', "'1e400' + 1", '"1" + 1.5', '"2" ** 2', '"2" ** "2"', '"-1" // 2', '"7" % 3', '2 ** "0.5"', '"+5" + 0', '"٣" * 2', '"1" if "1" == 1 else 2']),
    "equality-int-float-exact": ("Python compares int and float exactly; Rust casts the int to f64 (2**53+1 == 2.0**53)", ["9007199254740993 == 9007199254740992.0", "9007199254740992.0 == 9007199254740993", "9007199254740993 != 9007199254740992.0", "9007199254740993 > 9007199254740992.0", "9007199254740993 < 9007199254740992.0", "9223372036854775807 == 9.223372036854776e18", "9223372036854775807 < 9.223372036854776e18", ("a == n", B_ALL)]),
    "attribute-access": ("nested-map attribute access (DynObj vs dict flavours) and distribution over lists", [("m.a", B_ALL), ("m.nope", B_ALL), ("m.b.c", B_ALL), ("m.b.nope", B_ALL), ("l.a", B_ALL), ("[m, m].a", B_ALL), ("[m, m].b.c", B_ALL), ("{m}.a", B_ALL), ("{m.b.c}", B_ALL), ("m._p", B_ALL), ("s.a", B_ALL), ("n.a", B_ALL), ("a.b", B_ALL), ("m.a.b", B_ALL), ("[1,2].real", B_ALL), ("x.real", B_ALL), "(1).real", ("s.upper", B_ALL), ("m.items", B_ALL), ("m.keys", B_ALL)]),
    "comprehension-none": ("iterating None/an unbound name in a comprehension: upstream raises TypeError, Rust yields []", ["[0 for i in a]", ("[i for i in n]", B_ALL), ("[i for i in a]", B_ALL), ("[i for i in x]", B_ALL), ("[i for i in s]", B_ALL), ("[i for i in m]", B_ALL), ("[i + 1 for i in l if i > 1]", B_ALL), "[i for i in 5]", ("[k for k in m]", B_ALL), ("{k: v for k in l for v in l}", B_ALL)]),
    "none-propagation": ("SQL-style null propagation through operators and functions", [("n + 1", B_ALL), ("1 + n", B_ALL), ("n < 1", B_ALL), ("n == None", B_ALL), ("n != 1", B_ALL), ("not n", B_ALL), ("-n", B_ALL), ("n in l", B_ALL), ("a in n", B_ALL), ("upper(n)", B_ALL), ("len(n)", B_ALL), ("coalesce(n, 3)", B_ALL), ("case((n, 1), (True, 2))", B_ALL), ("n and 1", B_ALL), ("n or 1", B_ALL), ("sum([n, 1])", B_ALL), ("max(l + [n])", B_ALL), ("[n, 1][0] + 1", B_ALL), ("NULL + 1", B_ALL), ("strlen(NULL)", B_ALL)]),
    "builtins-misc": ("assorted built-in calls", [("round(b)", B_ALL), ("round(2.5)", B_ALL), ("round(3.5)", B_ALL), ("round(2.675, 2)", B_ALL), ("round(a, -1)", B_ALL), ("int(b)", B_ALL), ("int('1.5')", B_ALL), ("int(' 7 ')", B_ALL), ("float('inf')", B_ALL), ("float(' 1_0 ')", B_ALL), ("str(b)", B_ALL), ("str(l)", B_ALL), ("str(m)", B_ALL), ("str(None)", B_ALL), ("bool('')", B_ALL), ("bool([])", B_ALL), ("abs(-b)", B_ALL), ("floor(-b)", B_ALL), ("ceil(b)", B_ALL), ("substr(s, 1, 3)", B_ALL), ("substr(s, -2)", B_ALL), ("title('hello wORLD')", B_ALL), ("split('a,b,,c', ',')", B_ALL), ("split(' a  b ')", B_ALL), ("join(',', ['a','b'])", B_ALL), ("replace('aaa','a','b',2)", B_ALL), ("slugify('Hello, World!')", B_ALL), ("slugify('123 hits')", B_ALL), ("to_snake('HTTPResponse')", B_ALL), ("to_camel('snake_case')", B_ALL), ("sorted([3,1,2])", B_ALL), ("sorted(['b','a'])", B_ALL), ("sorted([1,'a'])", B_ALL), ("reversed(l)", B_ALL), ("first([])", B_ALL), ("last(l)", B_ALL), ("max([])", B_ALL), ("min(l)", B_ALL), ("sum(['a'])", B_ALL), ("sum([1, 2.5])", B_ALL), ("any([0, ''])", B_ALL), ("all([])", B_ALL), ("len('héllo')", B_ALL), ("strlen(l)", B_ALL), ("is_numeric('1_0')", B_ALL), ("is_numeric(True)", B_ALL), ("is_int(True)", B_ALL), ("is_float(1)", B_ALL), ("is_list([])", B_ALL), ("case()", B_ALL), ("case((False, 1))", B_ALL), ("coalesce()", B_ALL), ("uuid5('https://example.org/ns', 'x')", B_ALL), ("upper(l)", B_ALL), ("upper(['a', None, 'b'])", B_ALL), ("int(['1', '2'])", B_ALL), ("startswith('abc', 'a')", B_ALL), ("startswith('abc', ['a'])", B_ALL), ("contains({'a': 1}, [])", B_ALL), ("contains(m, 'a')", B_ALL), ("contains(l, 1)", B_ALL), ("list('ab')", B_ALL), ("tuple([1])", B_ALL), ("list(m)", B_ALL), ("max(1, 2)", B_ALL), ("min('b', 'a')", B_ALL), ("len()", B_ALL), ("round()", B_ALL), ("float('1', 2)", B_ALL)]),
    "unknown-names-and-functions": ("unbound names, unknown functions, private names", ["__import__('os')", "eval('1')", "print(1)", "open('x')", "zz", "zz.a", "zz + 1", "foo(1)", "_private(1)", "range(3)", "len_(1)", "type(1)", "isinstance(1, int)", ("max_(1)", B_ALL), "NULL", "None", "True", "False", "pi", "e", ("a.__class__", B_ALL), ("''.__class__", B_ALL), ("m.__dict__", B_ALL)]),
    "boolean-and-ternary": ("and/or return operands; ternary; chained comparison", ["1 and 2", "0 or 'x'", "[] or {}", "None and 1", "1 if 0 else 2", "(1 if 1 else 2) if 0 else 3", "1 < 2 < 3", "3 > 2 > 2", "1 == 1 == 1", "1 < 3 > 2", ("a < b < 3", B_ALL), ("a < n < 3", B_ALL), "not 1 == 2", "-2 ** 2", "2 ** -1", "2 ** 3 ** 2", "(-2) ** 0.5", "7 // -2", "-7 % 3", "7.5 // 2", "-7.5 % 2", "0.1 + 0.2", "1e16 + 1", "5 / 2", "4 / 2", "10 ** -2", "2 ** 0.5", "True + True", "True * 3", "~True", "-True", "+True", "1 ^ 3", "5 | 2", "6 & 3", "1 << 3", "-8 >> 1", "7 // 0.5", "1e308 * 10", "-1e308 * 10", "1e308 * 10 - 1e308 * 10", "0.0 * -1", "-0.0 + 0.0", "float('nan') == float('nan')"]),
    "literals-and-containers": ("list/tuple/dict/set displays, comprehensions, membership", ["[1, 2] + [3]", "[1] * 3", "3 * [1]", "(1, 2)", "(1,)", "()", "[]", "{}", "{'a': 1}", "{'a': 1}['a']", "{'a': 1}['b']", "[1, 2, 3][1]", "[1, 2, 3][-1]", "[1, 2, 3][3]", "'abc'[1]", "'abc'[5]", "(1, 2)[0]", "1 in [1, 2]", "'a' in 'abc'", "'a' in {'a': 1}", "1 in {1: 'a'}", "[1, [2]] == [1, [2]]", "(1, 2) == [1, 2]", "[1, 2] == (1, 2)", "{1, 2} == {2, 1}", "{'a': 1} == {'a': 1}", "[x*2 for x in [1, 2, 3] if x > 1]", "{x: x*2 for x in [1, 2]}", "list(x for x in [1, 2])", "dict(a=1)", "dict([('a', 1)])", "set([1, 1, 2])", "set()", "sorted({3, 1, 2})", "len({1, 1})", "{**{'a': 1}}", "[*[1, 2]]", "[[1, 2][i] for i in range(2)]", "[(i, j) for i in [1, 2] for j in [3, 4]]", "[i for i, j in [(1, 2), (3, 4)]]"]),

    "string-escapes": ("backslash escapes in string literals (\\xNN, \\uNNNN, \\UNNNNNNNN, \\N{..}, octal, line continuation) are not decoded", [r"'\x41'", r"'\u00e9'", r"'\U0001F600'", r"'\N{BULLET}'", r"'\101'", r"'\a'", r"'\q'", r'"\'"', "'a\\\nb'", r"'\0'", r"'\\'", r"'\n'", r"'\t'", r"'\''", r"'\"'", r"'\u2003'", r"'\x80'"]),
    "is-operator": ("`is` / `is not` are supported by simpleeval (identity); the Rust parser has no `is`", ["1 is 1", "None is None", ("a is None", B_ALL), ("n is None", B_ALL), "1 is not 2", ("n is not None", B_ALL), "'a' is 'a'", "[] is []", "True is True", "1 is 1.0"]),
    "tuple-vs-list": ("Value has no tuple: tuple displays evaluate as lists (distribute over functions, equal lists, concatenate with lists)", ["(1, 2) == [1, 2]", "() + [1]", "(1,) + (2,)", "[1] + (2,)", "to_snake(())", "floor(())", "upper(('a',))", "len((1, 2))", "(1, 2) < (1, 3)", "(1, 2)[0]", "().a", "list((1, 2))", "tuple([1, 2])", "tuple('ab')", "str((1, 2))", "str((1,))", "str(())", "[1, 2] in [(1, 2)]", "(1, 2) in [[1, 2]]", "[(1, 2)] == [[1, 2]]"]),
    "number-attr-lex": ("a float literal followed by `.name` is mis-lexed (`1.5.name`, `0.0._p`)", ["1.5.real", "0.0._p", "(1.5).real", "1 .real"]),
    "unrestricted-syntax-error": ("asteval swallows a SyntaxError (result null) while the restricted path raises; Rust raises", [(e, {}, U) for e in ["1 +", "x = ", "if :", "))", "'a", "1.id", "target =", "target = (", "target = 1 2", "@", "target = $x", "if 1:\ntarget = 1", "  target = 1", "target = 1\n  target = 2", "def", "1 1", "target = [1,", "else: 1"]]),
    "unrestricted-statements": ("statement forms beyond assignment / if / expression statements: for, while, aug-assign, del, pass, unpack, try, def, elif, one-line if", [(e, B_ALL, U) for e in ["pass", "target = 1\npass", "target = 1\ntarget += 1", "target = 5\ntarget -= 2", "target = [1]\ntarget *= 2", "tmp = 1\ndel tmp\ntarget = 1", "a, b = 1, 2\ntarget = a + b", "tmp, target = [1, 2]", "x = [1, 2]\nfor i in x:\n    target = i", "target = 0\nfor i in [1, 2, 3]:\n    target += i", "target = 0\nwhile target < 3:\n    target += 1", "if src.a:\n    target = 1\nelif src.b:\n    target = 2\nelse:\n    target = 3", "if src.a: target = 1", "if not src.a: target = 1\nelse: target = 2", "try:\n    target = 1 / 0\nexcept ZeroDivisionError:\n    target = 'z'", "def f(x):\n    return x * 2\ntarget = f(3)", "target = 1; target = 2", "target = 1 if src.a else 2", "target = [i * 2 for i in src.l]", "target = {k: v for k, v in [(1, 2)]}", "target = (1, 2)", "target = lambda: 1", "target: int = 3", "target = src.a = 7", "target = src.m.b.c", "assert src.a == 1\ntarget = 'ok'", "target = 1\ntarget", "target", "target = x = 5", "import math\ntarget = 1", "target = 'a' 'b'", "target = 0x10", "global target\ntarget = 1", "x = 1\nif x:\n    target = x\n    x = 2", "if True:\n    if True:\n        target = 'deep'", "if True:\n\ttarget = 'tab'", "if True:\n  target = 1\n    target = 2"]]),
    "unrestricted-namespace": ("asteval namespace: `src`, `target`, `uuid5` and Python builtins/math only; LinkML built-ins, bare slots, `NULL` and `{slot}` are undefined (NameError -> ValueError)", [(e, B_ALL, U) for e in ["target = upper('a')", "target = strlen('abc')", "target = len([1])", "target = max(1, 2)", "target = range(3)", "target = list(range(3))", "target = str(1)", "target = sorted([3, 1])", "target = round(2.5)", "target = abs(-1)", "target = int('7')", "target = len(src.l)", "target = src.a + 1", "target = {a}", "target = a", "target = NULL", "target = None", "target = uuid5('https://x', 'y')", "target = pi", "target = sqrt(4)", "target = sum([1, 2])", "target = min([])", "target = bool([])", "target = float('1.5')", "target = coalesce(None, 1)", "target = case((True, 1))", "target = slugify('A b')", "target = to_snake('AbC')", "target = first([1])", "target = is_int(1)", "target = type(1)", "target = isinstance(1, int)", "target = id(1)", "target = chr(65)", "target = ord('a')", "target = enumerate([1])", "target = zip([1], [2])", "target = dict(a=1)", "target = set([1])", "target = tuple([1])", "target = divmod(7, 2)", "target = pow(2, 3)", "target = reversed([1])", "target = any([0])", "target = repr('a')", "target = format(1)", "target = hash(1)", "target = getattr(src, 'a')", "target = src.__class__", "target = src.zz", "target = src.l[5]", "target = src.s.upper()", "target = 'x'.join(['a'])", "target = foo(1)", "target = uuid5(1, 2)", "target = [x for x in src.l]", "target = [x for x in src.nope]", "target = [x for x in src.n]", "target = len(src.n)", "target = src.n.a", "target = src.m.nope", "target = src.m.b.nope.q"]]),
    "unrestricted-fallback": ("restricted rejection (InvalidExpression/TypeError/ValueError) retried by asteval: a single expression leaves `target` unset -> null; other errors propagate", [(e, B_ALL, U) for e in ["foo(1)", "{1}", "{1, 2}", "{x for x in l}", "1 @ 2", "(lambda: 1)()", "slugify(", "upper('a', 'b', 'c')", "int('x')", "1/0", "zz.a", "zz", "zz + 1", "m.b.c + 1", "{l}", "[x for x in a]", "a.b.c", "s.upper()", "_p(1)", "case(1)", "max()", "[1][5]", "{'a': 1}['b']", "int([1])", "float(m)", "round('x')", "sorted([1, 'a'])", "len(1)", "1 in 2", "range(3)", "(1).real"]]),
    "strict-mode": ("strict: an unbound name or unresolved qualified root raises (NameError), also under unrestricted", [(e, B_ALL, m, True) for e in ["zz", "zz + 1", "zz.a", "a + zz", "n", "NULL", "m.zz", "a.zz", "[zz for i in l]", "upper(zz)", "{zz}", "target", "src", "src.a", "slot('x')"] for m in ("restricted", U)]),
    "comprehension-target-attr": ("attribute access on a comprehension-bound name is refused upstream (NameError) unless the name is also a binding", ["[i.a for i in l]", ("[i.a for i in l]", {"l": [{"a": 1}]}), ("[x.a for x in [m]]", B_ALL), ("[a.b for a in l]", B_ALL), ("[i for i in l if i.a]", {"l": [{"a": 1}]}), ("[[j.a for j in i] for i in l]", {"l": [[{"a": 1}]]}), ("[1 for i in l] + [i.a]", {"l": [1]}), ("{i.a: 1 for i in l}", {"l": [{"a": 1}]}), ("list(i.a for i in l)", {"l": [{"a": 1}]})]),
}


def normalise(p):
    if isinstance(p, str):
        return p, {}, "restricted", False
    expr, b, *rest = p
    return expr, b, (rest[0] if rest else "restricted"), (rest[1] if len(rest) > 1 else False)


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--write", type=Path)
    ap.add_argument("--quiet", action="store_true")
    a = ap.parse_args()
    cases, meta = [], []
    for cls, (reason, probes) in GROUPS.items():
        for p in probes:
            expr, b, mode, strict = normalise(p)
            cases.append({"expr": expr, "bindings": enc_bindings(b), "mode": mode, "strict": strict})
            meta.append((cls, reason))
    oracle = Oracle()
    rs = oracle.run(cases)
    entries = []
    counts: dict[str, list[int]] = {}
    for case, (cls, reason), r in zip(cases, meta, rs):
        py, d = py_pair(case, r)
        c = counts.setdefault(cls, [0, 0])
        c[0 if d is None else 1] += 1
        up = {k: v for k, v in py.items() if k in ("ok", "err", "message")}
        up.pop("message", None)
        entries.append({**case, "upstream": up, "status": "pass" if d is None else "divergence", "class": cls, "reason": reason if d is not None else ""})
        if not a.quiet and d is not None:
            print(f"[{cls}] {case['expr']!r}  bindings={'…' if case['bindings'] else '{}'}")
            print(f"     upstream={show(py['ok']) if 'ok' in py else 'ERR ' + py['err'] + ': ' + py['message'][:70]}")
            print(f"     rust    ={show(r['ok']) if 'ok' in r else 'ERR ' + r['err'] + ': ' + r.get('message', '')[:70]}")
    print({k: f"{v[0]} agree / {v[1]} diverge" for k, v in counts.items()})
    if a.write:
        a.write.write_text("\n".join(json.dumps(e, ensure_ascii=False) for e in entries) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
