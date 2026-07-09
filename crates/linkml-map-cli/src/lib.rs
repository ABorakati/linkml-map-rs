//! Public library surface of `linkml-map-cli`.
//!
//! Exposes [`run_map_data_config`] and [`run_validate_config`] so integration
//! tests can exercise the CLI logic without spawning a subprocess.

use std::path::Path;

use anyhow::Result;
use linkml_map_core::schema::SchemaProvider;
use linkml_map_core::validate::validate_spec_semantics;
use linkml_map_io::Format;
use linkml_map_pipeline::{load_transform_spec, PipelineConfig, PipelineStats};
use linkml_map_schemaview::SchemaViewProvider;

/// Re-exported from `linkml-map-core` so callers/tests of [`run_validate_config`]
/// can inspect message severity/content without depending on core directly.
pub use linkml_map_core::validate::{Severity, ValidationMessage};

// ── Format parsing (pub for tests) ───────────────────────────────────────────

/// Parse a user-supplied format string into `Option<Format>`.
/// `"auto"` returns `None` (auto-detect from path).
pub fn parse_format_str(s: &str) -> Result<Option<Format>> {
    match s.to_ascii_lowercase().as_str() {
        "auto" => Ok(None),
        "csv" => Ok(Some(Format::Csv)),
        "tsv" | "tab" => Ok(Some(Format::Tsv)),
        "json" => Ok(Some(Format::Json)),
        "jsonl" | "ndjson" => Ok(Some(Format::Jsonl)),
        "yaml" | "yml" => Ok(Some(Format::Yaml)),
        other => anyhow::bail!(
            "unknown format {:?}; expected one of: auto, csv, tsv, json, jsonl, yaml",
            other
        ),
    }
}

// ── map-data: programmatic entry point ───────────────────────────────────────

/// Configuration for a `map-data` invocation (mirroring the CLI flags).
pub struct MapDataConfig {
    /// Path to the transformation spec YAML (-T).
    pub spec: String,
    /// Path to the source LinkML schema YAML (-s).
    pub source_schema: String,
    /// Path to the target LinkML schema YAML. `None` → same as `source_schema`.
    pub target_schema: Option<String>,
    /// Output file path (-o). Required (stdout not yet supported).
    pub output: String,
    /// Source class name hint. `None` → engine resolves from spec.
    pub source_class: Option<String>,
    /// Input format override string ("auto" | "csv" | "tsv" | "json" | "jsonl" | "yaml").
    pub input_format: String,
    /// Output format override string (same options).
    pub output_format: String,
    /// Rayon worker threads. 0 → global pool default (≈ num_cpus).
    pub workers: usize,
    /// `true` → rows written in arrival order (max throughput, output order undefined).
    pub unordered: bool,
    /// Input data path — a single data file (default), or a *directory* of tables
    /// for multi-table joins (each file is a named table matched by filename stem;
    /// the primary table is streamed and joined tables feed a lookup index). See
    /// [`linkml_map_pipeline::PipelineConfig::source_path`].
    pub input: String,
}

impl MapDataConfig {
    /// Convenience constructor with sensible defaults for format + workers.
    pub fn new(
        spec: impl Into<String>,
        source_schema: impl Into<String>,
        input: impl Into<String>,
        output: impl Into<String>,
    ) -> Self {
        Self {
            spec: spec.into(),
            source_schema: source_schema.into(),
            target_schema: None,
            output: output.into(),
            source_class: None,
            input_format: "auto".into(),
            output_format: "auto".into(),
            workers: 4,
            unordered: false,
            input: input.into(),
        }
    }
}

/// Run `map-data` programmatically and return pipeline statistics.
///
/// Progress messages are written to stderr (same as the CLI).
pub async fn run_map_data_config(cfg: MapDataConfig) -> Result<PipelineStats> {
    run_map_data_config_with_options(cfg, false).await
}

/// Run `map-data` with its row-error policy.
///
/// `continue_on_error` mirrors the CLI flag of the same name.  It keeps
/// processing after a row-level transformation error, reports the error at
/// once, and returns a non-zero result after otherwise clean completion.
pub async fn run_map_data_config_with_options(
    cfg: MapDataConfig,
    continue_on_error: bool,
) -> Result<PipelineStats> {
    let source_format = parse_format_str(&cfg.input_format)?;
    let out_format = parse_format_str(&cfg.output_format)?;

    let target_schema = cfg
        .target_schema
        .unwrap_or_else(|| cfg.source_schema.clone());

    let pipeline_cfg = PipelineConfig {
        source_path: cfg.input.clone(),
        source_format,
        schema_path: cfg.source_schema.clone(),
        target_schema_path: target_schema,
        spec_path: cfg.spec.clone(),
        out_path: cfg.output.clone(),
        out_format,
        source_class: cfg.source_class.clone(),
        workers: cfg.workers,
        ordered: !cfg.unordered,
        channel_depth: 256,
    };

    eprintln!(
        "linkml-tr-rs map-data: {} -> {} (spec: {})",
        cfg.input, cfg.output, cfg.spec
    );

    let stats = linkml_map_pipeline::run_pipeline_with_options(pipeline_cfg, continue_on_error).await?;

    eprintln!(
        "rows_in={} rows_out={} elapsed={:.3}s throughput={:.0} rows/s",
        stats.rows_in,
        stats.rows_out,
        stats.elapsed.as_secs_f64(),
        stats.rows_per_sec,
    );

    Ok(stats)
}

// ── validate: programmatic entry point ────────────────────────────────────────

/// Configuration for a `validate` invocation (mirroring the CLI flags).
///
/// Validation is a read-only pre-flight check: it cross-references the spec
/// against whichever schemas are supplied and never runs a transform. At least
/// one of `source_schema` / `target_schema` should be set — with neither,
/// [`validate_spec_semantics`] has nothing to check and returns no messages.
pub struct ValidateConfig {
    /// Path to the transformation spec YAML (-T).
    pub spec: String,
    /// Path to the source LinkML schema YAML (-s). `None` → skip source checks.
    pub source_schema: Option<String>,
    /// Path to the target LinkML schema YAML. `None` → skip target checks.
    pub target_schema: Option<String>,
    /// When `true`, unresolved expression references are errors, not warnings.
    pub strict: bool,
}

/// Run `validate` programmatically and return every [`ValidationMessage`].
///
/// The source schema is loaded with the spec's `source_schema_patches` applied
/// (matching the transform path); the target schema is loaded unpatched (as it
/// is everywhere else in the codebase). The returned messages are in the same
/// order [`validate_spec_semantics`] produces them.
pub fn run_validate_config(cfg: ValidateConfig) -> Result<Vec<ValidationMessage>> {
    let spec = load_transform_spec(Path::new(&cfg.spec))?;

    let source = cfg
        .source_schema
        .as_deref()
        .map(|p| {
            SchemaViewProvider::load_from_path_with_patch(
                Path::new(p),
                spec.source_schema_patches.as_ref(),
            )
        })
        .transpose()?;

    let target = cfg
        .target_schema
        .as_deref()
        .map(|p| SchemaViewProvider::load_from_path(Path::new(p)))
        .transpose()?;

    let messages = validate_spec_semantics(
        &spec,
        source.as_ref().map(|p| p as &dyn SchemaProvider),
        target.as_ref().map(|p| p as &dyn SchemaProvider),
        cfg.strict,
    );

    Ok(messages)
}
