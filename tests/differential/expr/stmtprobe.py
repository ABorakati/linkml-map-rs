#!/usr/bin/env python
"""Block / statement probes against upstream asteval, unrestricted mode only.

``python stmtprobe.py [FILE]``  probes are `----`-separated blocks in FILE
(default: the built-in list); prints every probe where Rust and upstream differ.
Bindings: B below, as a `src` DynObj.
"""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))

from campaign import enc_bindings, py_pair  # noqa: E402
from codec import show  # noqa: E402
from oracle import Oracle  # noqa: E402

B = {"a": 1, "b": 2.5, "s": "hello", "n": None, "l": [1, 2, 3], "m": {"a": 1, "b": {"c": 2}}, "x": 0, "t": "", "ev": [{"y": 2000, "k": "A"}, {"y": 2010, "k": "B"}]}


def check(blocks: list[str], bindings=B, verbose=False) -> int:
    oracle = Oracle()
    cases = [{"expr": e, "bindings": enc_bindings(bindings), "mode": "unrestricted", "strict": False} for e in blocks]
    rs = oracle.run(cases)
    bad = 0
    for e, case, r in zip(blocks, cases, rs):
        py, d = py_pair(case, r)
        if d is not None:
            bad += 1
            print("DIVERGE", repr(e))
            print("   py:", show(py.get("ok")) if "ok" in py else f"ERR {py.get('err')}: {py.get('message', '')[:100]}")
            print("   rs:", show(r.get("ok")) if "ok" in r else f"ERR {r.get('err')}: {r.get('message', '')[:100]}")
        elif verbose:
            print("ok", repr(e), "->", show(py.get("ok")) if "ok" in py else "ERR")
    print(f"{len(blocks)} probes, {bad} diverge")
    return bad


if __name__ == "__main__":
    args = [a for a in sys.argv[1:] if a != "-v"]
    text = Path(args[0]).read_text() if args else None
    blocks = [b.strip("\n") for b in text.split("\n----\n")] if text else []
    sys.exit(1 if check(blocks, verbose="-v" in sys.argv) else 0)
