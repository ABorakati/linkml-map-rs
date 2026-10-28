//! YAML input with PyYAML `safe_load` semantics.
//!
//! Upstream reads every YAML *data* file with `yaml.safe_load`
//! (`loaders/data_loaders.py::YamlFileLoader`, `cli.py::_map_data_single`,
//! `utils/multi_file_transformer.py`), and transformation specifications the
//! same way (`transformer.py::load_transformer_specification`,
//! `utils/spec_merge.py`). PyYAML implements YAML **1.1**, so the scalars
//! resolve differently from a YAML 1.2 reader such as `serde_yaml`: `yes`/`on`
//! are booleans, `017` is the octal 15, `1_000` is 1000, `1:30` is 90, `~` is
//! null, `2024-01-01` is a `datetime.date`, while `1e3`, `0o7` and `1.0e20`
//! are strings.
//!
//! The structure (events: scalars with their style and tag, sequences,
//! mappings, anchors) comes from `yaml-rust2`. The meaning of each scalar comes
//! from `linkml_map_core::pyyaml`, which ports PyYAML's implicit resolvers and
//! `SafeConstructor`; the composition rules `SafeConstructor` adds on top
//! (`flatten_mapping` for `<<` merge keys, anchors and aliases) are here.
//!
//! What a Python value becomes:
//!
//! * `datetime.date` / `datetime.datetime` are [`Value::Temporal`].
//! * Integers beyond `i64` are an exact [`Value::Decimal`].
//! * A mapping key that is not a string is kept as the text a JSON/YAML
//!   writer prints for it (`1`, `true`, `null`, `1.5`, `2024-01-01`).
//!   PARITY: upstream keeps the key's type, so PyYAML writes `1: a` where this
//!   port writes `'1': a`; `simplejson` converts the key to text just as here.
//!
//! Not supported (they are `ConstructorError`s upstream too, or need Python
//! types [`Value`] cannot hold): `!!binary`, `!!set`, `!!omap`, `!!pairs`,
//! other tags, and recursive aliases.

use std::collections::HashMap;

use indexmap::IndexMap;
use linkml_map_core::pyfmt::py_float_repr;
use linkml_map_core::pyyaml::{KeyRole, LoadError, construct_scalar, key_role};
use linkml_map_core::value::Value;

use crate::libyaml::{Event, Parser};

const TAG_PREFIX: &str = "tag:yaml.org,2002:";

/// `yaml.safe_load_all` over the whole text: one [`Value`] per document.
///
/// An empty stream has no documents (`safe_load("")` is `None`; callers decide).
///
/// # Errors
/// [`LoadError::Yaml`] for anything PyYAML raises as a `yaml.YAMLError` and
/// [`LoadError::Value`] for the plain Python exceptions its constructors raise.
pub fn safe_load_all(text: &str) -> Result<Vec<Value>, LoadError> {
    let text = prepare(text)?;
    let mut builder = Builder::default();
    let mut parser = Parser::new(&text);
    while let Some(event) = parser.next_event().map_err(LoadError::Yaml)? {
        builder.on_event(event);
        if let Some(error) = builder.error.take() {
            return Err(error);
        }
    }
    if text.contains('\t') {
        reject_stray_tabs(&text, &builder.scalar_spans)?;
    }
    Ok(builder.documents)
}

/// What `open(path)` plus PyYAML's `Reader` do to the text before scanning:
/// universal newlines, a leading byte-order mark skipped, and a
/// `ReaderError` for any character outside YAML's printable set.
fn prepare(text: &str) -> Result<String, LoadError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push('\n');
            }
            // `Reader.NON_PRINTABLE`: not tab, LF, CR, printable ASCII, NEL,
            // or the ranges U+A0..U+D7FF, U+E000..U+FFFD, U+10000.. .
            '\t'
            | '\n'
            | ' '..='~'
            | '\u{85}'
            | '\u{a0}'..='\u{d7ff}'
            | '\u{e000}'..='\u{fffd}'
            | '\u{10000}'.. => out.push(c),
            other => {
                return Err(LoadError::Yaml(format!(
                    "unacceptable character #x{:04x}: special characters are not allowed",
                    other as u32
                )));
            }
        }
    }
    Ok(out)
}

/// PyYAML's scanner only skips *spaces* between tokens: a tab anywhere but inside
/// a scalar or a comment is `ScannerError: found character '\t' that cannot
/// start any token` (also after a plain scalar, `a: 1<TAB>`). libyaml allows tabs
/// in flow context and after `:`/`-`, so a tab-indented JSON document, which
/// PyYAML rejects (and `cli.py` then reads as JSON), would otherwise be read as
/// YAML here and give different numbers (`1e5` is a string in YAML 1.1).
///
/// `spans` are the character ranges of every scalar, in document order.
fn reject_stray_tabs(text: &str, spans: &[(usize, usize)]) -> Result<(), LoadError> {
    let mut spans = spans.iter().peekable();
    let mut in_comment = false;
    let mut prev_blank = true;
    for (index, c) in text.chars().enumerate() {
        while spans.next_if(|(_, end)| *end <= index).is_some() {}
        let in_scalar = spans
            .peek()
            .is_some_and(|(start, end)| (*start..*end).contains(&index));
        if in_scalar {
            prev_blank = false;
            continue;
        }
        match c {
            '\n' => in_comment = false,
            '#' if prev_blank => in_comment = true,
            '\t' if !in_comment => {
                return Err(LoadError::Yaml(
                    "while scanning for the next token: found character '\\t' that cannot \
                     start any token"
                        .to_owned(),
                ));
            }
            _ => {}
        }
        prev_blank = matches!(c, ' ' | '\t' | '\n');
    }
    Ok(())
}

/// A node being assembled.
enum Frame {
    Seq {
        anchor: Option<String>,
        items: Vec<Value>,
    },
    Map {
        anchor: Option<String>,
        entries: Vec<(KeyKind, Value)>,
        /// The key waiting for its value.
        key: Option<KeyKind>,
    },
}

enum KeyKind {
    Merge,
    Text(String),
}

/// Builds [`Value`]s from events the way `SafeConstructor` does, and applies
/// the composer's anchor rules.
#[derive(Default)]
struct Builder {
    stack: Vec<Frame>,
    /// Anchors of the current document and the value each one named.
    anchors: HashMap<String, Value>,
    documents: Vec<Value>,
    /// Character ranges of the scalars seen so far (see [`reject_stray_tabs`]).
    scalar_spans: Vec<(usize, usize)>,
    error: Option<LoadError>,
}

impl Builder {
    fn fail(&mut self, e: LoadError) {
        self.error.get_or_insert(e);
    }

    /// `Composer.compose_node`: an anchor may be defined once per document.
    fn check_anchor(&self, anchor: Option<&str>) -> Result<(), LoadError> {
        match anchor {
            Some(name) if self.anchors.contains_key(name) => Err(LoadError::Yaml(format!(
                "found duplicate anchor '{name}'; second occurrence"
            ))),
            _ => Ok(()),
        }
    }

    /// Attach a finished node to its parent (or make it the document).
    fn push_value(&mut self, anchor: Option<String>, value: Value) {
        if let Some(name) = anchor {
            self.anchors.insert(name, value.clone());
        }
        match self.stack.last_mut() {
            None => self.documents.push(value),
            Some(Frame::Seq { items, .. }) => items.push(value),
            Some(Frame::Map { entries, key, .. }) => match key.take() {
                Some(k) => entries.push((k, value)),
                None => match key_of(&value) {
                    Ok(k) => *key = Some(k),
                    Err(e) => self.fail(e),
                },
            },
        }
    }

    fn scalar(&mut self, text: &str, implicit: bool, anchor: Option<String>, tag: Option<&str>) {
        if let Err(e) = self.check_anchor(anchor.as_deref()) {
            return self.fail(e);
        }
        // A mapping key is decided here (not from its constructed value) so
        // that `<<` can be told apart from the string "<<".
        if let Some(Frame::Map {
            key: key @ None, ..
        }) = self.stack.last_mut()
            && key_role(tag, text, implicit) == KeyRole::Merge
        {
            *key = Some(KeyKind::Merge);
            return;
        }
        match construct_scalar(tag, text, implicit) {
            Ok(value) => self.push_value(anchor, value),
            Err(e) => self.fail(e),
        }
    }

    fn start(&mut self, frame: Frame, anchor: Option<&str>, tag: Option<&str>, expected: &str) {
        if let Some(t) = tag
            && t != "!"
            && t != format!("{TAG_PREFIX}{expected}")
        {
            return self.fail(LoadError::Yaml(format!(
                "could not determine a constructor for the tag '{t}'"
            )));
        }
        if let Err(e) = self.check_anchor(anchor) {
            return self.fail(e);
        }
        self.stack.push(frame);
    }

    fn end_seq(&mut self) {
        if let Some(Frame::Seq { anchor, items }) = self.stack.pop() {
            self.push_value(anchor, Value::List(items));
        }
    }

    fn end_map(&mut self) {
        let Some(Frame::Map {
            anchor, entries, ..
        }) = self.stack.pop()
        else {
            return;
        };
        match flatten_mapping(entries) {
            Ok(map) => self.push_value(anchor, Value::Map(map)),
            Err(e) => self.fail(e),
        }
    }

    fn on_event(&mut self, event: Event) {
        match event {
            Event::Scalar {
                anchor,
                tag,
                value,
                plain_implicit,
                span,
            } => {
                self.scalar_spans.push(span);
                self.scalar(&value, plain_implicit, anchor, tag.as_deref());
            }
            Event::SequenceStart { anchor, tag } => self.start(
                Frame::Seq {
                    anchor: anchor.clone(),
                    items: Vec::new(),
                },
                anchor.as_deref(),
                tag.as_deref(),
                "seq",
            ),
            Event::SequenceEnd => self.end_seq(),
            Event::MappingStart { anchor, tag } => self.start(
                Frame::Map {
                    anchor: anchor.clone(),
                    entries: Vec::new(),
                    key: None,
                },
                anchor.as_deref(),
                tag.as_deref(),
                "map",
            ),
            Event::MappingEnd => self.end_map(),
            Event::Alias { anchor } => match self.anchors.get(&anchor).cloned() {
                Some(value) => self.push_value(None, value),
                // PyYAML composes an alias to a node still being built (a
                // recursive structure); `Value` is a tree and cannot hold one.
                None => self.fail(LoadError::Yaml(format!(
                    "found undefined or recursive alias '{anchor}'"
                ))),
            },
            // Anchors live for one document.
            Event::DocumentStart => self.anchors.clear(),
            Event::StreamStart | Event::StreamEnd | Event::DocumentEnd => {}
        }
    }
}

/// A mapping key as text; collections are `TypeError: unhashable type`
/// (`construct_mapping`'s `ConstructorError`).
fn key_of(value: &Value) -> Result<KeyKind, LoadError> {
    Ok(KeyKind::Text(match value {
        Value::Str(s) => s.clone(),
        Value::Null => "null".to_owned(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) if f.is_nan() => "NaN".to_owned(),
        Value::Float(f) if f.is_infinite() => {
            if *f > 0.0 { "Infinity" } else { "-Infinity" }.to_owned()
        }
        Value::Float(f) => py_float_repr(*f),
        Value::Decimal(d) => d.to_sci_string(),
        Value::Temporal(t) => t.to_string(),
        Value::List(_) | Value::Map(_) | Value::Tuple(_) | Value::Set(_) | Value::Dict(_) => {
            return Err(LoadError::Yaml(
                "while constructing a mapping: found unhashable key".to_owned(),
            ));
        }
    }))
}

/// `SafeConstructor.flatten_mapping` + `construct_mapping`: merge-key values
/// come first (for a list of mappings the *last* is merged first, so earlier
/// ones win), then the mapping's own pairs; a repeated key keeps its first
/// position and its last value.
fn flatten_mapping(entries: Vec<(KeyKind, Value)>) -> Result<IndexMap<String, Value>, LoadError> {
    let merge_error = |found: &str| {
        LoadError::Yaml(format!(
            "while constructing a mapping: expected a mapping or list of mappings \
             for merging, but found {found}"
        ))
    };
    let mut merged: Vec<(String, Value)> = Vec::new();
    let mut own: Vec<(String, Value)> = Vec::new();
    for (key, value) in entries {
        match key {
            KeyKind::Text(k) => own.push((k, value)),
            KeyKind::Merge => match value {
                Value::Map(m) => merged.extend(m),
                Value::List(items) => {
                    let mut maps = Vec::with_capacity(items.len());
                    for item in items {
                        match item {
                            Value::Map(m) => maps.push(m),
                            _ => return Err(merge_error("non-mapping")),
                        }
                    }
                    for m in maps.into_iter().rev() {
                        merged.extend(m);
                    }
                }
                _ => return Err(merge_error("scalar")),
            },
        }
    }
    let mut map = IndexMap::with_capacity(merged.len() + own.len());
    for (k, v) in merged.into_iter().chain(own) {
        map.insert(k, v);
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(text: &str) -> Value {
        let mut docs = safe_load_all(text).unwrap();
        assert_eq!(docs.len(), 1, "{text:?}");
        docs.remove(0)
    }

    fn json(v: &Value) -> String {
        serde_json::to_string(v).unwrap()
    }

    #[test]
    fn scalars_resolve_as_pyyaml() {
        let v = load(
            "a: 1.0e20\nb: yes\nc: 0o7\nd: 2024-01-01\ne: ~\nf: .inf\ng: 017\nh: 1_000\ni: 1:30\n\
             j: '017'\nk: \"yes\"\nl: |\n  017\nm:\n",
        );
        assert_eq!(
            json(&v),
            r#"{"a":"1.0e20","b":true,"c":"0o7","d":"2024-01-01","e":null,"f":null,"g":15,"h":1000,"i":90,"j":"017","k":"yes","l":"017\n","m":null}"#
        );
        let Value::Map(m) = &v else { panic!() };
        assert!(matches!(m["d"], Value::Temporal(_)));
        assert!(matches!(m["f"], Value::Float(f) if f.is_infinite()));
    }

    #[test]
    fn merge_keys_follow_flatten_mapping() {
        let v = load(
            "base: &b {x: 1, y: 2}\nover: &o {y: 3, z: 4}\n\
             m1: {<<: *b, y: 9}\nm2: {<<: [*b, *o]}\nm3: {a: 0, <<: *b}\nq: {'<<': 5}\n",
        );
        assert_eq!(
            json(&v),
            r#"{"base":{"x":1,"y":2},"over":{"y":3,"z":4},"m1":{"x":1,"y":9},"m2":{"y":2,"z":4,"x":1},"m3":{"x":1,"y":2,"a":0},"q":{"<<":5}}"#
        );
    }

    #[test]
    fn non_string_keys_become_text_and_collections_fail() {
        assert_eq!(
            json(&load("{1: a, true: b, ~: c, 1.5: d, 2024-01-01: e}")),
            r#"{"1":"a","true":"b","null":"c","1.5":"d","2024-01-01":"e"}"#
        );
        assert!(matches!(
            safe_load_all("? [1, 2]\n: x\n"),
            Err(LoadError::Yaml(_))
        ));
    }

    #[test]
    fn documents_bom_and_newlines() {
        assert_eq!(safe_load_all("").unwrap(), Vec::<Value>::new());
        assert_eq!(safe_load_all("a: 1\n---\nb: 2\n").unwrap().len(), 2);
        assert_eq!(
            json(&load("\u{feff}a: 1\r\nb: 2\rc: 3\n")),
            r#"{"a":1,"b":2,"c":3}"#
        );
        // Reader.NON_PRINTABLE
        assert!(matches!(
            safe_load_all("a: \u{1}\n"),
            Err(LoadError::Yaml(_))
        ));
    }

    #[test]
    fn errors_are_classified_like_python_exceptions() {
        // Scanner / parser errors are YAMLError (the CLI then tries JSON).
        assert!(matches!(
            safe_load_all("a: [1, 2\n"),
            Err(LoadError::Yaml(_))
        ));
        assert!(matches!(
            safe_load_all("a: !custom 1\n"),
            Err(LoadError::Yaml(_))
        ));
        // ValueError from the timestamp constructor is not.
        assert!(matches!(
            safe_load_all("a: 2024-13-45\n"),
            Err(LoadError::Value(_))
        ));
    }
}
