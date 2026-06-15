//! Async streaming writers that consume [`Value`] items and write to a file.
//!
//! # Format behaviour
//!
//! | Format | Streaming | Notes |
//! |--------|-----------|-------|
//! | JSONL  | ✓ per-record | one JSON object per line |
//! | JSON   | buffered | writes a complete JSON array |
//! | CSV    | ✓ per-record | header derived from first record; all scalars stringified |
//! | TSV    | ✓ per-record | same as CSV with tab delimiter |
//! | YAML   | buffered | writes a YAML sequence |
//!
//! The writers consume an async `Stream<Item = Result<Value>>` (or a
//! `Vec<Value>` via the [`write_all`] helper).

use std::path::Path;

use anyhow::{bail, Context, Result};
use futures::{Stream, StreamExt};
use linkml_map_core::value::Value;
use tokio::fs::File;
use tokio::io::{AsyncWrite, AsyncWriteExt, BufWriter};

use crate::format::Format;

// ─── Public API ──────────────────────────────────────────────────────────────

/// Write every item from *stream* into *path* using *format*.
///
/// The file is created (or truncated) before writing begins.
pub async fn write_all<S>(path: impl AsRef<Path>, format: Format, stream: S) -> Result<()>
where
    S: Stream<Item = Result<Value>> + Unpin,
{
    let path = path.as_ref();
    let file = File::create(path)
        .await
        .with_context(|| format!("creating {:?}", path))?;
    let mut writer = BufWriter::new(file);

    match format {
        Format::Jsonl => write_jsonl(&mut writer, stream).await?,
        Format::Json => write_json(&mut writer, stream).await?,
        Format::Csv => write_csv(&mut writer, b',', stream).await?,
        Format::Tsv => write_csv(&mut writer, b'\t', stream).await?,
        Format::Yaml => write_yaml(&mut writer, stream).await?,
    }

    writer.flush().await.context("flushing output")?;
    Ok(())
}

/// Convenience: detect format from extension then call [`write_all`].
pub async fn write_all_auto<S>(path: impl AsRef<Path>, stream: S) -> Result<()>
where
    S: Stream<Item = Result<Value>> + Unpin,
{
    let path = path.as_ref();
    let fmt = Format::from_path(path)?;
    write_all(path, fmt, stream).await
}

/// Write a `Vec<Value>` to *path* in *format*.
pub async fn write_vec(path: impl AsRef<Path>, format: Format, values: Vec<Value>) -> Result<()> {
    let stream = futures::stream::iter(values.into_iter().map(Ok::<Value, anyhow::Error>));
    write_all(path, format, stream).await
}

// ─── JSONL ───────────────────────────────────────────────────────────────────

async fn write_jsonl<W, S>(writer: &mut W, mut stream: S) -> Result<()>
where
    W: AsyncWrite + Unpin,
    S: Stream<Item = Result<Value>> + Unpin,
{
    while let Some(item) = stream.next().await {
        let val = item?;
        let json = value_to_json(&val)?;
        let line = serde_json::to_string(&json).context("serialising JSONL record")?;
        writer.write_all(line.as_bytes()).await?;
        writer.write_all(b"\n").await?;
    }
    Ok(())
}

// ─── JSON array ──────────────────────────────────────────────────────────────

async fn write_json<W, S>(writer: &mut W, mut stream: S) -> Result<()>
where
    W: AsyncWrite + Unpin,
    S: Stream<Item = Result<Value>> + Unpin,
{
    writer.write_all(b"[\n").await?;
    let mut first = true;
    while let Some(item) = stream.next().await {
        let val = item?;
        let json = value_to_json(&val)?;
        let fragment = serde_json::to_string_pretty(&json).context("serialising JSON record")?;
        if !first {
            writer.write_all(b",\n").await?;
        }
        first = false;
        writer.write_all(fragment.as_bytes()).await?;
    }
    writer.write_all(b"\n]\n").await?;
    Ok(())
}

// ─── CSV / TSV ───────────────────────────────────────────────────────────────

async fn write_csv<W, S>(writer: &mut W, delimiter: u8, mut stream: S) -> Result<()>
where
    W: AsyncWrite + Unpin,
    S: Stream<Item = Result<Value>> + Unpin,
{
    let sep = delimiter as char;
    let mut headers: Option<Vec<String>> = None;

    while let Some(item) = stream.next().await {
        let val = item?;
        match &val {
            Value::Map(map) => {
                // First record: derive headers and write header row.
                if headers.is_none() {
                    let hdrs: Vec<String> = map.keys().cloned().collect();
                    let header_line = hdrs
                        .iter()
                        .map(|h| csv_escape(h, sep))
                        .collect::<Vec<_>>()
                        .join(&sep.to_string());
                    writer.write_all(header_line.as_bytes()).await?;
                    writer.write_all(b"\n").await?;
                    headers = Some(hdrs);
                }
                let hdrs = headers.as_ref().unwrap();
                let row: Vec<String> = hdrs
                    .iter()
                    .map(|h| {
                        let cell = map.get(h).unwrap_or(&Value::Null);
                        csv_escape(&value_to_csv_str(cell), sep)
                    })
                    .collect();
                let line = row.join(&sep.to_string());
                writer.write_all(line.as_bytes()).await?;
                writer.write_all(b"\n").await?;
            }
            other => bail!("CSV/TSV writer expects Value::Map records, got {:?}", other),
        }
    }
    Ok(())
}

// ─── YAML ────────────────────────────────────────────────────────────────────

async fn write_yaml<W, S>(writer: &mut W, mut stream: S) -> Result<()>
where
    W: AsyncWrite + Unpin,
    S: Stream<Item = Result<Value>> + Unpin,
{
    let mut items: Vec<serde_json::Value> = Vec::new();
    while let Some(item) = stream.next().await {
        let val = item?;
        items.push(value_to_json(&val)?);
    }
    let yaml_str = serde_yaml_ng::to_string(&items).context("serialising YAML")?;
    writer.write_all(yaml_str.as_bytes()).await?;
    Ok(())
}

// ─── Value → serde_json::Value ────────────────────────────────────────────────

/// Losslessly convert a [`Value`] to `serde_json::Value` for serialisation.
pub(crate) fn value_to_json(v: &Value) -> Result<serde_json::Value> {
    Ok(match v {
        Value::Null => serde_json::Value::Null,
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Int(i) => serde_json::Value::Number((*i).into()),
        Value::Float(f) => {
            let n = serde_json::Number::from_f64(*f)
                .with_context(|| format!("non-finite float: {}", f))?;
            serde_json::Value::Number(n)
        }
        Value::Str(s) => serde_json::Value::String(s.clone()),
        Value::List(items) => {
            serde_json::Value::Array(items.iter().map(value_to_json).collect::<Result<_>>()?)
        }
        Value::Map(map) => {
            let obj: serde_json::Map<String, serde_json::Value> = map
                .iter()
                .map(|(k, v)| value_to_json(v).map(|jv| (k.clone(), jv)))
                .collect::<Result<_>>()?;
            serde_json::Value::Object(obj)
        }
    })
}

// ─── Tabular helpers ─────────────────────────────────────────────────────────

/// Stringify a `Value` for a CSV/TSV cell.
/// Complex types (List, Map) are serialised as JSON; Null → empty string.
fn value_to_csv_str(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Str(s) => s.clone(),
        Value::List(_) | Value::Map(_) => value_to_json(v)
            .ok()
            .map(|j| j.to_string())
            .unwrap_or_default(),
    }
}

/// Escape a string for a CSV/TSV cell.
/// Values containing the delimiter, double-quote, or newlines are RFC 4180 quoted.
fn csv_escape(s: &str, sep: char) -> String {
    let needs_quote = s.contains(sep) || s.contains('"') || s.contains('\n') || s.contains('\r');
    if needs_quote {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_owned()
    }
}
