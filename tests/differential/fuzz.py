"""Hypothesis campaign: generate worlds, run both engines, shrink and record divergences.

    /home/user/upstream/upvenv/bin/python tests/differential/fuzz.py --examples 3000 --seed 1 [--time-budget SECS]
        [--profiles core,joins,...] [--corpus DIR] [--max-found N]

Each round calls ``hypothesis.find`` for "a world whose Rust run diverges from upstream with a signature not yet
seen"; Hypothesis shrinks it to a minimal counterexample, which is written to ``corpus/<hash>/``.  Signatures seen are
excluded from later rounds, so a campaign reports one minimised case per distinct divergence kind.
"""

from __future__ import annotations

import argparse
import json
import random
import re
import sys
import time
from pathlib import Path

from hypothesis import HealthCheck, Phase, Verbosity, settings
from hypothesis import find as hyp_find
from hypothesis.errors import NoSuchExample

sys.path.insert(0, str(Path(__file__).resolve().parent))
import gen  # noqa: E402
import harness as H  # noqa: E402

CORPUS = H.HERE / "corpus"
PROFILES = {
    "all": gen.ALL_FEATS,
    "core": frozenset({"core"}),
    "awkward": frozenset({"awkward", "typeof"}),
    "enums": frozenset({"enums"}),
    "expr": frozenset({"expr", "awkward"}),
    "joins": frozenset({"joins"}),
    "nested": frozenset({"nested"}),
    "multivalued": frozenset({"multivalued"}),
    "inherit": frozenset({"inherit", "constants"}),
    "units": frozenset({"units"}),
    "mint": frozenset({"mint"}),
    "yaml": frozenset({"yamlraw", "awkward", "multivalued"}),
    "tsv": frozenset({"rawtsv", "awkward"}),
}


def diff_hint(r: H.FormatResult) -> str:
    if r.kind != "output_diff":
        return ""
    a = r.up.out.decode("utf-8", "replace").splitlines()
    b = r.rs.out.decode("utf-8", "replace").splitlines()
    if len(a) != len(b):
        return f"lines {'more' if len(b) > len(a) else 'fewer'}"
    for x, y in zip(a, b):
        if x != y:
            vx, vy = (x.split(": ", 1) + [x])[1], (y.split(": ", 1) + [y])[1]
            shape = lambda v: re.sub(r"[A-Za-z]+", "a", re.sub(r"\d+", "#", v))[:12]  # noqa: E731
            return f"{shape(vx)}~{shape(vy)}"
    return "trailing"


def full_signature(results):
    parts = []
    for r in H.divergences(results):
        up = (r.up.exc_class or "") if not r.up.ok else ""
        rs = (r.rs.exc_class or "") if not r.rs.ok else ""
        parts.append(f"{r.fmt}:{r.kind}:{up}:{rs}:{diff_hint(r)}")
    return "|".join(parts)


def coarse_signature(results):
    # group formats: a divergence that appears in every format is one signature
    kinds = sorted({(r.kind, (r.up.exc_class or "") if not r.up.ok else "", (r.rs.exc_class or "") if not r.rs.ok else "", diff_hint(r))
                    for r in H.divergences(results)})
    return ";".join("/".join(k) for k in kinds)


def known_signatures(corpus: Path) -> set[str]:
    out = set()
    for f in corpus.glob("*/signature.txt"):
        out.add(f.read_text().strip())
    return out


class Campaign:
    def __init__(self, args):
        self.args = args
        self.seen: dict[str, str] = {}
        self.runs = 0
        self.t0 = time.time()
        self.mode = args.upstream_mode
        self.corpus_dir = Path(args.corpus) if args.corpus else CORPUS
        if args.known_from_corpus:
            for sig in known_signatures(CORPUS):
                self.seen[sig] = "known"

    def evaluate(self, w) -> list[H.FormatResult]:
        with H.tmp_case_dir() as d:
            case = gen.write_world(w, d)
            self.runs += 1
            return H.compare_case(case, self.mode)

    def predicate(self, w) -> bool:
        res = self.evaluate(w)
        divs = H.divergences(res)
        if not divs:
            return False
        if self.args.only_kind and not any(r.kind in self.args.only_kind for r in divs):
            return False
        sig = coarse_signature(res)
        return sig not in self.seen

    def save(self, w, profile: str, seed: int) -> Path | None:
        with H.tmp_case_dir() as d:
            case = gen.write_world(w, d)
            res = H.compare_case(case, self.mode)
            if not H.divergences(res):
                return None
            sig = coarse_signature(res)
            self.seen[sig] = "saved"
            dest = H.save_corpus_case(case, res, self.corpus_dir,
                                      note=f"signature: `{full_signature(res)}`", seed=f"fuzz.py --seed {seed} profile={profile}")
            (dest / "signature.txt").write_text(sig + "\n")
            return dest

    def run(self):
        a = self.args
        rnd_seed = a.seed
        profiles = a.profiles.split(",")
        found = []
        rounds = 0
        budget_left = lambda: (a.time_budget is None) or (time.time() - self.t0 < a.time_budget)  # noqa: E731
        while budget_left() and self.runs < a.examples and len(found) < a.max_found:
            prof = profiles[rounds % len(profiles)]
            rounds += 1
            feats = PROFILES[prof]
            per_find = min(a.per_find, max(50, a.examples - self.runs))
            st = settings(max_examples=per_find, database=None, deadline=None, derandomize=False,
                          suppress_health_check=list(HealthCheck), phases=[Phase.generate, Phase.shrink], verbosity=Verbosity.quiet)
            rng = random.Random(f"{rnd_seed}-{rounds}")
            try:
                w = hyp_find(gen.worlds(feats), self.predicate, settings=st, random=rng)
            except NoSuchExample:
                print(f"[{time.time()-self.t0:6.0f}s] round {rounds} profile={prof}: no new divergence in {per_find} examples (runs={self.runs})", flush=True)
                continue
            except Exception as e:  # generator/harness bug: report and carry on
                print(f"[round {rounds}] harness error: {type(e).__name__}: {e}", flush=True)
                import traceback
                traceback.print_exc()
                continue
            dest = self.save(w, prof, rnd_seed)
            if dest:
                found.append(dest)
                print(f"[{time.time()-self.t0:6.0f}s] round {rounds} profile={prof}: NEW divergence -> {dest.name} (runs={self.runs})", flush=True)
        print(f"campaign finished: {self.runs} examples run, {len(found)} distinct divergences, {rounds} rounds, {time.time()-self.t0:.0f}s")
        return found


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--examples", type=int, default=1000, help="total example budget (engine runs of a generated case)")
    ap.add_argument("--per-find", type=int, default=400, help="max examples per hypothesis.find round")
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--time-budget", type=float, default=None, help="seconds")
    ap.add_argument("--profiles", default="all,joins,enums,expr,nested,multivalued,awkward,inherit,units,mint,yaml,tsv,core")
    ap.add_argument("--corpus", default=None)
    ap.add_argument("--max-found", type=int, default=60)
    ap.add_argument("--only-kind", action="append", default=[], help="only chase these divergence kinds (output_diff, rust_error, ...)")
    ap.add_argument("--upstream-mode", default="inproc", choices=["inproc", "subprocess"])
    ap.add_argument("--known-from-corpus", action="store_true", help="do not re-report signatures already in the corpus (CI gate mode)")
    a = ap.parse_args()
    H.require_upvenv()
    c = Campaign(a)
    found = c.run()
    print(json.dumps([str(f.name) for f in found]))
    return 0


if __name__ == "__main__":
    sys.exit(main())
