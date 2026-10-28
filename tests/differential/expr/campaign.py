#!/usr/bin/env python
"""Differential campaign: Hypothesis-generated cases -> upstream vs Rust oracle.

    upvenv/bin/python campaign.py run --n 100000 --seed 1 --out /tmp/exprfuzz-run1

Phases: generate (Hypothesis, no shrinking of passing runs) -> evaluate in
batches (upstream in-process, Rust via the batch-file oracle) -> collect
divergences -> shrink representatives -> cluster by minimal template ->
write ``divergences.jsonl`` + ``clusters.md`` (+ a sample of agreeing cases).
"""

from __future__ import annotations

import argparse
import collections
import json
import random
import re
import resource
import sys
import time
from pathlib import Path

from hypothesis import HealthCheck, Phase, seed as hseed, settings, given

sys.path.insert(0, str(Path(__file__).parent))
sys.setrecursionlimit(20000)

import grammar  # noqa: E402
from codec import encode  # noqa: E402
from classes import classify  # noqa: E402
from compare import compare  # noqa: E402
from oracle import Oracle  # noqa: E402
from pyside import run_py  # noqa: E402
from shrink import shrink_bindings, shrink_tree  # noqa: E402

resource.setrlimit(resource.RLIMIT_AS, (6 * 2**30, 6 * 2**30))

# Every grammar feature that currently produces a known divergence; shrink this as they are fixed.
CLEAN_MASK = set(
    "setdisp tuple pow bitwise".split()
)
PENDING_SYNTAX: set[str] = set()  # exprsyntax port merged (d46df82); nothing pending there any more


def enc_bindings(b: dict) -> dict:
    return {k: encode(v) for k, v in b.items()}


def make_case(c: dict) -> dict:
    return {"expr": grammar.render(c["tree"]), "bindings": enc_bindings(c["bindings"]), "mode": c["mode"], "strict": c["strict"]}


def py_pair(case: dict, rs: dict) -> tuple[dict, dict | None]:
    """Upstream result (``obj`` flavour first, ``dict`` if that diverges) and the divergence (None = agree)."""
    py = run_py(case["expr"], case["bindings"], case["mode"], case["strict"], "obj")
    d = compare(py, rs)
    if d is None:
        return py, None
    py2 = run_py(case["expr"], case["bindings"], case["mode"], case["strict"], "dict")
    d2 = compare(py2, rs)
    if d2 is None:
        return py2, None
    return py, d


def signature(py: dict, d: dict) -> tuple:
    kind = d["kind"]
    via = py.get("via") if py.get("via") != "restricted" else None
    return _signature(d, kind) + ((via,) if via else ())


def _signature(d: dict, kind: str) -> tuple:
    if kind == "py_err_rs_ok":
        return (kind, d["py_err"])
    if kind == "py_ok_rs_err":
        return (kind, d["rs_kind"])
    if kind == "rs_crash":
        return (kind, d["rs_kind"])
    return (kind,)


def evaluate(oracle: Oracle, cases: list[dict]):
    rs = oracle.run(cases)
    out = []
    for case, r in zip(cases, rs):
        py, d = py_pair(case, r)
        out.append((py, r, d))
    return out


def shrink_case(oracle: Oracle, c: dict, sig: tuple) -> dict:
    """Greedy batch shrink of tree then bindings, keeping the signature."""

    def check_many(cands: list[dict]) -> list[bool]:
        cs, idx = [], []
        for i, x in enumerate(cands):
            try:
                cs.append(make_case(x))
                idx.append(i)
            except Exception:  # noqa: BLE001
                pass  # replacement produced an unrenderable tree
        flags = [False] * len(cands)
        for i, (py, _r, d) in zip(idx, evaluate(oracle, cs)):
            flags[i] = d is not None and signature(py, d) == sig
        return flags

    # Tree shrink: evaluate every candidate of a level in one oracle batch.
    from shrink import candidates, size

    cur = c
    for _ in range(60):
        cands = [x for x in candidates(cur["tree"]) if size(x) < size(cur["tree"])]
        cands.sort(key=size)
        cands = cands[:300]
        if not cands:
            break
        flags = check_many([{**cur, "tree": t} for t in cands])
        hit = next((t for t, f in zip(cands, flags) if f), None)
        if hit is None:
            break
        cur = {**cur, "tree": hit}
    cur["bindings"] = shrink_bindings(cur["bindings"], lambda b: check_many([{**cur, "bindings": b}])[0])
    return cur


def template(expr: str) -> str:
    t = re.sub(r"'(?:[^'\\]|\\.)*'|\"(?:[^\"\\]|\\.)*\"", "S", expr)
    t = re.sub(r"\b\d[\d_]*(?:\.\d*)?(?:[eE][+-]?\d+)?\b", "N", t)
    return t


def tags_for(c: dict, py: dict, d: dict) -> list[str]:
    kinds = grammar.tree_kinds(c["tree"])
    tags = []
    if kinds & PENDING_SYNTAX:
        tags.append("pending:exprsyntax")
    if c["mode"] == "unrestricted" and py.get("via") in ("fallback", "asteval-block"):
        tags.append("via:" + py["via"])
    return tags


def collect(n: int, seed_: int, batch: int, mode: str | None):
    """Yield lists of ``batch`` generated cases."""
    buf: list[dict] = []
    out: list[list[dict]] = []

    @hseed(seed_)
    @settings(max_examples=n, database=None, phases=[Phase.generate], deadline=None, suppress_health_check=list(HealthCheck), derandomize=False)
    @given(grammar.cases(mode))
    def gen(c):
        buf.append(c)

    gen()
    for i in range(0, len(buf), batch):
        out.append(buf[i : i + batch])
    return out


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("cmd", choices=["run"])
    ap.add_argument("--n", type=int, default=20000)
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--batch", type=int, default=2000)
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--mask", default="", help="comma list of grammar features to disable (see grammar.MASK)")
    ap.add_argument("--profile", choices=["clean"], default=None, help="mask every feature currently known to diverge (CI profile)")
    ap.add_argument("--fail-on-unclassified", action="store_true", help="exit 1 if a shrunk divergence matches no known class in classes.py")
    ap.add_argument("--fail-on-divergence", action="store_true", help="exit 1 if any divergence is found (after shrinking)")
    ap.add_argument("--mode", choices=["restricted", "unrestricted"], default=None)
    ap.add_argument("--reps", type=int, default=5, help="representatives shrunk per coarse signature")
    ap.add_argument("--agree-sample", type=int, default=0, help="keep N agreeing non-error cases")
    a = ap.parse_args()
    if a.profile == "clean":
        grammar.MASK.update(CLEAN_MASK)
    grammar.MASK.update(filter(None, a.mask.split(",")))
    a.out.mkdir(parents=True, exist_ok=True)
    oracle = Oracle()
    t0 = time.time()
    stats = collections.Counter()
    sigs: dict[tuple, list] = collections.defaultdict(list)
    agree: list[dict] = []
    rng = random.Random(a.seed)
    done = 0
    gen_t = time.time()
    batches = collect(a.n, a.seed, a.batch, a.mode)
    print(f"generated {sum(map(len, batches))} cases in {time.time() - gen_t:.0f}s", flush=True)
    seen: set[str] = set()
    for cs in batches:
        cases = []
        kept = []
        for c in cs:
            try:
                case = make_case(c)
            except RecursionError:
                continue
            key = json.dumps(case, sort_keys=True, ensure_ascii=False)
            if key in seen:
                stats["duplicate"] += 1
                continue
            seen.add(key)
            cases.append(case)
            kept.append(c)
        cs = kept
        res = evaluate(oracle, cases)
        for c, case, (py, rs, d) in zip(cs, cases, res):
            stats["total"] += 1
            stats[f"mode:{c['mode']}"] += 1
            stats["py_ok" if "ok" in py else "py_err"] += 1
            if rs.get("err") == "unrepresentable":
                stats["unrepresentable"] += 1
            if d is None:
                stats["agree"] += 1
                if a.agree_sample and "ok" in py and "ok" in rs and rng.random() < 0.05 and len(agree) < a.agree_sample:
                    agree.append({**case, "upstream": {"ok": py["ok"], "via": py.get("via")}})
                continue
            sigs[(c["mode"], c["tree"][0] == "block") + signature(py, d)].append((c, case, py, rs, d))
        done += len(cs)
        print(f"[{done}/{a.n}] {time.time() - t0:.0f}s agree={stats['agree']} diverge={sum(len(v) for v in sigs.values())}", flush=True)
    stats["diverge"] = sum(len(v) for v in sigs.values())
    print(dict(stats), flush=True)

    # Shrink representatives (smallest rendered expressions first).
    final = []
    for sig, items in sorted(sigs.items(), key=lambda kv: -len(kv[1])):
        items.sort(key=lambda it: len(it[1]["expr"]))
        reps = items[: a.reps] + rng.sample(items[a.reps :], min(2, max(0, len(items) - a.reps)))
        for c, case, py, rs, d in reps:
            full_sig = signature(py, d)
            try:
                m = shrink_case(oracle, c, full_sig)
            except Exception as e:  # noqa: BLE001
                print("shrink failed", e, file=sys.stderr)
                m = c
            mc = make_case(m)
            (mpy, mrs, md) = evaluate(oracle, [mc])[0]
            if md is None:
                m, mc, mpy, mrs, md = c, case, py, rs, d
            final.append({"sig": list(sig), "count": len(items), "case": mc, "py": mpy, "rs": mrs, "diff": md, "tags": tags_for(m, mpy, md), "template": template(mc["expr"]), "orig_expr": case["expr"][:300], "class": classify(mc["expr"], mc["mode"], mpy, mrs, md)[0]})
    (a.out / "divergences.jsonl").write_text("\n".join(json.dumps(x, ensure_ascii=False) for x in final), encoding="utf-8")
    (a.out / "stats.json").write_text(json.dumps({**stats, "seed": a.seed, "n": a.n}), encoding="utf-8")
    if agree:
        (a.out / "agree.jsonl").write_text("\n".join(json.dumps(x, ensure_ascii=False) for x in agree), encoding="utf-8")
    # clusters.md
    cl = collections.defaultdict(list)
    for x in final:
        cl[(tuple(x["sig"]), x["template"])].append(x)
    lines = [f"# campaign seed={a.seed} n={a.n}\n", f"stats: {dict(stats)}\n"]
    for (sig, tmpl), xs in sorted(cl.items(), key=lambda kv: (-kv[1][0]["count"], kv[0][1])):
        x = xs[0]
        lines.append(f"- sig={sig} (x{x['count']}) tags={x['tags']} mode={x['case']['mode']}\n  expr=`{x['case']['expr']!r}` bindings={json.dumps(x['case']['bindings'], ensure_ascii=False)[:200]}\n  diff={json.dumps(x['diff'], ensure_ascii=False)[:400]}")
    (a.out / "clusters.md").write_text("\n".join(lines), encoding="utf-8")
    print(f"done in {time.time() - t0:.0f}s; clusters={len(cl)}")
    if a.fail_on_unclassified:
        bad = [x for x in final if x["class"] == "unclassified"]
        for x in bad[:10]:
            print("UNCLASSIFIED", json.dumps(x["case"], ensure_ascii=False)[:300], "upstream:", str(x["py"])[:100], "rust:", str(x["rs"])[:100])
        print(f"{len(final)} divergence(s) shrunk, {len(bad)} unclassified; classes: {dict(collections.Counter(x['class'] for x in final))}")
        if bad:
            sys.exit(1)
    if a.fail_on_divergence and final:
        for x in final[:10]:
            print("DIVERGENCE", json.dumps(x["case"], ensure_ascii=False)[:300], "upstream:", str(x["py"])[:100], "rust:", str(x["rs"])[:100])
        sys.exit(1)


if __name__ == "__main__":
    main()
