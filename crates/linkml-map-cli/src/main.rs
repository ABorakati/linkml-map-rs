//! `linkml-tr-rs` — LinkML transformation engine CLI (Rust port of `linkml-tr`).
//!
//! # Subcommands
//!
//! - `map-data` — run the concurrent pipeline over a source data file.
//! - `derive-schema` — derive a target schema from a transformation spec.
//! - `validate` — read-only pre-flight check of a spec against its source/target
//!   schemas (no transform is run).

use anyhow::Result;
use clap::{Parser, Subcommand};
use linkml_map_cli::{
    MapDataConfig, Severity, ValidateConfig, parse_format_str, run_map_data_config_with_options,
    run_validate_config,
};
use linkml_map_core::inference::SchemaMapper;
use linkml_map_pipeline::load_transform_spec;
use linkml_map_schemaview::SchemaViewProvider;

// ── Top-level CLI ─────────────────────────────────────────────────────────────

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

    /// Target LinkML schema YAML file (defaults to source schema when omitted).
    #[arg(long = "target-schema", value_name = "TARGET_YAML")]
    target_schema: Option<String>,

    /// Output file path.  Pass `-` to write to stdout (requires --output-format).
    #[arg(short = 'o', long = "output", value_name = "OUT_FILE")]
    output: String,

    /// Source class name hint.
    /// When omitted, the engine resolves from the first class_derivation.
    #[arg(long = "source-class", value_name = "CLASS")]
    source_class: Option<String>,

    /// Input format override.
    /// One of: auto, csv, tsv, json, jsonl, yaml.
    /// Defaults to auto-detection from the input file extension.
    #[arg(long = "input-format", value_name = "FMT", default_value = "auto")]
    input_format: String,

    /// Output format override.
    /// One of: auto, csv, tsv, json, jsonl, yaml.
    /// Defaults to auto-detection from the output file extension.
    #[arg(long = "output-format", value_name = "FMT", default_value = "auto")]
    output_format: String,

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
        output: args.output,
        source_class: args.source_class,
        input_format: args.input_format,
        output_format: args.output_format,
        workers: args.workers,
        unordered: args.unordered,
        input: args.input,
    };

    run_map_data_config_with_options(cfg, args.continue_on_error).await?;
    Ok(())
}

fn dispatch_derive_schema(args: &DeriveSchemaArgs) -> Result<()> {
    let spec = load_transform_spec(std::path::Path::new(&args.spec))?;
    let source = SchemaViewProvider::load_from_path_with_patch(
        std::path::Path::new(&args.source_schema),
        spec.source_schema_patches.as_ref(),
    )?;
    let schema_doc = SchemaMapper::new(&source).derive_schema(&spec);
    let yaml = serde_yaml_ng::to_string(&schema_doc)?;

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
    match cli.command {
        Commands::MapData(args) => dispatch_map_data(args).await?,
        Commands::DeriveSchema(ref args) => dispatch_derive_schema(args)?,
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
}
