//! Async streaming writers that consume [`Value`] items and write to a file.
//!
//! This module mirrors upstream `linkml_map/writers/output_streams.py` (the
//! `StreamWriter` family and `MultiStreamWriter`) and the non-streaming
//! `linkml_map/cli/cli.py::dump_output`.
//!
//! # Format behaviour (streaming)
//!
//! | Format | Layout |
//! |--------|--------|
//! | JSONL  | one `_strip_nulls`-ed object per line, Python `json.dumps` spacing |
//! | JSON   | `[\n`, objects pretty-printed (indent 2) and joined by `,\n`, `\n]\n` |
//! | YAML   | one document per object (`---\n` after each), keys **sorted**, nulls dropped |
//! | CSV/TSV | nested dicts flattened with `__`; header = union of keys; the file is rewritten when later rows add columns |
//!
//! JSON, JSONL and YAML drop `null`-valued object members at every depth (list
//! elements that are `null` are kept); CSV/TSV render `null` as an empty cell.
//!
//! # Single-object mode
//!
//! Upstream's `map-data` only streams for tabular or directory input; a single
//! YAML/JSON object goes through `dump_output` instead.  Set
//! [`WriteOptions::single_object`] to mirror that: the whole result is buffered
//! and rendered by [`dump_output`] (one object → a mapping, several → a list;
//! YAML keeps key order and drops nulls *and* empty containers).
//!
//! The writers consume an async `Stream<Item = Result<Value>>` (or a
//! `Vec<Value>` via the [`write_vec`] helper).

use std::collections::HashSet;
use std::io::{BufRead, Write as _};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use futures::{Stream, StreamExt};
use linkml_map_core::value::Value;
use tokio::fs::File;
use tokio::io::{AsyncWrite, AsyncWriteExt, BufWriter};

use crate::format::Format;
use crate::pycompat::{
    dumps_json, flatten, py_str, sort_keys, strip_empty, strip_nulls, tabular_cell,
};

/// Records handed to a [`StreamWriter`] per `write_chunk` call (upstream
/// `--chunk-size` default).
pub const DEFAULT_CHUNK_SIZE: usize = 1000;

// ─── Options ─────────────────────────────────────────────────────────────────

/// Options for [`write_all_with`] / [`write_all_writer_with`]; mirrors the
/// `map-data` flags that influence how output is written.
#[derive(Debug, Clone, Default)]
pub struct WriteOptions {
    /// `--table-name`: table for DuckDB output (defaults to the output file's
    /// stem).  Applied to every DuckDB writer, including `-O` outputs; ignored
    /// by every other format.
    pub table_name: Option<String>,
    /// `--chunk-size`: records per `write_chunk` call.  `None` uses
    /// [`DEFAULT_CHUNK_SIZE`].
    pub chunk_size: Option<usize>,
    /// `-O/--additional-output`: further files that receive the same records;
    /// the format of each is inferred from its extension.
    pub additional_outputs: Vec<PathBuf>,
    /// Render with upstream's non-streaming `dump_output` instead of the stream
    /// writers (see the module docs).  Additional outputs are ignored in this
    /// mode, as upstream's `_map_data_single` takes no `-O`.
    pub single_object: bool,
}

// ─── StreamWriter trait ──────────────────────────────────────────────────────

/// A chunk-based writer: converts chunks of objects into text fragments.
///
/// Mirrors upstream `StreamWriter` (`write_chunk` / `finalize` /
/// `staging_path` / `finalize_path`).
pub trait StreamWriter: Send {
    /// Emit formatted text for a single chunk of objects.
    fn write_chunk(&mut self, chunk: &[Value]) -> Result<String>;

    /// Emit any trailing text after all chunks have been processed.
    fn finalize(&mut self) -> Result<String> {
        Ok(String::new())
    }

    /// Path this writer's text should go to instead of `target`.
    ///
    /// Writers whose final artifact is not the text they emit (a binary
    /// columnar file) stage the text elsewhere and convert it in
    /// [`finalize_path`](Self::finalize_path).
    fn staging_path(&self, _target: &Path) -> Option<PathBuf> {
        None
    }

    /// Post-process a file target once every fragment has been written.
    fn finalize_path(&mut self, _target: &Path, _staged: Option<&Path>) -> Result<()> {
        Ok(())
    }
}

/// Mirror of `make_stream_writer`: the writer for *format*.
pub fn make_stream_writer(
    format: Format,
    table_name: Option<&str>,
) -> Result<Box<dyn StreamWriter>> {
    // `table_name` only matters for DuckDB output; every other format ignores it.
    let _ = table_name;
    Ok(match format {
        Format::Json => Box::new(JsonStreamWriter::new()),
        Format::Jsonl => Box::new(JsonlStreamWriter),
        Format::Yaml => Box::new(YamlStreamWriter),
        Format::Tsv => Box::new(TabularStreamWriter::new('\t')),
        Format::Csv => Box::new(TabularStreamWriter::new(',')),
    })
}

// ─── JSON array ──────────────────────────────────────────────────────────────

/// Mirror of `JSONStreamWriter`: a JSON array emitted incrementally.
#[derive(Debug)]
pub struct JsonStreamWriter {
    started: bool,
    first_object: bool,
}

impl JsonStreamWriter {
    /// A writer that has not emitted its opening bracket yet.
    pub fn new() -> Self {
        Self {
            started: false,
            first_object: true,
        }
    }

    fn emit_preamble(&mut self, out: &mut String) {
        if !self.started {
            self.started = true;
            out.push_str("[\n");
        }
    }
}

impl Default for JsonStreamWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamWriter for JsonStreamWriter {
    fn write_chunk(&mut self, chunk: &[Value]) -> Result<String> {
        let mut out = String::new();
        self.emit_preamble(&mut out);
        for obj in chunk {
            if !self.first_object {
                out.push_str(",\n");
            }
            self.first_object = false;
            // PARITY: output_streams.py::JSONStreamWriter.write_chunk — `_strip_nulls`
            // then `dumps_json(indent=2)`.
            out.push_str(&dumps_json(obj, Some(2), true));
        }
        Ok(out)
    }

    fn finalize(&mut self) -> Result<String> {
        let mut out = String::new();
        self.emit_preamble(&mut out);
        out.push_str("\n]\n");
        Ok(out)
    }
}

// ─── JSONL ───────────────────────────────────────────────────────────────────

/// Mirror of `JSONLStreamWriter`: one JSON object per line.
#[derive(Debug, Default)]
pub struct JsonlStreamWriter;

impl StreamWriter for JsonlStreamWriter {
    fn write_chunk(&mut self, chunk: &[Value]) -> Result<String> {
        let mut out = String::new();
        for obj in chunk {
            out.push_str(&jsonl_line(obj));
            out.push('\n');
        }
        Ok(out)
    }
}

/// One JSONL record without its newline: `dumps_json(_strip_nulls(obj))`.
///
/// Shared with the pipeline's JSONL fast paths so they stay identical to the
/// generic writer.
pub fn jsonl_line(obj: &Value) -> String {
    dumps_json(obj, None, true)
}

// ─── YAML ────────────────────────────────────────────────────────────────────

/// Mirror of `YAMLStreamWriter` without `key_name` (the CLI never sets one):
/// each object is its own YAML document, followed by `---\n`.
#[derive(Debug, Default)]
pub struct YamlStreamWriter;

impl StreamWriter for YamlStreamWriter {
    fn write_chunk(&mut self, chunk: &[Value]) -> Result<String> {
        let mut out = String::new();
        for obj in chunk {
            // PARITY: output_streams.py::YAMLStreamWriter.write_chunk —
            // `dump_yaml(_strip_nulls(obj))`; `dump_yaml` is `yaml.dump` with its
            // default `sort_keys=True`, so stream YAML is key-sorted (unlike
            // `dump_output`, which passes `sort_keys=False`).
            out.push_str(&dump_yaml(&sort_keys(&strip_nulls(obj)))?);
            out.push_str("---\n");
        }
        Ok(out)
    }
}

/// `yaml.dump(obj, default_flow_style=False, allow_unicode=True)`.
fn dump_yaml(value: &Value) -> Result<String> {
    serde_yaml_ng::to_string(value).context("serialising YAML")
}

// ─── CSV / TSV ───────────────────────────────────────────────────────────────

/// Mirror of `TabularStreamWriter`: TSV/CSV with dynamic header handling.
///
/// Nested objects are flattened with `__`.  The header row is written when the
/// first object arrives; columns that appear later are appended to the running
/// header and, for file targets, the file is rewritten at the end with the full
/// header and padded rows ([`finalize_path`](StreamWriter::finalize_path)).
#[derive(Debug)]
pub struct TabularStreamWriter {
    separator: char,
    headers: Vec<String>,
    header_set: HashSet<String>,
    initial_headers: Vec<String>,
    headers_changed: bool,
}

impl TabularStreamWriter {
    /// A writer with the given column separator (`'\t'` for TSV, `','` for CSV).
    pub fn new(separator: char) -> Self {
        Self {
            separator,
            headers: Vec::new(),
            header_set: HashSet::new(),
            initial_headers: Vec::new(),
            headers_changed: false,
        }
    }

    /// Whether later rows introduced columns after the header was written.
    pub fn headers_changed(&self) -> bool {
        self.headers_changed
    }

    /// The final set of headers after streaming.
    pub fn final_headers(&self) -> &[String] {
        &self.headers
    }

    /// Mirror of `_escape_value`: quote values holding the separator, `"` or a newline.
    fn escape_value(&self, value: &Value) -> String {
        let text = tabular_cell(value);
        if text.contains(self.separator) || text.contains('"') || text.contains('\n') {
            format!("\"{}\"", text.replace('"', "\"\""))
        } else {
            text
        }
    }
}

impl StreamWriter for TabularStreamWriter {
    fn write_chunk(&mut self, chunk: &[Value]) -> Result<String> {
        let mut out = String::new();
        let sep = self.separator.to_string();
        for obj in chunk {
            let flat = flatten(obj, "__")
                .with_context(|| format!("CSV/TSV writer expects object records, got {obj:?}"))?;

            // Track new headers.
            for key in flat.keys() {
                if self.header_set.insert(key.clone()) {
                    self.headers.push(key.clone());
                }
            }

            // Emit the header row on the first object.
            if self.initial_headers.is_empty() {
                self.initial_headers = self.headers.clone();
                out.push_str(&self.headers.join(&sep));
                out.push('\n');
            }

            // Emit the data row.
            let row: Vec<String> = self
                .headers
                .iter()
                .map(|h| {
                    flat.get(h)
                        .map_or_else(String::new, |v| self.escape_value(v))
                })
                .collect();
            out.push_str(&row.join(&sep));
            out.push('\n');
        }
        Ok(out)
    }

    fn finalize(&mut self) -> Result<String> {
        if self.headers != self.initial_headers {
            self.headers_changed = true;
        }
        Ok(String::new())
    }

    fn finalize_path(&mut self, target: &Path, _staged: Option<&Path>) -> Result<()> {
        if !self.headers_changed {
            return Ok(());
        }
        log::info!("Rewriting {} with updated headers", target.display());
        let mut tmp_name = target.as_os_str().to_owned();
        tmp_name.push(".tmp");
        let tmp_path = PathBuf::from(tmp_name);
        {
            let src = std::fs::File::open(target)
                .with_context(|| format!("reopening {}", target.display()))?;
            let mut dst = std::io::BufWriter::new(
                std::fs::File::create(&tmp_path)
                    .with_context(|| format!("creating {}", tmp_path.display()))?,
            );
            rewrite_header_and_pad(
                std::io::BufReader::new(src),
                &self.headers,
                self.separator,
                &mut dst,
            )?;
            dst.flush()?;
        }
        std::fs::rename(&tmp_path, target)
            .with_context(|| format!("replacing {}", target.display()))?;
        Ok(())
    }
}

/// Mirror of `rewrite_header_and_pad`: copy a delimited file, replacing its
/// header row with *final_headers* and padding short rows with empty fields.
///
/// Records are parsed so quoted fields holding the separator or newlines survive
/// (upstream uses the `csv` module; fields are re-quoted with `QUOTE_MINIMAL`).
pub fn rewrite_header_and_pad(
    mut lines: impl BufRead,
    final_headers: &[String],
    separator: char,
    out: &mut impl std::io::Write,
) -> Result<()> {
    write_csv_row(out, final_headers.iter().map(String::as_str), separator)?;

    let mut first = true;
    let mut record = String::new();
    let mut line = String::new();
    loop {
        line.clear();
        if lines.read_line(&mut line)? == 0 {
            break;
        }
        record.push_str(&line);
        // A quoted field may span several physical lines.
        if record.matches('"').count() % 2 == 1 {
            continue;
        }
        let text = record.trim_end_matches(['\n', '\r']);
        if first {
            // Skip the original header.
            first = false;
        } else {
            let mut fields = split_csv_record(text, separator);
            if fields.len() < final_headers.len() {
                fields.resize(final_headers.len(), String::new());
            }
            write_csv_row(out, fields.iter().map(String::as_str), separator)?;
        }
        record.clear();
    }
    Ok(())
}

/// Split one logical record into fields, honouring `"`-quoting with `""` escapes.
fn split_csv_record(record: &str, separator: char) -> Vec<String> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut chars = record.chars().peekable();
    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    field.push('"');
                    chars.next();
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(c);
            }
        } else if c == '"' {
            in_quotes = true;
        } else if c == separator {
            fields.push(std::mem::take(&mut field));
        } else {
            field.push(c);
        }
    }
    fields.push(field);
    fields
}

/// `csv.writer(..., lineterminator="\n").writerow(fields)` with `QUOTE_MINIMAL`.
fn write_csv_row<'a>(
    out: &mut impl std::io::Write,
    fields: impl Iterator<Item = &'a str>,
    separator: char,
) -> Result<()> {
    let mut first = true;
    for field in fields {
        if !first {
            write!(out, "{separator}")?;
        }
        first = false;
        if field.contains([separator, '"', '\n', '\r']) {
            write!(out, "\"{}\"", field.replace('"', "\"\""))?;
        } else {
            write!(out, "{field}")?;
        }
    }
    writeln!(out)?;
    Ok(())
}

// ─── Fan-out driver (MultiStreamWriter) ──────────────────────────────────────

type BoxedSink<'a> = Box<dyn AsyncWrite + Send + Unpin + 'a>;

/// Where a [`StreamWriter`]'s text goes.
pub enum Target<'a> {
    /// A file: created (or truncated) by the driver and eligible for
    /// [`finalize_path`](StreamWriter::finalize_path) post-processing.
    Path(PathBuf),
    /// A caller-owned sink (stdout, a socket, an in-memory buffer): written to
    /// directly, flushed but never rewritten.
    Writer(BoxedSink<'a>),
}

/// Mirror of `MultiStreamWriter`: fans the same chunks out to several
/// `(StreamWriter, Target)` pairs.
pub struct MultiStreamWriter<'a> {
    outputs: Vec<(Box<dyn StreamWriter>, Target<'a>)>,
}

impl<'a> MultiStreamWriter<'a> {
    /// A driver over the given outputs.
    pub fn new(outputs: Vec<(Box<dyn StreamWriter>, Target<'a>)>) -> Self {
        Self { outputs }
    }

    /// Consume *stream* once, writing records to every output in chunks of
    /// *chunk_size*.
    pub async fn write_all<S>(self, chunk_size: usize, mut stream: S) -> Result<()>
    where
        S: Stream<Item = Result<Value>> + Unpin,
    {
        if chunk_size == 0 {
            // PARITY: upstream's `more_itertools.chunked(it, 0)` yields no chunks, so
            // `--chunk-size 0` silently writes an empty result.  Failing loudly instead
            // is a deliberate divergence (suggested upstream issue).
            bail!("--chunk-size must be at least 1");
        }
        let mut writers: Vec<Box<dyn StreamWriter>> = Vec::new();
        let mut sinks: Vec<BufWriter<BoxedSink<'a>>> = Vec::new();
        let mut finish: Vec<Option<(PathBuf, Option<PathBuf>)>> = Vec::new();

        for (writer, target) in self.outputs {
            match target {
                Target::Path(path) => {
                    // A writer whose artifact isn't the text it emits streams to a
                    // staging file instead, and builds the real artifact in
                    // `finalize_path`.
                    let staged = writer.staging_path(&path);
                    let open_at = staged.as_deref().unwrap_or(&path);
                    let file = File::create(open_at)
                        .await
                        .with_context(|| format!("creating {open_at:?}"))?;
                    sinks.push(BufWriter::new(Box::new(file) as BoxedSink<'a>));
                    finish.push(Some((path, staged)));
                }
                Target::Writer(sink) => {
                    sinks.push(BufWriter::new(sink));
                    finish.push(None);
                }
            }
            writers.push(writer);
        }

        let mut chunk: Vec<Value> = Vec::with_capacity(chunk_size.min(DEFAULT_CHUNK_SIZE));
        while let Some(item) = stream.next().await {
            chunk.push(item?);
            if chunk.len() >= chunk_size {
                fan_out(&mut writers, &mut sinks, &chunk).await?;
                chunk.clear();
            }
        }
        if !chunk.is_empty() {
            fan_out(&mut writers, &mut sinks, &chunk).await?;
        }

        // Finalise each writer, then close every sink.
        for (writer, sink) in writers.iter_mut().zip(sinks.iter_mut()) {
            let fragment = writer.finalize()?;
            sink.write_all(fragment.as_bytes()).await?;
        }
        for sink in &mut sinks {
            sink.flush().await.context("flushing output")?;
        }
        drop(sinks);

        // Let each writer build or repair its own file artifact.
        for (mut writer, target) in writers.into_iter().zip(finish) {
            let Some((path, staged)) = target else {
                continue;
            };
            tokio::task::spawn_blocking(move || writer.finalize_path(&path, staged.as_deref()))
                .await
                .context("finalising output file")??;
        }
        Ok(())
    }
}

async fn fan_out(
    writers: &mut [Box<dyn StreamWriter>],
    sinks: &mut [BufWriter<BoxedSink<'_>>],
    chunk: &[Value],
) -> Result<()> {
    for (writer, sink) in writers.iter_mut().zip(sinks.iter_mut()) {
        let fragment = writer.write_chunk(chunk)?;
        sink.write_all(fragment.as_bytes()).await?;
    }
    Ok(())
}

// ─── Single-object output (cli.py::dump_output) ──────────────────────────────

/// Mirror of `cli.dump_output` for a transformed object (or list of them).
///
/// * YAML: `dump_yaml(_strip_empty(data), sort_keys=False)`.
/// * JSON: `dumps_json(_strip_nulls(data), indent=2) + "\n"`.
/// * JSONL: one line per list item (or one line for an object).
/// * CSV/TSV: flattened rows, header = union of keys in first-seen order,
///   cells rendered with Python's `str()` and **not** quoted.
pub fn dump_output(data: &Value, format: Format) -> Result<String> {
    Ok(match format {
        Format::Yaml => dump_yaml(&strip_empty(data))?,
        Format::Json => format!("{}\n", dumps_json(data, Some(2), true)),
        Format::Jsonl => match data {
            Value::List(items) => {
                let lines: Vec<String> = items.iter().map(jsonl_line).collect();
                format!("{}\n", lines.join("\n"))
            }
            other => format!("{}\n", jsonl_line(other)),
        },
        Format::Csv => dump_delimited(data, ",")?,
        Format::Tsv => dump_delimited(data, "\t")?,
    })
}

fn dump_delimited(data: &Value, separator: &str) -> Result<String> {
    let records: Vec<&Value> = match data {
        Value::List(items) => items.iter().collect(),
        other => vec![other],
    };
    let rows = records
        .into_iter()
        .map(|r| flatten(r, "__").with_context(|| format!("cannot flatten {r:?}")))
        .collect::<Result<Vec<_>>>()?;
    if rows.is_empty() {
        return Ok(String::new());
    }
    let mut headers: Vec<&String> = rows[0].keys().collect();
    let mut seen: HashSet<&String> = headers.iter().copied().collect();
    for row in &rows[1..] {
        for key in row.keys() {
            if seen.insert(key) {
                headers.push(key);
            }
        }
    }
    let mut lines = vec![
        headers
            .iter()
            .map(|h| h.as_str())
            .collect::<Vec<_>>()
            .join(separator),
    ];
    for row in &rows {
        // PARITY: cli.py::dump_output — cells are `str(c)` with no quoting, and a
        // present-but-null value renders as the text `None` (a missing key is empty).
        let cells: Vec<String> = headers
            .iter()
            .map(|h| row.get(*h).map_or_else(String::new, |v| py_str(v)))
            .collect();
        lines.push(cells.join(separator));
    }
    Ok(format!("{}\n", lines.join("\n")))
}

/// The value `dump_output` receives for buffered results: a lone object as
/// itself, otherwise the list.
fn single_object_value(mut items: Vec<Value>) -> Value {
    if items.len() == 1 {
        items.remove(0)
    } else {
        Value::List(items)
    }
}

// ─── Public API ──────────────────────────────────────────────────────────────

/// Write every item from *stream* into *path* using *format*.
///
/// The file is created (or truncated) before writing begins.
pub async fn write_all<S>(path: impl AsRef<Path>, format: Format, stream: S) -> Result<()>
where
    S: Stream<Item = Result<Value>> + Unpin,
{
    write_all_with(path, format, &WriteOptions::default(), stream).await
}

/// [`write_all`] with explicit [`WriteOptions`] (table name, chunk size,
/// additional outputs, single-object mode).
pub async fn write_all_with<S>(
    path: impl AsRef<Path>,
    format: Format,
    options: &WriteOptions,
    stream: S,
) -> Result<()>
where
    S: Stream<Item = Result<Value>> + Unpin,
{
    let path = path.as_ref();
    if options.single_object {
        let file = File::create(path)
            .await
            .with_context(|| format!("creating {path:?}"))?;
        let mut sink: BoxedSink<'_> = Box::new(file);
        return write_single_object(&mut sink, format, stream).await;
    }
    let mut outputs: Vec<(Box<dyn StreamWriter>, Target<'_>)> = vec![(
        make_stream_writer(format, options.table_name.as_deref())?,
        Target::Path(path.to_owned()),
    )];
    outputs.extend(additional_outputs(options)?);
    MultiStreamWriter::new(outputs)
        .write_all(options.chunk_size.unwrap_or(DEFAULT_CHUNK_SIZE), stream)
        .await
}

/// Write every item from *stream* into *writer* using *format*.
///
/// The caller owns the writer — this function buffers it internally and
/// flushes before returning.  Use this instead of [`write_all`] when the
/// destination is not a filesystem path (e.g. stdout, a network socket, or
/// an in-memory buffer).
pub async fn write_all_writer<W, S>(writer: W, format: Format, stream: S) -> Result<()>
where
    W: AsyncWrite + Send + Unpin,
    S: Stream<Item = Result<Value>> + Unpin,
{
    write_all_writer_with(writer, format, &WriteOptions::default(), stream).await
}

/// [`write_all_writer`] with explicit [`WriteOptions`].  Additional outputs are
/// still written to their own files.
pub async fn write_all_writer_with<W, S>(
    writer: W,
    format: Format,
    options: &WriteOptions,
    stream: S,
) -> Result<()>
where
    W: AsyncWrite + Send + Unpin,
    S: Stream<Item = Result<Value>> + Unpin,
{
    if options.single_object {
        let mut sink: BoxedSink<'_> = Box::new(writer);
        return write_single_object(&mut sink, format, stream).await;
    }
    let mut outputs: Vec<(Box<dyn StreamWriter>, Target<'_>)> = vec![(
        make_stream_writer(format, options.table_name.as_deref())?,
        Target::Writer(Box::new(writer)),
    )];
    outputs.extend(additional_outputs(options)?);
    MultiStreamWriter::new(outputs)
        .write_all(options.chunk_size.unwrap_or(DEFAULT_CHUNK_SIZE), stream)
        .await
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

/// Build the `-O` outputs of *options*: one `(writer, path)` per extra file,
/// the format inferred from the extension (mirror of `_build_additional_outputs`).
fn additional_outputs(
    options: &WriteOptions,
) -> Result<Vec<(Box<dyn StreamWriter>, Target<'static>)>> {
    options
        .additional_outputs
        .iter()
        .map(|path| {
            let format = Format::from_path(path).map_err(|_| {
                let ext = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| format!(".{}", e.to_ascii_lowercase()))
                    .unwrap_or_default();
                anyhow::anyhow!("Cannot infer output format from extension: {ext}")
            })?;
            Ok((
                make_stream_writer(format, options.table_name.as_deref())?,
                Target::Path(path.clone()),
            ))
        })
        .collect()
}

/// Reject `-O` paths that duplicate the primary output (upstream
/// `_map_data_streaming`: "Primary output path duplicated in -O").
///
/// `primary` is `None` when the primary output is stdout.
pub fn validate_additional_outputs(primary: Option<&Path>, options: &WriteOptions) -> Result<()> {
    // Surface unknown extensions before any output is opened.
    additional_outputs(options)?;
    if let Some(primary) = primary {
        let resolved = resolve(primary);
        if options
            .additional_outputs
            .iter()
            .any(|p| resolve(p) == resolved)
        {
            bail!(
                "Primary output path duplicated in -O: {}",
                primary.display()
            );
        }
    }
    Ok(())
}

/// `Path.resolve()` for paths that may not exist yet.
fn resolve(path: &Path) -> PathBuf {
    if let Ok(p) = path.canonicalize() {
        return p;
    }
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_owned());
    match (
        abs.parent().and_then(|p| p.canonicalize().ok()),
        abs.file_name(),
    ) {
        (Some(parent), Some(name)) => parent.join(name),
        _ => abs,
    }
}

async fn write_single_object<S>(
    sink: &mut BoxedSink<'_>,
    format: Format,
    mut stream: S,
) -> Result<()>
where
    S: Stream<Item = Result<Value>> + Unpin,
{
    let mut items = Vec::new();
    while let Some(item) = stream.next().await {
        items.push(item?);
    }
    let text = dump_output(&single_object_value(items), format)?;
    sink.write_all(text.as_bytes()).await?;
    sink.flush().await.context("flushing output")?;
    Ok(())
}

// ─── Value → serde_json::Value ────────────────────────────────────────────────

/// Losslessly convert a [`Value`] to `serde_json::Value` for serialisation.
pub fn value_to_json(v: &Value) -> Result<serde_json::Value> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn v(json: &str) -> Value {
        serde_json::from_str(json).unwrap()
    }

    fn stream_of(rows: Vec<Value>) -> futures::stream::Iter<std::vec::IntoIter<Result<Value>>> {
        futures::stream::iter(rows.into_iter().map(Ok).collect::<Vec<_>>())
    }

    async fn render(format: Format, options: &WriteOptions, rows: Vec<Value>) -> String {
        let mut buf: Vec<u8> = Vec::new();
        write_all_writer_with(&mut buf, format, options, stream_of(rows))
            .await
            .unwrap();
        String::from_utf8(buf).unwrap()
    }

    /// Upstream `NULL_ROW` from `test_writers_drop_null_slots`.
    fn null_row() -> Value {
        v(r#"{"id": "p1", "n": null, "q": {"unit": "mg", "value": null}, "tags": ["a", null]}"#)
    }

    fn stripped_row() -> Value {
        v(r#"{"id": "p1", "q": {"unit": "mg"}, "tags": ["a", null]}"#)
    }

    // Port of upstream `test_writers_drop_null_slots` (YAML, JSON, JSONL without key_name).
    #[tokio::test]
    async fn yaml_stream_drops_null_slots() {
        let text = render(Format::Yaml, &WriteOptions::default(), vec![null_row()]).await;
        let doc: serde_json::Value =
            serde_yaml_ng::from_str(text.trim_end_matches("---\n")).unwrap();
        assert_eq!(Value::from(&doc), stripped_row());
    }

    #[tokio::test]
    async fn json_stream_drops_null_slots() {
        let text = render(Format::Json, &WriteOptions::default(), vec![null_row()]).await;
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(Value::from(&parsed), Value::List(vec![stripped_row()]));
    }

    #[tokio::test]
    async fn jsonl_stream_drops_null_slots() {
        let text = render(Format::Jsonl, &WriteOptions::default(), vec![null_row()]).await;
        assert_eq!(
            text,
            "{\"id\": \"p1\", \"q\": {\"unit\": \"mg\"}, \"tags\": [\"a\", null]}\n"
        );
    }

    /// A dict emptied by stripping stays as `{}`; an all-null object is `{}`.
    #[tokio::test]
    async fn stripping_keeps_emptied_containers() {
        let text = render(
            Format::Jsonl,
            &WriteOptions::default(),
            vec![v(r#"{"a": {"b": null}}"#), v(r#"{"a": null}"#)],
        )
        .await;
        assert_eq!(text, "{\"a\": {}}\n{}\n");
    }

    #[tokio::test]
    async fn json_stream_layout() {
        let rows = vec![v(r#"{"b": 1, "a": [1, {"c": null}]}"#), v(r#"{"b": 2}"#)];
        let text = render(Format::Json, &WriteOptions::default(), rows).await;
        assert_eq!(
            text,
            "[\n{\n  \"b\": 1,\n  \"a\": [\n    1,\n    {}\n  ]\n},\n{\n  \"b\": 2\n}\n]\n"
        );
        // An empty stream still yields an (empty) array, as upstream does.
        assert_eq!(
            render(Format::Json, &WriteOptions::default(), vec![]).await,
            "[\n\n]\n"
        );
    }

    /// Upstream stream YAML: multi-document, `---` after each, keys sorted.
    #[tokio::test]
    async fn yaml_stream_layout_is_sorted_multi_document() {
        let rows = vec![
            v(r#"{"id": "a", "age": 3, "n": {"z": 1, "b": 2}}"#),
            v(r#"{"id": "b"}"#),
        ];
        let text = render(Format::Yaml, &WriteOptions::default(), rows).await;
        assert_eq!(text, "age: 3\nid: a\nn:\n  b: 2\n  z: 1\n---\nid: b\n---\n");
        assert_eq!(
            render(Format::Yaml, &WriteOptions::default(), vec![]).await,
            ""
        );
    }

    /// Upstream `dump_output` YAML: insertion order, nulls and empty containers dropped,
    /// no document markers.
    #[tokio::test]
    async fn single_object_yaml_keeps_order_and_strips_empty() {
        let options = WriteOptions {
            single_object: true,
            ..WriteOptions::default()
        };
        let row = v(
            r#"{"id": "a", "z": null, "e": [], "m": {"k": null}, "b": [{"x": 1}, {}], "age": 3}"#,
        );
        let text = render(Format::Yaml, &options, vec![row]).await;
        assert_eq!(text, "id: a\nb:\n- x: 1\nage: 3\n");
    }

    #[tokio::test]
    async fn single_object_json_is_an_object_not_an_array() {
        let options = WriteOptions {
            single_object: true,
            ..WriteOptions::default()
        };
        let text = render(Format::Json, &options, vec![v(r#"{"id": "a", "n": null}"#)]).await;
        assert_eq!(text, "{\n  \"id\": \"a\"\n}\n");
        let text = render(
            Format::Json,
            &options,
            vec![v(r#"{"id": "a"}"#), v(r#"{"id": "b"}"#)],
        )
        .await;
        assert_eq!(
            text,
            "[\n  {\n    \"id\": \"a\"\n  },\n  {\n    \"id\": \"b\"\n  }\n]\n"
        );
    }

    #[tokio::test]
    async fn single_object_delimited_follows_dump_output() {
        let options = WriteOptions {
            single_object: true,
            ..WriteOptions::default()
        };
        let row =
            v(r#"{"id": "a", "q": {"u": "mg"}, "n": null, "ok": true, "t": ["x"], "f": 2.0}"#);
        let text = render(Format::Tsv, &options, vec![row]).await;
        // `str()` cells, no quoting, a present null is the text `None`.
        assert_eq!(
            text,
            "id\tq__u\tn\tok\tt\tf\na\tmg\tNone\tTrue\t['x']\t2.0\n"
        );
    }

    /// CSV/TSV: header is the union of keys; for files the header is rewritten.
    #[tokio::test]
    async fn tabular_file_rewrites_header_when_columns_appear_late() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.tsv");
        let rows = vec![
            v(r#"{"id": "a", "q": {"u": "mg"}}"#),
            v(r#"{"id": "b", "extra": "x\ty", "q": {"u": "g"}}"#),
        ];
        write_vec(&path, Format::Tsv, rows).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "id\tq__u\textra\na\tmg\t\nb\tg\t\"x\ty\"\n"
        );
        assert!(!dir.path().join("out.tsv.tmp").exists());
    }

    /// A caller-owned sink is never rewritten, as upstream.
    #[tokio::test]
    async fn tabular_stream_to_writer_keeps_first_row_header() {
        let rows = vec![v(r#"{"id": "a"}"#), v(r#"{"id": "b", "late": 1}"#)];
        let text = render(Format::Csv, &WriteOptions::default(), rows).await;
        assert_eq!(text, "id\na\nb,1\n");
    }

    #[tokio::test]
    async fn tabular_rejects_non_object_records() {
        let mut buf: Vec<u8> = Vec::new();
        let err = write_all_writer(&mut buf, Format::Csv, stream_of(vec![Value::Int(1)]))
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("expects object records"),
            "{err:#}"
        );
    }

    #[tokio::test]
    async fn chunk_size_does_not_change_output() {
        let rows: Vec<Value> = (0..7)
            .map(|i| v(&format!(r#"{{"id": {i}, "s": "x"}}"#)))
            .collect();
        for format in [Format::Json, Format::Jsonl, Format::Yaml, Format::Csv] {
            let one = render(
                format,
                &WriteOptions {
                    chunk_size: Some(1),
                    ..WriteOptions::default()
                },
                rows.clone(),
            )
            .await;
            let all = render(format, &WriteOptions::default(), rows.clone()).await;
            assert_eq!(one, all, "{format:?}");
        }
    }

    #[tokio::test]
    async fn chunk_size_zero_is_rejected() {
        let mut buf: Vec<u8> = Vec::new();
        let options = WriteOptions {
            chunk_size: Some(0),
            ..WriteOptions::default()
        };
        let err = write_all_writer_with(&mut buf, Format::Jsonl, &options, stream_of(vec![]))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("--chunk-size"), "{err:#}");
    }

    /// `-O`: every output receives the same records in its own format.
    #[tokio::test]
    async fn additional_outputs_receive_the_same_records() {
        let dir = tempfile::tempdir().unwrap();
        let primary = dir.path().join("out.jsonl");
        let extra_tsv = dir.path().join("extra.tsv");
        let extra_json = dir.path().join("extra.JSON");
        let options = WriteOptions {
            additional_outputs: vec![extra_tsv.clone(), extra_json.clone()],
            ..WriteOptions::default()
        };
        let rows = vec![v(r#"{"id": "a", "n": null}"#), v(r#"{"id": "b", "n": 1}"#)];
        write_all_with(&primary, Format::Jsonl, &options, stream_of(rows))
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(&primary).unwrap(),
            "{\"id\": \"a\"}\n{\"id\": \"b\", \"n\": 1}\n"
        );
        assert_eq!(
            std::fs::read_to_string(&extra_tsv).unwrap(),
            "id\tn\na\t\nb\t1\n"
        );
        let parsed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&extra_json).unwrap()).unwrap();
        assert_eq!(parsed.as_array().unwrap().len(), 2);
    }

    #[test]
    fn additional_output_validation() {
        let dir = tempfile::tempdir().unwrap();
        let primary = dir.path().join("out.jsonl");
        let dup = WriteOptions {
            additional_outputs: vec![dir.path().join("./out.jsonl")],
            ..WriteOptions::default()
        };
        let err = validate_additional_outputs(Some(&primary), &dup).unwrap_err();
        assert!(
            err.to_string()
                .starts_with("Primary output path duplicated in -O")
        );

        let bad = WriteOptions {
            additional_outputs: vec![dir.path().join("out.xyz")],
            ..WriteOptions::default()
        };
        let err = validate_additional_outputs(Some(&primary), &bad).unwrap_err();
        assert_eq!(
            err.to_string(),
            "Cannot infer output format from extension: .xyz"
        );
        validate_additional_outputs(None, &WriteOptions::default()).unwrap();
    }

    #[test]
    fn rewrite_header_and_pad_handles_quoted_newlines() {
        let input = "a\tb\n\"x\ny\"\tz\nonly\n";
        let mut out = Vec::new();
        rewrite_header_and_pad(
            input.as_bytes(),
            &["a".into(), "b".into(), "c".into()],
            '\t',
            &mut out,
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "a\tb\tc\n\"x\ny\"\tz\t\nonly\t\t\n"
        );
    }
}
