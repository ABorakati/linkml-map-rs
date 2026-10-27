//! `linkml-tr-rs` — LinkML transformation engine CLI (Rust port of `linkml-tr`).
//!
//! # Subcommands
//!
//! - `map-data` — run the concurrent pipeline over a source data file.
//! - `derive-schema` — derive a target schema from a transformation spec.
//! - `validate` — read-only pre-flight check of a spec against its source/target
//!   schemas (no transform is run).

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use linkml_map_cli::{
    MapDataConfig, Severity, ValidateConfig, parse_format_str, run_map_data_config_with_options,
    run_validate_config,
};
use linkml_map_core::inference::SchemaMapper;
use linkml_map_io::WriteOptions;
use linkml_map_pipeline::load_transform_spec;

// ── Top-level CLI ─────────────────────────────────────────────────────────────

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

// ── Logging ──────────────────────────────────────────────────────────────────
//
// PARITY: upstream `src/linkml_map/cli/cli.py` (commit `c69db3e0fe`) declares
// `-v/--verbose` (count) and `-q/--quiet` (is_flag) as group-level options, so
// they are global in clap and may precede the subcommand. The resolved root
// level ladder is: (none) -> WARNING, -q -> ERROR, -v -> INFO, -vv -> DEBUG.
// Upstream's `main()` body sets the verbose level first, then overrides to
// ERROR when `quiet` is truthy, so `-q` wins when both flags are passed. We
// mirror that precedence exactly in `resolve_log_level` below.

/// Resolve the global `log::LevelFilter` from the CLI verbosity flags.
fn resolve_log_level(verbose: u8, quiet: bool) -> log::LevelFilter {
    let level = match verbose {
        0 => log::LevelFilter::Warn,
        1 => log::LevelFilter::Info,
        _ => log::LevelFilter::Debug,
    };
    if quiet {
        log::LevelFilter::Error
    } else {
        level
    }
}

/// Minimal `log::Log` implementation for the CLI.
///
/// Writes every record to stderr so log output never contaminates the `-o -`
/// stdout data path (the `map-data` subcommand can stream transformed rows to
/// stdout). Level filtering is enforced by the global `log::set_max_level`
/// call in `main`, not re-implemented here.
struct StderrLogger;

impl log::Log for StderrLogger {
    fn enabled(&self, _metadata: &log::Metadata) -> bool {
        // The `log!` macros check the global max level (set via
        // `log::set_max_level`) before calling this; accept what reaches us.
        true
    }

    fn log(&self, record: &log::Record) {
        eprintln!("{}: {}", record.level(), record.args());
    }

    fn flush(&self) {}
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Transform a source data file using a LinkML transformation spec.
    MapData(MapDataArgs),
    /// Derive a target schema from a transformation spec.
    DeriveSchema(DeriveSchemaArgs),
    /// Validate a transformation spec against its source/target schemas.
    #[command(visible_alias = "validate-spec")]
    Validate(ValidateArgs),
}

// ── map-data ──────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
struct MapDataArgs {
    /// Transformation specification YAML file (-T / --specification).
    #[arg(short = 'T', long = "specification", value_name = "SPEC_YAML")]
    spec: String,

    /// Source LinkML schema YAML file.
    #[arg(short = 's', long = "source-schema", value_name = "SCHEMA_YAML")]
    source_schema: String,

    /// Target LinkML schema YAML file. When omitted no target schema is used (as upstream).
    #[arg(long = "target-schema", value_name = "TARGET_YAML")]
    target_schema: Option<String>,

    /// Output file. Omit it, or pass `-`, to write to stdout (YAML unless
    /// --output-format says otherwise).
    #[arg(short = 'o', long = "output", value_name = "OUT_FILE")]
    output: Option<String>,

    /// Source type/class name for the input data.
    /// When omitted, the engine resolves it from the first class_derivation.
    /// `--source-class` is the earlier Rust name for this flag.
    #[arg(
        long = "source-type",
        visible_alias = "source-class",
        value_name = "CLASS"
    )]
    source_class: Option<String>,

    /// Input format override.
    /// One of: auto, csv, tsv, json, jsonl, yaml.
    /// Defaults to auto-detection from the input file extension.
    #[arg(long = "input-format", value_name = "FMT", default_value = "auto")]
    input_format: String,

    /// Output format. Defaults to YAML, or is inferred from the output file extension.
    /// One of: auto, csv, tsv, json, jsonl, yaml, parquet, duckdb (the last two need a
    /// build with `--features columnar` and an output file).
    #[arg(
        short = 'f',
        long = "output-format",
        value_name = "FMT",
        default_value = "auto"
    )]
    output_format: String,

    /// Number of records to process per chunk (for streaming output).
    #[arg(long = "chunk-size", value_name = "N", default_value_t = 1000)]
    chunk_size: usize,

    /// Table name for DuckDB output. Defaults to the output file's stem.
    #[arg(long = "table-name", value_name = "NAME")]
    table_name: Option<String>,

    /// Additional output files. Format inferred from extension. Can be repeated.
    /// Ignored for single YAML/JSON object input, as upstream.
    #[arg(short = 'O', long = "additional-output", value_name = "FILE")]
    additional_output: Vec<String>,

    /// Number of rayon worker threads.
    /// 0 (default) uses the rayon global pool size (≈ number of logical CPUs).
    #[arg(long = "workers", value_name = "N", default_value_t = 0)]
    workers: usize,

    /// Write output rows in the same order as the input (default: ordered).
    /// Pass --unordered for maximum throughput (output order undefined).
    #[arg(long = "unordered", default_value_t = false)]
    unordered: bool,

    /// Continue after row-level transformation errors. Each error is reported
    /// immediately; a clean run with any failed rows exits non-zero.
    #[arg(long = "continue-on-error", default_value_t = false)]
    continue_on_error: bool,

    /// Raise an error when an expression references a name that is not a slot
    /// on the source class, or a value for an enum-ranged target slot matches no
    /// permissible value, or more than one. Recommended for real data transforms;
    /// the default warns and carries on for backward compatibility.
    #[arg(long = "strict", default_value_t = false, overrides_with = "no_strict")]
    strict: bool,

    /// Disable strict mode (the default).
    #[arg(long = "no-strict", overrides_with = "strict")]
    no_strict: bool,

    /// Allow unrestricted evaluation of expressions. Accepted for drop-in
    /// compatibility; it has no effect.
    // PARITY: upstream cli.py `--unrestricted-eval/--no-unrestricted-eval`
    // gates asteval. The Rust evaluator merges the restricted and unrestricted
    // paths and always behaves as unrestricted, so both spellings are no-ops.
    #[arg(
        long = "unrestricted-eval",
        default_value_t = false,
        overrides_with = "no_unrestricted_eval"
    )]
    unrestricted_eval: bool,

    /// Restrict expression evaluation (the upstream default). Has no effect.
    #[arg(long = "no-unrestricted-eval", overrides_with = "unrestricted_eval")]
    no_unrestricted_eval: bool,

    /// Input data file, or a directory of tables for multi-table joins.
    ///
    /// A single file (default) is transformed row-by-row. A *directory* enables
    /// multi-table mode: each file is a named table matched by filename stem to a
    /// source class, the primary table (the source class) is streamed, and every
    /// table referenced by a spec `joins:` block is loaded into a lookup index so
    /// `{Table.col}` join references resolve. Joined tables absent from the
    /// directory resolve to null.
    #[arg(value_name = "INPUT")]
    input: String,
}

// ── derive-schema ─────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
struct DeriveSchemaArgs {
    /// Transformation specification YAML file.
    #[arg(short = 'T', long = "specification", value_name = "SPEC_YAML")]
    spec: String,

    /// Source LinkML schema YAML file.
    #[arg(short = 's', long = "source-schema", value_name = "SCHEMA_YAML")]
    source_schema: String,

    /// Output schema YAML file. Defaults to stdout.
    #[arg(short = 'o', long = "output", value_name = "OUT_YAML")]
    output: Option<String>,
}

// ── validate ──────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
struct ValidateArgs {
    /// Transformation specification YAML file (-T / --specification).
    #[arg(short = 'T', long = "specification", value_name = "SPEC_YAML")]
    spec: String,

    /// Source LinkML schema YAML file (optional).
    /// Validation can run source-only, target-only, or both.
    #[arg(short = 's', long = "source-schema", value_name = "SCHEMA_YAML")]
    source_schema: Option<String>,

    /// Target LinkML schema YAML file (optional).
    #[arg(long = "target-schema", value_name = "TARGET_YAML")]
    target_schema: Option<String>,

    /// Treat unresolved expression references as errors instead of warnings.
    #[arg(long = "strict", default_value_t = false)]
    strict: bool,
}

// ── Subcommand dispatch ───────────────────────────────────────────────────────

async fn dispatch_map_data(args: MapDataArgs) -> Result<()> {
    // Validate format strings early (before building config) so we get a
    // clean error message rather than a confusing anyhow chain.
    parse_format_str(&args.input_format)?;
    parse_format_str(&args.output_format)?;

    let cfg = MapDataConfig {
        spec: args.spec,
        source_schema: args.source_schema,
        target_schema: args.target_schema,
        output: args.output.unwrap_or_else(|| "-".to_owned()),
        source_class: args.source_class,
        input_format: args.input_format,
        output_format: args.output_format,
        workers: args.workers,
        unordered: args.unordered,
        input: args.input,
        write_options: WriteOptions {
            table_name: args.table_name,
            chunk_size: Some(args.chunk_size),
            additional_outputs: args.additional_output.iter().map(PathBuf::from).collect(),
            ..WriteOptions::default()
        },
        strict: args.strict,
    };

    run_map_data_config_with_options(cfg, args.continue_on_error).await?;
    Ok(())
}

fn dispatch_derive_schema(args: &DeriveSchemaArgs) -> Result<()> {
    let spec = load_transform_spec(std::path::Path::new(&args.spec))?;
    // PARITY: cli.py `derive_schema` builds `SchemaMapper` over a plain
    // `SchemaView(schema)`; `source_schema_patches` are not applied to it.
    let mut mapper = SchemaMapper::from_path(std::path::Path::new(&args.source_schema))?;
    let schema_doc = mapper.derive_schema(&spec)?;
    // PARITY: cli.py `derive-schema` writes `yaml_dumper.dumps(schema)`, i.e.
    // `yaml.dump(..., Dumper=SafeDumper, sort_keys=False, allow_unicode=True)`.
    let yaml = linkml_map_io::pyyaml::dump_yaml(
        &schema_doc,
        linkml_map_io::pyyaml::DumpOptions {
            sort_keys: false,
            ..Default::default()
        },
    );

    if let Some(output) = &args.output {
        std::fs::write(output, yaml)?;
    } else {
        print!("{yaml}");
    }
    Ok(())
}

fn dispatch_validate(args: ValidateArgs) -> Result<()> {
    // Nothing to check against — exit cleanly with a hint rather than an error.
    if args.source_schema.is_none() && args.target_schema.is_none() {
        eprintln!(
            "linkml-tr-rs validate: no --source-schema or --target-schema given; \
             nothing to check the spec against."
        );
        return Ok(());
    }

    let messages = run_validate_config(ValidateConfig {
        spec: args.spec,
        source_schema: args.source_schema,
        target_schema: args.target_schema,
        strict: args.strict,
    })?;

    let (mut errors, mut warnings, mut infos) = (0usize, 0usize, 0usize);
    for msg in &messages {
        println!("{msg}");
        match msg.severity {
            Severity::Error => errors += 1,
            Severity::Warning => warnings += 1,
            Severity::Info => infos += 1,
        }
    }

    eprintln!("{errors} error(s), {warnings} warning(s), {infos} info(s)");

    // Non-zero exit on any error makes this usable in CI/scripts; warnings and
    // info do not affect the exit code.
    if errors > 0 {
        std::process::exit(1);
    }
    Ok(())
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Install the logger and apply the verbosity flags before dispatch.
    let filter = resolve_log_level(cli.verbose, cli.quiet);
    // `set_logger` returns `Err` if a logger is already installed (e.g. a test
    // process invoking `main` more than once); ignore that and just apply the
    // level filter for this invocation.
    let _ = log::set_logger(&StderrLogger);
    log::set_max_level(filter);

    match cli.command {
        Commands::MapData(args) => dispatch_map_data(args).await?,
        Commands::DeriveSchema(args) => dispatch_derive_schema(&args)?,
        Commands::Validate(args) => dispatch_validate(args)?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// Both `validate` and `validate-spec` MUST parse as the Validate subcommand
    /// and dispatch to the same handler.  This keeps the CLI a drop-in
    /// replacement for upstream `linkml-tr`.  The alias is `visible_alias` so
    /// `--help` advertises the upstream-compatible name.
    #[test]
    fn test_validate_and_validate_spec_both_parse() {
        let args1 = Cli::try_parse_from(["linkml-tr-rs", "validate", "-T", "spec.yaml"]);
        assert!(args1.is_ok(), "`validate` should parse: {args1:?}");
        let cli1 = args1.unwrap();
        assert!(matches!(cli1.command, Commands::Validate(_)));

        let args2 = Cli::try_parse_from(["linkml-tr-rs", "validate-spec", "-T", "spec.yaml"]);
        assert!(args2.is_ok(), "`validate-spec` should parse: {args2:?}");
        let cli2 = args2.unwrap();
        assert!(matches!(cli2.command, Commands::Validate(_)));
    }

    /// `validate-spec` with the same flags as `validate` must parse identically.
    #[test]
    fn test_validate_spec_full_flags() {
        let args = Cli::try_parse_from([
            "linkml-tr-rs",
            "validate-spec",
            "-T",
            "spec.yaml",
            "-s",
            "source.yaml",
            "--target-schema",
            "target.yaml",
            "--strict",
        ]);
        assert!(args.is_ok(), "{args:?}");
        let cli = args.unwrap();
        match cli.command {
            Commands::Validate(v) => {
                assert_eq!(v.spec, "spec.yaml");
                assert_eq!(v.source_schema.unwrap(), "source.yaml");
                assert_eq!(v.target_schema.unwrap(), "target.yaml");
                assert!(v.strict);
            }
            _ => panic!("expected Validate, got something else"),
        }
    }

    /// Each of `-q`, `--quiet`, `-v`, `--verbose` placed BEFORE a subcommand
    /// parses successfully and does NOT consume the subcommand. This pins the
    /// upstream bug fixed in `c69db3e0fe` (a value-taking `-q` swallowed the
    /// subcommand and printed group help instead of running).
    #[test]
    fn test_logging_flags_do_not_consume_subcommand() {
        for flag in ["-q", "--quiet", "-v", "--verbose"] {
            let args = Cli::try_parse_from(["linkml-tr-rs", flag, "validate", "-T", "spec.yaml"]);
            assert!(
                args.is_ok(),
                "{flag} before a subcommand should parse: {args:?}"
            );
            let cli = args.unwrap();
            assert!(
                matches!(cli.command, Commands::Validate(_)),
                "{flag} swallowed the subcommand"
            );
        }
    }

    /// `--strict/--no-strict` and `--unrestricted-eval/--no-unrestricted-eval`
    /// parse as upstream's click flag pairs: the last one given wins.
    #[test]
    fn test_map_data_flag_pairs() {
        let base = [
            "linkml-tr-rs",
            "map-data",
            "-T",
            "s.yaml",
            "-s",
            "src.yaml",
            "in.tsv",
        ];
        let parse = |extra: &[&str]| {
            let argv: Vec<&str> = base.iter().chain(extra).copied().collect();
            match Cli::try_parse_from(argv)
                .expect("map-data should parse")
                .command
            {
                Commands::MapData(m) => m,
                _ => panic!("expected MapData"),
            }
        };
        assert!(!parse(&[]).strict);
        assert!(parse(&["--strict"]).strict);
        assert!(!parse(&["--strict", "--no-strict"]).strict);
        assert!(parse(&["--no-strict", "--strict"]).strict);
        assert!(parse(&["--unrestricted-eval"]).unrestricted_eval);
        assert!(!parse(&["--unrestricted-eval", "--no-unrestricted-eval"]).unrestricted_eval);
    }

    /// `-v` accumulates: `-vv` yields `verbose == 2`.
    #[test]
    fn test_verbose_counts() {
        let cli = Cli::try_parse_from(["linkml-tr-rs", "-vv", "validate", "-T", "spec.yaml"])
            .expect("-vv should parse");
        assert_eq!(cli.verbose, 2);
        assert!(!cli.quiet);
    }

    /// The flag -> level ladder pins upstream's exact mapping:
    /// (none) -> WARN, -q -> ERROR, -v -> INFO, -vv -> DEBUG, and -q wins
    /// over -v when both are passed.
    #[test]
    fn test_resolve_log_level_ladder() {
        use log::LevelFilter;
        assert_eq!(resolve_log_level(0, false), LevelFilter::Warn);
        assert_eq!(resolve_log_level(0, true), LevelFilter::Error);
        assert_eq!(resolve_log_level(1, false), LevelFilter::Info);
        assert_eq!(resolve_log_level(2, false), LevelFilter::Debug);
        // Quiet wins over verbose when both are passed (upstream applies the
        // quiet override after the verbose ladder).
        assert_eq!(resolve_log_level(2, true), LevelFilter::Error);
    }

    fn parse_map_data(extra: &[&str]) -> MapDataArgs {
        let mut argv = vec!["linkml-tr-rs", "map-data", "-T", "t.yaml", "-s", "s.yaml"];
        argv.extend_from_slice(extra);
        argv.push("input.tsv");
        match Cli::try_parse_from(argv)
            .expect("map-data args should parse")
            .command
        {
            Commands::MapData(args) => args,
            other => panic!("expected MapData, got {other:?}"),
        }
    }

    /// Upstream names the flag `--source-type`; `--source-class` stays as an alias.
    /// Port of the flag used throughout upstream `tests/test_cli/test_cli_tabular.py`.
    #[test]
    fn test_source_type_and_source_class_both_parse() {
        for flag in ["--source-type", "--source-class"] {
            let args = parse_map_data(&[flag, "Person"]);
            assert_eq!(args.source_class.as_deref(), Some("Person"), "{flag}");
        }
        assert_eq!(parse_map_data(&[]).source_class, None);
    }

    /// Upstream `map-data` options: `-o` optional, `-f`, `--chunk-size`, repeatable `-O`.
    #[test]
    fn test_output_options_match_upstream() {
        let args = parse_map_data(&[]);
        assert_eq!(args.output, None, "-o is optional (stdout)");
        assert_eq!(args.chunk_size, 1000);
        assert_eq!(args.output_format, "auto");
        assert!(args.additional_output.is_empty());

        let args = parse_map_data(&[
            "-o",
            "out.jsonl",
            "-f",
            "csv",
            "--chunk-size",
            "7",
            "-O",
            "a.tsv",
            "-O",
            "b.json",
        ]);
        assert_eq!(args.output.as_deref(), Some("out.jsonl"));
        assert_eq!(args.output_format, "csv");
        assert_eq!(args.chunk_size, 7);
        assert_eq!(args.additional_output, ["a.tsv", "b.json"]);
    }
}
