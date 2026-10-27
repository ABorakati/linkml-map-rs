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

## Triage

A new counterexample is written with `expected_divergence.md` (`category: untriaged`). Triage it by editing the
`category:` line (the first line is shown as the skip reason in `cargo test`) and, if it is an upstream bug
rather than a port bug, say so. When the Rust output starts matching, delete `expected_divergence.md`; the replay
test then enforces it from then on.
