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
    "joins": frozenset({"joins", "joinvals"}),
    "nested": frozenset({"nested"}),
    "multivalued": frozenset({"multivalued"}),
    "inherit": frozenset({"inherit", "constants"}),
    "units": frozenset({"units"}),
    "mint": frozenset({"mint"}),
    "yaml": frozenset({"yamlraw", "awkward", "multivalued"}),
    "tsv": frozenset({"rawtsv", "awkward"}),
    "sources": frozenset({"sources", "inherit", "nested", "awkward"}),
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


def msg_shape(msg: str) -> str:
    """Message with paths, numbers and quoted names abstracted, so equal causes share a signature."""
    m = re.sub(r"/[^\s:'\"]+", "P", msg or "")
    m = re.sub(r"'[^']*'|\"[^\"]*\"", "Q", m)
    m = re.sub(r"\d+", "#", m)
    return m[:90]


def coarse_signature(results):
    # group formats: a divergence that appears in every format is one signature
    kinds = sorted({(r.kind, ((r.up.exc_class or "") + msg_shape(r.up.exc_msg)) if not r.up.ok else "",
                     ((r.rs.exc_class or "") + msg_shape(r.rs.exc_msg)) if not r.rs.ok else "", diff_hint(r))
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

    fmts = ["yaml", "json"]

    def evaluate(self, w) -> list[H.FormatResult]:
        with H.tmp_case_dir() as d:
            case = gen.write_world(w, d)
            case.formats = list(self.fmts)  # search on a subset of formats; the saved case is re-run on all five
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
            feats = PROFILES[prof] | ({"knownfree", "singleblock"} if a.avoid_known else set())
            per_find = min(a.per_find, max(50, a.examples - self.runs))
            st = settings(max_examples=per_find, database=None, deadline=None, derandomize=False,
                          suppress_health_check=list(HealthCheck), phases=[Phase.generate, Phase.shrink], verbosity=Verbosity.quiet)
            rng = random.Random(f"{rnd_seed}-{rounds}")
            self.fmts = ["yaml", "json", ["jsonl", "tsv", "csv"][rounds % 3]]
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


def promote_passing(a, n: int) -> list[Path]:
    """Save ``n`` generated cases on which both engines agree on non-trivial output, as regression guards for the replay test."""
    corpus = Path(a.corpus) if a.corpus else CORPUS
    saved: list[Path] = []
    mode = a.upstream_mode
    seen_hashes: set[str] = set()

    def good(w) -> bool:
        with H.tmp_case_dir() as d:
            case = gen.write_world(w, d)
            res = H.compare_case(case, mode)
            if not all(r.kind == "match" for r in res):
                return False
            if len(res[1].up.out) < 120:  # json output: demand some real content
                return False
            return H.case_hash(case) not in seen_hashes

    for i in range(n):
        prof = ["all", "joins", "enums", "expr", "nested", "multivalued"][i % 6]
        st = settings(max_examples=600, database=None, deadline=None, suppress_health_check=list(HealthCheck),
                      phases=[Phase.generate])
        try:
            w = hyp_find(gen.worlds(PROFILES[prof] | {"knownfree"}), good, settings=st, random=random.Random(f"promote-{a.seed}-{i}"))
        except NoSuchExample:
            continue
        with H.tmp_case_dir() as d:
            case = gen.write_world(w, d)
            res = H.compare_case(case, mode)
            seen_hashes.add(H.case_hash(case))
            saved.append(H.save_corpus_case(case, res, corpus, note="promoted passing case (regression guard)",
                                            seed=f"fuzz.py --promote seed={a.seed} profile={prof}"))
    return saved


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--examples", type=int, default=1000, help="total example budget (engine runs of a generated case)")
    ap.add_argument("--per-find", type=int, default=400, help="max examples per hypothesis.find round")
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--time-budget", type=float, default=None, help="seconds")
    ap.add_argument("--profiles", default="all,joins,enums,expr,nested,multivalued,awkward,inherit,units,mint,yaml,tsv,core,sources")
    ap.add_argument("--corpus", default=None)
    ap.add_argument("--max-found", type=int, default=60)
    ap.add_argument("--only-kind", action="append", default=[], help="only chase these divergence kinds (output_diff, rust_error, ...)")
    ap.add_argument("--upstream-mode", default="inproc", choices=["inproc", "subprocess"])
    ap.add_argument("--avoid-known", action="store_true", help="avoid generating inputs that hit already-reported open divergences (empty TSV cells, JSON stringification of non-ASCII, ...)")
    ap.add_argument("--promote", type=int, default=0, help="also save N agreeing, non-trivial cases as regression guards")
    ap.add_argument("--known-from-corpus", action="store_true", help="do not re-report signatures already in the corpus (CI gate mode)")
    a = ap.parse_args()
    H.require_upvenv()
    c = Campaign(a)
    found = c.run()
    if a.promote:
        print("promoted:", [p.name for p in promote_passing(a, a.promote)])
    print(json.dumps([str(f.name) for f in found]))
    return 0


if __name__ == "__main__":
    sys.exit(main())
