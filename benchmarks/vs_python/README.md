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
- `bench_python.py [N]` — Python transform-only, one JSON line.
- `bench_python.py gen <SRC> <N>` — write N source rows as JSONL (shared input).
- `bench_python.py e2e <SRC> <OUT>` — Python end-to-end over a JSONL file.
- `cargo run --release -p linkml-map-pipeline --bin bench-vs-python [N]` — Rust
  transform-only (`rust-1thread`, `rust-Ncore`).
- `cargo run --release -p linkml-map-pipeline --bin bench-e2e <SRC>` — Rust
  end-to-end via `run_pipeline` (`rust-e2e-1thread`, `rust-e2e-Ncore`).

## Reading the numbers

Two comparisons matter:

- **`rust-1thread` vs `python`** — the *per-operation* win from native compiled
  Rust vs the CPython interpreter. Apples-to-apples: both single-threaded
  (Python is single-threaded per process under the GIL).
- **`rust-Ncore` vs `python`** — the *real-world* win: native speed **plus** the
  pipeline fanning rows across all cores, which base Python can't do in one
  process.

### Example (16-core machine, 50k rows)

**Transform-only** (per-row engine CPU, no I/O):

| impl          | workers | rows/sec | speedup |
|---------------|--------:|---------:|--------:|
| python        |       1 |    7,442 |    1.0× |
| rust-1thread  |       1 |  113,819 | **15.3×** |
| rust-Ncore    |      16 |  423,596 | **56.9×** |

**End-to-end** (read JSONL → transform → write JSONL):

| impl              | workers | rows/sec | speedup |
|-------------------|--------:|---------:|--------:|
| python            |       1 |    6,009 |    1.0× |
| rust-e2e-1thread  |       1 |   38,583 | **6.4×** |
| rust-e2e-Ncore    |      16 |   36,778 |  6.1×   |

Two different stories, and the contrast is the point:

- **Transform-only is CPU-bound** → cores scale it (15× → 57×). This is the pure
  engine win.
- **End-to-end on tiny rows is I/O-bound** → the serial JSONL reader/parser +
  writer is the bottleneck, so extra transform threads add nothing (`Ncore` ≈
  `1thread`, even marginally slower from coordination overhead). Rust is still
  ~6× faster (native JSON parse/serialise + faster engine), but **multicore
  can't beat a serial I/O wall.**

Takeaway: parallelism helps only the CPU-bound part. If your rows are tiny and
the job is dominated by reading/writing, you're I/O-bound and ~6× is the honest
number; if per-row work is heavy (big nested objects, many exprs), the
transform-only regime dominates and you get the larger multicore speedups.

Absolute numbers vary by CPU, disk, and core count, and run-to-run (cold caches
on the first run). The per-thread transform-only ratio is the most portable
figure for raw engine speed.

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
