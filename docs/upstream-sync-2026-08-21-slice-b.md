# Upstream sync review (Slice B: CLI surface and writer API) — 2026-08-21

Point-in-time snapshot. This review covers two commits from upstream
`linkml/linkml-map` touching the command-line interface (CLI) surface and writer
APIs following tag `v0.5.3` (commit `ea0d4ee8c`).

## Summary

This slice reviews two upstream commits:
1. `c69db3e0fe` ("Make -q/--quiet a boolean flag", 2026-08-05)
2. `5f92b32129` ("Collapse map-data onto a single writer API", 2026-08-04)

## Commit-by-commit

| Commit | Date | Summary | Status |
|---|---|---|---|
| `c69db3e0fe` | 2026-08-05 | Make `-q`/`--quiet` a boolean flag instead of a value-taking option (`is_flag=True`) | **Needs porting.** Top-level logging flags (`-q`/`--quiet` and `-v`/`--verbose`) do not exist in `crates/linkml-map-cli/src/main.rs`. |
| `5f92b32129` | 2026-08-04 | Collapse `map-data` single-output and multi-output paths onto `MultiStreamWriter`, deprecating legacy generator functions (`yaml_stream`, `json_stream`, `get_stream_writer`) | **Already matching.** Rust `linkml-map-io` already routes all formats through a single unified async writer API (`write_all` / `write_all_writer`). Legacy Python function deprecations are **N/A to Rust**. |

---

## 1. Commit `c69db3e0fe917402216d165fb7f9f4fed13abba6` — "Make -q/--quiet a boolean flag" (2026-08-05)

### Upstream behaviour

- **Before `c69db3e0fe`:**
  In `src/linkml_map/cli/cli.py`, the main CLI group declared:
  ```python
  @click.group()
  @click.option("-v", "--verbose", count=True)
  @click.option("-q", "--quiet")
  def main(verbose: int, quiet: bool) -> None:
  ```
  In Click, omitting `is_flag=True` makes an option value-taking (`type=str`).
  Passing `-q` before a subcommand (e.g. `linkml-map -q derive-schema -T ...`)
  caused Click to consume `"derive-schema"` as the string value of `-q`.
  Click never dispatched the subcommand and printed group help instead.
  Passing an explicit value (e.g. `linkml-map --quiet true ...`) assigned
  `quiet="true"`, which evaluated as truthy in `if quiet:`.

- **After `c69db3e0fe`:**
  ```python
  @click.group()
  @click.option("-v", "--verbose", count=True, help="Increase log verbosity; repeat for debug.")
  @click.option("-q", "--quiet", is_flag=True, help="Suppress warnings; only report errors.")
  def main(verbose: int, quiet: bool) -> None:
  ```
  `-q` and `--quiet` are now boolean flags accepting zero arguments.
  Log levels map as follows:
  - Default (no flags): `logging.WARNING`
  - `-q` / `--quiet`: `logging.ERROR`
  - `-v` / `--verbose`: `logging.INFO`
  - `-vv`: `logging.DEBUG`

- **Contract change & invalid invocations:**
  Passing an explicit value (such as `--quiet=true` or `--quiet false`) now raises
  a Click usage error (`--quiet option does not take a value`).
  The canonical invocation `linkml-map -q <subcommand>` now works as intended.

### Rust evidence

- **`crates/linkml-map-cli/src/main.rs:22-32`:**
  ```rust
  #[derive(Parser, Debug)]
  #[command(
      name = "linkml-tr-rs",
      version = "0.1.0",
      about = "LinkML transformation engine (Rust port of linkml-tr)",
      long_about = None,
  )]
  struct Cli {
      #[command(subcommand)]
      command: Commands,
  }
  ```
  The top-level `Cli` struct defines no global flags. Passing `-q`, `--quiet`,
  `-v`, or `--verbose` fails with a `clap` error (`unexpected argument found`).

- **`crates/linkml-map-cli/src/main.rs:47-107, 111-124, 128-146`:**
  Subcommand structs (`MapDataArgs`, `DeriveSchemaArgs`, `ValidateArgs`) do not
  accept `-q` or `-v`.

- **`crates/linkml-map-cli/src/main.rs:229-238`:**
  `main()` parses `Cli` and immediately dispatches commands without initializing
  a logger or applying verbosity settings.

### Verdict

**Needs porting.**

### Implementation instructions

1. **Target file & symbols:** `crates/linkml-map-cli/src/main.rs`
2. **Add global clap arguments to `Cli`:**
   ```rust
   #[derive(Parser, Debug)]
   #[command(
       name = "linkml-tr-rs",
       version = "0.1.0",
       about = "LinkML transformation engine (Rust port of linkml-tr)",
       long_about = None,
   )]
   struct Cli {
       /// Suppress warnings; only report errors.
       #[arg(short = 'q', long = "quiet", global = true, action = clap::ArgAction::SetTrue)]
       quiet: bool,

       /// Increase log verbosity; repeat for debug.
       #[arg(short = 'v', long = "verbose", global = true, action = clap::ArgAction::Count)]
       verbose: u8,

       #[command(subcommand)]
       command: Commands,
   }
   ```
3. **Initialize logging level in `main()`:**
   Determine the log filter:
   - `cli.quiet` $\rightarrow$ `log::LevelFilter::Error`
   - `cli.verbose == 1` $\rightarrow$ `log::LevelFilter::Info`
   - `cli.verbose >= 2` $\rightarrow$ `log::LevelFilter::Debug`
   - Default $\rightarrow$ `log::LevelFilter::Warn`
   Initialize the logger (e.g. using `env_logger` or standard logger builder)
   before dispatching subcommands.
4. **Three-surface API sync rule (`harness-ownership.toml`):**
   - Rust public core API (`crates/linkml-map-core/**`): **No changes.**
   - PyO3 binding (`crates/linkml-map-py/src/lib.rs`): **No changes.** (PyO3
     exposes the `Transformer` library class, not CLI flags).
   - Python-facing surface (`_native.pyi`, `python/linkml_map/**`): **No changes.**
   - The three-surface sync rule is **not tripped**. The change is isolated to
     `crates/linkml-map-cli/` (owned by `runtime-engineer`).

### Drift-catching tests

- **Current test coverage:**
  - `crates/linkml-map-cli/src/main.rs:241-287`: Unit tests verify subcommand
    aliases (`validate` vs `validate-spec`), but no tests verify top-level
    logging flags.
  - `crates/linkml-map-cli/tests/integration_map_data.rs` and
    `integration_validate.rs`: Exercise `run_map_data_config` and
    `run_validate_config` programmatically without parsing CLI flags.
  - `crates/linkml-map-py/tests/`: Tests the PyO3 library surface, not the CLI.
- **Recommended test additions:**
  Add unit tests in `crates/linkml-map-cli/src/main.rs`:
  - `Cli::try_parse_from(["linkml-tr-rs", "-q", "derive-schema", "-T", "spec.yaml", "-s", "source.yaml"])`
  - `Cli::try_parse_from(["linkml-tr-rs", "--quiet", "derive-schema", "-T", "spec.yaml", "-s", "source.yaml"])`
  - `Cli::try_parse_from(["linkml-tr-rs", "-v", "map-data", ...])`
  - `Cli::try_parse_from(["linkml-tr-rs", "-vv", "map-data", ...])`
  Assert that `cli.quiet` and `cli.verbose` reflect the expected boolean and count
  values and that the subcommands parse cleanly without token consumption.

---

## 2. Commit `5f92b32129e68c9ef954d92fbe246b8f068c8f16` — "Collapse map-data onto a single writer API" (2026-08-04)

### Upstream behaviour

- **Previous dual-branch writer path in `src/linkml_map/cli/cli.py`:**
  - When `-O` / `--additional-output` was present: `map-data` instantiated
    `MultiStreamWriter([(make_stream_writer(fmt), primary_target), *extra_outputs]).write_all(chunks)`.
  - When `-O` was absent: `map-data` used the legacy function-based stream
    generator `get_stream_writer(fmt)`, iterated chunks directly, and performed
    a secondary temporary-file rewrite (`output + ".tmp"`) via
    `rewrite_header_and_pad` if tabular headers expanded dynamically.
- **Unified writer path:**
  - `src/linkml_map/cli/cli.py` now always builds `all_outputs = [(make_stream_writer(fmt), primary_target), *extra_outputs]`
    and delegates directly to `MultiStreamWriter(all_outputs).write_all(chunks)`.
  - In `src/linkml_map/writers/output_streams.py`, legacy generator functions
    (`yaml_stream`, `json_stream`, `jsonl_stream`, `tsv_stream`, `csv_stream`,
    `get_stream_writer`, `TabularStreamWriter.stream`) are marked deprecated and
    emit a `DeprecationWarning`.
- **Observable output contract:**
  - **Output bytes:** Unchanged across all formats (JSON, JSONL, YAML, CSV, TSV).
  - **Output file naming:** Unchanged (`-o` path or `-O format:path`).
  - **Stdout vs file:** Unchanged (`sys.stdout` when `-o` is omitted, file path otherwise).
  - **Format dispatch:** Unchanged (`make_stream_writer` handles all `OutputFormat` variants).

### Rust evidence

- **`crates/linkml-map-io/src/writers.rs:28-43` (`write_all`):**
  Creates or truncates the destination file and delegates to `write_all_inner`.
- **`crates/linkml-map-io/src/writers.rs:45-60` (`write_all_writer`):**
  Accepts any generic `AsyncWrite + Unpin` stream (such as `tokio::io::stdout()`)
  and delegates to `write_all_inner`.
- **`crates/linkml-map-io/src/writers.rs:80-93` (`write_all_inner`):**
  Single internal dispatch point for all formats:
  ```rust
  async fn write_all_inner<W, S>(writer: &mut W, format: Format, mut stream: S) -> Result<()>
  where
      W: AsyncWrite + Unpin,
      S: Stream<Item = Result<Value>> + Unpin,
  {
      match format {
          Format::Jsonl => write_jsonl(writer, &mut stream).await?,
          Format::Json => write_json(writer, &mut stream).await?,
          Format::Csv => write_csv(writer, b',', &mut stream).await?,
          Format::Tsv => write_csv(writer, b'\t', &mut stream).await?,
          Format::Yaml => write_yaml(writer, &mut stream).await?,
      }
      Ok(())
  }
  ```
- **`crates/linkml-map-pipeline/src/lib.rs:918-938` (`write_stream_to_output`):**
  Routes `-` to `write_all_writer(tokio::io::stdout(), ...)` and file paths to
  `write_all(out_path, ...)`.
- Rust never implemented the legacy Python generator functions (`yaml_stream`,
  `get_stream_writer`), nor did it have the dual-path CLI structure.

### Verdict

**Already matching.**
The Rust architecture already uses a unified async writer pipeline.
Deprecation of legacy Python standalone generator functions is **N/A to Rust**.

### Implementation instructions

None. No code changes required.

### Drift-catching tests

- **`crates/linkml-map-cli/tests/integration_map_data.rs:115-205`:** Tests file
  writing for YAML $\rightarrow$ JSONL.
- **`crates/linkml-map-cli/tests/integration_map_data.rs:389-430`:** Tests stdout
  writing (`-o -`) with explicit `--output-format jsonl`.
- **`crates/linkml-map-io/src/lib.rs:450-530`:** Unit tests for `write_all_auto`
  and `write_all_writer`.

---

## Verification Boundaries

- **Verified:** Upstream patch diffs fetched and analyzed from GitHub; Rust code
  inspected at exact file and line references via `codedb` and `read`.
- **Unverified / Not run:** `cargo test` and `cargo build` were not run in
  accordance with the read-only concurrency constraint (sibling agents active
  in the workspace).
