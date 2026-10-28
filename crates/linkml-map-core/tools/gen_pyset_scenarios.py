#!/usr/bin/env python3
"""Generate crates/linkml-map-core/tests/data/pyset_scenarios.json: random set
operation sequences with CPython's iteration order after every step (the
reference for src/pyset.rs). Run with CPython 3.13:

    python3 tools/gen_pyset_scenarios.py > tests/data/pyset_scenarios.json
"""
import json
import random
import struct
import sys

rng = random.Random(20261028)


def enc(v):
    if v is None:
        return None
    if isinstance(v, bool):
        return v
    if isinstance(v, int):
        return v
    if isinstance(v, float):
        return {"f": struct.pack(">d", v).hex()}
    if isinstance(v, tuple):
        return {"t": [enc(x) for x in v]}
    raise TypeError(v)


def scalar():
    r = rng.random()
    if r < 0.55:
        return rng.choice([rng.randint(-20, 40), rng.randint(-1000, 1000), rng.randint(0, 5) * 8, rng.randint(0, 5) * 64, rng.randint(-(2**62), 2**62), rng.randint(0, 3) * 2**61])
    if r < 0.65:
        return rng.choice([True, False])
    if r < 0.72:
        return None
    if r < 0.9:
        return rng.choice([0.5, 1.5, 2.0, -3.0, 0.1, 1e300, -0.0, float("inf"), float("-inf"), 7.25, 1e-320, 3.0, rng.random() * 100])
    return rng.choice([0.0, 1.0, 2.0])


def elem(depth=0):
    if depth < 2 and rng.random() < 0.12:
        return tuple(elem(depth + 1) for _ in range(rng.randint(0, 3)))
    return scalar()


def scenario():
    sets = {}
    ops = []
    snaps = []
    names = ["a", "b", "c", "d"]

    def snap():
        snaps.append({n: [enc(x) for x in s] for n, s in sets.items()})

    def pick():
        return rng.choice(list(sets))

    def other_than(s):
        # CPython special-cases `a op a` (identity); the engine's values have no identity.
        others = [n for n in sets if n != s]
        return rng.choice(others) if others else None

    for n in names[:2]:
        items = [elem() for _ in range(rng.randint(0, 14))]
        sets[n] = set(items)
        ops.append(["new", n, [enc(x) for x in items]])
        snap()
    for _ in range(rng.randint(3, 16)):
        k = rng.choice(["add"] * 4 + ["discard"] * 3 + ["pop", "clear", "op", "op", "op", "op", "update", "dupdate", "inter_u", "sym_u", "copy", "setdict", "new", "union_items", "diff_items", "inter_items", "sym_items"])
        s = pick()
        if k == "add":
            e = elem()
            sets[s].add(e)
            ops.append(["add", s, enc(e)])
        elif k == "discard":
            e = rng.choice(list(sets[s])) if sets[s] and rng.random() < 0.7 else elem()
            sets[s].discard(e)
            ops.append(["discard", s, enc(e)])
        elif k == "pop":
            if not sets[s]:
                continue
            sets[s].pop()
            ops.append(["pop", s])
        elif k == "clear":
            sets[s].clear()
            ops.append(["clear", s])
        elif k == "op":
            o = rng.choice(["|", "&", "-", "^"])
            x = pick()
            y = other_than(x)
            if y is None:
                continue
            d = rng.choice(names)
            r = {"|": sets[x] | sets[y], "&": sets[x] & sets[y], "-": sets[x] - sets[y], "^": sets[x] ^ sets[y]}[o]
            sets[d] = r
            ops.append(["op", d, o, x, y])
        elif k == "update":
            y = other_than(s)
            if y is None:
                continue
            sets[s].update(sets[y])
            ops.append(["update", s, y])
        elif k == "dupdate":
            keys = [elem() for _ in range(rng.randint(0, 12))]
            d = dict.fromkeys(keys)
            sets[s].update(d)
            ops.append(["dupdate", s, [enc(x) for x in d]])
        elif k == "inter_u":
            y = other_than(s)
            if y is None:
                continue
            sets[s].intersection_update(sets[y])
            ops.append(["inter_u", s, y])
        elif k == "sym_u":
            y = other_than(s)
            if y is None:
                continue
            sets[s].symmetric_difference_update(sets[y])
            ops.append(["sym_u", s, y])
        elif k == "copy":
            d = rng.choice(names)
            sets[d] = sets[s].copy()
            ops.append(["copy", d, s])
        elif k == "setdict":
            keys = [elem() for _ in range(rng.randint(0, 12))]
            d = rng.choice(names)
            sets[d] = set(dict.fromkeys(keys))
            ops.append(["setdict", d, [enc(x) for x in dict.fromkeys(keys)]])
        elif k == "new":
            items = [elem() for _ in range(rng.randint(0, 20))]
            d = rng.choice(names)
            sets[d] = set(items)
            ops.append(["new", d, [enc(x) for x in items]])
        elif k in ("union_items", "diff_items", "inter_items", "sym_items"):
            items = [elem() for _ in range(rng.randint(0, 10))]
            d = rng.choice(names)
            meth = {"union_items": "union", "diff_items": "difference", "inter_items": "intersection", "sym_items": "symmetric_difference"}[k]
            sets[d] = getattr(sets[s], meth)(items)
            ops.append([k, d, s, [enc(x) for x in items]])
        snap()
    return {"ops": ops, "snaps": snaps}


json.dump([scenario() for _ in range(int(sys.argv[1]) if len(sys.argv) > 1 else 400)], sys.stdout, separators=(",", ":"))
