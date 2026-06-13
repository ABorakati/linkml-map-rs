//! Public library surface of `linkml-map-cli`.
//!
//! Exposes [`run_map_data_config`] so integration tests can exercise the
//! pipeline logic without spawning a subprocess.

use anyhow::Result;
use linkml_map_io::Format;
use linkml_map_pipeline::{PipelineConfig, PipelineStats};

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
    /// Input data file path.
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

    let stats = linkml_map_pipeline::run_pipeline(pipeline_cfg).await?;

    eprintln!(
        "rows_in={} rows_out={} elapsed={:.3}s throughput={:.0} rows/s",
        stats.rows_in,
        stats.rows_out,
        stats.elapsed.as_secs_f64(),
        stats.rows_per_sec,
    );

    Ok(stats)
}
