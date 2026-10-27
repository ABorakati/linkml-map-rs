"""Assign a ``category:`` to the ``expected_divergence.md`` of corpus cases, from stored artefacts only (no engine runs).

    python3 tests/differential/triage.py [--corpus DIR] [--all]     # --all re-triages already categorised cases

Rules are ordered; the first match wins.  Each rule gives (category, summary).  Categories carry a suffix naming the
suspected cause: ``port:<name>`` for the in-flight ports (multiblock, restricted-eval, exprsyntax, yaml), ``rust-gap`` for a
Rust deviation, ``rust-lenient`` where Rust accepts what upstream rejects, ``upstream-quirk`` where upstream itself looks
buggy (suggested upstream issue).
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent


def load(case: Path) -> dict:
    meta = json.loads((case / "case.json").read_text())
    ctx = {"dir": case, "meta": meta, "flags": meta.get("flags", []), "spec": (case / meta["spec"]).read_text(),
           "schema": (case / meta["schema"]).read_text(), "fmts": {}}
    ctx["target"] = (case / meta["target_schema"]).read_text() if meta.get("target_schema") else ""
    for fmt in meta.get("formats", ["yaml", "json", "jsonl", "tsv", "csv"]):
        e = case / "expected"
        o = case / "observed_rust"
        up_out = (e / f"{fmt}.out").read_bytes().decode("utf-8", "replace") if (e / f"{fmt}.out").exists() else None
        up_err = json.loads((e / f"{fmt}.error").read_text()) if (e / f"{fmt}.error").exists() else None
        rs_out = (o / f"{fmt}.out").read_bytes().decode("utf-8", "replace") if (o / f"{fmt}.out").exists() else None
        rs_err = json.loads((o / f"{fmt}.error").read_text()) if (o / f"{fmt}.error").exists() else None
        if up_out is None and up_err is None:
            continue
        ctx["fmts"][fmt] = {"up_out": up_out, "up_err": up_err, "rs_out": rs_out, "rs_err": rs_err,
                            "divergent": rs_out is not None or rs_err is not None}
    return ctx


def first_div(ctx: dict) -> dict | None:
    for f in ctx["fmts"].values():
        if f["divergent"]:
            return f
    return None


def rs_msg(f) -> str:
    return (f["rs_err"] or {}).get("message", "") if f else ""


def up_msg(f) -> str:
    return (f["up_err"] or {}).get("message", "") if f else ""


def has(ctx, pat):
    return re.search(pat, ctx["spec"]) is not None


def stream_cases(ctx):
    return ctx["meta"]["input"] == "data" or ctx["meta"]["input"].endswith((".tsv", ".csv"))


RULES = [
    ("rust-gap: nested class_derivations in list form rejected",
     "Rust rejects slot-level `class_derivations:` written as a list (the documented form); upstream accepts list and mapping.",
     lambda c, f: "expected a map" in rs_msg(f) and has(c, r"(?m)^\s+class_derivations:\n\s+- name:")),
    ("rust-gap: scalar given for a list-valued spec field",
     "Rust requires `missing_values` (and similar `Any`-multivalued fields) to be a list; upstream accepts a scalar.",
     lambda c, f: "expected a sequence" in rs_msg(f)),
    ("port:known-open: TSV/CSV empty cell kept as ''",
     "Upstream's loader drops empty cells (the slot is absent); Rust keeps an empty string.",
     lambda c, f: c["meta"]["input"].endswith((".tsv", ".csv")) or c["meta"]["input"] == "data" and any(
         re.search(r"(?m)(\t\t|\t$|,,|,$)", p.read_text()) for p in (c["dir"] / "data").glob("*.[tc]sv"))
     and f["up_out"] is not None and f["rs_out"] is not None and ("''" in f["rs_out"] or '""' in f["rs_out"] or "\t\t" in f["rs_out"])),
    ("rust-gap: stringification delimiter on a single value into a multivalued target",
     "With a multivalued target slot upstream splits the (single) value on the delimiter into a list; Rust keeps the scalar.",
     lambda c, f: has(c, r"stringification:") and f["up_out"] is not None and f["rs_out"] is not None),
    ("rust-gap: pivot_operation (melt/unmelt) not applied",
     "Rust ignores `pivot_operation`: melt yields nothing and unmelt yields the wrong shape.",
     lambda c, f: has(c, r"pivot_operation")),
    ("rust-gap: offset result not coerced to the integer target range",
     "Upstream coerces `offset` results through the target slot range (25); Rust emits 25.0.",
     lambda c, f: has(c, r"offset:") and f["up_out"] is not None and f["rs_out"] is not None),
    ("rust-gap: enum_derivation expr semantics",
     "`enum_derivations.*.expr` is evaluated differently (result must be used verbatim even when a permissible-value derivation also matches).",
     lambda c, f: has(c, r"enum_derivations") and has(c, r"expr:") and f["up_out"] is not None and f["rs_out"] is not None),
    ("rust-gap: dotted attribute access on partially populated inline objects",
     "`a.b.c` in an expression must propagate None for absent keys of an inlined dict (upstream DynObj); Rust errors `value of type X has no attribute`.",
     lambda c, f: "has no attribute" in rs_msg(f)),
    ("rust-gap: schema slot absent from the row must evaluate to None",
     "Upstream resolves a slot declared in the schema but absent from the row to None (even with --strict); Rust raises `name 'x' is not defined`.",
     lambda c, f: "is not defined" in rs_msg(f) and f["up_out"] is not None),
    ("rust-gap: integers beyond 64 bits",
     "Rust's YAML/spec reader rejects integers outside i64/u64 (`JSON number out of range`) and TSV integers above i64 stay strings; Python ints are arbitrary precision.",
     lambda c, f: "number out of range" in rs_msg(f) or has(c, r"\d{19,}") or any(
         re.search(r"\d{19,}", p.read_text()) for p in (c["dir"] / "data").rglob("*") if p.is_file())),
    ("port:exprsyntax: expression syntax not supported",
     "Rust's expression parser rejects syntax upstream's evaluator accepts (reported at plan-compile time, even with no rows).",
     lambda c, f: "parse error" in rs_msg(f) or "could not parse expression" in rs_msg(f)),
    ("rust-lenient: nested derivation without a target schema",
     "Upstream raises (`'NoneType' object has no attribute 'induced_slot'`) when slot-level class_derivations run with no target schema; Rust emits the nested object.",
     lambda c, f: "induced_slot" in up_msg(f) and f["rs_out"] is not None),
    ("rust-lenient: derived slot absent from the target schema",
     "Upstream raises `No such slot ...` when a slot derivation names a slot the target class lacks; Rust ignores it.",
     lambda c, f: "No such slot" in up_msg(f) and f["rs_out"] is not None),
    ("upstream-quirk: value_mappings with an empty-string value crash spec loading",
     "Upstream raises `TypeError: 'str' object does not support item assignment` while loading a `value_mappings` whose value is ''; Rust accepts it.",
     lambda c, f: "does not support item assignment" in up_msg(f)),
    ("upstream-quirk: list-valued `value:` truncated to its first element",
     "Upstream's spec loader keeps only the first item of `value: [a, b]`; Rust keeps the list.",
     lambda c, f: has(c, r"(?ms)value:\n\s+- ") and f["up_out"] is not None and f["rs_out"] is not None),
    ("port:restricted-eval: restricted evaluator gate",
     "Upstream's default (restricted) evaluator rejects what Rust (always unrestricted) evaluates.",
     lambda c, f: "--unrestricted-eval" not in c["flags"] and f["up_err"] is not None and f["rs_out"] is not None
     and "TransformationError" in (f["up_err"].get("class") or "")),
    ("port:multiblock: several class derivations streamed from several tables",
     "Rust emits nothing/less for specs with more than one table-reading block (multiblock streaming).",
     lambda c, f: stream_cases(c) and len(re.findall(r"(?m)^  \w+:$|^- name:", c["spec"].split("class_derivations:")[-1])) >= 2
     and f["up_out"] is not None and f["rs_out"] is not None and len(f["rs_out"]) < len(f["up_out"])),
]


def categorise(ctx: dict) -> tuple[str, str]:
    f = first_div(ctx)
    if f is None:
        return "none", ""
    for cat, summary, pred in RULES:
        try:
            if pred(ctx, f):
                return cat, summary
        except Exception:  # noqa: BLE001
            continue
    return "untriaged", ""


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--corpus", type=Path, default=HERE / "corpus")
    ap.add_argument("--all", action="store_true")
    a = ap.parse_args()
    counts: dict[str, int] = {}
    for case in sorted(a.corpus.iterdir()):
        marker = case / "expected_divergence.md"
        if not marker.exists():
            continue
        old = marker.read_text().splitlines()[0] if marker.read_text() else ""
        if not a.all and "untriaged" not in old:
            counts[old] = counts.get(old, 0) + 1
            continue
        cat, summary = categorise(load(case))
        marker.write_text(f"category: {cat}\n\n{summary}\n\nSee notes.md (what differs per format) and observed_rust/ (Rust's output when found).\n"
                          "Delete this file once the Rust output matches expected/.\n")
        counts[f"category: {cat}"] = counts.get(f"category: {cat}", 0) + 1
    for k, v in sorted(counts.items(), key=lambda kv: -kv[1]):
        print(f"{v:4d}  {k}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
