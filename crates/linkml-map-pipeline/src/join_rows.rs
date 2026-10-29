//! The primary rows of upstream's set-based join engine.
//!
//! Port of the value half of `transformer/join_engine.py::transform_block_via_join`.
//! A block with `joins:` over CSV, TSV or JSON tables does not read its primary
//! table through the loader: DuckDB reads it (every CSV/TSV column as text, every
//! JSON column cast to `VARCHAR`) and `_parse_numeric` is then run over *every*
//! cell of every row, whatever the schema says about the column:
//!
//! * a CSV/TSV cell `"007"` reads as `7`, `"1e3"` as `1000.0` (the loader would
//!   keep text for a column that is not numeric in the schema); `read_csv_auto` is
//!   called with `all_varchar=true`, so DuckDB infers no types for CSV at all;
//! * a JSON column has the one type `read_json_auto` infers for all its values
//!   (see [`detect_type`]): `BOOLEAN`, `BIGINT`/`HUGEINT` (all integers print as
//!   digits), `DOUBLE`, `VARCHAR`, `DATE`, `TIME`, `TIMESTAMP`, `UUID`, a `LIST` or
//!   `STRUCT`, or `JSON` for a mix of types.  Its cells are the `VARCHAR` casts of
//!   that: a boolean is the text `true`, an integer in a column that also holds a
//!   float is `3.0`, a string in a mixed column keeps its JSON quotes (`"x"`), a
//!   list is `[a, 'b,c']`, `2020-01-01T10:00:00Z` in a timestamp column reads back
//!   as `2020-01-01 10:00:00`, an integer beyond 64 bits that DuckDB reads as a
//!   `DOUBLE` (below `-2^63` or above `2^64 - 1`) reads back as a float;
//! * a key missing from some JSON rows is a `NULL` cell.
//!
//! Not modelled: `MAP` inference (a struct of 200 or more similar fields, or an
//! object whose fields mostly appear in a tenth of the sampled records), numbers
//! inside a `JSON`-typed column are printed from the parsed value rather than from
//! the file's own spelling (`2.50` reads as `2.5` in a list in a mixed column), the
//! last line of a newline-delimited file sampled on its own, and what DuckDB does
//! with a value after the first 20480 records that does not fit the type inferred
//! from them beyond the common cases (an error in both).

mod casts;
mod duckdb_text;

use std::sync::OnceLock;

use anyhow::Result;
use linkml_map_core::{pyfmt::py_float_repr, value::Value};
use linkml_map_io::parse_numeric;

use duckdb_text::{StrpTimeFormat, date_formats, date_text, time_text, timestamp_formats};

/// `JSONReaderOptions::sample_size`: records sampled for type detection.
const SAMPLE_SIZE: usize = 20480;
/// `STANDARD_VECTOR_SIZE`: records per scan, and so per refinement of the candidate types.
const VECTOR_SIZE: usize = 2048;

/// A CSV/TSV row read as text: `_parse_numeric` over every cell.
pub(crate) fn parse_text_row(row: Value) -> Value {
    match row {
        Value::Map(cells) => Value::Map(
            cells
                .into_iter()
                .map(|(k, v)| match v {
                    Value::Str(text) => (k, parse_numeric(&text)),
                    other => (k, other),
                })
                .collect(),
        ),
        other => other,
    }
}

// ─── structure detection (`extension/json/json_functions/json_structure.cpp`) ────────────────

/// `LogicalTypeId` of a `JSONStructureDescription`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Null,
    Bool,
    /// A negative integer (`YYJSON_SUBTYPE_SINT`).
    Bigint,
    /// A non-negative integer (`YYJSON_SUBTYPE_UINT`).
    Ubigint,
    Hugeint,
    Double,
    Varchar,
    List,
    Struct,
}

/// A string type `InitializeCandidateTypes` offers for a `VARCHAR` description.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Candidate {
    Uuid,
    Timestamp,
    Date,
    Time,
}

/// `JSONStructureDescription`.
#[derive(Debug)]
struct Description {
    kind: Kind,
    /// An integer above `i64::MAX` was seen (`JSONStructureDescription::has_large_ubigint`).
    has_large_ubigint: bool,
    /// A struct's fields, or a list's one element node.
    children: Vec<Node>,
    candidates: Vec<Candidate>,
}

/// `JSONStructureNode`.
#[derive(Debug)]
struct Node {
    key: String,
    initialized: bool,
    descriptions: Vec<Description>,
    count: usize,
    null_count: usize,
}

impl Node {
    fn new(key: &str) -> Self {
        Self {
            key: key.to_owned(),
            initialized: false,
            descriptions: Vec::new(),
            count: 0,
            null_count: 0,
        }
    }

    /// `JSONStructureNode::GetOrCreateDescription`: the index of the description for `kind`.
    fn description(&mut self, kind: Kind) -> usize {
        let new = |kind| Description {
            kind,
            has_large_ubigint: false,
            children: Vec::new(),
            candidates: Vec::new(),
        };
        if self.descriptions.is_empty() {
            self.descriptions.push(new(kind));
            return 0;
        }
        if self.descriptions.len() == 1 && self.descriptions[0].kind == Kind::Null {
            // only a NULL in there, override
            self.descriptions[0].kind = kind;
            return 0;
        }
        if kind == Kind::Null {
            return self.descriptions.len() - 1;
        }
        for (i, d) in self.descriptions.iter_mut().enumerate() {
            if d.kind == kind {
                return i;
            }
            if is_numeric(kind) && is_numeric(d.kind) {
                d.kind = max_numeric_type(kind, d.kind);
                return i;
            }
        }
        self.descriptions.push(new(kind));
        self.descriptions.len() - 1
    }

    /// `JSONStructure::ExtractStructure`.
    fn extract(&mut self, val: &Value) {
        self.count += 1;
        match val {
            Value::Null => {
                self.null_count += 1;
                self.description(Kind::Null);
            }
            Value::List(items) | Value::Tuple(items) => {
                let i = self.description(Kind::List);
                let d = &mut self.descriptions[i];
                if d.children.is_empty() {
                    d.children.push(Node::new(""));
                }
                for item in items {
                    d.children[0].extract(item);
                }
            }
            Value::Map(fields) => {
                let i = self.description(Kind::Struct);
                let d = &mut self.descriptions[i];
                for (at, (k, v)) in fields.iter().enumerate() {
                    // columns usually come in the same order in every record
                    let found = if d.children.get(at).is_some_and(|c| c.key == *k) {
                        Some(at)
                    } else {
                        d.children.iter().position(|c| c.key == *k)
                    };
                    let child = match found {
                        Some(j) => j,
                        None => {
                            d.children.push(Node::new(k));
                            d.children.len() - 1
                        }
                    };
                    d.children[child].extract(v);
                }
            }
            scalar => {
                let kind = scalar_kind(scalar);
                let i = self.description(kind);
                // `ExtractStructureVal`
                if kind == Kind::Ubigint && !matches!(scalar, Value::Int(_)) {
                    self.descriptions[i].has_large_ubigint = true;
                }
            }
        }
    }

    /// `JSONStructureNode::ContainsVarchar`.
    fn contains_varchar(&self) -> bool {
        let [d] = self.descriptions.as_slice() else {
            return false;
        };
        d.kind == Kind::Varchar || d.children.iter().any(Self::contains_varchar)
    }

    /// `JSONStructureNode::InitializeCandidateTypes` (no `convert_strings_to_integers`, no user formats).
    fn initialize_candidates(&mut self) {
        let [d] = self.descriptions.as_mut_slice() else {
            return;
        };
        if d.kind == Kind::Varchar && !self.initialized {
            d.candidates = vec![
                Candidate::Uuid,
                Candidate::Timestamp,
                Candidate::Date,
                Candidate::Time,
            ];
            self.initialized = true;
        } else {
            for child in &mut d.children {
                child.initialize_candidates();
            }
        }
    }

    /// `JSONStructureNode::RefineCandidateTypes` over the values of one scan vector (`None`: the key is
    /// absent from that record).
    fn refine_candidates(&mut self, vals: &[Option<&Value>]) {
        if self.descriptions.len() != 1 || !self.contains_varchar() {
            return;
        }
        let d = &mut self.descriptions[0];
        match d.kind {
            Kind::List => {
                let flat: Vec<Option<&Value>> = vals
                    .iter()
                    .flat_map(|v| match v {
                        Some(Value::List(items) | Value::Tuple(items)) => items.as_slice(),
                        _ => &[],
                    })
                    .map(Some)
                    .collect();
                d.children[0].refine_candidates(&flat);
            }
            Kind::Struct => {
                for child in &mut d.children {
                    let col: Vec<Option<&Value>> = vals
                        .iter()
                        .map(|v| match v {
                            Some(Value::Map(m)) => m.get(&child.key),
                            _ => None,
                        })
                        .collect();
                    child.refine_candidates(&col);
                }
            }
            Kind::Varchar => {
                if d.candidates.is_empty() {
                    return;
                }
                let strings = vals.iter().filter_map(|v| match v {
                    Some(Value::Str(s)) => Some(s.as_bytes()),
                    _ => None,
                });
                eliminate_candidate_types(&mut d.candidates, strings);
            }
            _ => {}
        }
    }
}

/// `IsNumeric`.
fn is_numeric(kind: Kind) -> bool {
    matches!(
        kind,
        Kind::Double | Kind::Ubigint | Kind::Bigint | Kind::Hugeint
    )
}

/// `MaxNumericType` of two different numeric kinds.
fn max_numeric_type(a: Kind, b: Kind) -> Kind {
    if a == Kind::Double || b == Kind::Double {
        Kind::Double
    } else {
        // `HUGEINT`, or a `BIGINT` and a `UBIGINT`, which need `HUGEINT` for both ranges
        Kind::Hugeint
    }
}

/// `scalar_kind` of a non-container value: `JSONCommon::ValTypeToLogicalTypeId`.  An integer outside
/// `[-2^63, 2^64 - 1]` and a float beyond `f64` are `YYJSON_TYPE_RAW`, which DuckDB reads as a `DOUBLE`.
fn scalar_kind(v: &Value) -> Kind {
    match v {
        Value::Bool(_) => Kind::Bool,
        Value::Int(i) if *i < 0 => Kind::Bigint,
        Value::Int(_) => Kind::Ubigint,
        // only `2^63..2^64` reaches here as an integer; the rest is raw, which DuckDB reads as a `DOUBLE`
        Value::BigInt(n) => match n.to_string().parse::<i128>() {
            Ok(i) if i > i128::from(i64::MAX) && i <= i128::from(u64::MAX) => Kind::Ubigint,
            Ok(i) if i < 0 && i >= i128::from(i64::MIN) => Kind::Bigint,
            Ok(i) if (0..=i128::from(i64::MAX)).contains(&i) => Kind::Ubigint,
            _ => Kind::Double,
        },
        Value::Float(_) => Kind::Double,
        // not from JSON text
        _ => Kind::Varchar,
    }
}

fn date_format_list() -> &'static [StrpTimeFormat] {
    static FORMATS: OnceLock<Vec<StrpTimeFormat>> = OnceLock::new();
    FORMATS.get_or_init(date_formats)
}

fn timestamp_format_list() -> &'static [StrpTimeFormat] {
    static FORMATS: OnceLock<Vec<StrpTimeFormat>> = OnceLock::new();
    FORMATS.get_or_init(timestamp_formats)
}

/// `JSONStructureNode::EliminateCandidateTypes`: the last candidate is tried first, and dropped when some
/// string of this vector does not parse as it.  `DATE` and `TIMESTAMP` need one format (tried from the last)
/// that parses every string; `TIME` and `UUID` are strict casts.
fn eliminate_candidate_types<'a>(
    candidates: &mut Vec<Candidate>,
    strings: impl Iterator<Item = &'a [u8]> + Clone,
) {
    while let Some(&candidate) = candidates.last() {
        let ok = match candidate {
            Candidate::Date => date_format_list()
                .iter()
                .rev()
                .any(|f| strings.clone().all(|s| f.try_parse_date(s).is_some())),
            Candidate::Timestamp => timestamp_format_list()
                .iter()
                .rev()
                .any(|f| strings.clone().all(|s| f.try_parse_timestamp(s).is_some())),
            Candidate::Time => strings
                .clone()
                .all(|s| duckdb_text::try_convert_time(s).is_some()),
            Candidate::Uuid => strings.clone().all(|s| duckdb_text::uuid_text(s).is_some()),
        };
        if ok {
            return;
        }
        candidates.pop();
    }
}

/// `JSONScan::DetectStructure`: the structure of the first [`SAMPLE_SIZE`] records, read a vector of
/// [`VECTOR_SIZE`] records at a time, the candidate string types refined after each.
fn detect_structure(rows: &[Value]) -> Node {
    let mut root = Node::new("");
    for chunk in rows[..rows.len().min(SAMPLE_SIZE)].chunks(VECTOR_SIZE) {
        for record in chunk {
            root.extract(record);
        }
        if !root.contains_varchar() {
            continue;
        }
        root.initialize_candidates();
        let vals: Vec<Option<&Value>> = chunk.iter().map(Some).collect();
        root.refine_candidates(&vals);
    }
    root
}

// ─── the inferred type ───────────────────────────────────────────────────────────────────────

/// The type DuckDB's `read_json_auto` gives a column (or a nested value).
#[derive(Debug, Clone, PartialEq)]
enum Ty {
    Null,
    Bool,
    Bigint,
    Hugeint,
    Double,
    Str,
    Date,
    Time,
    Timestamp,
    Uuid,
    List(Box<Ty>),
    Struct(Vec<(String, Ty)>),
    /// `MAP(VARCHAR, JSON)`, what an object without fields is read as.
    EmptyObject,
    /// The `JSON` type: values of different types, kept as raw JSON text.
    Json,
}

/// `JSONStructure::StructureToType` (without the `MAP` cases, see the module docs).
fn structure_to_type(node: &Node) -> Ty {
    let [d] = node.descriptions.as_slice() else {
        // no data observed, or inconsistent types
        return Ty::Json;
    };
    match d.kind {
        Kind::Null => Ty::Null,
        Kind::Bool => Ty::Bool,
        Kind::Bigint => Ty::Bigint,
        Kind::Hugeint => Ty::Hugeint,
        Kind::Ubigint if d.has_large_ubigint => Ty::Hugeint,
        Kind::Ubigint => Ty::Bigint,
        Kind::Double => Ty::Double,
        Kind::Varchar => match d.candidates.last() {
            None => Ty::Str,
            Some(Candidate::Uuid) => Ty::Uuid,
            Some(Candidate::Timestamp) => Ty::Timestamp,
            Some(Candidate::Date) => Ty::Date,
            Some(Candidate::Time) => Ty::Time,
        },
        Kind::List => Ty::List(Box::new(structure_to_type(&d.children[0]))),
        // `map_inference_threshold` is set: an empty struct is `MAP(VARCHAR, JSON)`
        Kind::Struct if d.children.is_empty() => Ty::EmptyObject,
        Kind::Struct => Ty::Struct(
            d.children
                .iter()
                .map(|c| (c.key.clone(), structure_to_type(c)))
                .collect(),
        ),
    }
}

// ─── casting to text (`JSONTransform::Transform`, then `CAST(... AS VARCHAR)`) ───────────────

/// One value as DuckDB holds it after the transform to its column type, ready to be cast to `VARCHAR`.
#[derive(Debug)]
enum Cell {
    Null,
    /// The text of a scalar, quoted inside a list or struct when it could be mistaken for syntax.
    Text(String),
    /// The text of a `JSON` value, which a list or struct prints as it is.
    Json(String),
    List(Vec<Cell>),
    Struct(Vec<(String, Cell)>),
}

impl Cell {
    /// `CAST(v AS VARCHAR)`; `nested` inside a list or struct.
    fn text(&self, nested: bool) -> String {
        match self {
            Self::Null => "NULL".to_owned(),
            Self::Text(s) if nested => quote_nested(s),
            Self::Text(s) | Self::Json(s) => s.clone(),
            Self::List(items) => {
                let parts: Vec<String> = items.iter().map(|i| i.text(true)).collect();
                format!("[{}]", parts.join(", "))
            }
            Self::Struct(fields) => {
                let parts: Vec<String> = fields
                    .iter()
                    .map(|(k, c)| format!("{}: {}", quote_always(k), c.text(true)))
                    .collect();
                format!("{{{}}}", parts.join(", "))
            }
        }
    }

    /// The cell as the join engine hands it on: `NULL`, or the text run through `_parse_numeric`.
    fn into_value(self) -> Value {
        match self {
            Self::Null => Value::Null,
            Self::Text(s) | Self::Json(s) => parse_numeric(&s),
            other => parse_numeric(&other.text(false)),
        }
    }
}

/// DuckDB quotes a string inside a list or struct when it is empty, has leading
/// or trailing (ASCII) whitespace, reads as `NULL`, or holds any of `{}[](),:='"`.
fn quote_nested(s: &str) -> String {
    let needs = s.is_empty()
        || s.starts_with(is_c_space)
        || s.ends_with(is_c_space)
        || s.eq_ignore_ascii_case("null")
        || s.contains(['{', '}', '[', ']', '(', ')', ',', ':', '=', '\'', '"']);
    if needs { quote_always(s) } else { s.to_owned() }
}

/// C's `isspace` (`StringUtil::CharacterIsSpace`).
fn is_c_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\u{b}' | '\u{c}' | '\r')
}

fn quote_always(s: &str) -> String {
    format!("'{}'", s.replace('\'', "\\'"))
}

fn is_null(v: Option<&Value>) -> bool {
    matches!(v, None | Some(Value::Null))
}

/// The error `JSONTransform` raises for a value that does not fit the column type.
fn transform_error(v: &Value, ty: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "Invalid Input Error: JSON transform error in file, in record/value: Failed to cast value to {ty}: {}",
        serde_json::to_string(v).unwrap_or_default()
    )
}

/// A value as yyjson writes it (`JSONCommon::WriteVal`), the text of a `JSON`-typed cell: compact, a
/// double in ECMAScript form with the point kept (`100.0`, `1.2345678901234568e29`), an integer beyond
/// 64 bits as the digits it was read from.
fn json_text(v: &Value) -> String {
    let mut out = String::new();
    write_json(v, &mut out);
    out
}

fn write_json(v: &Value, out: &mut String) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Int(i) => out.push_str(&i.to_string()),
        Value::BigInt(n) => out.push_str(&n.to_string()),
        Value::Float(f) => out.push_str(&yyjson_f64(*f)),
        Value::Str(s) => write_json_string(s, out),
        Value::List(items) | Value::Tuple(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_json(item, out);
            }
            out.push(']');
        }
        Value::Map(fields) => {
            out.push('{');
            for (i, (k, item)) in fields.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_json_string(k, out);
                out.push(':');
                write_json(item, out);
            }
            out.push('}');
        }
        other => out.push_str(&serde_json::to_string(other).unwrap_or_default()),
    }
}

/// yyjson's string writer: quotes, backslashes and control characters are escaped, everything else
/// (including `/` and non-ASCII) is copied.
fn write_json_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if c < ' ' => out.push_str(&format!("\\u{:04X}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// `write_f64_raw` (`third_party/yyjson/yyjson.cpp`): ECMAScript's `Number::toString`, except that the
/// point is kept (`1.0`), `-0.0` keeps its sign and a positive exponent has no sign.  A double beyond `f64`
/// is a raw number in DuckDB's reader (`YYJSON_READ_BIGNUM_AS_RAW`) and keeps the text it had.
fn yyjson_f64(f: f64) -> String {
    if f.is_infinite() {
        return if f > 0.0 { "1e400" } else { "-1e400" }.to_owned();
    }
    if f == 0.0 {
        return if f.is_sign_negative() { "-0.0" } else { "0.0" }.to_owned();
    }
    let sign = if f < 0.0 { "-" } else { "" };
    let sci = format!("{:e}", f.abs());
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let exp10: i32 = exp.parse().unwrap_or(0);
    let n = digits.len() as i32;
    // the decimal point position relative to the first digit
    let dot = exp10 + 1;
    if -6 < dot && dot <= 21 {
        if dot <= 0 {
            format!("{sign}0.{}{digits}", "0".repeat((-dot) as usize))
        } else if dot >= n {
            format!("{sign}{digits}{}.0", "0".repeat((dot - n) as usize))
        } else {
            format!(
                "{sign}{}.{}",
                &digits[..dot as usize],
                &digits[dot as usize..]
            )
        }
    } else if n > 1 {
        format!("{sign}{}.{}e{exp10}", &digits[..1], &digits[1..])
    } else {
        format!("{sign}{digits}e{exp10}")
    }
}

/// `StringCast::Operation<double>`.
fn double_text(f: f64) -> String {
    py_float_repr(f)
}

/// `JSONTransform::Transform` of one vector of values to `ty`, cast to text.
fn transform(ty: &Ty, vals: &[Option<&Value>], out: &mut Vec<Cell>) -> Result<()> {
    match ty {
        Ty::List(elem) => return transform_lists(elem, vals, out),
        Ty::Struct(fields) => return transform_structs(fields, vals, out),
        Ty::EmptyObject => {
            for &v in vals {
                out.push(match v {
                    None | Some(Value::Null) => Cell::Null,
                    Some(Value::Map(_)) => Cell::Text("{}".to_owned()),
                    Some(other) => return Err(transform_error(other, "MAP")),
                });
            }
            return Ok(());
        }
        Ty::Date | Ty::Timestamp => return transform_with_formats(ty, vals, out),
        _ => {}
    }
    for &v in vals {
        let Some(v) = v.filter(|v| !matches!(v, Value::Null)) else {
            out.push(Cell::Null);
            continue;
        };
        out.push(match (ty, v) {
            (Ty::Json, v) => Cell::Json(json_text(v)),
            (Ty::Bool, v) => match casts::to_bool(v) {
                Some(b) => Cell::Text(b.to_string()),
                None => return Err(transform_error(v, "BOOLEAN")),
            },
            (Ty::Bigint | Ty::Hugeint, v) => match casts::to_integer(v, *ty == Ty::Hugeint) {
                Some(digits) => Cell::Text(digits),
                None => return Err(transform_error(v, "numerical")),
            },
            (Ty::Double, v) => match casts::to_double(v) {
                Some(f) => Cell::Text(double_text(f)),
                None => return Err(transform_error(v, "numerical")),
            },
            (Ty::Str, Value::Str(s)) => Cell::Text(s.clone()),
            // `GetValueString`: any other scalar of a `VARCHAR` column is written as text
            (Ty::Str, Value::Bool(b)) => Cell::Text(b.to_string()),
            (Ty::Str, Value::Int(i)) => Cell::Text(i.to_string()),
            (Ty::Str, Value::BigInt(n)) => Cell::Text(n.to_string()),
            (Ty::Str, Value::Float(f)) => Cell::Text(double_text(*f)),
            (Ty::Str, other @ (Value::List(_) | Value::Map(_))) => Cell::Text(json_text(other)),
            (Ty::Time, Value::Str(s)) => match duckdb_text::try_convert_time(s.as_bytes()) {
                Some(micros) => Cell::Text(time_text(micros)),
                None => return Err(transform_error(v, "TIME")),
            },
            (Ty::Uuid, Value::Str(s)) => match duckdb_text::uuid_text(s.as_bytes()) {
                Some(text) => Cell::Text(text),
                None => return Err(transform_error(v, "UUID")),
            },
            (Ty::Null, _) => return Err(transform_error(v, "NULL")),
            (ty, v) => return Err(transform_error(v, &format!("{ty:?}"))),
        });
    }
    Ok(())
}

/// `TransformFromStringWithFormat`: a `DATE` or `TIMESTAMP` column.  The format that parses the first
/// parseable value of the vector (the last of the templates first) is where each value's search starts; a
/// value is tried against that format and every earlier one.
fn transform_with_formats(ty: &Ty, vals: &[Option<&Value>], out: &mut Vec<Cell>) -> Result<()> {
    let date = *ty == Ty::Date;
    let formats = if date {
        date_format_list()
    } else {
        timestamp_format_list()
    };
    let parse = |format: &StrpTimeFormat, s: &[u8]| {
        if date {
            format.try_parse_date(s).map(date_text)
        } else {
            format.try_parse_timestamp(s).map(timestamp_text_of)
        }
    };
    let mut strings = Vec::with_capacity(vals.len());
    for &v in vals {
        strings.push(match v {
            None | Some(Value::Null) => None,
            Some(Value::Str(s)) => Some(s.as_bytes()),
            Some(other) => {
                return Err(transform_error(
                    other,
                    if date { "DATE" } else { "TIMESTAMP" },
                ));
            }
        });
    }
    let matched = strings.iter().flatten().find_map(|s| {
        (0..formats.len())
            .rev()
            .find(|&j| parse(&formats[j], s).is_some())
    });
    let limit = matched.map_or(formats.len(), |j| j + 1);
    for (s, v) in strings.iter().zip(vals) {
        let Some(s) = s else {
            out.push(Cell::Null);
            continue;
        };
        match (0..limit).rev().find_map(|j| parse(&formats[j], s)) {
            Some(text) => out.push(Cell::Text(text)),
            None => {
                return Err(transform_error(
                    v.unwrap_or(&Value::Null),
                    if date { "DATE" } else { "TIMESTAMP" },
                ));
            }
        }
    }
    Ok(())
}

fn timestamp_text_of(micros: i64) -> String {
    duckdb_text::timestamp_text(micros)
}

/// `TransformArrayToList`: the elements of every list, transformed as one vector.
fn transform_lists(elem: &Ty, vals: &[Option<&Value>], out: &mut Vec<Cell>) -> Result<()> {
    let mut flat: Vec<Option<&Value>> = Vec::new();
    let mut lengths: Vec<Option<usize>> = Vec::with_capacity(vals.len());
    for &v in vals {
        match v {
            None | Some(Value::Null) => lengths.push(None),
            Some(Value::List(items) | Value::Tuple(items)) => {
                lengths.push(Some(items.len()));
                flat.extend(items.iter().map(Some));
            }
            Some(other) => return Err(transform_error(other, "LIST")),
        }
    }
    let mut cells = Vec::with_capacity(flat.len());
    transform(elem, &flat, &mut cells)?;
    let mut cells = cells.into_iter();
    for len in lengths {
        out.push(match len {
            None => Cell::Null,
            Some(n) => Cell::List(cells.by_ref().take(n).collect()),
        });
    }
    Ok(())
}

/// `TransformObjectInternal`: each field transformed as a vector of its own.
fn transform_structs(
    fields: &[(String, Ty)],
    vals: &[Option<&Value>],
    out: &mut Vec<Cell>,
) -> Result<()> {
    for &v in vals {
        if !matches!(v, None | Some(Value::Null | Value::Map(_))) {
            return Err(transform_error(v.unwrap_or(&Value::Null), "STRUCT"));
        }
    }
    let mut columns: Vec<std::vec::IntoIter<Cell>> = Vec::with_capacity(fields.len());
    for (name, ty) in fields {
        let col: Vec<Option<&Value>> = vals
            .iter()
            .map(|v| match v {
                Some(Value::Map(m)) => m.get(name),
                _ => None,
            })
            .collect();
        let mut cells = Vec::with_capacity(col.len());
        transform(ty, &col, &mut cells)?;
        columns.push(cells.into_iter());
    }
    for &v in vals {
        let row: Vec<Cell> = columns
            .iter_mut()
            .map(|c| c.next().unwrap_or(Cell::Null))
            .collect();
        out.push(if is_null(v) {
            Cell::Null
        } else {
            Cell::Struct(
                fields
                    .iter()
                    .map(|(name, _)| name.clone())
                    .zip(row)
                    .collect(),
            )
        });
    }
    Ok(())
}

/// `QueryResult::DeduplicateColumns`: a column name that repeats an earlier one, ignoring case, gets
/// `_1`, `_2`, ... appended (`a`, `A` read as `a`, `A_1`).
fn deduplicate_columns<'a>(names: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    names
        .map(|name| {
            let low = name.to_ascii_lowercase();
            if !seen.contains_key(&low) {
                seen.insert(low, 1);
                return name.to_owned();
            }
            let mut candidate = format!("{name}_{}", seen[&low]);
            while seen.contains_key(&candidate.to_ascii_lowercase()) {
                *seen.get_mut(&low).expect("present") += 1;
                candidate = format!("{name}_{}", seen[&low]);
            }
            *seen.entry(candidate.to_ascii_lowercase()).or_insert(0) += 1;
            candidate
        })
        .collect()
}

/// The rows of a JSON table as upstream's join engine sees them: every column
/// (the keys in order of first appearance) cast to text by its inferred type and
/// run through `_parse_numeric`; an absent key is `NULL`.
///
/// # Errors
/// DuckDB's `InvalidInputException` when a record after the first 20480 does not
/// fit the type inferred from those, or a vector of strings is read as the wrong
/// kind of string.
pub(crate) fn json_rows(rows: Vec<Value>) -> Result<Vec<Value>> {
    let root = detect_structure(&rows);
    match structure_to_type(&root) {
        Ty::Struct(fields) => {
            let names = deduplicate_columns(fields.iter().map(|(name, _)| name.as_str()));
            let mut out: Vec<Value> = Vec::with_capacity(rows.len());
            for chunk in rows.chunks(VECTOR_SIZE) {
                let mut columns: Vec<std::vec::IntoIter<Cell>> = Vec::with_capacity(fields.len());
                for (name, ty) in &fields {
                    let vals: Vec<Option<&Value>> = chunk
                        .iter()
                        .map(|r| match r {
                            Value::Map(m) => m.get(name),
                            _ => None,
                        })
                        .collect();
                    let mut cells = Vec::with_capacity(vals.len());
                    transform(ty, &vals, &mut cells)?;
                    columns.push(cells.into_iter());
                }
                for _ in chunk {
                    out.push(Value::Map(
                        names
                            .iter()
                            .zip(&mut columns)
                            .map(|(name, col)| {
                                (name.clone(), col.next().unwrap_or(Cell::Null).into_value())
                            })
                            .collect(),
                    ));
                }
            }
            Ok(out)
        }
        // `record_type` falls back to `VALUES`: one column named `json`.
        ty => {
            let mut out: Vec<Value> = Vec::with_capacity(rows.len());
            for chunk in rows.chunks(VECTOR_SIZE) {
                let vals: Vec<Option<&Value>> = chunk.iter().map(Some).collect();
                let mut cells = Vec::with_capacity(vals.len());
                transform(&ty, &vals, &mut cells)?;
                out.extend(cells.into_iter().map(|c| {
                    Value::Map(std::iter::once(("json".to_owned(), c.into_value())).collect())
                }));
            }
            Ok(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(json: &str) -> Vec<Value> {
        match serde_json::from_str::<Value>(json).unwrap() {
            Value::List(items) => json_rows(items).unwrap(),
            other => panic!("not a list: {other:?}"),
        }
    }

    fn col(rows: &[Value], k: &str) -> Vec<Value> {
        rows.iter()
            .map(|r| match r {
                Value::Map(m) => m.get(k).cloned().unwrap(),
                _ => unreachable!(),
            })
            .collect()
    }

    fn s(t: &str) -> Value {
        Value::Str(t.to_owned())
    }

    #[test]
    fn text_cells_are_parsed_numerically() {
        let row =
            parse_text_row(serde_json::from_str(r#"{"a": "007", "b": "x", "c": "1e3"}"#).unwrap());
        assert_eq!(
            row,
            serde_json::from_str::<Value>(r#"{"a": 7, "b": "x", "c": 1000.0}"#).unwrap()
        );
    }

    // Each case below was checked against DuckDB 1.5 (`CAST(columns(*) AS VARCHAR)
    // FROM read_json_auto(...)`) followed by upstream's `_parse_numeric`.

    #[test]
    fn booleans_become_text() {
        let r = rows(r#"[{"a": true}, {"a": false}]"#);
        assert_eq!(col(&r, "a"), [s("true"), s("false")]);
    }

    #[test]
    fn a_mixed_column_keeps_json_text() {
        let r = rows(r#"[{"a": true}, {"a": "x"}, {"a": 5}]"#);
        assert_eq!(col(&r, "a"), [s("true"), s("\"x\""), Value::Int(5)]);
    }

    #[test]
    fn integers_in_a_float_column_are_floats() {
        let r = rows(r#"[{"a": 1}, {"a": 2.5}]"#);
        assert_eq!(col(&r, "a"), [Value::Float(1.0), Value::Float(2.5)]);
    }

    #[test]
    fn strings_are_parsed_numerically() {
        let r = rows(r#"[{"a": "007"}, {"a": "x"}]"#);
        assert_eq!(col(&r, "a"), [Value::Int(7), s("x")]);
    }

    #[test]
    fn absent_keys_are_null() {
        let r = rows(r#"[{"a": "x"}, {"b": "y"}]"#);
        assert_eq!(col(&r, "a"), [s("x"), Value::Null]);
        assert_eq!(col(&r, "b"), [Value::Null, s("y")]);
    }

    #[test]
    fn nested_values_are_duckdb_text() {
        let r =
            rows(r#"[{"a": ["x", "y,z", ""], "s": {"k": "v", "n": 1}}, {"a": [], "s": {"j": 2}}]"#);
        assert_eq!(col(&r, "a"), [s("[x, 'y,z', '']"), s("[]")]);
        assert_eq!(
            col(&r, "s"),
            [
                s("{'k': v, 'n': 1, 'j': NULL}"),
                s("{'k': NULL, 'n': NULL, 'j': 2}")
            ]
        );
    }

    fn decode(cell: &serde_json::Value) -> Value {
        match cell {
            serde_json::Value::Null => Value::Null,
            serde_json::Value::Array(parts) => {
                let text = parts[1].as_str().unwrap();
                match parts[0].as_str().unwrap() {
                    "i" => parse_numeric(text),
                    "f" => Value::Float(f64::from_bits(u64::from_str_radix(text, 16).unwrap())),
                    _ => Value::Str(text.to_owned()),
                }
            }
            other => panic!("bad cell {other}"),
        }
    }

    /// Floats are equal when their bits are (`-0.0`), everything else as `Value`s.
    fn same(got: &Value, want: &Value) -> bool {
        match (got, want) {
            (Value::Float(a), Value::Float(b)) => a.to_bits() == b.to_bits(),
            (a, b) => std::mem::discriminant(a) == std::mem::discriminant(b) && a == b,
        }
    }

    fn replay(fixture: &str) -> usize {
        let mut failures = Vec::new();
        let mut cases = 0;
        for line in fixture.lines() {
            cases += 1;
            let case: serde_json::Value = serde_json::from_str(line).unwrap();
            let text = case["json"].as_str().unwrap();
            let table = linkml_map_core::value::parse_json(text);
            let got = match table {
                Ok(Value::List(items)) => json_rows(items),
                Ok(other) => json_rows(vec![other]),
                Err(e) => {
                    failures.push(format!("{text}\n  loader: {e}"));
                    continue;
                }
            };
            if let Some(want) = case.get("rows") {
                let want = want.as_array().unwrap();
                let ok = match &got {
                    Ok(rows) => {
                        rows.len() == want.len()
                            && rows.iter().zip(want).all(|(row, want)| {
                                let Value::Map(cells) = row else { return false };
                                let want = want.as_object().unwrap();
                                cells.len() == want.len()
                                    && cells
                                        .iter()
                                        .zip(want)
                                        .all(|((k, v), (wk, wv))| k == wk && same(v, &decode(wv)))
                            })
                    }
                    Err(_) => false,
                };
                if !ok {
                    failures.push(format!("{text}\n  want {want:?}\n  got  {got:?}"));
                }
            } else if got.is_ok() {
                failures.push(format!(
                    "{text}\n  want error {}\n  got  {got:?}",
                    case["error"]
                ));
            }
        }
        assert!(
            failures.is_empty(),
            "{} of {cases} DuckDB cases differ:\n{}",
            failures.len(),
            failures
                .iter()
                .take(15)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
        );
        cases
    }

    /// `tests/differential/duckdb_cells.py` writes the fixture: random JSON tables read by DuckDB 1.5 with
    /// `SELECT CAST(columns(*) AS VARCHAR) FROM read_json_auto(?)`, then `_parse_numeric`, as upstream's join
    /// engine does.  `DUCKDB_CELLS=path` replays a bigger local file as well.
    #[test]
    fn replays_duckdb_cells() {
        replay(include_str!("join_rows/duckdb_cells.jsonl"));
        if let Ok(path) = std::env::var("DUCKDB_CELLS") {
            replay(&std::fs::read_to_string(path).unwrap());
        }
    }
}
