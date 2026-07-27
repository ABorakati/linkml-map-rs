# Parity Follow-ups Investigation — 2026-07-25

Point-in-time investigation report covering three open parity questions in `linkml-map-rs` (pinned at upstream `v0.5.3` / commit `ea0d4ee8c`).

---

## QUESTION 1: Per-Row Expression Re-Parse Cost

### Status
**Solved on the pipeline path, but genuinely missing on the PyO3 / Python bindings path and direct `ObjectTransformer::new` library usage.**

### Findings & Evidence

1. **Pipeline Path (CLI / `linkml-map-pipeline`) — [VERIFIED]**
   - **`crates/linkml-map-pipeline/src/lib.rs:20-25`**: Module doc comment notes that `CompiledPlan::load` parses every `expr:` string in the spec into a `CompiledExprs` AST cache shared via `Arc<CompiledPlan>`.
   - **`crates/linkml-map-pipeline/src/lib.rs:251`**: `CompiledExprs::build(&spec)` is called once when loading the plan.
   - **`crates/linkml-map-pipeline/src/lib.rs:403, 423`**: Every worker thread attaches `.with_compiled_exprs(&self.compiled_exprs)` when building per-row `ObjectTransformer` instances.
   - **`crates/linkml-map-core/src/engine/mod.rs:1023-1031, 1686-1694`**: `eval_expr_for_slot` and enum-level evaluation check `self.compiled.and_then(...)`. When present, they invoke `eval_parsed` / `eval_parsed_strict` (walking the pre-parsed AST directly with no re-parsing).
   - **Result**: Zero per-row expression re-parsing on the CLI/pipeline path.

2. **PyO3 Bindings / In-Memory Transformer Path — [VERIFIED]**
   - **`crates/linkml-map-py/src/lib.rs:264-268`**: `run_transform` constructs `ObjectTransformer::new(...)` directly without calling `CompiledExprs::build` or `.with_compiled_exprs`.
   - **`crates/linkml-map-py/src/lib.rs:305-340`**: `PyTransformer` stores `TransformationSpecification`, `SchemaViewProvider`, and `LookupIndex`, but does **not** store or construct a `CompiledExprs` instance.
   - **`crates/linkml-map-core/src/engine/mod.rs:1029-1030`**: When `self.compiled` is `None`, evaluation falls back to `eval_expr_with_mapping(expr, &bindings)`, which invokes `parse_expr(expr)` on every single row (`crates/linkml-map-core/src/expr/eval.rs:239`).
   - **Result**: The Python bindings (`linkml_map` wheel) re-lex and re-parse expression strings per row for every evaluation call.

3. **Direct `linkml-map-core` Usage — [VERIFIED]**
   - Callers constructing `ObjectTransformer::new` without explicitly invoking `CompiledExprs::build(&spec)` and `.with_compiled_exprs(&compiled)` bypass AST caching and re-parse expressions per row.

### Options & Trade-offs

- **Option A: Pre-compile in `PyTransformer` (Recommended)**
  - Update `PyTransformer` (`crates/linkml-map-py/src/lib.rs:305`) to build `CompiledExprs` once during `new()`, `from_yaml()`, or `from_python()`, store `compiled_exprs: CompiledExprs` on `PyTransformer`, and attach `.with_compiled_exprs(&self.compiled_exprs)` inside `run_transform`.
  - **Trade-off**: Small one-time cost at `PyTransformer` instantiation; eliminates all per-row expression re-parsing in the Python bindings.

---

## QUESTION 2: `validate` vs `validate-spec` Subcommand Naming

### Status
**Subcommand naming divergence (`validate` vs `validate-spec`). Fixable via a non-breaking `clap` subcommand alias.**

### Findings & Evidence

1. **Upstream Reference — [VERIFIED]**
   - **`reference/upstream-python/`**: Does **not** vendor `cli.py` (0 matching CLI files found in reference tree).
   - **`docs/upstream-sync-2026-07-25.md:15-16, 56-60`**: Upstream `linkml-map` v0.5.3 (commit `4616df5`) added the CLI subcommand as **`validate-spec`**.

2. **Rust CLI Implementation — [VERIFIED]**
   - **`crates/linkml-map-cli/src/main.rs:24, 41`**: Binary name is `linkml-tr-rs`. Subcommand is defined as `Validate(ValidateArgs)`, which `clap` exposes as **`validate`**.
   - **`crates/linkml-map-cli/src/main.rs:127-145`**: `ValidateArgs` accepts `-T` / `--specification`, `-s` / `--source-schema`, `--target-schema`, and `--strict`.
   - **`crates/linkml-map-cli/src/main.rs:207-222`**: Writes validation messages to stdout and error/warning summary counts to stderr. Exits with status code `1` if any error-level message is emitted, otherwise `0`.

3. **Broader Naming Context — [VERIFIED]**
   - The binary name `linkml-tr-rs` differs from upstream `linkml-map` / `linkml-tr`.
   - Subcommand names in Rust (`map-data`, `derive-schema`, `validate`) were simplified during initial porting.

### Options & Trade-offs

- **Option A: Rename `Validate` to `validate-spec`**
  - **Trade-off**: Achieves exact name alignment with upstream v0.5.3, but breaks existing scripts or tests relying on `linkml-tr-rs validate`.
- **Option B: Add `alias = "validate-spec"` to `Validate` (Recommended)**
  - Using `clap` v4 (`Cargo.toml:25` workspace dependency), annotate `Validate` in `crates/linkml-map-cli/src/main.rs:41` with `#[command(name = "validate-spec", visible_alias = "validate")]` or `#[command(alias = "validate-spec")]`.
  - **Trade-off**: Zero breaking changes. Accepts both `linkml-tr-rs validate-spec` and `linkml-tr-rs validate`.
- **Option C: Document the divergence**
  - **Trade-off**: Zero code changes, but leaves CLI invocations non-interchangeable.

---

## QUESTION 3: CLI Stdout Output Support

### Status
**Currently unsupported because `PipelineConfig` and pipeline execution functions hardcode file paths (`File::create`), and output format auto-detection requires a file extension.**

### Findings & Evidence

1. **Upstream Behavior — [INFERRED / UNVERIFIED IN REPO]**
   - `reference/upstream-python/` does not vendor `cli.py`. Upstream CLI stdout conventions (`-o -` vs defaulting to stdout when `-o` is omitted) cannot be verified directly from the vendored source.

2. **Why Stdout is Blocked in `linkml-map-rs` — [VERIFIED]**
   - **Hardcoded `File::create`**:
     - `crates/linkml-map-pipeline/src/lib.rs:636`: `tokio::fs::File::create(&config.out_path)` in FK JSONL path.
     - `crates/linkml-map-pipeline/src/lib.rs:933`: `tokio::fs::File::create(&config.out_path)` in fast-path JSONL.
     - `crates/linkml-map-pipeline/src/lib.rs:671, 827`: calls `write_all(&config.out_path, ...)` which calls `File::create(path)` (`crates/linkml-map-io/src/writers.rs:36`).
   - **Low-Level Writers are already generic**:
     - `crates/linkml-map-io/src/writers.rs:71, 88, 111`: `write_jsonl`, `write_json`, `write_csv`, `write_yaml` all accept generic `&mut W` where `W: AsyncWrite + Unpin`. They do **not** require a seekable file or `Path`.
   - **Format Auto-Detection**:
     - `crates/linkml-map-pipeline/src/lib.rs:532`: `Format::from_path(&config.out_path)` fails when `out_path` is `"-"` or empty because there is no file extension. `--output-format` must be specified or default to a fallback (e.g., `jsonl`).
   - **Stderr Isolation — [VERIFIED]**:
     - `crates/linkml-map-cli/src/lib.rs:127-140`: `run_map_data_config_with_options` prints progress and summary stats to `stderr` (`eprintln!`).
     - Transformed data stream on stdout will **not** be corrupted by log messages.

### Implementation Sketch & Work Sizing

1. **Changes Required**:
   - **`crates/linkml-map-io/src/writers.rs`**: Add `write_all_writer<W, S>(writer: W, format: Format, stream: S)` accepting any `AsyncWrite + Unpin`.
   - **`crates/linkml-map-pipeline/src/lib.rs`**: Support stdout stream target (when `out_path == "-"` or `stdout`), defaulting auto format to `Format::Jsonl` or requiring `--output-format`. Route writes to `tokio::io::stdout()`.
   - **`crates/linkml-map-cli/src/main.rs` & `lib.rs`**: Allow `-o` / `--output` to accept `"-"` for stdout. Handle `ErrorKind::BrokenPipe` when stdout is piped to downstream commands like `head`.
2. **Size of Work**:
   - **Files touched**: ~4 files (`linkml-map-io/src/writers.rs`, `linkml-map-pipeline/src/lib.rs`, `linkml-map-cli/src/main.rs`, `linkml-map-cli/src/lib.rs`).
   - **Complexity**: Low to Moderate (~50-80 lines of changes).
