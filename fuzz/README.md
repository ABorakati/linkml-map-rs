# Fuzz targets

Coverage-guided fuzzing with [cargo-fuzz](https://rust-fuzz.github.io/book/cargo-fuzz.html).
Nightly Rust is required; nothing here needs Python.

```bash
rustup toolchain install nightly --profile minimal
cargo install cargo-fuzz
fuzz/seed.sh                                   # seed fuzz/corpus/ from the repo's fixtures
cargo +nightly fuzz run -a expr_parse_eval -- -max_total_time=300 -max_len=2048
cargo check --manifest-path fuzz/Cargo.toml --bins   # compile check on stable
```

Use `-a` (debug assertions) so arithmetic overflow is caught. Give each worktree its own
`CARGO_TARGET_DIR`.

| Target | What it exercises |
|--------|-------------------|
| `expr_parse_eval` | expression lexer, parser, statement parser and evaluator (lax and strict) |
| `spec_load_compile` | spec YAML, normalisation, join synthesis, expression compilation, validation, the engine |
| `loader_yaml`, `loader_jsonl`, `loader_tsv` | the file loaders (`loader_tsv` also drives the CSV reader) |
| `decimal_parse` | `Decimal::parse_python` and its spellings, ordering and normalisation |
| `writers_tabular` | TSV/CSV/JSON/JSONL/YAML stream writers and `dump_output` over arbitrary values |
| `pyyaml_emit` | the PyYAML-compatible emitter: no panics, and a lossless round trip for values that are not YAML 1.1/1.2 ambiguous |

The fuzzers skip inputs that hit crashes already reported by the property tests (see the
`#[ignore = "bug: ..."]` tests in `crates/*/tests/proptest_*.rs`). Build with
`--features unguarded` to lift the guards. The property tests themselves run under plain
`cargo test`; set `PROPTEST_CASES=<n>` for a long run.
