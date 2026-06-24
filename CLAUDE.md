# CLAUDE.md — agent guide for `linkml-map-rs`

A fast, parallel **Rust port of the Python [`linkml-map`](https://github.com/linkml/linkml-map)**
data-transformation engine. This file tells agents how to work in this repo.
`AGENTS.md` is a symlink to this file.

## Project aim

Be a **drop-in replacement for Python `linkml-map`**: accept the same spec/schema
files, pass the ported compliance suite (55/55) and golden fixtures (14/14), and
produce **byte-identical output** to the reference engine. Correctness against
Python is the product; speed is the reason it exists.

## Prime directive: mirror upstream Python

Every Rust function should track the **behaviour and the style** of its upstream
Python counterpart. When porting or changing code, find the corresponding upstream
source first and match it — name and structure helpers so the mapping is obvious.

Diverge **only** when one of these forces it:

1. **Exact replication is impossible in Rust.** Python's dynamic/untyped values
   map to the `Value` enum in [`crates/linkml-map-core/src/value.rs`](crates/linkml-map-core/src/value.rs)
   (note its `py_eq`/`is_truthy`/`try_numeric` deliberately mirror Python
   semantics); `asteval` maps to the hand-written evaluator in
   [`crates/linkml-map-core/src/expr/`](crates/linkml-map-core/src/expr). Keep the
   observable behaviour identical even where the mechanism differs.
2. **Async/concurrency buys speed.** Parallelism lives in the **pipeline**
   ([`crates/linkml-map-pipeline`](crates/linkml-map-pipeline)) and async I/O in
   [`crates/linkml-map-io`](crates/linkml-map-io). The engine itself
   ([`crates/linkml-map-core/src/engine`](crates/linkml-map-core/src/engine)) stays
   a **synchronous, `Send + Sync` pure function** — do not add concurrency there.

## Preserve upstream bugs — don't "fix" them in the port

Byte-identical parity depends on reproducing upstream behaviour, **including its
bugs and inefficiencies**. Do not silently correct them here. When you find one:

- Keep the upstream behaviour so output stays identical.
- Add a `// PARITY:` comment citing the upstream file/function (and line at the
  pinned commit) explaining what's being matched and why.
- Surface it as a **suggested upstream issue** in your report to the user — do not
  auto-file anything.

A real fix only lands here once it has landed upstream and we re-pin.

## Upstream reference

- Pinned upstream commit: **`19b1985889ec0fc247145e9aa03eb74a4d7b3588`**.
- Read-only Python source the port mirrors:
  [`reference/upstream-python/`](reference/upstream-python/) (object_transformer,
  engine, eval_utils, fk_utils, lookup_index, schema_patch, transformer_model,
  inference/inverter). Consult these for the "mirror Python" doctrine — they are
  **not built or imported**.
- Vendored compliance suite + fixtures: [`tests/compliance/`](tests/compliance/),
  provenance in [`tests/compliance/PROVENANCE.md`](tests/compliance/PROVENANCE.md).
- The `schemaview` backend is git-pinned in
  [`crates/linkml-map-schemaview/Cargo.toml`](crates/linkml-map-schemaview/Cargo.toml).

## Architecture

Workspace crates (core has no inter-crate deps; everything else depends on core):
`linkml-map-core` (engine, datamodel, schema, expr, inference) · `-schemaview`
(SchemaProvider over the LinkML metamodel) · `-io` (async JSONL/JSON/YAML/CSV) ·
`-pipeline` (rayon+tokio concurrent transform) · `-cli` · `-py` (PyO3 bindings +
`linkml_map` shim) · `-conformance` (compliance + golden runner) · `-duckdb`
(optional SQL backend).

Full diagram and data flow live in the **README — see
[Architecture](README.md#architecture)**. Don't duplicate it here; keep the README
the single source of truth.

## Build / test / parity verification

```bash
cargo build --release
cargo test --workspace                       # all crates
cargo test --workspace --exclude linkml-map-py   # when Python isn't on PATH
cargo clippy --workspace --all-targets       # keep clippy-clean
```

- The `conformance_suite` test (in `crates/linkml-map-conformance`) re-runs every
  golden/example fixture and regenerates [`tests/CONFORMANCE_REPORT.md`](tests/CONFORMANCE_REPORT.md);
  the compliance suite drives [`tests/COMPLIANCE_REPORT.md`](tests/COMPLIANCE_REPORT.md).
  **PASS/FAIL/SKIP counts must not regress.**
- Byte-identical head-to-head vs real Python: [`benchmarks/vs_python/`](benchmarks/vs_python)
  (`./run.sh <rows>`).
- `linkml-map-py` requires Python 3 + `linkml-runtime` on PATH to test; exclude it
  otherwise (this is an environment constraint, not a failure).

## Working conventions

- **Idiomatic, clippy-clean Rust.** Follow the idiomatic-code doctrine; cleanups
  stay behaviour-preserving unless explicitly asked otherwise.
- **Use parallel sonnet/haiku subagents** for multi-step work: sonnet for
  implementation/refactor, haiku for trivial/mechanical steps; sequence only steps
  that share a file or depend on prior output.
- **Navigate with the codedb MCP** (`codedb_symbol`, `codedb_callers`,
  `codedb_outline`, `codedb_word`) rather than brute-force reading large files —
  `engine/mod.rs` alone is ~5k lines.
