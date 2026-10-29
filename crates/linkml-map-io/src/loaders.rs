//! Async streaming loaders that produce [`Value`] items from files.
//!
//! # Format behaviour
//!
//! | Format | Streaming | Item shape |
//! |--------|-----------|-----------|
//! | CSV    | ✓ row-by-row | `Value::Map` keyed by header column names, read as upstream's `CsvLoader` does (`delimited.rs`); text unless hinted numeric |
//! | TSV    | ✓ row-by-row | same as CSV |
//! | JSONL  | ✓ line-by-line | one `Value` per line |
//! | JSON   | whole-file | array → one `Value` per element; object → single `Value` |
//! | YAML   | whole-file | PyYAML `safe_load` semantics (YAML 1.1 scalars); array → one `Value` per element |
//!
//! [`load_stream`] does no type coercion. The schema-aware entry points accept
//! precomputed numeric column hints, which lets CSV/TSV inputs coerce exactly
//! the numeric-ranged columns, as upstream does through its source schema
//! (and consequently preserves e.g. ZIP codes and enum values).

use std::collections::{HashSet, VecDeque};
use std::path::Path;

use anyhow::{Context, Result};
use futures::stream::{self, BoxStream, StreamExt};
use linkml_map_core::pyyaml::LoadError;
use linkml_map_core::value::Value;
use tokio::fs::File;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

use crate::delimited::{CsvReader, DictReader, NewlineTranslator};
use crate::format::Format;
use crate::yaml_load::safe_load_all;

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
        Format::Csv => Ok(delimited_stream(path, ',', NumericColumnHints::new())),
        Format::Tsv => Ok(delimited_stream(path, '\t', NumericColumnHints::new())),
        Format::Jsonl => jsonl_stream(path).await,
        Format::Json => json_stream(path).await,
        Format::Yaml => yaml_stream(path).await,
        Format::Parquet | Format::DuckDb => {
            anyhow::bail!("{} is an output-only format", format.extension())
        }
    }
}

/// Names of the columns upstream coerces to numbers: the slots of the source
/// class whose range is (a subtype of) `integer`, `float`, `double` or
/// `decimal` (`data_loaders._numeric_slots_for`).
///
/// Callers build this once from their already-loaded source schema, then reuse
/// it for every row. This avoids reparsing a source schema for each input file.
/// Every named column goes through `_parse_numeric` (`int`, then `float`, else
/// the text), whichever numeric range it declares; every other column is text.
pub type NumericColumnHints = HashSet<String>;

/// Like [`load_stream`], but applies precomputed source-schema numeric hints to
/// CSV and TSV columns. Other formats are unchanged.
pub async fn load_stream_with_numeric_hints(
    path: impl AsRef<Path>,
    format: Format,
    numeric_hints: NumericColumnHints,
) -> Result<BoxStream<'static, Result<Value>>> {
    let path = path.as_ref().to_owned();
    match format {
        Format::Csv => Ok(delimited_stream(path, ',', numeric_hints)),
        Format::Tsv => Ok(delimited_stream(path, '\t', numeric_hints)),
        Format::Jsonl => jsonl_stream(path).await,
        Format::Json => json_stream(path).await,
        Format::Yaml => yaml_stream(path).await,
        Format::Parquet | Format::DuckDb => {
            anyhow::bail!("{} is an output-only format", format.extension())
        }
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

/// [`load_stream_with_numeric_hints`] (no hints) for a table upstream reads with DuckDB's
/// `read_csv_auto(..., all_varchar=true)`: the join engine's primary table and every joined table. A CSV/TSV
/// file keeps the spaces at the start of its fields and loses a leading byte-order mark; the other formats
/// are read as usual.
pub async fn load_stream_duckdb(
    path: impl AsRef<Path>,
    format: Format,
) -> Result<BoxStream<'static, Result<Value>>> {
    let path = path.as_ref().to_owned();
    match format {
        Format::Csv => Ok(delimited_stream_with(
            path,
            ',',
            NumericColumnHints::new(),
            true,
        )),
        Format::Tsv => Ok(delimited_stream_with(
            path,
            '\t',
            NumericColumnHints::new(),
            true,
        )),
        other => load_stream_with_numeric_hints(path, other, NumericColumnHints::new()).await,
    }
}

/// Collect all values from [`load_stream_duckdb`].
pub async fn load_all_duckdb(path: impl AsRef<Path>, format: Format) -> Result<Vec<Value>> {
    let mut stream = load_stream_duckdb(path, format).await?;
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

/// Bytes read from the file per round trip.
const CHUNK_BYTES: usize = 64 * 1024;

/// Streaming state for [`delimited_stream`].
struct DelimitedState {
    path: std::path::PathBuf,
    file: Option<File>,
    /// Undecoded bytes: the tail of the previous chunk that ends mid-character.
    pending: Vec<u8>,
    newlines: NewlineTranslator,
    reader: CsvReader,
    dict: DictReader,
    ready: VecDeque<Value>,
    done: bool,
    /// DuckDB's reader drops a leading byte-order mark.
    strip_bom: bool,
}

impl DelimitedState {
    /// Read and parse one more chunk, queueing any finished rows.
    async fn fill(&mut self) -> Result<()> {
        if self.file.is_none() {
            let file = File::open(&self.path)
                .await
                .with_context(|| format!("failed to open {:?}", self.path))?;
            self.file = Some(file);
        }
        let Some(file) = self.file.as_mut() else {
            return Ok(());
        };
        let mut chunk = vec![0u8; CHUNK_BYTES];
        let n = file.read(&mut chunk).await?;
        let at_eof = n == 0;
        self.pending.extend_from_slice(&chunk[..n]);

        // `open(source)` decodes strictly; a multi-byte character may straddle
        // two chunks.
        let valid = match std::str::from_utf8(&self.pending) {
            Ok(text) => text.len(),
            Err(e) if e.error_len().is_none() && !at_eof => e.valid_up_to(),
            Err(e) => anyhow::bail!(
                "{:?}: UnicodeDecodeError: invalid utf-8 sequence at byte {}",
                self.path,
                e.valid_up_to()
            ),
        };
        let mut text = String::with_capacity(valid + 1);
        let mut decoded = std::str::from_utf8(&self.pending[..valid])?;
        if self.strip_bom && !decoded.is_empty() {
            decoded = decoded.strip_prefix('\u{feff}').unwrap_or(decoded);
            self.strip_bom = false;
        }
        self.newlines.translate(decoded, &mut text);
        self.pending.drain(..valid);
        if at_eof {
            self.newlines.finish(&mut text);
        }

        for c in text.chars() {
            self.reader.feed(c)?;
            if c == '\n'
                && let Some(record) = self.reader.end_of_line()
            {
                self.ready.extend(self.dict.push_record(record));
            }
        }
        if at_eof {
            if let Some(record) = self.reader.finish() {
                self.ready.extend(self.dict.push_record(record));
            }
            self.done = true;
        }
        Ok(())
    }
}

/// Stream the rows of a CSV/TSV file exactly as upstream's `TsvLoader` /
/// `CsvLoader` yield them (see `delimited.rs`). The file is opened and
/// read lazily, one chunk at a time.
///
/// `numeric_hints` is upstream's `_numeric_slots` for a schema-aware load: only
/// those columns are coerced.
fn delimited_stream(
    path: std::path::PathBuf,
    delimiter: char,
    numeric_hints: NumericColumnHints,
) -> BoxStream<'static, Result<Value>> {
    delimited_stream_with(path, delimiter, numeric_hints, false)
}

/// [`delimited_stream`], optionally as DuckDB's `read_csv_auto(..., all_varchar=true)` reads the file: a
/// leading byte-order mark is dropped and spaces at the start of a field are kept.
fn delimited_stream_with(
    path: std::path::PathBuf,
    delimiter: char,
    numeric_hints: NumericColumnHints,
    duckdb: bool,
) -> BoxStream<'static, Result<Value>> {
    let reader = CsvReader::new(delimiter);
    let state = DelimitedState {
        path,
        file: None,
        pending: Vec::new(),
        newlines: NewlineTranslator::default(),
        reader: if duckdb {
            reader.without_initial_space_skipping()
        } else {
            reader
        },
        dict: DictReader::new(Some(numeric_hints)),
        ready: VecDeque::new(),
        done: false,
        strip_bom: duckdb,
    };
    stream::unfold(state, |mut state| async move {
        loop {
            if let Some(row) = state.ready.pop_front() {
                return Some((Ok(row), state));
            }
            if state.done {
                return None;
            }
            if let Err(e) = state.fill().await {
                // The error ends the stream: nothing further can be read.
                state.done = true;
                state.ready.clear();
                return Some((Err(e), state));
            }
        }
    })
    .boxed()
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
                    let val: Result<Value> = linkml_map_core::value::parse_json(&trimmed)
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

    let val = linkml_map_core::value::parse_json_bytes(&bytes).context("JSON parse error")?;

    let items = values_of(val);
    Ok(stream::iter(items.into_iter().map(Ok)).boxed())
}

// ─── YAML (whole-file) ────────────────────────────────────────────────────────

/// Read a file as text the way `open(path)` does: strict UTF-8.
async fn read_text(path: &Path) -> Result<String> {
    let bytes = tokio::fs::read(path)
        .await
        .with_context(|| format!("opening {path:?}"))?;
    String::from_utf8(bytes).with_context(|| format!("{path:?}: UnicodeDecodeError"))
}

/// The documents of a YAML text as `yaml.safe_load` builds them (see
/// [`crate::yaml_load`]). One document is the data itself; a multi-document
/// stream (what `YamlStreamWriter` emits: one document per record, each
/// followed by `---`) yields every document in turn, and the empty document
/// after a trailing `---` is not a record.
///
/// PARITY (extension): `yaml.safe_load` rejects a multi-document stream
/// (`ComposerError: expected a single document`); reading one back is useful
/// for round trips, and no valid upstream input is affected.
fn yaml_documents(text: &str) -> std::result::Result<Vec<Value>, LoadError> {
    let mut docs = safe_load_all(text)?;
    if docs.len() > 1 {
        while docs.len() > 1 && docs.last().is_some_and(Value::is_null) {
            docs.pop();
        }
    }
    Ok(match docs.len() {
        0 => vec![Value::Null],
        1 => values_of(docs.remove(0)),
        _ => docs,
    })
}

async fn yaml_stream(path: impl AsRef<Path>) -> Result<BoxStream<'static, Result<Value>>> {
    let path = path.as_ref();
    let text = read_text(path).await?;
    let items = yaml_documents(&text).map_err(|e| anyhow::anyhow!("{path:?}: {e}"))?;
    Ok(stream::iter(items.into_iter().map(Ok)).boxed())
}

// ─── Single-object input (`map-data FILE.yaml|FILE.json`) ─────────────────────

/// Open a `.yaml` / `.json` file the way `cli.py::_map_data_single` does:
/// `yaml.safe_load` first, then `json.loads` when PyYAML raises a
/// `yaml.YAMLError`.
///
/// JSON therefore goes through the YAML 1.1 loader whenever PyYAML accepts it,
/// which changes some values: `1e5` and `1E+5` (an exponent with no `.`) are
/// the *strings* `"1e5"`, `"1E+5"`; `0123`-style octals cannot occur in JSON.
/// A plain Python exception from the constructors (an impossible date such as
/// `2024-13-45`) is *not* a `YAMLError` and ends the run.
///
/// Directory and tabular input does not take this path: `DataLoader` reads
/// `.json` with `json.load` and `.yaml` with `yaml.safe_load`
/// ([`load_stream_with_numeric_hints`]).
pub async fn load_stream_single_object(
    path: impl AsRef<Path>,
    format: Format,
) -> Result<BoxStream<'static, Result<Value>>> {
    let path = path.as_ref();
    if !matches!(format, Format::Yaml | Format::Json) {
        return load_stream_with_numeric_hints(path, format, NumericColumnHints::new()).await;
    }
    let text = read_text(path).await?;
    let items = match yaml_documents(&text) {
        Ok(items) => items,
        Err(LoadError::Value(msg)) => anyhow::bail!("{path:?}: {msg}"),
        Err(LoadError::Yaml(yaml_error)) => {
            let json = linkml_map_core::value::parse_json(&text).map_err(|json_error| {
                anyhow::anyhow!("{path:?}: {yaml_error}; as JSON: {json_error}")
            })?;
            values_of(json)
        }
    };
    Ok(stream::iter(items.into_iter().map(Ok)).boxed())
}

/// Collect [`load_stream_single_object`].
pub async fn load_all_single_object(path: impl AsRef<Path>, format: Format) -> Result<Vec<Value>> {
    let mut stream = load_stream_single_object(path, format).await?;
    let mut out = Vec::new();
    while let Some(item) = stream.next().await {
        out.push(item?);
    }
    Ok(out)
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// A loaded document as items: arrays are expanded, anything else is one item.
fn values_of(v: Value) -> Vec<Value> {
    match v {
        Value::List(items) => items,
        other => vec![other],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn duckdb_csv_keeps_leading_spaces_and_drops_the_bom() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.csv");
        std::fs::write(&path, "\u{feff}id,note\na,  x \n").unwrap();
        let rows = load_all_duckdb(&path, Format::Csv).await.unwrap();
        let Value::Map(row) = &rows[0] else {
            panic!("map")
        };
        assert_eq!(row["id"], Value::Str("a".into()));
        assert_eq!(row["note"], Value::Str("  x ".into()));
        // upstream's own loader keeps the BOM in the header and skips the spaces
        let rows = load_all(&path, Format::Csv).await.unwrap();
        let Value::Map(row) = &rows[0] else {
            panic!("map")
        };
        assert_eq!(row["note"], Value::Str("x ".into()));
        assert!(row.contains_key("\u{feff}id"));
    }
}
