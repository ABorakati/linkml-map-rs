#!/usr/bin/env python3
"""Benchmark base Python linkml-map ObjectTransformer on the measurements fixture.

Mirrors the Rust `pipeline-bench` transform-only workload: N synthetic
PersonQuantityValue rows ({id, height:{value, unit}}) mapped through the
`qv-to-scalar` transform (a `case(...)` expr selecting cm values). Single
process, single thread — that is what base Python gives you (GIL).

Usage:
    python bench_python.py [N_ROWS]      # default 100_000

Emits one JSON line to stdout: {"impl":"python","n_rows":N,"elapsed_s":..,"rows_per_sec":..}
plus a human summary to stderr.
"""

import json
import sys
import time
from pathlib import Path

# Fixture lives at <repo>/tests/examples/measurements (this file is at
# <repo>/benchmarks/vs_python/bench_python.py).
REPO = Path(__file__).resolve().parents[2]
FIXTURE = REPO / "tests" / "examples" / "measurements"
SOURCE_SCHEMA = FIXTURE / "source" / "quantity_value.yaml"
SPEC_YAML = FIXTURE / "transform" / "qv-to-scalar.transform.yaml"
SOURCE_CLASS = "Person"


def load_mapper():
    import yaml
    from linkml_runtime import SchemaView
    from linkml_map.transformer.object_transformer import ObjectTransformer

    sv = SchemaView(str(SOURCE_SCHEMA))
    spec_dict = yaml.safe_load(SPEC_YAML.read_text())

    # Parse the spec YAML into a TransformationSpecification the same way the
    # upstream compliance suite does.
    loader = ObjectTransformer()
    loader.create_transformer_specification(spec_dict)
    spec = loader.specification

    return ObjectTransformer(source_schemaview=sv, specification=spec)


def make_rows(n):
    return [
        {"id": f"P:{i}", "height": {"value": 150.0 + (i % 100), "unit": "cm"}}
        for i in range(n)
    ]


def transform_only(n_rows):
    mapper = load_mapper()
    rows = make_rows(n_rows)
    for r in rows[: min(100, n_rows)]:  # warm up
        mapper.map_object(r)
    start = time.perf_counter()
    ok = sum(1 for r in rows if mapper.map_object(r) is not None)
    elapsed = time.perf_counter() - start
    assert ok == n_rows, f"transformed {ok}/{n_rows}"
    rps = n_rows / elapsed if elapsed > 0 else float("inf")
    print(json.dumps({"impl": "python", "mode": "transform", "n_rows": n_rows,
                      "elapsed_s": elapsed, "rows_per_sec": rps}))
    print(f"[python transform] {n_rows} rows in {elapsed:.3f}s = {rps:,.0f} rows/sec",
          file=sys.stderr)


def gen(path, n_rows):
    """Write N source rows as JSONL for the shared end-to-end input."""
    with open(path, "w") as f:
        for r in make_rows(n_rows):
            f.write(json.dumps(r) + "\n")
    print(f"[gen] wrote {n_rows} rows to {path}", file=sys.stderr)


def e2e(src, out):
    """End-to-end: read JSONL file -> build mapper -> transform -> write JSONL.

    Times EVERYTHING (schema/spec load + read + transform + write), matching how
    the Rust pipeline is timed — i.e. the real cost of running the tool on a file.
    """
    start = time.perf_counter()
    mapper = load_mapper()  # SchemaView + spec parse (one-time, counted)
    n = 0
    with open(src) as fin, open(out, "w") as fout:
        for line in fin:
            line = line.strip()
            if not line:
                continue
            o = mapper.map_object(json.loads(line))
            o = {k: v for k, v in (o or {}).items() if v is not None}
            fout.write(json.dumps(o) + "\n")
            n += 1
    elapsed = time.perf_counter() - start
    rps = n / elapsed if elapsed > 0 else float("inf")
    print(json.dumps({"impl": "python", "mode": "e2e", "n_rows": n,
                      "elapsed_s": elapsed, "rows_per_sec": rps}))
    print(f"[python e2e] {n} rows (read+transform+write) in {elapsed:.3f}s = {rps:,.0f} rows/sec",
          file=sys.stderr)


def main():
    args = sys.argv[1:]
    if args and args[0] == "gen":
        gen(args[1], int(args[2]))
    elif args and args[0] == "e2e":
        e2e(args[1], args[2])
    else:
        transform_only(int(args[0]) if args else 100_000)


if __name__ == "__main__":
    main()
