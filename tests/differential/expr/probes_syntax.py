#!/usr/bin/env python
"""Probes for the expression syntax the parser and lexer cover, written to ``corpus/syntax.jsonl``.

``syntax_probes.json`` lists the expressions (class, expression, mode) and the shared
bindings; this script runs each through upstream and the Rust oracle, records upstream's
result, and marks it ``pass`` (they agree) or ``divergence`` (with a reason, for the few
places that cannot agree: integers beyond ``i64``, tuples and sets as lists, ...).

    python probes_syntax.py            # report disagreements
    python probes_syntax.py --write corpus/syntax.jsonl
"""

from __future__ import annotations

import argparse
import json
import sys
import zlib
from pathlib import Path

HERE = Path(__file__).parent
sys.path.insert(0, str(HERE))

from campaign import enc_bindings, py_pair  # noqa: E402
from codec import show  # noqa: E402
from oracle import Oracle  # noqa: E402

KNOWN = [
    ("bigint", "integers beyond i64 have no Value (a literal becomes a float, a result is an error)", lambda e, py, rs: any(c in e for c in ("9223372036854775808", "0xffffffffffffffff", "1e20"))),
    ("tuple-vs-list", "Value has no tuple or set: they evaluate as lists", lambda e, py, rs: "f'{1,2}'" in e or "f'{ {1,2} }'" in e),
    ("dict-keys-stringified", "dict displays stringify non-string keys", lambda e, py, rs: "{1: 2," in e),
    ("unrestricted-statements", "statement forms beyond the interpreter's (chained assignment, augmented assignment)", lambda e, py, rs: e.startswith("a += ") or "src.a = 1" in e),
    ("unrestricted-namespace", "the asteval namespace is not the LinkML function table", lambda e, py, rs: any(w in e for w in ("upper(", "key=len", "target = type", "target = print", "bytes(1)"))),
    ("complex-attr", "complex numbers have no Value (`1j.real` is evaluated by asteval)", lambda e, py, rs: "1j.real" in e),
    ("dict-methods", "a dict display in asteval is a real dict (`{}.keys()`); here it is a map read like the engine's DynObj", lambda e, py, rs: e.endswith("{}.keys()")),
    ("str-format-attr", "`'{0.__class__}'.format(..)` reads attributes of arbitrary objects", lambda e, py, rs: "__class__" in e and "format" in e),
]


def dump(entry: dict) -> str | None:
    """One JSON line; ``None`` for an entry holding a lone surrogate (upstream's
    ``'\\ud800'``), which neither UTF-8 nor ``serde_json`` can carry."""
    try:
        return json.dumps(entry, ensure_ascii=False).encode("utf-8").decode("utf-8")
    except UnicodeEncodeError:
        return None


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--write", type=Path)
    ap.add_argument("--quiet", action="store_true")
    a = ap.parse_args()
    data = json.loads((HERE / "syntax_probes.json").read_text(encoding="utf-8"))
    bindings = enc_bindings(data["bindings"])
    cases, meta = [], []
    for p in data["probes"]:
        cases.append({"expr": p["expr"], "bindings": bindings, "mode": p["mode"], "strict": False})
        meta.append(p["class"])
    rs = Oracle().run(cases)
    entries, diverge = [], 0
    for case, cls, r in zip(cases, meta, rs):
        py, d = py_pair(case, r)
        # A SyntaxError asteval swallows (result null) is a documented, deliberate difference.
        if case["mode"] == "unrestricted" and r.get("err") in ("lex", "parse") and py.get("ok", 1) is None:
            continue
        up = {k: v for k, v in py.items() if k in ("ok", "err")}
        reason = ""
        if d is not None:
            known = next((k for k in KNOWN if k[2](case["expr"], py, r)), None)
            cls, reason = (known[0], known[1]) if known else (cls, "unclassified: see the probe")
            diverge += 1
            if not a.quiet:
                print(f"[{cls}] {case['mode'][0]} {case['expr']!r}")
                print(f"     upstream={show(py['ok']) if 'ok' in py else 'ERR ' + py['err'] + ': ' + py['message'][:70]}")
                print(f"     rust    ={show(r['ok']) if 'ok' in r else 'ERR ' + r['err'] + ': ' + r.get('message', '')[:70]}")
        # Statement-block (asteval) probes mirror the restricted ones; keep a quarter of
        # them (chosen by a stable hash) plus every divergence to bound the corpus.
        if case["mode"] == "unrestricted" and d is None and zlib.crc32(case["expr"].encode("utf-8", "replace")) % 4:
            continue
        entries.append({**case, "upstream": up, "status": "pass" if d is None else "divergence", "class": cls, "reason": reason})
    print(f"{len(entries)} probes, {diverge} diverge")
    if a.write:
        lines = [line for line in map(dump, entries) if line is not None]
        a.write.write_text("\n".join(lines) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
