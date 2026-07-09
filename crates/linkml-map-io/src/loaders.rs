//! Async streaming loaders that produce [`Value`] items from files.
//!
//! # Format behaviour
//!
//! | Format | Streaming | Item shape |
//! |--------|-----------|-----------|
//! | CSV    | ✓ row-by-row | `Value::Map` keyed by header column names; strings by default, schema-hinted numeric columns are numbers |
//! | TSV    | ✓ row-by-row | same as CSV |
//! | JSONL  | ✓ line-by-line | one `Value` per line |
//! | JSON   | whole-file | array → one `Value` per element; object → single `Value` |
//! | YAML   | whole-file | same semantics as JSON |
//!
//! By default type coercion is intentionally **not** done here.  The
//! schema-aware entry points accept precomputed numeric column hints, which
//! lets CSV/TSV inputs preserve numeric values without guessing from their
//! lexical form (and consequently preserves e.g. ZIP codes and enum values).

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use csv_async::AsyncReaderBuilder;
use futures::stream::{self, BoxStream, StreamExt};
use linkml_map_core::value::Value;
use tokio::fs::File;
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::format::Format;

// ─── Public API ─────────────────────────────────────────────────────────────

/// Open *path* and return a boxed `Stream<Item = Result<Value>>`.
///
/// For streaming formats (CSV, TSV, JSONL) the file is read incrementally.
/// For whole-file formats (JSON, YAML) the file is read in one shot and the
/// results are returned as a stream of owned `Value`s.
pub async fn load_stream(
    path: impl AsRef<Path>,
    format: Format,
) -> Result<BoxStream<'static, Result<Value>>> {
    let path = path.as_ref().to_owned();
    match format {
        Format::Csv => Ok(csv_stream_inner(path, b',', NumericColumnHints::new())),
        Format::Tsv => Ok(csv_stream_inner(path, b'\t', NumericColumnHints::new())),
        Format::Jsonl => jsonl_stream(path).await,
        Format::Json => json_stream(path).await,
        Format::Yaml => yaml_stream(path).await,
    }
}

/// Scalar type to use for a numeric tabular column.
///
/// This deliberately only represents numeric LinkML ranges. String and enum
/// ranges must never be inferred from a numeric-looking cell value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumericColumnKind {
    Integer,
    Float,
}

/// Numeric type hints keyed by CSV/TSV header name.
///
/// Callers build this once from their already-loaded source schema, then reuse
/// it for every row. This avoids reparsing a source schema for each input file.
pub type NumericColumnHints = HashMap<String, NumericColumnKind>;

/// Like [`load_stream`], but applies precomputed source-schema numeric hints to
/// CSV and TSV columns. Other formats are unchanged.
pub async fn load_stream_with_numeric_hints(
    path: impl AsRef<Path>,
    format: Format,
    numeric_hints: NumericColumnHints,
) -> Result<BoxStream<'static, Result<Value>>> {
    let path = path.as_ref().to_owned();
    match format {
        Format::Csv => Ok(csv_stream_inner(path, b',', numeric_hints)),
        Format::Tsv => Ok(csv_stream_inner(path, b'\t', numeric_hints)),
        Format::Jsonl => jsonl_stream(path).await,
        Format::Json => json_stream(path).await,
        Format::Yaml => yaml_stream(path).await,
    }
}

/// Convenience: detect format from extension then call [`load_stream`].
pub async fn load_stream_auto(path: impl AsRef<Path>) -> Result<BoxStream<'static, Result<Value>>> {
    let path = path.as_ref();
    let fmt = Format::from_path(path)?;
    load_stream(path, fmt).await
}

/// Collect all items from a stream into a `Vec<Value>`.
pub async fn load_all(path: impl AsRef<Path>, format: Format) -> Result<Vec<Value>> {
    let mut stream = load_stream(path, format).await?;
    let mut out = Vec::new();
    while let Some(item) = stream.next().await {
        out.push(item?);
    }
    Ok(out)
}

/// Collect all values from [`load_stream_with_numeric_hints`].
pub async fn load_all_with_numeric_hints(
    path: impl AsRef<Path>,
    format: Format,
    numeric_hints: NumericColumnHints,
) -> Result<Vec<Value>> {
    let mut stream = load_stream_with_numeric_hints(path, format, numeric_hints).await?;
    let mut out = Vec::new();
    while let Some(item) = stream.next().await {
        out.push(item?);
    }
    Ok(out)
}

// ─── CSV / TSV ───────────────────────────────────────────────────────────────

/// Build a `BoxStream` for CSV/TSV. The file-open and header-read happen
/// inside the stream itself so this function is sync and infallible.
fn csv_stream_inner(
    path: std::path::PathBuf,
    delimiter: u8,
    numeric_hints: NumericColumnHints,
) -> BoxStream<'static, Result<Value>> {
    use csv_async::StringRecord;

    // The state machine used in `stream::unfold`:
    // - `Init`: file not yet opened
    // - `Running`: headers known, reader ready for data rows
    enum State {
        Init(std::path::PathBuf, u8, NumericColumnHints),
        Running {
            rdr: csv_async::AsyncReader<File>,
            headers: Vec<String>,
            numeric_hints: NumericColumnHints,
        },
    }

    let init = State::Init(path, delimiter, numeric_hints);

    stream::unfold(init, |state| async move {
        match state {
            State::Init(path, delim, numeric_hints) => {
                // Open file and read headers, then yield the first data row.
                let file = match File::open(&path).await {
                    Ok(f) => f,
                    Err(e) => {
                        return Some((
                            Err(anyhow::anyhow!("failed to open {:?}: {}", path, e)),
                            State::Init(path, delim, numeric_hints),
                        ));
                    }
                };
                let mut rdr = AsyncReaderBuilder::new()
                    .delimiter(delim)
                    .has_headers(true)
                    // Sparse delimited tables (upstream #210) often have rows
                    // with fewer fields than the header — trailing columns are
                    // empty and the trailing delimiters are simply omitted. The
                    // delimiter is already fixed by the declared `Format` (`\t`
                    // for TSV, `,` for CSV), so there is no sniffing to get
                    // wrong; we just need to tolerate ragged rows instead of
                    // aborting with `UnequalLengths`. Short records are padded
                    // to the header width in `row_to_value` via `get(i)`.
                    .flexible(true)
                    .create_reader(file);

                let headers: Vec<String> = match rdr.headers().await {
                    Ok(h) => h.iter().map(|s| s.to_owned()).collect(),
                    Err(e) => {
                        return Some((
                            Err(anyhow::anyhow!("CSV header error: {}", e)),
                            State::Init(path, delim, numeric_hints),
                        ));
                    }
                };

                // Read first data row.
                let mut record = StringRecord::new();
                match rdr.read_record(&mut record).await {
                    Ok(true) => {
                        let val = row_to_value(&headers, &record, &numeric_hints);
                        Some((
                            Ok(val),
                            State::Running {
                                rdr,
                                headers,
                                numeric_hints,
                            },
                        ))
                    }
                    Ok(false) => None, // empty file (headers only)
                    Err(e) => Some((
                        Err(anyhow::anyhow!("CSV read error: {}", e)),
                        State::Running {
                            rdr,
                            headers,
                            numeric_hints,
                        },
                    )),
                }
            }
            State::Running {
                mut rdr,
                headers,
                numeric_hints,
            } => {
                let mut record = StringRecord::new();
                match rdr.read_record(&mut record).await {
                    Ok(true) => {
                        let val = row_to_value(&headers, &record, &numeric_hints);
                        Some((
                            Ok(val),
                            State::Running {
                                rdr,
                                headers,
                                numeric_hints,
                            },
                        ))
                    }
                    Ok(false) => None, // EOF
                    Err(e) => Some((
                        Err(anyhow::anyhow!("CSV read error: {}", e)),
                        State::Running {
                            rdr,
                            headers,
                            numeric_hints,
                        },
                    )),
                }
            }
        }
    })
    .boxed()
}

/// Convert a `StringRecord` into a `Value::Map` using the header names as keys.
/// Only columns explicitly identified as numeric from the source schema are
/// coerced; malformed numeric cells remain strings for later validation.
fn row_to_value(
    headers: &[String],
    record: &csv_async::StringRecord,
    numeric_hints: &NumericColumnHints,
) -> Value {
    // Build through serde_json::Map so we get `Value::Map(IndexMap)` without
    // depending on `indexmap` directly in this crate.
    let mut obj = serde_json::Map::new();
    for (i, header) in headers.iter().enumerate() {
        let cell = record.get(i).unwrap_or("");
        let value = match numeric_hints.get(header) {
            Some(NumericColumnKind::Integer) => cell
                .parse::<i64>()
                .map(serde_json::Value::from)
                .unwrap_or_else(|_| serde_json::Value::String(cell.to_owned())),
            Some(NumericColumnKind::Float) => cell
                .parse::<f64>()
                .ok()
                .filter(|value| value.is_finite())
                .map(serde_json::Value::from)
                .unwrap_or_else(|| serde_json::Value::String(cell.to_owned())),
            None => serde_json::Value::String(cell.to_owned()),
        };
        obj.insert(header.clone(), value);
    }
    json_to_value(serde_json::Value::Object(obj))
}

// ─── JSONL ────────────────────────────────────────────────────────────────────

async fn jsonl_stream(path: impl AsRef<Path>) -> Result<BoxStream<'static, Result<Value>>> {
    let file = File::open(&path)
        .await
        .with_context(|| format!("opening {:?}", path.as_ref()))?;

    let reader = BufReader::new(file);
    let lines = reader.lines();

    let s = stream::unfold(lines, |mut lines| async move {
        loop {
            match lines.next_line().await {
                Err(e) => return Some((Err(anyhow::Error::from(e)), lines)),
                Ok(None) => return None, // EOF
                Ok(Some(line)) => {
                    let trimmed = line.trim().to_owned();
                    if trimmed.is_empty() {
                        continue; // skip blank lines
                    }
                    let val: Result<Value> = serde_json::from_str(&trimmed)
                        .map_err(|e| anyhow::anyhow!("JSONL parse error: {}: {}", e, trimmed));
                    return Some((val, lines));
                }
            }
        }
    });

    Ok(s.boxed())
}

// ─── JSON (whole-file) ────────────────────────────────────────────────────────

async fn json_stream(path: impl AsRef<Path>) -> Result<BoxStream<'static, Result<Value>>> {
    let bytes = tokio::fs::read(&path)
        .await
        .with_context(|| format!("opening {:?}", path.as_ref()))?;

    let val: serde_json::Value = serde_json::from_slice(&bytes).context("JSON parse error")?;

    let items = serde_json_value_to_values(val);
    Ok(stream::iter(items.into_iter().map(Ok)).boxed())
}

// ─── YAML (whole-file) ────────────────────────────────────────────────────────

async fn yaml_stream(path: impl AsRef<Path>) -> Result<BoxStream<'static, Result<Value>>> {
    let bytes = tokio::fs::read(&path)
        .await
        .with_context(|| format!("opening {:?}", path.as_ref()))?;

    let val: serde_json::Value = serde_yaml_ng::from_slice(&bytes).context("YAML parse error")?;

    let items = serde_json_value_to_values(val);
    Ok(stream::iter(items.into_iter().map(Ok)).boxed())
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Convert a `serde_json::Value` into a `Vec<Value>`.
/// Arrays are expanded; single objects/scalars yield a one-element vec.
fn serde_json_value_to_values(v: serde_json::Value) -> Vec<Value> {
    match v {
        serde_json::Value::Array(items) => items.into_iter().map(json_to_value).collect(),
        other => vec![json_to_value(other)],
    }
}

/// Recursively convert `serde_json::Value` → [`Value`].
pub(crate) fn json_to_value(v: serde_json::Value) -> Value {
    Value::from(&v)
}
