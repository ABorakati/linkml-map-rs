# Differential testing against upstream Python `linkml-map`

Proves the Rust CLI (`linkml-tr-rs`) behaves like upstream `linkml-map` (pinned `v0.6.0`, see `UPSTREAM_PIN`) end to end:
same bytes for every output format, and failure where upstream fails.

| file | purpose |
|---|---|
| `harness.py` | runs upstream (click `CliRunner` in-process, or the real `linkml-map` script) and the Rust binary on a `Case`, classifies each output format (`match`, `both_error`, `output_diff`, `rust_error`, `upstream_error`, `rust_panic`), writes corpus cases |
| `examples_sweep.py` | deterministic sweep of every upstream example fixture: `map-data` x {yaml,json,jsonl,tsv,csv} x {restricted, `--unrestricted-eval`} (+ `-o FILE`), `derive-schema`, `validate-spec`. Writes `EXAMPLES_REPORT.md` |
| `gen.py` | Hypothesis strategies: source schema, matching target schema (or none), spec, data (TSV/CSV/YAML/JSON, single object or table directory) |
| `fuzz.py` | campaign driver. Uses `hypothesis.find` so every distinct divergence is shrunk to a minimal case and written to `corpus/<hash>/` |
| `corpus/<hash>/` | `schema.yaml spec.yaml [target.yaml] data/ case.json expected/<fmt>.out\|.error observed_rust/ notes.md signature.txt [expected_divergence.md]` |
| `duckdb_cells.py` | oracle for `crates/linkml-map-pipeline/src/join_rows.rs`: random JSON tables (big integers, floats, ISO dates/times/timestamps, UUIDs, lists, mixed columns; `--big`: tables past one scan vector or the 20480-record type-detection sample) read the way upstream's join engine reads a JSON primary table (`CAST(columns(*) AS VARCHAR) FROM read_json_auto(?)`, then `_parse_numeric`). Writes the fixtures `join_rows/duckdb_cells*.jsonl` that the Rust unit test `join_rows::tests::replays_duckdb_cells` replays (`DUCKDB_CELLS=file` replays a bigger local file too); `--check FILE` re-runs a fixture against the installed DuckDB |
| `capture_plugin.py`, `import_captured.py` | pytest plugin that records every top-level `map_object` call of upstream's own test-suite (schema, spec, target, input), and the importer that turns them into corpus cases with upstream's expected output |
| `triage.py` | assigns `category:` (suspected cause) to `expected_divergence.md` from stored artefacts; `--prune K` keeps at most K fuzz-found cases per category |
| `refresh_corpus.py` | after engine changes: re-runs Rust over the corpus with no Python, deletes the markers of cases that now match (FIXED), refreshes `observed_rust/` and categories of the rest, reports REGRESSIONS |
| `rerecord_corpus.py` | re-runs upstream over every corpus case and rewrites `expected/` where upstream's bytes (or success/failure) changed: the re-pin step. `--check` only reports (exit 1 on any difference), which is what the weekly `upstream-drift` workflow runs. Error messages are compared with paths normalised; an upstream YAML dump of its own in-memory objects (`!!python/object`, paths/uuids/hashes) is skipped as non-deterministic |
| `UPSTREAM_PIN` | the one place the pinned upstream tag is written for CI (`.github/workflows/upstream-drift.yml` reads it). Bump it with the pin |
| `known_sweep_divergences.txt` | allow-list for the examples sweep gate (`--fail-on-diff`); regenerate with `examples_sweep.py --write-allow` |
| `run.sh` | entry point (`--examples`, `--fuzz N`, `--seed`, `--time-budget`, `--profiles`) |
| `crates/linkml-map-cli/tests/differential_corpus.rs` | replays the corpus in plain `cargo test`, no Python needed |

## Running

```bash
cargo build --release -p linkml-map-cli
UPSTREAM_PY=/path/to/upstream/venv/bin/python tests/differential/run.sh --examples --fuzz 2000 --seed 1
cargo test -p linkml-map-cli --test differential_corpus                 # replay (must pass)
cargo test -p linkml-map-cli --test differential_corpus -- --ignored    # which known divergences are still open
```

The upstream venv needs upstream `linkml-map` at the pinned tag plus `hypothesis`. Never install it into the PyO3
compat environment (see CLAUDE.md).

## After merging engine changes

```bash
cargo build --release -p linkml-map-cli
python3 tests/differential/refresh_corpus.py     # FIXED / still divergent / REGRESSIONS (needs no upstream venv)
python3 tests/differential/triage.py --all       # re-categorise
```

## Re-pinning to a new upstream release

Do the upstream re-recording on **Linux** (CI, WSL), never on Windows or macOS: a case-insensitive filesystem makes the
`org` join open `Org.tsv` (the corpus has `Org.tsv`/`org.tsv` collisions), `core.autocrlf=true` writes CRLF fixtures and the
console mangles non-ASCII text, so upstream itself disagrees with the stored expectations there. Install upstream's own
`uv.lock` versions (DuckDB's timestamp text and pint change with the version), then:

```bash
cargo build --release -p linkml-map-cli
export LINKML_TR_RS=$PWD/target/release/linkml-tr-rs
UP=/path/to/upstream-venv/bin/python
$UP tests/differential/rerecord_corpus.py --json /tmp/changes.json   # rewrite expected/ (explain every change by a commit)
python3 tests/differential/refresh_corpus.py --dry-run                # FIXED markers to delete / REGRESSIONS (omit --dry-run only if you accept its rewriting of hand-written expected_divergence.md text)
UPSTREAM_PY=$UP UPSTREAM_DIR=/path/to/upstream-checkout tests/differential/run.sh --examples --fail-on-diff
cargo test -p linkml-map-cli --test differential_corpus
```

Also regenerate `tests/yaml_e2e/expected` and `REGEN=1 tests/input_e2e/run.sh`, and bump `UPSTREAM_PIN`.

## Triage

A new counterexample is written with `expected_divergence.md` (`category: untriaged`). Triage it by editing the
`category:` line (the first line is shown as the skip reason in `cargo test`) and, if it is an upstream bug
rather than a port bug, say so. When the Rust output starts matching, delete `expected_divergence.md`; the replay
test then enforces it from then on.
