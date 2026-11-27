"""Re-run upstream ``linkml-map`` over the corpus and update the stored ``expected/`` bytes (an upstream re-pin).

    python rerecord_corpus.py [--corpus DIR] [--check] [--json FILE]

Run under the upstream venv, on Linux (a case-insensitive filesystem makes ``org`` open ``Org.tsv`` and CRLF
checkouts change the bytes, so the Windows answer is not the upstream answer).  For every case and format it runs
upstream in-process exactly as ``harness.compare_case`` does and compares with ``expected/<fmt>.out|.error``:

* success on both sides: rewritten only when the bytes differ;
* failure on both sides: kept as stored unless the exception class or the (path-normalised) message differs;
* success/failure flipped: rewritten.

``--check`` only reports (exit 1 on any difference): this is the cheap "does upstream still agree with the stored
expectations" gate used by the weekly drift workflow.  Without it the files are rewritten and ``notes.md``'s
per-format kind lines are refreshed against the Rust binary.  Run ``refresh_corpus.py`` afterwards to drop the
markers of cases that now match and refresh ``observed_rust/``.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import harness as H  # noqa: E402


def load_case(d: Path) -> H.Case:
    meta = json.loads((d / "case.json").read_text())
    return H.Case(root=str(d), schema=meta["schema"], spec=meta["spec"], input=meta["input"], source_type=meta["source_type"],
                  target_schema=meta.get("target_schema"), flags=meta.get("flags", []), formats=meta.get("formats", list(H.FORMATS)))


def stored(d: Path, fmt: str) -> tuple[str, bytes | dict]:
    o, e = d / "expected" / f"{fmt}.out", d / "expected" / f"{fmt}.error"
    if o.is_file():
        return "out", o.read_bytes()
    if e.is_file():
        return "error", json.loads(e.read_text())
    return "missing", b""


def same_error(old: dict, up: H.Run) -> bool:
    return old.get("class") == up.exc_class and H.norm_msg(old.get("message") or "") == H.norm_msg(up.exc_msg or "")


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--corpus", type=Path, default=H.HERE / "corpus")
    ap.add_argument("--check", action="store_true", help="report only; exit 1 when upstream disagrees with a stored expectation")
    ap.add_argument("--json", type=Path, help="write the list of changed (case, format, before, after) entries here")
    a = ap.parse_args()
    H.require_upvenv()
    changes = []
    skipped: list[str] = []
    n_cases = n_cells = 0
    for d in sorted(p for p in a.corpus.iterdir() if (p / "case.json").is_file()):
        case = load_case(d)
        n_cases += 1
        changed_fmts = []
        flipped = False
        for fmt in case.formats:
            n_cells += 1
            up = H.run_upstream(case.map_data_args(fmt, spec_flag="-T"), "inproc")
            kind, old = stored(d, fmt)
            if up.ok:
                if kind == "out" and old == up.out:
                    continue
                if b"!!python/object" in up.out and kind == "out" and b"!!python/object" in old:
                    # upstream leaks its in-memory objects (source_file paths, uuids, hashes): different on every run
                    skipped.append(f"{d.name} {fmt}")
                    continue
                what = "out->out" if kind == "out" else f"{kind}->out"
            else:
                if kind == "error" and same_error(old, up):
                    continue
                what = "error->error" if kind == "error" else f"{kind}->error"
            changes.append({"case": d.name, "fmt": fmt, "change": what, "has_marker": (d / "expected_divergence.md").exists()})
            changed_fmts.append(fmt)
            flipped = flipped or what in ("error->out", "out->error")
            if not a.check:
                for stale in ((d / "expected" / f"{fmt}.out"), (d / "expected" / f"{fmt}.error")):
                    stale.unlink(missing_ok=True)
                H.write_run(d / "expected", fmt, up)
        if flipped and not a.check:  # notes.md grades are history: re-grade only when upstream flipped success/failure
            refresh_notes(d, case)
    print(f"{n_cases} cases, {n_cells} case-format cells, {len(changes)} expectations "
          f"{'differ from upstream' if a.check else 're-recorded'} in {len({c['case'] for c in changes})} cases")
    if skipped:
        print(f"skipped (non-deterministic upstream object dump): {', '.join(skipped)}")
    for c in changes:
        print(f"  {c['case']} {c['fmt']}: {c['change']}{' (marked divergence)' if c['has_marker'] else ''}")
    if a.json:
        a.json.write_text(json.dumps(changes, indent=1) + "\n")
    return 1 if (a.check and changes) else 0


def refresh_notes(d: Path, case: H.Case) -> None:
    """Re-grade the ``- `fmt`: kind`` lines of notes.md against the new expectations (Rust vs the stored upstream bytes)."""
    notes = d / "notes.md"
    if not notes.is_file():
        return
    text = notes.read_text()
    for fmt in case.formats:
        rs = H.run_rust(case.map_data_args(fmt))
        kind, old = stored(d, fmt)
        if kind == "out":
            k = "match" if rs.ok and rs.out == old else ("rust_error" if not rs.ok else "output_diff")
            suffix = ""
        else:
            k = "both_error" if not rs.ok else "upstream_error"
            suffix = ""
        m = re.search(rf"^- `{fmt}`: (\w+).*$", text, flags=re.M)
        if m and m.group(1) != k:  # keep the line (and its "message match" grade) when the kind did not move
            text = text[: m.start()] + f"- `{fmt}`: {k}{suffix}" + text[m.end():]
    notes.write_text(text)


if __name__ == "__main__":
    sys.exit(main())
