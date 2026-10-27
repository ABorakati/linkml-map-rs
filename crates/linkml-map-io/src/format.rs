//! File format detection and the `Format` enum.

use std::path::Path;

use anyhow::{Result, bail};

/// Supported I/O formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Comma-separated values (header row required).
    Csv,
    /// Tab-separated values (header row required).
    Tsv,
    /// A single JSON value: array → stream of items, object → single item.
    Json,
    /// Newline-delimited JSON; one JSON value per line.
    Jsonl,
    /// YAML document (loaded whole).
    Yaml,
    /// Parquet file built by DuckDB from the staged JSONL (output only; needs the
    /// `columnar` feature).
    Parquet,
    /// Table in a DuckDB database file (output only; needs the `columnar` feature).
    DuckDb,
}

impl Format {
    /// Detect format from a file-path extension.
    ///
    /// ```
    /// # use linkml_map_io::format::Format;
    /// assert_eq!(Format::from_path("data.csv").unwrap(), Format::Csv);
    /// assert_eq!(Format::from_path("data.jsonl").unwrap(), Format::Jsonl);
    /// assert_eq!(Format::from_path("schema.yaml").unwrap(), Format::Yaml);
    /// ```
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self> {
        let ext = path
            .as_ref()
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        match ext.as_str() {
            "csv" => Ok(Format::Csv),
            "tsv" | "tab" => Ok(Format::Tsv),
            "json" => Ok(Format::Json),
            "jsonl" | "ndjson" => Ok(Format::Jsonl),
            "yaml" | "yml" => Ok(Format::Yaml),
            "parquet" => Ok(Format::Parquet),
            "duckdb" => Ok(Format::DuckDb),
            other => bail!("unsupported file extension: {:?}", other),
        }
    }

    /// The canonical file extension for this format.
    pub fn extension(self) -> &'static str {
        match self {
            Format::Csv => "csv",
            Format::Tsv => "tsv",
            Format::Json => "json",
            Format::Jsonl => "jsonl",
            Format::Yaml => "yaml",
            Format::Parquet => "parquet",
            Format::DuckDb => "duckdb",
        }
    }

    /// Whether this is a binary columnar output format (Parquet or DuckDB), which
    /// cannot be written to stdout and cannot be read back as input.
    pub fn is_columnar(self) -> bool {
        matches!(self, Format::Parquet | Format::DuckDb)
    }
}
