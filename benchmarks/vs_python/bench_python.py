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


def main():
    n_rows = int(sys.argv[1]) if len(sys.argv) > 1 else 100_000
    mapper = load_mapper()
    rows = make_rows(n_rows)

    # Warm up (JIT-free, but primes caches / induced_slot memoization).
    for r in rows[: min(100, n_rows)]:
        mapper.map_object(r)

    start = time.perf_counter()
    ok = 0
    for r in rows:
        out = mapper.map_object(r)
        ok += 1 if out is not None else 0
    elapsed = time.perf_counter() - start

    assert ok == n_rows, f"transformed {ok}/{n_rows}"
    rps = n_rows / elapsed if elapsed > 0 else float("inf")

    print(
        json.dumps(
            {"impl": "python", "n_rows": n_rows, "elapsed_s": elapsed, "rows_per_sec": rps}
        )
    )
    print(
        f"[python] {n_rows} rows in {elapsed:.3f}s = {rps:,.0f} rows/sec (single thread)",
        file=sys.stderr,
    )


if __name__ == "__main__":
    main()
