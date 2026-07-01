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

### PyO3 drop-in compat leg (`python-compat` CI job)

The `python-compat` job in [`.github/workflows/ci.yml`](.github/workflows/ci.yml) is the
**only** CI leg that actually exercises the compiled `linkml_map_rs` wheel + the Rust-backed
`linkml_map` shim (the main `test` job builds the wheel but its Python suite runs against
*upstream* `linkml-map`). It builds the wheel, installs it into a **clean** env (matrix:
Python 3.11, 3.12), and drives it through the byte-identical-to-Python golden corpus in
[`crates/linkml-map-py/tests/test_compat_api.py`](crates/linkml-map-py/tests/test_compat_api.py),
proving the PyO3 surface stays a drop-in for the Python API and fails the build on drift.

- **Never co-install upstream `linkml-map` / `linkml` in this leg.** Upstream also imports as
  `linkml_map`, so co-installing it makes `import linkml_map` resolve to upstream and the
  wheel is silently not under test. Compat deps are **exactly** `pytest pyyaml linkml-runtime`.
- **Local repro** (env WITHOUT upstream linkml-map):
  ```bash
  python -m venv /tmp/compat && . /tmp/compat/bin/activate   # clean, isolated env
  pip install pytest pyyaml linkml-runtime                   # NO linkml / linkml-map
  maturin build --release --out dist -m crates/linkml-map-py/Cargo.toml
  pip install dist/*.whl
  pytest crates/linkml-map-py/tests/test_compat_api.py -v
  ```
- **Making it block merges:** the YAML guarantees the job runs unconditionally on every PR
  and fails on drift, but a GitHub check only *blocks merges* when a repo admin adds it to
  the `master` **branch-protection required-checks** list. Add the check names
  **`python-compat (3.11)`** and **`python-compat (3.12)`** there. YAML alone cannot make a
  check "required".

## Working conventions

- **Idiomatic, clippy-clean Rust.** Follow the idiomatic-code doctrine; cleanups
  stay behaviour-preserving unless explicitly asked otherwise.
- **Use parallel sonnet/haiku subagents** for multi-step work: sonnet for
  implementation/refactor, haiku for trivial/mechanical steps; sequence only steps
  that share a file or depend on prior output.
- **Navigate with the codedb MCP** (`codedb_symbol`, `codedb_callers`,
  `codedb_outline`, `codedb_word`) rather than brute-force reading large files —
  `engine/mod.rs` alone is ~5k lines. Pass `project=C:/Users/abora/linkml-map-rs`.
  If `project=` errors `SnapshotLoadFailed`, or the index is stale after big edits,
  rebuild the gitignored repo-root snapshot with the codedb binary (the MCP
  `codedb_index` tool fails in-sandbox; the binary works):
  `~/.claude/codedb-windows-x86_64.exe "C:/Users/abora/linkml-map-rs" index && \
   ~/.claude/codedb-windows-x86_64.exe "C:/Users/abora/linkml-map-rs" snapshot`.
  To force a full rescan, first delete `codedb.snapshot` and the live store
  `~/.codedb/projects/c98c95ba07df091a/`. A running MCP caches per session — the
  fresh snapshot is served after the next session/MCP restart.

## Harness: linkml-map-rs crate development

**Goal:** route every change to its subsystem owner, keep the three API surfaces
(Rust / PyO3 / Python `.pyi`+docs) in sync, and end every change in verification.

**Trigger:** for any development task on the engine, bindings, CLI, io/pipeline,
conformance suite, or benches — including follow-ups ("redo", "update", "fix",
"also…") — use the `harness-orchestrator` skill. It routes to the owner agents
(`transform-engine-engineer`, `pyo3-binding-engineer`, `runtime-engineer`,
`conformance-engineer`) on **`model: opus`** and always ends in `harness-qa`.
Pure questions can be answered directly.

**Reliable sync gate:** `harness-ownership.toml` is the machine-readable owner map
+ three-surface rule. Both CI (`.github/workflows/ci.yml` → `harness-gate` job) and
the in-session Stop hook (`.claude/settings.json`) enforce it via the same binary:
`cargo xtask harness-gate` (crate `./xtask`). Bypass a non-behavioural change with
`[skip-harness]` in the commit message or `HARNESS_SKIP=1`.

**Change history:**
| Date | Change | Target | Reason |
|------|--------|--------|--------|
| 2026-06-26 | Initial harness: 4 owner agents + harness-qa, 7 skills, xtask sync gate, CI job, Stop hook, `_native.pyi` stub | whole repo | - |
| 2026-06-26 | Model tiering: opus for engine + runtime (correctness-critical), sonnet for pyo3-binding + conformance, haiku for harness-qa (gate/cmd execution + shape-diff, no synthesis) | agents/, harness-orchestrator | efficiency — reserve opus for hardest reasoning |
