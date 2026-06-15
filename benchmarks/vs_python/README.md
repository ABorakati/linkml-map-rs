# Rust vs base Python `linkml-map` benchmark

Compares this Rust engine against the reference Python `linkml-map`
`ObjectTransformer` on an **identical** workload, so the speedup reflects the
engine — not different work.

## Workload

- Fixture: `tests/examples/measurements` (`PersonQuantityValue` → flat scalar).
- Spec: `qv-to-scalar.transform.yaml` — a `case(...)` expression selecting the
  cm-valued height. Exercises expr eval + nested field access + schema lookup.
- Data: N synthetic rows `{id, height:{value, unit}}`, generated identically on
  both sides.
- **Transform-only** (no file I/O): isolates per-row engine CPU. Each side maps
  every row through its `ObjectTransformer` and times the loop.

## Run

```bash
./run.sh 100000        # bash / Git Bash
```
```powershell
./run.ps1 100000       # PowerShell
```

It creates a local venv, `pip install`s `linkml-map`, builds the Rust bench in
`--release`, runs both, and prints a speedup table. First run is slow (pip +
cargo build); re-runs are fast.

Pieces, if you want them separately:
- `bench_python.py [N]` — Python side, emits one JSON line.
- `cargo run --release -p linkml-map-pipeline --bin bench-vs-python [N]` — Rust
  side, emits two JSON lines (`rust-1thread`, `rust-Ncore`).

## Reading the numbers

Two comparisons matter:

- **`rust-1thread` vs `python`** — the *per-operation* win from native compiled
  Rust vs the CPython interpreter. Apples-to-apples: both single-threaded
  (Python is single-threaded per process under the GIL).
- **`rust-Ncore` vs `python`** — the *real-world* win: native speed **plus** the
  pipeline fanning rows across all cores, which base Python can't do in one
  process.

### Example (16-core machine, 50k rows)

| impl          | workers | rows/sec | speedup vs Python |
|---------------|--------:|---------:|------------------:|
| python        |       1 |    4,347 |              1.0× |
| rust-1thread  |       1 |   55,553 |          **12.8×** |
| rust-Ncore    |      16 |  316,227 |          **72.7×** |

Single-thread ≈ **13×** (pure engine speed); all-cores ≈ **73×** (engine +
multicore). Absolute numbers vary by CPU and core count; the per-thread ratio
is the most portable figure.

## Caveats / fairness

- Transform-only — excludes parsing the input file and serialising output. End-
  to-end throughput is capped lower by serial I/O on tiny-per-row data (see
  `pipeline-bench` for the full read→transform→write picture).
- The Rust side reuses a cached-AST `CompiledPlan` (spec/exprs parsed once);
  Python likewise builds the `ObjectTransformer` once before the timed loop, so
  neither pays per-row setup.
- Both warm up before timing.
- Same fixture, spec, and row shape on both sides — verified by the shared
  fixture path.
