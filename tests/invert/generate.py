#!/usr/bin/env python3
"""Regenerate `expected/`, the replay corpus for `invert`.

Every (specification, source schema, mode) triple in `cases.json` is run through upstream
`linkml-map invert` (v0.6.0-rc1). Its stdout is stored as `expected/<name>.yaml`; when
upstream raises, the case is marked `"error"` with the exception type and message (the
last lines of the traceback) and has no expected file. The replay test
`crates/linkml-map-cli/tests/invert_upstream_parity.rs` runs `linkml-tr-rs invert` over
the same triples.

    /home/user/upstream/upvenv/bin/python tests/invert/generate.py [--discover] [--only SUBSTR] [-j N]

`--discover` rewrites `cases.json` from:
  * `tests/derive_schema/cases.json` (the shipped examples, golden fixtures and the exported
    compliance combos), and
  * `inputs/<schema>__<case>.spec.yaml` (hand-written; `<schema>` names `inputs/<schema>.yaml`
    or, for `src`, `inputs/src.yaml`), and
  * `inputs/fuzz/<id>.spec.yaml` + `<id>.schema.yaml` (written by `fuzz.py --emit`).
Each pair runs in both modes (`strict` is upstream's default, `nostrict` is `--no-strict`).
"""

import json
import re
import subprocess
import sys
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
UPSTREAM_CLI = Path("/home/user/upstream/upvenv/bin/linkml-map")
MODES = {"strict": [], "nostrict": ["--no-strict"]}


def discover():
    pairs = []
    for c in json.loads((ROOT / "tests/derive_schema/cases.json").read_text()):
        pairs.append((f"shipped__{c['name']}", c["spec"], c["schema"]))
    inputs = HERE / "inputs"
    for spec in sorted(inputs.glob("*.spec.yaml")):
        schema_name, _, case = spec.name.removesuffix(".spec.yaml").partition("__")
        pairs.append((f"hand__{schema_name}__{case}", str(spec.relative_to(ROOT)), str((inputs / f"{schema_name}.yaml").relative_to(ROOT))))
    for spec in sorted((inputs / "fuzz").glob("*.spec.yaml")):
        schema = spec.with_name(spec.name.replace(".spec.", ".schema."))
        pairs.append((f"fuzz__{spec.name.removesuffix('.spec.yaml')}", str(spec.relative_to(ROOT)), str(schema.relative_to(ROOT))))
    cases = []
    for name, spec, schema in pairs:
        for mode in MODES:
            cases.append({"name": f"{name}__{mode}", "spec": spec, "schema": schema, "strict": mode == "strict"})
    return cases


ERROR_LINE = re.compile(r"^(?:[\w.]+\.)?(\w+(?:Error|Exception|Warning)): ", re.M)


def run_case(case):
    args = [] if case["strict"] else ["--no-strict"]
    proc = subprocess.run(
        [str(UPSTREAM_CLI), "invert", *args, "-T", str(ROOT / case["spec"]), str(ROOT / case["schema"])],
        capture_output=True,
        text=True,
        cwd=ROOT,
    )
    return case, proc


def main():
    cases_path = HERE / "cases.json"
    recorded = {c["name"]: c for c in json.loads(cases_path.read_text())} if cases_path.exists() else {}
    cases = discover() if "--discover" in sys.argv else json.loads(cases_path.read_text())
    only = sys.argv[sys.argv.index("--only") + 1] if "--only" in sys.argv else None
    jobs = int(sys.argv[sys.argv.index("-j") + 1]) if "-j" in sys.argv else 3
    (HERE / "expected").mkdir(exist_ok=True)
    todo = [c for c in cases if not only or only in c["name"]]
    results = {}
    with ThreadPoolExecutor(jobs) as pool:
        for case, proc in pool.map(run_case, todo):
            results[case["name"]] = (case, proc)
    out = []
    for c in cases:
        if c["name"] not in results:
            out.append(recorded.get(c["name"], c))
            continue
        c, proc = results[c["name"]]
        c = {k: v for k, v in c.items() if k not in ("error", "exit")}
        target = HERE / "expected" / f"{c['name']}.yaml"
        if proc.returncode == 0:
            target.write_text(proc.stdout, encoding="utf-8")
        else:
            target.unlink(missing_ok=True)
            matches = list(ERROR_LINE.finditer(proc.stderr))
            if matches:
                last = matches[-1]
                c["error"] = {"type": last.group(1), "message": proc.stderr[last.end():].rstrip("\n")}
            else:
                c["error"] = {"type": "Unknown", "message": proc.stderr.strip()[-200:]}
            c["exit"] = proc.returncode
        out.append(c)
    cases_path.write_text(json.dumps(out, indent=1) + "\n")
    errors = sum(1 for c in out if "error" in c)
    print(f"{len(out)} cases, {errors} upstream errors")


if __name__ == "__main__":
    main()
