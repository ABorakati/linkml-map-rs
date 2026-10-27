#!/usr/bin/env python
"""Assemble ``corpus/*.jsonl`` from campaign output directories.

    python build_corpus.py --runs /tmp/exprfuzz-A /tmp/exprfuzz-D ... [--probes] [--leaf]

* ``agree.jsonl``        sampled agreeing cases (``status: pass``) from the runs' ``agree.jsonl``
* ``shrunk.jsonl``       the runs' shrunk divergences, classified (``status: divergence``)
* ``curated.jsonl``      ``probes.py`` output (pass + divergence, one group per class)
* ``leaf.jsonl``         ``leaf.py --write-corpus`` output
Upstream results are recorded here so the Rust replay needs no Python.
"""

from __future__ import annotations

import argparse
import collections
import json
import random
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).parent
sys.path.insert(0, str(HERE))
from classes import classify  # noqa: E402

CORPUS = HERE / "corpus"


def load(path: Path) -> list[dict]:
    if not path.exists():
        return []
    return [json.loads(x) for x in path.read_text(encoding="utf-8").split("\n") if x.strip()]


def dump(path: Path, rows: list[dict]) -> None:
    path.write_text("\n".join(json.dumps(r, ensure_ascii=False) for r in rows) + "\n", encoding="utf-8")


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--runs", nargs="*", type=Path, default=[])
    ap.add_argument("--agree-cap", type=int, default=2500)
    ap.add_argument("--probes", action="store_true")
    ap.add_argument("--leaf", type=int, default=0, help="leaf inputs per function (0 = skip)")
    ap.add_argument("--seed", type=int, default=1)
    a = ap.parse_args()
    CORPUS.mkdir(exist_ok=True)
    rng = random.Random(a.seed)

    if a.runs:
        agree = []
        for run in a.runs:
            for r in load(run / "agree.jsonl"):
                up = {"ok": r["upstream"]["ok"]}
                case = {k: r[k] for k in ("expr", "bindings", "mode", "strict")}
                agree.append({**case, "upstream": up, "status": "pass", "class": "agree", "reason": ""})
        rng.shuffle(agree)
        seen: set[str] = set()
        out = []
        for r in agree:
            k = json.dumps([r["expr"], r["mode"]], ensure_ascii=False)
            if k not in seen:
                seen.add(k)
                out.append(r)
        dump(CORPUS / "agree.jsonl", out[: a.agree_cap])
        print(f"agree.jsonl: {min(len(out), a.agree_cap)} entries")

        shrunk: dict[tuple, dict] = {}
        unclassified = []
        for run in a.runs:
            for r in load(run / "divergences.jsonl"):
                c = r["case"]
                py = {k: v for k, v in r["py"].items()}
                name, reason = classify(c["expr"], c["mode"], py, r["rs"], r["diff"])
                if name == "unclassified":
                    unclassified.append((c["expr"], r["diff"]))
                key = (name, r["template"])
                if key in shrunk:
                    continue
                up = {k: v for k, v in py.items() if k in ("ok", "err")}
                shrunk[key] = {**c, "upstream": up, "status": "divergence", "class": name, "reason": reason}
                if r["rs"].get("err") in ("hang", "crash"):
                    shrunk[key]["hazard"] = r["rs"]["err"]
        rows = sorted(shrunk.values(), key=lambda x: (x["class"], len(x["expr"])))
        dump(CORPUS / "shrunk.jsonl", rows)
        cnt = collections.Counter(r["class"] for r in rows)
        print("shrunk.jsonl:", dict(cnt))
        for expr, diff in unclassified[:40]:
            print("UNCLASSIFIED", repr(expr)[:120], {k: (v[:80] if isinstance(v, str) else v) for k, v in diff.items()})

    if a.probes:
        env_py = sys.executable
        subprocess.run([env_py, str(HERE / "probes.py"), "--quiet", "--write", str(CORPUS / "curated.jsonl")], check=True)
        rows = load(CORPUS / "curated.jsonl")
        print("curated.jsonl:", len(rows), "divergent:", sum(r["status"] == "divergence" for r in rows))

    if a.leaf:
        subprocess.run([sys.executable, str(HERE / "leaf.py"), "--n", str(a.leaf), "--seed", str(a.seed), "--show", "0", "--write-corpus", str(CORPUS / "leaf.jsonl")], check=True)
        rows = load(CORPUS / "leaf.jsonl")
        print("leaf.jsonl:", len(rows), "divergent:", sum(r["status"] == "divergence" for r in rows))


if __name__ == "__main__":
    main()
