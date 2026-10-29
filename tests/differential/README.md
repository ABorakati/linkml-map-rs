# Differential testing against upstream Python `linkml-map`

Proves the Rust CLI (`linkml-tr-rs`) behaves like upstream `linkml-map` (pinned `v0.6.0-rc1`) end to end:
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

## Triage

A new counterexample is written with `expected_divergence.md` (`category: untriaged`). Triage it by editing the
`category:` line (the first line is shown as the skip reason in `cargo test`) and, if it is an upstream bug
rather than a port bug, say so. When the Rust output starts matching, delete `expected_divergence.md`; the replay
test then enforces it from then on.
