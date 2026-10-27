//! Delimited (CSV / TSV) input, ported from upstream's loader stack.
//!
//! Upstream reads tabular input through
//! `linkml.validator.loaders.delimited_file_loader._DelimitedFileLoader`
//! (`TsvLoader` / `CsvLoader`, wrapped by `linkml_map.loaders.data_loaders`),
//! which is built from three pieces of the Python standard library and a little
//! linkml code. Each is ported here under the same names:
//!
//! * `open(source)`: text mode with *universal newlines* (`\r\n` and a lone `\r`
//!   both read as `\n`) and strict UTF-8 decoding. A byte-order mark is **not**
//!   stripped, so it ends up in the first header name.
//! * `csv.reader(..., delimiter=..., skipinitialspace=True)`: the state machine
//!   in CPython's `Modules/_csv.c` ([`CsvReader`]).
//! * `csv.DictReader`: the first record names the fields, blank records are
//!   skipped, short records are padded with `None` and long records spill into
//!   a list under the key `None` ([`DictReader`]).
//! * `_DelimitedFileLoader._rows`: with `skip_empty_rows`, a row whose values
//!   are all falsy is dropped; cells equal to `""` are dropped; the rest go
//!   through `_coerce_value` / `_parse_numeric` ([`parse_numeric`]).
//!
//! PARITY: a *short* row (fewer cells than the header, as in sparse tables) does
//! not lose the missing columns: `DictReader` fills them with `restval=None`,
//! `v != ""` keeps them, and the row carries `None` (here [`Value::Null`]) for
//! each. An empty cell inside the row is dropped, a missing one is `None`.

use std::collections::HashSet;

use indexmap::IndexMap;
use linkml_map_core::decimal::Decimal;
use linkml_map_core::value::Value;

/// `csv.field_size_limit()`'s default; a longer field raises `csv.Error`.
const FIELD_SIZE_LIMIT: usize = 131_072;

/// `_csv.c` reader states (`ParserState`). Escape-character states are omitted:
/// the dialect has no `escapechar`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    StartRecord,
    StartField,
    InField,
    InQuotedField,
    QuoteInQuotedField,
    EatCrnl,
}

/// The `csv.reader` parser for the dialect upstream uses: `delimiter` as given,
/// `quotechar='"'`, `doublequote=True`, `skipinitialspace=True`,
/// `quoting=QUOTE_MINIMAL`, `strict=False`, no `escapechar`.
///
/// Characters are fed one at a time with [`CsvReader::feed`]; a record is
/// complete at the end of every *line* (`\n`, after newline translation) unless
/// the parser is inside a quoted field. [`CsvReader::end_of_line`] plays the
/// role of CPython's `EOL` sentinel and returns the finished record, if any.
#[derive(Debug)]
pub(crate) struct CsvReader {
    delimiter: char,
    state: State,
    field: String,
    fields: Vec<String>,
    /// Characters have been fed since the last [`CsvReader::end_of_line`].
    dirty: bool,
}

impl CsvReader {
    pub(crate) fn new(delimiter: char) -> Self {
        Self {
            delimiter,
            state: State::StartRecord,
            field: String::new(),
            fields: Vec::new(),
            dirty: false,
        }
    }

    fn save_field(&mut self) {
        self.fields.push(std::mem::take(&mut self.field));
    }

    fn add_char(&mut self, c: char) -> Result<(), CsvError> {
        if self.field.len() >= FIELD_SIZE_LIMIT {
            return Err(CsvError::FieldTooLarge);
        }
        self.field.push(c);
        Ok(())
    }

    /// `parse_process_char` for an ordinary character (the input was already
    /// newline-translated, so `\r` never reaches here).
    pub(crate) fn feed(&mut self, c: char) -> Result<(), CsvError> {
        self.dirty = true;
        match self.state {
            State::StartRecord => {
                if c == '\n' {
                    self.state = State::EatCrnl;
                    return Ok(());
                }
                self.state = State::StartField;
                self.start_field(c)
            }
            State::StartField => self.start_field(c),
            State::InField => {
                if c == '\n' {
                    self.save_field();
                    self.state = State::EatCrnl;
                } else if c == self.delimiter {
                    self.save_field();
                    self.state = State::StartField;
                } else {
                    self.add_char(c)?;
                }
                Ok(())
            }
            State::InQuotedField => {
                if c == '"' {
                    // `doublequote`: `""` stands for `"`.
                    self.state = State::QuoteInQuotedField;
                    Ok(())
                } else {
                    self.add_char(c)
                }
            }
            State::QuoteInQuotedField => {
                if c == '"' {
                    self.add_char('"')?;
                    self.state = State::InQuotedField;
                } else if c == self.delimiter {
                    self.save_field();
                    self.state = State::StartField;
                } else if c == '\n' {
                    self.save_field();
                    self.state = State::EatCrnl;
                } else {
                    // `strict=False`: a stray character after the closing quote
                    // is kept and the field continues unquoted.
                    self.add_char(c)?;
                    self.state = State::InField;
                }
                Ok(())
            }
            // Only reached on a character after the newline that ends a line;
            // lines are split at `\n`, so the next character belongs to a new
            // record and `end_of_line` has already reset the state.
            State::EatCrnl => unreachable!("EatCrnl is left by end_of_line"),
        }
    }

    /// `START_FIELD` handling (also the fall-through from `START_RECORD`).
    fn start_field(&mut self, c: char) -> Result<(), CsvError> {
        if c == '\n' {
            self.save_field();
            self.state = State::EatCrnl;
        } else if c == '"' {
            self.state = State::InQuotedField;
        } else if c == ' ' {
            // `skipinitialspace`: ignore space at the start of a field.
        } else if c == self.delimiter {
            self.save_field();
        } else {
            self.add_char(c)?;
            self.state = State::InField;
        }
        Ok(())
    }

    /// The `EOL` sentinel CPython feeds after the last character of each line.
    /// Returns the finished record, or `None` while a quoted field spans lines.
    pub(crate) fn end_of_line(&mut self) -> Option<Vec<String>> {
        self.dirty = false;
        match self.state {
            State::StartRecord | State::EatCrnl => {}
            State::StartField | State::InField | State::QuoteInQuotedField => self.save_field(),
            // Inside a quoted field the newline is data; wait for the next line.
            State::InQuotedField => return None,
        }
        self.state = State::StartRecord;
        Some(std::mem::take(&mut self.fields))
    }

    /// End of input. A last line without a newline still ends with the `EOL`
    /// sentinel; input that stops inside a quoted field emits the half-read
    /// record (`csv.reader.__next__`, `strict=False`).
    pub(crate) fn finish(&mut self) -> Option<Vec<String>> {
        if self.state == State::InQuotedField {
            self.save_field();
            self.state = State::StartRecord;
            self.dirty = false;
            return Some(std::mem::take(&mut self.fields));
        }
        if self.dirty {
            return self.end_of_line();
        }
        None
    }
}

/// A `csv.Error` that aborts the read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CsvError {
    FieldTooLarge,
}

impl std::fmt::Display for CsvError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CsvError::FieldTooLarge => {
                write!(f, "field larger than field limit ({FIELD_SIZE_LIMIT})")
            }
        }
    }
}

impl std::error::Error for CsvError {}

/// Universal-newline translation for text read with `open(source)`: `\r\n` and
/// `\r` become `\n`. A `\r` at the end of a chunk is held back (`pending_cr`)
/// until the next chunk shows whether a `\n` follows.
#[derive(Debug, Default)]
pub(crate) struct NewlineTranslator {
    pending_cr: bool,
}

impl NewlineTranslator {
    /// Translate `chunk`, appending to `out`.
    pub(crate) fn translate(&mut self, chunk: &str, out: &mut String) {
        for c in chunk.chars() {
            if self.pending_cr {
                self.pending_cr = false;
                out.push('\n');
                if c == '\n' {
                    continue;
                }
            }
            if c == '\r' {
                self.pending_cr = true;
            } else {
                out.push(c);
            }
        }
    }

    /// Flush a trailing `\r` at end of input.
    pub(crate) fn finish(&mut self, out: &mut String) {
        if std::mem::take(&mut self.pending_cr) {
            out.push('\n');
        }
    }
}

/// `csv.DictReader` plus `_DelimitedFileLoader._rows`, turning records into
/// row values.
#[derive(Debug)]
pub(crate) struct DictReader {
    /// `fieldnames`: `None` until the first record has been read.
    fieldnames: Option<Vec<String>>,
    numeric: Option<HashSet<String>>,
}

impl DictReader {
    /// `numeric_slots`: `Some(names)` coerces only those columns (a schema was
    /// given); `None` coerces every column (`_numeric_slots is None`).
    pub(crate) fn new(numeric_slots: Option<HashSet<String>>) -> Self {
        Self {
            fieldnames: None,
            numeric: numeric_slots,
        }
    }

    /// Consume one `csv.reader` record. Returns the row to yield, if any: the
    /// first record only names the fields.
    pub(crate) fn push_record(&mut self, record: Vec<String>) -> Option<Value> {
        let Some(fieldnames) = &self.fieldnames else {
            self.fieldnames = Some(record);
            return None;
        };
        // DictReader: "we prefer not to return blanks".
        if record.is_empty() {
            return None;
        }
        // `d = dict(zip(self.fieldnames, row))`, then `restval` for missing
        // trailing fields, `restkey` (`None`) for surplus ones.
        let mut d: IndexMap<&str, Option<&str>> = IndexMap::new();
        for (name, cell) in fieldnames.iter().zip(&record) {
            d.insert(name, Some(cell));
        }
        let has_rest = record.len() > fieldnames.len();
        if fieldnames.len() > record.len() {
            for name in &fieldnames[record.len()..] {
                d.insert(name, None);
            }
        }
        // `skip_empty_rows and not any(row.values())`; the `None`-keyed list of
        // surplus cells is truthy whenever it exists.
        if !has_rest && d.values().all(|v| v.is_none_or(str::is_empty)) {
            return None;
        }
        let mut row = IndexMap::with_capacity(d.len());
        for (key, cell) in d {
            match cell {
                // `v != ""`
                Some("") => {}
                Some(text) => {
                    let coerced = if self.numeric.as_ref().is_none_or(|n| n.contains(key)) {
                        parse_numeric(text)
                    } else {
                        Value::Str(text.to_owned())
                    };
                    row.insert(key.to_owned(), coerced);
                }
                None => {
                    row.insert(key.to_owned(), Value::Null);
                }
            }
        }
        Some(Value::Map(row))
    }
}

/// Python's whitespace for `str.strip()` / `int()` / `float()`.
fn is_py_space(c: char) -> bool {
    c.is_whitespace() || matches!(c, '\u{1c}'..='\u{1f}')
}

/// Remove the underscores `int()` / `float()` allow between digits; `None`
/// when one is misplaced (leading, trailing, doubled, or next to `.`/`e`/sign).
fn strip_digit_underscores(text: &str) -> Option<String> {
    if !text.contains('_') {
        return Some(text.to_owned());
    }
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    for (i, &c) in chars.iter().enumerate() {
        if c == '_' {
            let digit_before = i > 0 && chars[i - 1].is_ascii_digit();
            let digit_after = chars.get(i + 1).is_some_and(char::is_ascii_digit);
            if !(digit_before && digit_after) {
                return None;
            }
        } else {
            out.push(c);
        }
    }
    Some(out)
}

/// `int(text)`: surrounding whitespace, an optional sign, decimal digits with
/// single underscores between them. Integers beyond `i64` become an exact
/// [`Value::Decimal`] (Python's `int` is unbounded and [`Value`] has no big
/// integer; a decimal with exponent zero prints identically).
fn py_int(text: &str) -> Option<Value> {
    let trimmed = text.trim_matches(is_py_space);
    let digits = strip_digit_underscores(trimmed)?;
    let body = digits.strip_prefix(['+', '-']).unwrap_or(&digits);
    if body.is_empty() || !body.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    match digits.parse::<i64>() {
        Ok(i) => Some(Value::Int(i)),
        Err(_) => {
            let normalised = digits.strip_prefix('+').unwrap_or(&digits);
            Decimal::parse_python(normalised).ok().map(Value::Decimal)
        }
    }
}

/// `float(text)`: surrounding whitespace, optional sign, digits with `.` and an
/// exponent, underscores between digits; `inf`/`nan` spellings are accepted too
/// (but never reach here, see [`parse_numeric`]).
fn py_float(text: &str) -> Option<f64> {
    let trimmed = text.trim_matches(is_py_space);
    let cleaned = strip_digit_underscores(trimmed)?;
    // Rust's parser also takes `inf`, `infinity` and `nan` (any case), like
    // Python's, and rejects hexadecimal and other spellings Python rejects.
    cleaned.parse::<f64>().ok()
}

/// `_parse_numeric` from linkml's `delimited_file_loader.py`: a value with no
/// ASCII digit stays text; otherwise `int(value)`, then `float(value)`, then the
/// text unchanged. The declared range does not matter: an integer-looking cell
/// in a `float` slot stays an `int`, and `2.5` in an `integer` slot is a float.
pub(crate) fn parse_numeric(text: &str) -> Value {
    if !text.bytes().any(|b| b.is_ascii_digit()) {
        return Value::Str(text.to_owned());
    }
    if let Some(v) = py_int(text) {
        return v;
    }
    if let Some(f) = py_float(text) {
        return Value::Float(f);
    }
    Value::Str(text.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn records(delimiter: char, text: &str) -> Vec<Vec<String>> {
        let mut translated = String::new();
        let mut nl = NewlineTranslator::default();
        nl.translate(text, &mut translated);
        nl.finish(&mut translated);
        let mut reader = CsvReader::new(delimiter);
        let mut out = Vec::new();
        for c in translated.chars() {
            reader.feed(c).unwrap();
            if c == '\n'
                && let Some(rec) = reader.end_of_line()
            {
                out.push(rec);
            }
        }
        out.extend(reader.finish());
        out
    }

    fn row(text: &[&str]) -> Vec<String> {
        text.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn skipinitialspace_and_quotes() {
        assert_eq!(
            records(',', "a, b,  \"c, d\" ,e\n"),
            vec![row(&["a", "b", "c, d ", "e"])]
        );
        assert_eq!(
            records(',', "x,\"a\"\"b\"\n"),
            vec![row(&["x", "a\"b"])],
            "doubled quote is one quote"
        );
        // strict=False: stray text after a closing quote is appended.
        assert_eq!(records(',', "\"ab\"cd,e\n"), vec![row(&["abcd", "e"])]);
        // A quote in the middle of an unquoted field is literal.
        assert_eq!(records(',', "a\"b,c\n"), vec![row(&["a\"b", "c"])]);
    }

    #[test]
    fn blank_lines_are_empty_records_and_spaces_one_empty_field() {
        assert_eq!(
            records('\t', "a\tb\n\n  \nx\ty"),
            vec![row(&["a", "b"]), row(&[]), row(&[""]), row(&["x", "y"])]
        );
    }

    #[test]
    fn quoted_newlines_and_universal_newlines() {
        assert_eq!(
            records(',', "a,\"x\r\ny\"\r\nb,c\rd,e"),
            vec![row(&["a", "x\ny"]), row(&["b", "c"]), row(&["d", "e"])]
        );
        // Input that stops inside a quoted field still yields the record.
        let mut r = CsvReader::new(',');
        for c in "a,\"unterminated".chars() {
            r.feed(c).unwrap();
        }
        assert_eq!(r.finish(), Some(row(&["a", "unterminated"])));
    }

    #[test]
    fn newline_translator_spans_chunks() {
        let mut t = NewlineTranslator::default();
        let mut out = String::new();
        t.translate("a\r", &mut out);
        t.translate("\nb\r", &mut out);
        t.finish(&mut out);
        assert_eq!(out, "a\nb\n");
    }

    #[test]
    fn parse_numeric_follows_python() {
        let cases: &[(&str, Value)] = &[
            ("2", Value::Int(2)),
            ("+7", Value::Int(7)),
            ("-0", Value::Int(0)),
            ("007", Value::Int(7)),
            (" 12 ", Value::Int(12)),
            ("1_000", Value::Int(1000)),
            ("2.5", Value::Float(2.5)),
            ("1e3", Value::Float(1000.0)),
            ("1_0.5", Value::Float(10.5)),
            (".5", Value::Float(0.5)),
            ("5.", Value::Float(5.0)),
            ("1e999", Value::Float(f64::INFINITY)),
        ];
        for (text, want) in cases {
            assert_eq!(&parse_numeric(text), want, "{text:?}");
        }
        for text in [
            "", "abc", "inf", "nan", "-", "1,000", "0x10", "1__0", "_1", "1_", "1_.5", "1.2.3",
            "12abc", "1 2", "--1", "e5", "1e",
        ] {
            assert_eq!(parse_numeric(text), Value::Str(text.to_owned()), "{text:?}");
        }
        // Beyond i64: Python's unbounded int, held as an exact decimal.
        assert!(matches!(
            parse_numeric("123456789012345678901234567890"),
            Value::Decimal(_)
        ));
    }

    #[test]
    fn dict_reader_pads_short_rows_with_none_and_drops_empty_cells() {
        let mut dr = DictReader::new(None);
        assert!(dr.push_record(row(&["id", "a", "b"])).is_none());
        // Empty cell dropped; missing trailing cell is None (Null).
        let Some(Value::Map(m)) = dr.push_record(row(&["1", ""])) else {
            panic!("row expected");
        };
        assert_eq!(m.get("id"), Some(&Value::Int(1)));
        assert!(!m.contains_key("a"));
        assert_eq!(m.get("b"), Some(&Value::Null));
        // All-empty rows and blank records are skipped; surplus cells make a
        // row non-empty even when every cell is empty.
        assert!(dr.push_record(row(&["", "", ""])).is_none());
        assert!(dr.push_record(row(&[])).is_none());
        assert_eq!(
            dr.push_record(row(&["", "", "", ""])),
            Some(Value::Map(IndexMap::new()))
        );
    }

    #[test]
    fn dict_reader_duplicate_headers_keep_first_position_last_value() {
        let mut dr = DictReader::new(Some(HashSet::new()));
        dr.push_record(row(&["a", "b", "a"]));
        let Some(Value::Map(m)) = dr.push_record(row(&["1", "2", "3"])) else {
            panic!("row expected");
        };
        let items: Vec<_> = m.iter().collect();
        assert_eq!(items[0], (&"a".to_owned(), &Value::Str("3".into())));
        assert_eq!(items[1], (&"b".to_owned(), &Value::Str("2".into())));
        // A short row overwrites the duplicate with None (DictReader quirk).
        let Some(Value::Map(m)) = dr.push_record(row(&["1", "2"])) else {
            panic!("row expected");
        };
        assert_eq!(m.get("a"), Some(&Value::Null));
    }
}
