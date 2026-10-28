#!/usr/bin/env python
"""Leaf-semantics oracles: Rust helpers vs CPython / upstream, one process per function.

    upvenv/bin/python leaf.py --n 20000 --seed 1 [--only round,title] [--out DIR]

Each leaf generates ``n`` random inputs, evaluates the Python reference in this
process, ships the same inputs to the Rust oracle in one batch and reports every
disagreement (with the first few minimal-looking examples per class).
References are CPython stdlib (``float``, ``repr``, ``decimal``, ``str``
methods, ``json``) except where the Rust code ports an upstream helper
(``FUNCTIONS`` in eval_utils, ``dumps_json`` in utils/serialization, python-slugify).
"""

from __future__ import annotations

import argparse
import collections
import decimal
import json
import math
import random
import struct
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
sys.setrecursionlimit(5000)

from codec import canon, encode, h2f, show  # noqa: E402
from grammar import FLOAT_EDGE_TEXT, STR_EDGES  # noqa: E402
from oracle import Oracle  # noqa: E402

from linkml_map.utils import eval_utils as EU  # noqa: E402
from linkml_map.utils.serialization import dumps_json  # noqa: E402

# ─── random generators ───────────────────────────────────────────────────────

UNI_POOL = (
    list("abcxyzABCXYZ0123456789 _-.,'\"\\\t\n/")
    + list("éÉèüÜñÑçÇøØåÅßẞſŉǆǅǄǈǇǋǊʼnΐΰϊΣσςΑΩαωİıǰﬁﬂﬃﬆ")
    + list("ΑΣΣΔ日本語한글ЖжЯя😀🎉∑∂ǅ")
    + [" ", " ", " ", "　", "​", "﻿", "\x85", "\x1c", "\x1f", "́", "̈", "ͅ", "ẞ", "Σ", "İ", "٣", "１", "²", "Ⅷ", "½", "ǰ", "ΐ", "\x00", "\x7f", "\x80", "­"]
)


def rstr(rng: random.Random, maxlen: int = 8) -> str:
    r = rng.random()
    if r < 0.35:
        return rng.choice(STR_EDGES)
    if r < 0.85:
        return "".join(rng.choice(UNI_POOL) for _ in range(rng.randint(0, maxlen)))
    return "".join(chr(rng.choice([rng.randint(0x20, 0x7E), rng.randint(0xA0, 0x24F), rng.randint(0x370, 0x3FF), rng.randint(0x400, 0x4FF), rng.randint(0x1E00, 0x1FFF), rng.randint(0x2000, 0x206F), rng.randint(0x10000, 0x1FFFF)])) for _ in range(rng.randint(0, maxlen)))


def rfloat(rng: random.Random) -> float:
    r = rng.random()
    if r < 0.15:
        return float(rng.choice(FLOAT_EDGE_TEXT).replace("_", ""))
    if r < 0.30:
        return rng.choice([math.inf, -math.inf, math.nan, 0.0, -0.0, 5e-324, 1.7976931348623157e308, 2.2250738585072014e-308, 1e16, 9007199254740992.0, 9007199254740993.0, 123456789012345680.0, 0.1 + 0.2])
    if r < 0.50:
        return struct.unpack(">d", rng.getrandbits(64).to_bytes(8, "big"))[0]
    if r < 0.65:
        return rng.randint(-10**6, 10**6) / rng.choice([1, 2, 4, 8, 10, 100, 1000])  # exact halves and friends
    if r < 0.8:
        return rng.randint(-10**3, 10**3) + rng.choice([0.5, 0.25, 0.125, 0.05, 0.005, 0.0005, 0.15, 0.25, 0.35, 0.45, 0.55, 0.65, 0.75, 0.85])
    if r < 0.85:
        return float(f"{rng.randint(1, 10**rng.randint(1, 17))}e{rng.randint(-330, 310)}")
    if r < 0.93:
        # 2^46..2^53 binades: spacing 1/64..1, where shortest-repr ties (x.25, x.125) appear
        return (2**52 + rng.getrandbits(52)) * 2.0 ** rng.randint(-6, 0) * rng.choice([1, 1, 0.5, 0.25])
    return rng.uniform(-1e6, 1e6)


def _float_or_inf(v):
    """``float(v)``; an int beyond the float range is +-inf (the Rust ``try_numeric`` has no way to raise)."""
    try:
        return float(v)
    except OverflowError:
        return math.inf if v > 0 else -math.inf


def rint(rng: random.Random) -> int:
    r = rng.random()
    if r < 0.2:
        return rng.choice([0, 1, -1, 2**31, 2**53, 2**53 + 1, -(2**53) - 1, 2**63 - 1, -(2**63), 255, 2**63, -(2**63) - 1, 2**64, 10**30])
    if r < 0.6:
        return rng.randint(-100, 100)
    if r < 0.85:
        return rng.randint(-(2**62), 2**62)
    return rng.randint(-(2**200), 2**200) * rng.choice([1, 1, 2**rng.randint(0, 900)])  # beyond i64, up to the float range and past it


def rscalar(rng: random.Random):
    return rng.choice([lambda: None, lambda: rng.random() < 0.5, lambda: rint(rng), lambda: rfloat(rng), lambda: rstr(rng), lambda: rstr(rng)])()


def rvalue(rng: random.Random, depth: int = 0):
    r = rng.random()
    if depth < 3 and r < 0.2:
        return [rvalue(rng, depth + 1) for _ in range(rng.randint(0, 3))]
    if depth < 3 and r < 0.3:
        return {rng.choice(["a", "b", "", "é", "k 1", "x\"y", "\n", "1"]): rvalue(rng, depth + 1) for _ in range(rng.randint(0, 3))}
    return rscalar(rng)


def numeric_text(rng: random.Random) -> str:
    """Strings float()/int()/Decimal() meet in the wild."""
    r = rng.random()
    sign = rng.choice(["", "", "-", "+"])
    digits = lambda n: "".join(rng.choice("0123456789") for _ in range(n))  # noqa: E731
    if r < 0.15:
        return rstr(rng)
    if r < 0.25:
        return rng.choice(["inf", "-inf", "+Infinity", "nan", "-NaN", "snan", "INF", "Inf", "infinit", "iNFINITY", "nan1", "sNaN12", "NaN123"])
    pad = lambda s: rng.choice(["", "", " ", "\t", "\n", " ", " ", "\x1c"]) + s + rng.choice(["", "", " ", "\n", " "])  # noqa: E731
    base = digits(rng.randint(0, 5))
    if rng.random() < 0.5:
        base += "." + digits(rng.randint(0, 5))
    if rng.random() < 0.3:
        base = base.replace(rng.choice("0123456789"), "_", 1) if rng.random() < 0.5 else base
    if rng.random() < 0.3:
        base += rng.choice(["e", "E"]) + rng.choice(["", "+", "-"]) + digits(rng.randint(0, 4))
    if rng.random() < 0.1:
        base = "".join(rng.choice([c, rng.choice("٠١٢٣٤٥٦٧٨٩０１２３４５６７８９")]) for c in base)
    if rng.random() < 0.05:
        base = rng.choice(["0x", "0b", "0o"]) + base
    return pad(sign + base)


# ─── leaf specs ──────────────────────────────────────────────────────────────


def _bool(x):
    return bool(x)


def py_eq_ref(a, b):
    return a == b


def py_json(x, indent=None):
    return dumps_json(x, indent=indent) if indent is not None else dumps_json(x)


def strip_nulls(o):
    if isinstance(o, dict):
        return {k: strip_nulls(v) for k, v in o.items() if v is not None}
    if isinstance(o, list):
        return [strip_nulls(v) for v in o]
    return o


def dec_ref(text: str):
    try:
        d = decimal.Decimal(text)
    except decimal.InvalidOperation:
        return {"err": "invalid"}
    if not d.is_finite():
        return {"err": "notfinite"}
    return {"ok": encode({"sci": str(d), "fixed": format(d, "f"), "norm_sci": str(d.normalize()), "zero": d == 0, "neg": d.is_signed(), "f64": float(d)})}


def FN(name):  # noqa: N802
    return EU.FUNCTIONS[name]


def typed(rng, kind):
    return {"str": lambda: rstr(rng), "int": lambda: rint(rng), "float": lambda: rfloat(rng), "num": lambda: rfloat(rng) if rng.random() < 0.7 else rint(rng), "any": lambda: rvalue(rng), "list": lambda: [rvalue(rng, 2) for _ in range(rng.randint(0, 4))], "smallint": lambda: rng.randint(-5, 8), "ndigits": lambda: rng.randint(-3, 6), "str_numeric": lambda: numeric_text(rng)}[kind]()


def fn_spec(name, kinds, p_wrong=0.12, extra=None):
    """Spec calling the upstream FUNCTIONS[name] with typed random args."""

    def gen(rng):
        args = []
        for k in kinds:
            if k.endswith("?"):
                if rng.random() < 0.5:
                    continue
                k = k[:-1]
            args.append(typed(rng, "any") if rng.random() < p_wrong else typed(rng, k))
        if name == "round" and len(args) > 1 and isinstance(args[1], int) and abs(args[1]) > 10**5:
            # CPython computes 10**-ndigits: a huge ndigits hangs upstream itself (reported separately).
            args[1] = rng.randint(-5, 5)
        return args

    return {"rust": f"fn:{name}", "gen": gen, "ref": lambda *a: FN(name)(*a)}


LEAF_REASONS = {
    "py_eq": "int vs float equality: CPython compares exactly, Rust casts the int to f64 (2**53+1 == 2.0**53)",
    "try_numeric": "float() accepts unicode decimal digits; Rust's parse does not (and Rust accepts \\x1c-\\x1f padding that CPython rejects)",
    "parse_float": "float() accepts unicode decimal digits; Rust's parse does not (and Rust accepts \\x1c-\\x1f padding that CPython rejects)",
    "float_repr": "shortest-repr ties: Python rounds half to even (x.2), Rust's `{:e}` rounds half up (x.3)",
    "float_repr_io": "shortest-repr ties: Python rounds half to even (x.2), Rust's `{:e}` rounds half up (x.3)",
    "repr_str": "repr() escapes every non-printable code point (Cc, Cf, Zs except space, Zl, Zp, Co, Cn); Rust escapes only ASCII control characters",
    "py_repr": "repr() escapes every non-printable code point; Rust escapes only ASCII control characters",
    "py_str": "str(list/dict) uses repr() of elements: non-printable escapes missing",
    "str": "str(list/dict) uses repr() of elements: non-printable escapes missing; float tie digits",
    "str_float": "shortest-repr ties: Python rounds half to even, Rust half up",
    "decimal": "Decimal() accepts unicode digits, leading/trailing underscores, \\x1c-\\x1f padding; Rust accepts ASCII digits and single inner underscores only",
    "int": "int() and float(): Rust accepts \\x1c-\\x1f padding (CPython rejects)",
    "int_any": "\\x1c-\\x1f padding accepted by Rust",
    "float": "Rust float() accepts \\x1c-\\x1f padding that CPython rejects",
    "float_any": "Rust float() accepts \\x1c-\\x1f padding that CPython rejects",
    "title": "Unicode data skew: Rust's tables are newer than CPython 3.13's (Unicode 16 vs 15.1)",
    "capitalize": "Unicode data skew: Rust's tables are newer than CPython 3.13's (Unicode 16 vs 15.1)",
    "upper": "Unicode data skew: Rust's tables are newer than CPython 3.13's (Unicode 16 vs 15.1)",
    "startswith": "a list prefix raises TypeError upstream; Rust answers false",
    "endswith": "a list suffix raises TypeError upstream; Rust answers false",
    "contains": "an unhashable needle raises TypeError upstream; Rust answers false",
    "slugify": "transliteration tables differ: deunicode (Rust) vs text-unidecode (python-slugify)",
    "is_numeric": "Rust accepts \\x1c-\\x1f padding that CPython float() rejects",
}

SPECS: dict[str, dict] = {
    "py_eq": {"rust": "py_eq", "gen": lambda r: [rvalue(r, 1), rvalue(r, 1)] if r.random() < 0.5 else [rfloat(r), rint(r)] if r.random() < 0.5 else [rscalar(r)] * 2, "ref": lambda a, b: a == b},
    "truthy": {"rust": "truthy", "gen": lambda r: [rvalue(r)], "ref": _bool},
    "try_numeric": {"rust": "try_numeric", "gen": lambda r: [rvalue(r, 2) if r.random() < 0.5 else numeric_text(r)], "ref": lambda x: (lambda v: _float_or_inf(v) if v is not None else None)(EU._try_numeric(x))},
    "parse_float": {"rust": "parse_float", "gen": lambda r: [numeric_text(r)], "ref": lambda s: _pf(s)},
    "float_repr": {"rust": "float_repr", "gen": lambda r: [rfloat(r)], "ref": repr},
    "float_repr_io": {"rust": "float_repr_io", "gen": lambda r: [rfloat(r)], "ref": repr},
    "repr_str": {"rust": "repr_str", "gen": lambda r: [rstr(r, 12)], "ref": repr},
    "py_repr": {"rust": "py_repr", "gen": lambda r: [rvalue(r)], "ref": repr},
    "py_str": {"rust": "py_str", "gen": lambda r: [rvalue(r)], "ref": str},
    "json_dumps": {"rust": "json_dumps", "gen": lambda r: [rvalue(r), r.choice([None, 2, 4, 0, 1])] , "ref": lambda x, ind: py_json(x, ind)},
    "json_dumps_skipnull": {"rust": "json_dumps_skipnull", "gen": lambda r: [rvalue(r), r.choice([None, 2])], "ref": lambda x, ind: py_json(strip_nulls(x), ind)},
    "decimal": {"rust": "decimal", "gen": lambda r: [numeric_text(r)], "ref": dec_ref, "wrapped": True},
    # builtins (upstream FUNCTIONS) -------------------------------------------------
    "int": fn_spec("int", ["str_numeric"], 0.2),
    "int_any": {**fn_spec("int", ["any"], 0.0), "gen": lambda r: [typed(r, r.choice(["float", "int", "str", "any", "str_numeric"]))]},
    "float": {**fn_spec("float", ["str_numeric"], 0.2)},
    "float_any": {**fn_spec("float", ["any"], 0.0), "gen": lambda r: [typed(r, r.choice(["float", "int", "str", "any", "str_numeric"]))]},
    "str": fn_spec("str", ["any"], 0.0),
    "str_float": {**fn_spec("str", ["float"], 0.0), "gen": lambda r: [rfloat(r)]},
    "bool": fn_spec("bool", ["any"], 0.0),
    "abs": fn_spec("abs", ["num"]),
    "round1": fn_spec("round", ["num"]),
    "round2": fn_spec("round", ["num", "ndigits"]),
    "floor": fn_spec("floor", ["num"]),
    "ceil": fn_spec("ceil", ["num"]),
    "title": fn_spec("title", ["str"]),
    "capitalize": fn_spec("capitalize", ["str"]),
    "upper": fn_spec("upper", ["str"]),
    "lower": fn_spec("lower", ["str"]),
    "strip": fn_spec("strip", ["str", "str?"]),
    "lstrip": fn_spec("lstrip", ["str", "str?"]),
    "rstrip": fn_spec("rstrip", ["str", "str?"]),
    "replace": fn_spec("replace", ["str", "str", "str", "smallint?"]),
    "startswith": fn_spec("startswith", ["str", "str"]),
    "endswith": fn_spec("endswith", ["str", "str"]),
    "contains": fn_spec("contains", ["str", "str"]),
    "split": fn_spec("split", ["str", "str?", "smallint?"]),
    "join": fn_spec("join", ["str", "list"]),
    "substr": fn_spec("substr", ["str", "smallint", "smallint?"]),
    "strlen": fn_spec("strlen", ["str"]),
    "slugify": fn_spec("slugify", ["str", "str?"]),
    "to_snake": fn_spec("to_snake", ["str"]),
    "to_camel": fn_spec("to_camel", ["str"]),
    "to_pascal": fn_spec("to_pascal", ["str"]),
    "is_numeric": fn_spec("is_numeric", ["any"], 0.0),
    "sum": fn_spec("sum", ["list"]),
    "max": fn_spec("max", ["list"]),
    "min": fn_spec("min", ["list"]),
    "sorted": fn_spec("sorted", ["list"]),
    "len": fn_spec("len", ["any"], 0.0),
}


def _pf(s: str):
    try:
        return float(s)
    except ValueError:
        return None


def py_encode_result(fn, args):
    try:
        v = fn(*args)
        return {"ok": encode(v)}
    except BaseException as e:  # noqa: BLE001
        return {"err": type(e).__name__, "message": str(e)[:200]}


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--n", type=int, default=10000)
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--only", default="")
    ap.add_argument("--out", type=Path, default=None)
    ap.add_argument("--show", type=int, default=6)
    ap.add_argument("--write-corpus", type=Path, default=None, help="write replay entries (agreeing samples + shrunk-by-size divergences)")
    ap.add_argument("--agree-per-leaf", type=int, default=25)
    ap.add_argument("--div-per-class", type=int, default=4)
    ap.add_argument("--expect-clean", default="", help="comma list of leaves that must show no divergence (exit 1 otherwise)")
    a = ap.parse_args()
    oracle = Oracle()
    only = set(filter(None, a.only.split(",")))
    summary = {}
    allrows = []
    corpus: list[dict] = []
    failed_clean: list[str] = []
    clean = set(filter(None, a.expect_clean.split(",")))
    for name, spec in SPECS.items():
        if only and name not in only:
            continue
        rng = random.Random(f"{a.seed}:{name}")
        inputs, exp = [], []
        seen = set()
        while len(inputs) < a.n:
            args = spec["gen"](rng)
            key = repr(args)
            if key in seen:
                continue
            seen.add(key)
            inputs.append(args)
            if spec.get("wrapped"):
                exp.append(spec["ref"](*args))
            else:
                exp.append(py_encode_result(spec["ref"], args))
        cases = [{"leaf": spec["rust"], "args": [encode(x) for x in args]} for args in inputs]
        rs = oracle.run(cases)
        bad = collections.defaultdict(list)
        agree_rows = []
        for args, e, r in zip(inputs, exp, rs):
            if r.get("err") == "unrepresentable":
                continue
            pe, re_ = "ok" in e, "ok" in r
            if not pe and not re_:
                continue
            if pe != re_:
                cls = "py_ok_rs_err" if pe else "py_err_rs_ok"
            elif canon(e["ok"]) != canon(r["ok"]):
                cls = "value"
            else:
                agree_rows.append((args, e))
                continue
            bad[cls].append((args, e, r))
        if name in clean and bad:
            failed_clean.append(name)
        if a.write_corpus:
            pick = random.Random(f"corp:{name}")
            for args, e in pick.sample(agree_rows, min(a.agree_per_leaf, len(agree_rows))):
                corpus.append({"leaf": spec["rust"], "ref": name, "args": [encode(x) for x in args], "upstream": {k: v for k, v in e.items() if k in ("ok", "err")}, "status": "pass", "class": "leaf-agree", "reason": ""})
            for cls, rows in bad.items():
                rows.sort(key=lambda t: len(repr(t[0])))
                chosen = rows[: a.div_per_class // 2] + pick.sample(rows[a.div_per_class // 2 :], min(a.div_per_class - a.div_per_class // 2, max(0, len(rows) - a.div_per_class // 2)))
                for args, e, r in chosen:
                    corpus.append({"leaf": spec["rust"], "ref": name, "args": [encode(x) for x in args], "upstream": {k: v for k, v in e.items() if k in ("ok", "err")}, "status": "divergence", "class": f"leaf:{name}:{cls}", "reason": LEAF_REASONS.get(name, "see tests/differential/expr/leaf.py output")})
        total_bad = sum(len(v) for v in bad.values())
        summary[name] = {"n": len(inputs), "diverge": total_bad, **{k: len(v) for k, v in bad.items()}}
        print(f"{name:22s} n={len(inputs)} diverge={total_bad} " + " ".join(f"{k}={len(v)}" for k, v in bad.items()), flush=True)
        for cls, rows in bad.items():
            rows.sort(key=lambda t: len(repr(t[0])))
            for args, e, r in rows[: a.show]:
                pr = show(e["ok"]) if "ok" in e else f"ERR {e['err']}: {e.get('message', '')[:80]}"
                rr = show(r["ok"]) if "ok" in r else f"ERR {r['err']}: {r.get('message', '')[:80]}"
                print(f"    [{cls}] {name}({', '.join(show(encode(x)) for x in args)})\n        py={pr}\n        rs={rr}")
            for args, e, r in rows:
                allrows.append({"leaf": name, "cls": cls, "args": [encode(x) for x in args], "py": e, "rs": r})
    if a.write_corpus:
        a.write_corpus.write_text("\n".join(json.dumps(x, ensure_ascii=False) for x in corpus) + "\n", encoding="utf-8")
    if failed_clean:
        print("EXPECTED CLEAN BUT DIVERGED:", failed_clean)
        sys.exit(1)
    if a.out:
        a.out.mkdir(parents=True, exist_ok=True)
        (a.out / "leaf_summary.json").write_text(json.dumps(summary, indent=1))
        with (a.out / "leaf_divergences.jsonl").open("w") as fh:
            for row in allrows:
                fh.write(json.dumps(row, ensure_ascii=False) + "\n")


if __name__ == "__main__":
    main()
