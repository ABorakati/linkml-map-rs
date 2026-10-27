"""Import cases recorded by ``capture_plugin.py`` (upstream's own test inputs) into ``corpus/``.

    python import_captured.py CAPTURE_DIR [--corpus DIR] [--limit N]

Upstream's expected output is stored for every format; cases where Rust differs get ``expected_divergence.md``.
Run ``triage.py`` afterwards to categorise them.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import harness as H  # noqa: E402


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("capture_dir", type=Path)
    ap.add_argument("--corpus", type=Path, default=H.HERE / "corpus")
    ap.add_argument("--limit", type=int, default=100000)
    a = ap.parse_args()
    H.require_upvenv()
    n = bad = 0
    for d in sorted(p for p in a.capture_dir.iterdir() if (p / "case.json").is_file())[: a.limit]:
        meta = json.loads((d / "case.json").read_text())
        case = H.Case(root=str(d), schema=meta["schema"], spec=meta["spec"], input=meta["input"], source_type=meta["source_type"],
                      target_schema=meta.get("target_schema"), flags=meta.get("flags", []))
        res = H.compare_case(case)
        dest = H.save_corpus_case(case, res, a.corpus, note=f"captured from upstream test `{meta.get('origin', '').split(' ')[0]}`",
                                  seed="capture_plugin.py")
        n += 1
        bad += any(not r.ok for r in res)
    print(f"imported {n} cases, {bad} divergent")
    return 0


if __name__ == "__main__":
    sys.exit(main())
