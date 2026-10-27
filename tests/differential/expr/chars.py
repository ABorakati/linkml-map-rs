#!/usr/bin/env python
"""Exhaustive per-code-point leaf scan (every scalar value U+0000..U+10FFFF, no surrogates).

For each string function, compares Rust with upstream for the one-character
string ``chr(cp)`` (and ``"a" + chr(cp) + "b"`` for whitespace-sensitive ones),
then prints the differing code points as ranges. Run on the same CPython whose
Unicode version upstream used (the skew between CPython's and Rust's Unicode
tables shows up here, not as a logic bug).
"""

from __future__ import annotations

import sys
import unicodedata
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))

from codec import canon, encode  # noqa: E402
from leaf import SPECS, py_encode_result  # noqa: E402
from oracle import Oracle  # noqa: E402

CPS = [c for c in range(0x110000) if not 0xD800 <= c < 0xE000]


def ranges(cps: list[int]) -> str:
    out, i = [], 0
    while i < len(cps):
        j = i
        while j + 1 < len(cps) and cps[j + 1] == cps[j] + 1:
            j += 1
        out.append(f"U+{cps[i]:04X}" + (f"..U+{cps[j]:04X}" if j > i else ""))
        i = j + 1
    return ", ".join(out)


SCANS = {
    "slugify": ("slugify", lambda c: [chr(c)]),
    "upper": ("upper", lambda c: [chr(c)]),
    "lower": ("lower", lambda c: [chr(c)]),
    "title": ("title", lambda c: ["a" + chr(c) + "b"]),
    "title1": ("title", lambda c: [chr(c)]),
    "capitalize": ("capitalize", lambda c: [chr(c) + "xY"]),
    "capitalize1": ("capitalize", lambda c: [chr(c)]),
    "repr_str": ("repr_str", lambda c: [chr(c)]),
    "strip": ("strip", lambda c: [chr(c) + "x" + chr(c)]),
    "split_ws": ("split", lambda c: ["a" + chr(c) + "b"]),
    "float": ("float", lambda c: [chr(c) + "5"]),
    "float_digit": ("float", lambda c: [chr(c)]),
    "int": ("int", lambda c: [chr(c) + "5"]),
    "int_digit": ("int", lambda c: [chr(c)]),
    "is_numeric": ("is_numeric", lambda c: ["7" + chr(c)]),
    "to_snake": ("to_snake", lambda c: ["a" + chr(c) + "B"]),
    "to_pascal": ("to_pascal", lambda c: ["a" + chr(c) + "b"]),
}


def main() -> None:
    only = set(sys.argv[1].split(",")) if len(sys.argv) > 1 else set()
    oracle = Oracle()
    for scan, (spec_name, mk) in SCANS.items():
        if only and scan not in only:
            continue
        spec = SPECS.get(spec_name) or SPECS[spec_name + "1"]
        cases, exp = [], []
        for c in CPS:
            args = mk(c)
            cases.append({"leaf": spec["rust"], "args": [encode(a) for a in args]})
            if spec_name == "repr_str":
                exp.append({"ok": encode(repr(args[0]))})
            else:
                exp.append(py_encode_result(spec["ref"], args))
        rs = oracle.run(cases)
        bad: dict[str, list[int]] = {}
        sample: dict[str, tuple] = {}
        for c, e, r in zip(CPS, exp, rs):
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
                continue
            bad.setdefault(cls, []).append(c)
            sample.setdefault(cls, (c, e, r))
        print(f"== {scan}: {sum(map(len, bad.values()))} differing of {len(CPS)}")
        for cls, cps in bad.items():
            c, e, r = sample[cls]
            print(f"  [{cls}] {len(cps)}: {ranges(cps)[:1500]}")
            print(f"     e.g. U+{c:04X} ({unicodedata.category(chr(c))}) py={str(e.get('ok', e))[:80]} rs={str(r.get('ok', r))[:80]}")


if __name__ == "__main__":
    main()
