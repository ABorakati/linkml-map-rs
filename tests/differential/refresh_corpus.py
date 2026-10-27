"""Re-run the Rust CLI over the corpus after engine changes; refresh ``observed_rust/`` and the divergence markers.

    python refresh_corpus.py [--corpus DIR] [--dry-run]

No Python/upstream needed (stored upstream output is used).  For each case: cases whose Rust output now matches lose
``expected_divergence.md`` and ``observed_rust/`` ("fixed"); cases that still differ get a fresh ``observed_rust/`` and
are re-triaged; cases without a marker that now differ are reported as REGRESSIONS (and given a marker if --mark).
"""

from __future__ import annotations

import argparse
import json
import shutil
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import harness as H  # noqa: E402
import triage  # noqa: E402


def check(case_dir: Path) -> dict[str, H.Run]:
    meta = json.loads((case_dir / "case.json").read_text())
    case = H.Case(root=str(case_dir), schema=meta["schema"], spec=meta["spec"], input=meta["input"], source_type=meta["source_type"],
                  target_schema=meta.get("target_schema"), flags=meta.get("flags", []))
    bad: dict[str, H.Run] = {}
    for fmt in meta.get("formats", H.FORMATS):
        rs = H.run_rust(case.map_data_args(fmt))
        eo, ee = case_dir / "expected" / f"{fmt}.out", case_dir / "expected" / f"{fmt}.error"
        if eo.exists():
            ok = rs.ok and rs.out == eo.read_bytes()
        else:
            ok = (not rs.ok) and rs.exc_class != "panic"
        if not ok:
            bad[fmt] = rs
    return bad


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--corpus", type=Path, default=H.HERE / "corpus")
    ap.add_argument("--dry-run", action="store_true")
    ap.add_argument("--mark", action="store_true", help="give regressed (unmarked) cases an expected_divergence.md")
    a = ap.parse_args()
    fixed = still = regress = 0
    for d in sorted(p for p in a.corpus.iterdir() if (p / "case.json").is_file()):
        marker = d / "expected_divergence.md"
        bad = check(d)
        if not bad:
            if marker.exists():
                fixed += 1
                print(f"FIXED  {d.name}: {marker.read_text().splitlines()[0]}")
                if not a.dry_run:
                    marker.unlink()
                    shutil.rmtree(d / "observed_rust", ignore_errors=True)
            continue
        if not a.dry_run:
            shutil.rmtree(d / "observed_rust", ignore_errors=True)
            for fmt, rs in bad.items():
                H.write_run(d / "observed_rust", fmt, rs)
        if marker.exists():
            still += 1
            if not a.dry_run:
                cat, summary = triage.categorise(triage.load(d))
                marker.write_text(f"category: {cat}\n\n{summary}\n\nSee notes.md (what differs per format) and observed_rust/ (Rust's output when found).\n"
                                  "Delete this file once the Rust output matches expected/.\n")
        else:
            regress += 1
            print(f"REGRESSION {d.name}: formats {sorted(bad)}")
            if a.mark and not a.dry_run:
                cat, summary = triage.categorise(triage.load(d))
                marker.write_text(f"category: {cat}\n\n{summary}\n\nRegression found by refresh_corpus.py.\n")
    print(f"fixed {fixed}, still divergent {still}, regressions {regress}")
    return 1 if regress else 0


if __name__ == "__main__":
    sys.exit(main())
