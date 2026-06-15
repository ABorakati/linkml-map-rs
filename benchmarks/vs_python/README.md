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

### Example (16-core machine, 100k rows)

**Transform-only** (per-row engine CPU, no I/O):

| impl          | workers | rows/sec | speedup |
|---------------|--------:|---------:|--------:|
| python        |       1 |    6,379 |    1.0× |
| rust-1thread  |       1 |   75,835 | **11.9×** |
| rust-Ncore    |      16 |  305,035 | **47.8×** |

**End-to-end** (read JSONL → transform → write JSONL):

| impl              | workers | rows/sec | speedup |
|-------------------|--------:|---------:|--------:|
| python            |       1 |    4,743 |    1.0× |
| rust-e2e-1thread  |       1 |   44,717 | **9.4×** |
| rust-e2e-Ncore    |      16 |  190,475 | **40.2×** |

End-to-end now scales with cores too (~9× → ~40×), nearly tracking
transform-only — because the pipeline parallelises the *whole* JSONL path:

- **Bulk I/O, not per-line async.** The reader does one big read and the writer
  one big buffered write; at 1M rows, phase timing shows read ≈ 0.03s, write ≈
  0.03s — I/O is negligible. (The earlier version did `next_line().await` and a
  `write_all` per row — hundreds of thousands of awaits — which capped e2e at a
  flat ~6× regardless of cores. That was the I/O wall; it's gone.)
- **Parse + transform + serialise run in parallel** on a rayon pool sized to
  `workers`, so all the CPU work (including `serde_json` parse/serialise, which
  transform-only skips) scales. At 1M rows: transform phase 20.3s → 6.4s (1 →
  16 threads).
- **Correct FK routing.** A spec with an in-row nested access like
  `{height}.value` is *not* a cross-row foreign key (the object is inlined in
  the row). The pipeline now keeps those on the fast streaming path instead of
  buffering the whole dataset to build an `ObjectIndex`. True cross-row FK specs
  still buffer + index (they must — referenced rows can appear anywhere), and
  that branch is parallelised too, but its serial buffer+index preamble caps
  scaling.

So both regimes now parallelise. The remaining transform-only vs e2e gap is the
JSON parse+serialise e2e must do; it's parallel, just extra work.

Set `PIPELINE_PHASE_TIMING=1` to print per-phase (read/transform/write) timings.

Absolute numbers vary by CPU, disk, and core count, and run-to-run (cold caches
on the first run). The per-thread ratio is the most portable figure.

## Caveats / fairness

- Transform-only — excludes parsing the input file and serialising output. The
  e2e numbers add that (parse + serialise + file read/write); both run in
  parallel, so e2e tracks transform-only minus the extra serde work.
- The Rust side reuses a cached-AST `CompiledPlan` (spec/exprs parsed once);
  Python likewise builds the `ObjectTransformer` once before the timed loop, so
  neither pays per-row setup.
- Both warm up before timing.
- Same fixture, spec, and row shape on both sides — verified by the shared
  fixture path.
