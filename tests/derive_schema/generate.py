#!/usr/bin/env python3
"""Regenerate `expected/*.yaml`, the replay corpus for `derive-schema`.

For every (specification, source schema) pair listed in `cases.json` this runs
upstream `linkml-map derive-schema` (v0.6.0-rc1) and stores its stdout.
`crates/linkml-map-cli/tests/derive_schema_upstream_parity.rs` replays the same
pairs through `linkml-tr-rs derive-schema` and asserts byte equality.

Usage (from anywhere):

    /home/user/upstream/upvenv/bin/python tests/derive_schema/generate.py [--discover]

`--only SUBSTRING` re-runs just the matching cases (biolink is slow).
`--discover` rewrites `cases.json` first by pairing every transform spec with
every schema under the example/golden directories, plus the hand-written
`inputs/<schema>_<case>.spec.yaml` feature cases and the exported `compliance/` combos. Pairs upstream rejects get `"error": true`
and no expected file; the Rust test then only checks that we fail too.
"""

import json
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
UPSTREAM_CLI = Path("/home/user/upstream/upvenv/bin/linkml-map")
EXAMPLE_ROOTS = ["tests/examples", "tests/golden"]


def discover():
    cases = []
    for root in EXAMPLE_ROOTS:
        for d in sorted((ROOT / root).iterdir()):
            specs = sorted((d / "transform").glob("*.yaml")) if (d / "transform").is_dir() else []
            schemas = sorted((d / "source").glob("*.yaml")) if (d / "source").is_dir() else []
            for spec in specs:
                for schema in schemas:
                    cases.append(
                        {
                            "name": f"{root.split('/')[-1]}__{d.name}__{spec.stem}__{schema.stem}",
                            "spec": str(spec.relative_to(ROOT)),
                            "schema": str(schema.relative_to(ROOT)),
                        }
                    )
    # Hand-written feature cases: `inputs/<schema>_<case>.spec.yaml` runs against `inputs/<schema>.yaml`.
    inputs = HERE / "inputs"
    for spec in sorted(inputs.glob("*.spec.yaml")):
        schema = inputs / (spec.name.split("_")[0] + ".yaml")
        cases.append(
            {
                "name": f"inputs__{spec.name.removesuffix('.spec.yaml')}",
                "spec": str(spec.relative_to(ROOT)),
                "schema": str(schema.relative_to(ROOT)),
            }
        )
    # Upstream's legacy "attic" specs (some are rejected by the current datamodel, which is the point).
    attic = ROOT / "tests/compliance/attic"
    for spec, schema in [("transform-s1-to-s2", "personinfo_s1"), ("denormalize-mapping", "personinfo_s1")]:
        cases.append(
            {
                "name": f"attic__{spec}__{schema}",
                "spec": f"tests/compliance/attic/{spec}.yaml",
                "schema": f"tests/compliance/attic/{schema}.yaml",
            }
        )
    # The compliance suite's 55 programmatic combos, exported by
    #   COMPLIANCE_EXPORT_DIR=tests/derive_schema/compliance cargo test -p linkml-map-conformance export_cases
    for spec in sorted((HERE / "compliance").glob("*.spec.yaml")):
        schema = spec.with_name(spec.name.replace(".spec.", ".schema."))
        cases.append(
            {
                "name": f"compliance__{spec.name.removesuffix('.spec.yaml')}",
                "spec": str(spec.relative_to(ROOT)),
                "schema": str(schema.relative_to(ROOT)),
            }
        )
    return cases


def main():
    cases_path = HERE / "cases.json"
    recorded = {c["name"]: c for c in json.loads(cases_path.read_text())} if cases_path.exists() else {}
    if "--discover" in sys.argv:
        found = discover()
        cases = found
    else:
        cases = json.loads(cases_path.read_text())
    (HERE / "expected").mkdir(exist_ok=True)
    only = sys.argv[sys.argv.index("--only") + 1] if "--only" in sys.argv else None
    out = []
    for c in cases:
        if only and only not in c["name"]:
            out.append(recorded.get(c["name"], c))  # keep as recorded
            continue
        c = {k: v for k, v in c.items() if k != "error"}
        proc = subprocess.run(
            [str(UPSTREAM_CLI), "derive-schema", "-T", str(ROOT / c["spec"]), str(ROOT / c["schema"])],
            capture_output=True,
            text=True,
            cwd=ROOT,
        )
        target = HERE / "expected" / f"{c['name']}.yaml"
        if proc.returncode == 0 and proc.stdout:
            target.write_text(proc.stdout, encoding="utf-8")
        else:
            c["error"] = True
            target.unlink(missing_ok=True)
        out.append(c)
    cases_path.write_text(json.dumps(out, indent=1) + "\n")
    print(f"{len(out)} cases, {sum(1 for c in out if c.get('error'))} upstream errors")


if __name__ == "__main__":
    main()
