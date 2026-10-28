//! A PyYAML-compatible YAML emitter.
//!
//! Upstream writes every YAML it produces with PyYAML's pure-Python
//! `yaml.dump` (`utils/serialization.py::dump_yaml`, the YAML stream writer,
//! `cli.py::dump_output`, the multivalued `stringification` syntax) or with
//! linkml-runtime's `yaml_dumper`, which is `yaml.dump(..., Dumper=SafeDumper,
//! sort_keys=False, allow_unicode=True)`. Scalar quoting, 80-column folding,
//! multi-line strings, special floats, `...` document-end markers and the
//! indentation of sequences inside mappings are all decided by PyYAML's
//! `Emitter`, so this module ports that emitter rather than approximating it:
//!
//! * `yaml/resolver.py`: the YAML 1.1 implicit resolvers ([`resolve_implicit`]),
//!   which decide whether a string needs quoting to stay a string.
//! * `yaml/representer.py`: how `None`, `bool`, `int`, `float`, `str`,
//!   `Decimal` (via upstream's `_represent_decimal`), `list` and `dict` become
//!   nodes (`represent_float`, `represent_mapping` with `sort_keys`, ...).
//! * `yaml/serializer.py`: how a scalar's implicit-tag flags are derived.
//! * `yaml/emitter.py`: `analyze_scalar`, `choose_scalar_style`, the
//!   `write_plain` / `write_single_quoted` / `write_double_quoted` writers and
//!   the block and flow collection state machines.
//!
//! Function and field names follow the Python originals. PyYAML drives its
//! emitter with an event queue and a state stack; here the same states are
//! recursive methods over the [`Value`] tree, which preserves the emitted text
//! (the queue only provides the look-ahead for "is this collection empty?").
//!
//! Not ported because the upstream call sites cannot reach them:
//! `write_folded` / `write_literal` (no node carries a `|` / `>` style),
//! anchors and aliases, tags other than the five core scalar tags, canonical
//! output, explicit document markers and the `indent` / `width` / `line_break`
//! options (upstream leaves them at their defaults).
//!
//! PARITY: PyYAML turns a repeated Python object into an anchor / alias pair
//! (`&id001` / `*id001`) for everything `SafeRepresenter.ignore_aliases` does
//! not exempt. `Decimal` is not exempt, so upstream writes the *same*
//! `Decimal` object twice (for example one source value copied into two target
//! slots) as `a: &id001 1.50` then `b: *id001`. [`Value`] has no object
//! identity, so this port always writes the value out twice.

mod constructor;

pub use constructor::{KeyRole, LoadError, construct_scalar, key_role};

use std::borrow::Cow;

use crate::pyfmt::py_float_repr;
use crate::value::Value;

/// The keyword arguments upstream passes to `yaml.dump`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DumpOptions {
    /// `default_flow_style`: `true` writes every collection in flow style
    /// (`[a, b]`, `{k: v}`); `false` writes block style, except that empty
    /// collections are always `[]` / `{}`.
    pub default_flow_style: bool,
    /// `allow_unicode`: when `false`, non-ASCII characters force a
    /// double-quoted scalar with `\xXX` / `\uXXXX` / `\UXXXXXXXX` escapes.
    pub allow_unicode: bool,
    /// `sort_keys`: sort every mapping by key (code-point order).
    pub sort_keys: bool,
}

impl Default for DumpOptions {
    /// Upstream `dump_yaml(obj)`: `default_flow_style=False`,
    /// `allow_unicode=True`, and `yaml.dump`'s own `sort_keys=True`.
    fn default() -> Self {
        Self {
            default_flow_style: false,
            allow_unicode: true,
            sort_keys: true,
        }
    }
}

/// Dump `value` as one YAML document, exactly as upstream's
/// `dump_yaml(value, **kwargs)` would.
///
/// ```
/// # use linkml_map_core::pyyaml::{dump_yaml, DumpOptions};
/// # use linkml_map_core::value::Value;
/// let v: Value = serde_json::from_str(r#"{"b": ["yes", 1.0e20], "a": "x: y"}"#).unwrap();
/// assert_eq!(
///     dump_yaml(&v, DumpOptions::default()),
///     "a: 'x: y'\nb:\n- 'yes'\n- 1.0e+20\n"
/// );
/// ```
pub fn dump_yaml(value: &Value, opts: DumpOptions) -> String {
    let mut out = String::new();
    dump_yaml_into(&mut out, value, opts);
    out
}

/// [`dump_yaml`], appending to `out` instead of allocating.
pub fn dump_yaml_into(out: &mut String, value: &Value, opts: DumpOptions) {
    let mut emitter = Emitter::new(out, opts);
    emitter.emit_document(value);
}

// ─── Resolver (yaml/resolver.py) ─────────────────────────────────────────────

/// The core scalar tags a plain scalar can resolve to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tag {
    Str,
    Int,
    Float,
    Bool,
    Null,
    Merge,
    Timestamp,
    Value,
    Yaml,
}

impl Tag {
    /// The text `prepare_tag` produces for `tag:yaml.org,2002:<name>`.
    fn prepared(self) -> &'static str {
        match self {
            Tag::Str => "!!str",
            Tag::Int => "!!int",
            Tag::Float => "!!float",
            Tag::Bool => "!!bool",
            Tag::Null => "!!null",
            Tag::Merge => "!!merge",
            Tag::Timestamp => "!!timestamp",
            Tag::Value => "!!value",
            Tag::Yaml => "!!yaml",
        }
    }
}

/// `Resolver.resolve(ScalarNode, value, (True, False))`: the tag a *plain*
/// scalar with this text would be read back as, [`Tag::Str`] when no implicit
/// resolver matches.
///
/// The resolvers are tried in registration order (bool, float, int, merge,
/// null, timestamp, value, yaml) among those registered for the first
/// character. Each pattern ends in `$`, which `re.match` lets match before one
/// trailing newline, so one is ignored here too.
fn resolve_implicit(text: &str) -> Tag {
    let Some(first) = text.chars().next() else {
        // `yaml_implicit_resolvers['']` is the null resolver (its pattern
        // contains an empty alternative).
        return Tag::Null;
    };
    let body = text.strip_suffix('\n').unwrap_or(text);
    let b = body.as_bytes();
    if matches!(
        first,
        'y' | 'Y' | 'n' | 'N' | 't' | 'T' | 'f' | 'F' | 'o' | 'O'
    ) && is_bool(b)
    {
        return Tag::Bool;
    }
    if matches!(first, '-' | '+' | '0'..='9' | '.') && is_float(b) {
        return Tag::Float;
    }
    if matches!(first, '-' | '+' | '0'..='9') && is_int(b) {
        return Tag::Int;
    }
    if first == '<' && b == b"<<" {
        return Tag::Merge;
    }
    if matches!(first, '~' | 'n' | 'N') && matches!(b, b"~" | b"null" | b"Null" | b"NULL") {
        return Tag::Null;
    }
    if first.is_ascii_digit() && is_timestamp(b) {
        return Tag::Timestamp;
    }
    if first == '=' && b == b"=" {
        return Tag::Value;
    }
    if matches!(first, '!' | '&' | '*') && matches!(b, b"!" | b"&" | b"*") {
        return Tag::Yaml;
    }
    Tag::Str
}

fn is_bool(b: &[u8]) -> bool {
    matches!(
        b,
        b"yes"
            | b"Yes"
            | b"YES"
            | b"no"
            | b"No"
            | b"NO"
            | b"true"
            | b"True"
            | b"TRUE"
            | b"false"
            | b"False"
            | b"FALSE"
            | b"on"
            | b"On"
            | b"ON"
            | b"off"
            | b"Off"
            | b"OFF"
    )
}

/// Skip a leading `[-+]`.
fn skip_sign(b: &[u8]) -> &[u8] {
    match b.first() {
        Some(b'-' | b'+') => &b[1..],
        _ => b,
    }
}

/// Length of the leading run of bytes satisfying `f`.
fn run(b: &[u8], f: impl Fn(u8) -> bool) -> usize {
    b.iter().take_while(|&&c| f(c)).count()
}

fn is_digit_us(c: u8) -> bool {
    c.is_ascii_digit() || c == b'_'
}

/// `(?:[eE][-+][0-9]+)?` followed by the end of the text.
fn exponent_to_end(b: &[u8]) -> bool {
    if b.is_empty() {
        return true;
    }
    matches!(b[0], b'e' | b'E')
        && matches!(b.get(1), Some(b'-' | b'+'))
        && b.len() > 2
        && b[2..].iter().all(u8::is_ascii_digit)
}

/// One or more `:[0-5]?[0-9]` sexagesimal groups; returns the remainder.
fn sexagesimal_groups(mut b: &[u8]) -> Option<&[u8]> {
    let mut groups = 0;
    while b.first() == Some(&b':') {
        let rest = &b[1..];
        let take = match (rest.first(), rest.get(1)) {
            (Some(d1), Some(d2)) if (b'0'..=b'5').contains(d1) && d2.is_ascii_digit() => 2,
            (Some(d1), _) if d1.is_ascii_digit() => 1,
            _ => return None,
        };
        b = &rest[take..];
        groups += 1;
    }
    (groups > 0).then_some(b)
}

/// The float resolver's pattern.
fn is_float(b: &[u8]) -> bool {
    // [-+]?(?:[0-9][0-9_]*)\.[0-9_]*(?:[eE][-+][0-9]+)?
    let body = skip_sign(b);
    if body.first().is_some_and(u8::is_ascii_digit) {
        let n = run(body, is_digit_us);
        if body.get(n) == Some(&b'.') {
            let rest = &body[n + 1..];
            let m = run(rest, is_digit_us);
            if exponent_to_end(&rest[m..]) {
                return true;
            }
        }
        // [-+]?[0-9][0-9_]*(?::[0-5]?[0-9])+\.[0-9_]*
        if let Some(rest) = sexagesimal_groups(&body[n..])
            && let Some(frac) = rest.strip_prefix(b".")
            && frac.iter().all(|&c| is_digit_us(c))
        {
            return true;
        }
    }
    // \.[0-9][0-9_]*(?:[eE][-+][0-9]+)?
    if let Some(rest) = b.strip_prefix(b".")
        && rest.first().is_some_and(u8::is_ascii_digit)
    {
        let m = run(rest, is_digit_us);
        if exponent_to_end(&rest[m..]) {
            return true;
        }
    }
    // [-+]?\.(?:inf|Inf|INF)  |  \.(?:nan|NaN|NAN)
    matches!(body, b".inf" | b".Inf" | b".INF") || matches!(b, b".nan" | b".NaN" | b".NAN")
}

/// The int resolver's pattern.
fn is_int(b: &[u8]) -> bool {
    let body = skip_sign(b);
    // [-+]?0b[0-1_]+
    if let Some(rest) = body.strip_prefix(b"0b")
        && !rest.is_empty()
        && rest.iter().all(|&c| matches!(c, b'0' | b'1' | b'_'))
    {
        return true;
    }
    // [-+]?0[0-7_]+
    if let Some(rest) = body.strip_prefix(b"0")
        && !rest.is_empty()
        && rest.iter().all(|&c| matches!(c, b'0'..=b'7' | b'_'))
    {
        return true;
    }
    // [-+]?(?:0|[1-9][0-9_]*)
    if body == b"0" {
        return true;
    }
    let decimal_head = body.first().is_some_and(|c| (b'1'..=b'9').contains(c));
    if decimal_head {
        let n = run(body, is_digit_us);
        if n == body.len() {
            return true;
        }
        // [-+]?[1-9][0-9_]*(?::[0-5]?[0-9])+
        if sexagesimal_groups(&body[n..]).is_some_and(<[u8]>::is_empty) {
            return true;
        }
    }
    // [-+]?0x[0-9a-fA-F_]+
    body.strip_prefix(b"0x").is_some_and(|rest| {
        !rest.is_empty() && rest.iter().all(|&c| c.is_ascii_hexdigit() || c == b'_')
    })
}

/// The timestamp resolver's pattern (written with `re.X`, so its literal
/// spaces are insignificant).
fn is_timestamp(b: &[u8]) -> bool {
    let digits = |s: &[u8], n: usize| s.len() >= n && s[..n].iter().all(u8::is_ascii_digit);
    // [0-9]{4}-[0-9]{2}-[0-9]{2}
    if b.len() == 10
        && digits(b, 4)
        && b[4] == b'-'
        && digits(&b[5..], 2)
        && b[7] == b'-'
        && digits(&b[8..], 2)
    {
        return true;
    }
    // [0-9]{4}-[0-9][0-9]?-[0-9][0-9]?(?:[Tt]|[ \t]+)[0-9][0-9]?:[0-9][0-9]:[0-9][0-9]
    //   (?:\.[0-9]*)?(?:[ \t]*(?:Z|[-+][0-9][0-9]?(?::[0-9][0-9])?))?
    if !(digits(b, 4) && b.get(4) == Some(&b'-')) {
        return false;
    }
    let mut rest = &b[5..];
    let one_or_two = |s: &mut &[u8]| -> bool {
        let n = run(s, |c| c.is_ascii_digit()).min(2);
        if n == 0 {
            return false;
        }
        *s = &s[n..];
        true
    };
    if !one_or_two(&mut rest) || rest.first() != Some(&b'-') {
        return false;
    }
    rest = &rest[1..];
    if !one_or_two(&mut rest) {
        return false;
    }
    match rest.first() {
        Some(b'T' | b't') => rest = &rest[1..],
        Some(b' ' | b'\t') => rest = &rest[run(rest, |c| c == b' ' || c == b'\t')..],
        _ => return false,
    }
    if !one_or_two(&mut rest) || rest.first() != Some(&b':') {
        return false;
    }
    rest = &rest[1..];
    if !(digits(rest, 2) && rest.get(2) == Some(&b':') && digits(&rest[3..], 2)) {
        return false;
    }
    rest = &rest[5..];
    if let Some(frac) = rest.strip_prefix(b".") {
        rest = &frac[run(frac, |c| c.is_ascii_digit())..];
    }
    if rest.is_empty() {
        return true;
    }
    rest = &rest[run(rest, |c| c == b' ' || c == b'\t')..];
    if rest == b"Z" {
        return true;
    }
    if !matches!(rest.first(), Some(b'-' | b'+')) {
        return false;
    }
    rest = &rest[1..];
    if !one_or_two(&mut rest) {
        return false;
    }
    rest.is_empty() || (rest[0] == b':' && rest.len() == 3 && digits(&rest[1..], 2))
}

// ─── Representer (yaml/representer.py) ───────────────────────────────────────

/// `represent_float`.
fn represent_float(f: f64) -> String {
    if f.is_nan() {
        ".nan".to_owned()
    } else if f == f64::INFINITY {
        ".inf".to_owned()
    } else if f == f64::NEG_INFINITY {
        "-.inf".to_owned()
    } else {
        let value = py_float_repr(f).to_lowercase();
        // `repr(1e17)` is `1e+17`, not a valid `!!float`: PyYAML inserts `.0`.
        if !value.contains('.') && value.contains('e') {
            value.replacen('e', ".0e", 1)
        } else {
            value
        }
    }
}

/// The scalar node for a non-collection value: `(value, tag)`.
fn represent_scalar(value: &Value) -> (Cow<'_, str>, Tag) {
    match value {
        Value::Null => (Cow::Borrowed("null"), Tag::Null),
        Value::Bool(true) => (Cow::Borrowed("true"), Tag::Bool),
        Value::Bool(false) => (Cow::Borrowed("false"), Tag::Bool),
        Value::Int(i) => (Cow::Owned(i.to_string()), Tag::Int),
        Value::Float(f) => (Cow::Owned(represent_float(*f)), Tag::Float),
        // upstream `_represent_decimal`: `format(d, 'f')`, tagged float when it
        // has a decimal point and int otherwise.
        Value::Decimal(d) => {
            let text = d.to_fixed_string();
            let tag = if text.contains('.') {
                Tag::Float
            } else {
                Tag::Int
            };
            (Cow::Owned(text), tag)
        }
        // `represent_date` / `represent_datetime`: `isoformat(' ')`, tagged
        // `!!timestamp`; the text matches the timestamp resolver, so it is plain.
        Value::Temporal(t) => (Cow::Owned(t.to_string()), Tag::Timestamp),
        Value::Str(s) => (Cow::Borrowed(s.as_str()), Tag::Str),
        Value::List(_) | Value::Map(_) | Value::Tuple(_) | Value::Set(_) | Value::Dict(_) => {
            unreachable!("collections are not scalars")
        }
    }
}

// ─── Emitter (yaml/emitter.py) ───────────────────────────────────────────────

/// Python's `'\0 \t\r\n\x85  '`.
fn is_space_or_break(c: char) -> bool {
    matches!(
        c,
        '\0' | ' ' | '\t' | '\r' | '\n' | '\u{85}' | '\u{2028}' | '\u{2029}'
    )
}

/// Python's `'\n\x85  '`.
fn is_break(c: char) -> bool {
    matches!(c, '\n' | '\u{85}' | '\u{2028}' | '\u{2029}')
}

/// The parts of `ScalarAnalysis` the emitter's callers consult.
#[derive(Debug, Clone, Copy)]
struct ScalarAnalysis {
    /// `len(scalar)` in characters.
    len: usize,
    empty: bool,
    multiline: bool,
    allow_flow_plain: bool,
    allow_block_plain: bool,
    allow_single_quoted: bool,
}

/// `Emitter.analyze_scalar`.
fn analyze_scalar(scalar: &str, allow_unicode: bool) -> ScalarAnalysis {
    // Empty scalar is a special case.
    if scalar.is_empty() {
        return ScalarAnalysis {
            len: 0,
            empty: true,
            multiline: false,
            allow_flow_plain: false,
            allow_block_plain: true,
            allow_single_quoted: true,
        };
    }

    let mut block_indicators = false;
    let mut flow_indicators = false;
    let mut line_breaks = false;
    let mut special_characters = false;

    let mut leading_space = false;
    let mut leading_break = false;
    let mut trailing_space = false;
    let mut trailing_break = false;
    let mut break_space = false;
    let mut space_break = false;

    // Check document indicators.
    if scalar.starts_with("---") || scalar.starts_with("...") {
        block_indicators = true;
        flow_indicators = true;
    }

    // First character or preceded by a whitespace.
    let mut preceded_by_whitespace = true;
    // The previous character is a space / a break.
    let mut previous_space = false;
    let mut previous_break = false;

    let mut chars = scalar.chars().peekable();
    let mut index = 0usize;
    while let Some(ch) = chars.next() {
        let next = chars.peek().copied();
        // Last character or followed by a whitespace.
        let followed_by_whitespace = next.is_none_or(is_space_or_break);
        let is_last = next.is_none();

        // Check for indicators.
        if index == 0 {
            // Leading indicators are special characters.
            if matches!(
                ch,
                '#' | ','
                    | '['
                    | ']'
                    | '{'
                    | '}'
                    | '&'
                    | '*'
                    | '!'
                    | '|'
                    | '>'
                    | '\''
                    | '"'
                    | '%'
                    | '@'
                    | '`'
            ) {
                flow_indicators = true;
                block_indicators = true;
            }
            if ch == '?' || ch == ':' {
                flow_indicators = true;
                if followed_by_whitespace {
                    block_indicators = true;
                }
            }
            if ch == '-' && followed_by_whitespace {
                flow_indicators = true;
                block_indicators = true;
            }
        } else {
            // Some indicators cannot appear within a scalar as well.
            if matches!(ch, ',' | '?' | '[' | ']' | '{' | '}') {
                flow_indicators = true;
            }
            if ch == ':' {
                flow_indicators = true;
                if followed_by_whitespace {
                    block_indicators = true;
                }
            }
            if ch == '#' && preceded_by_whitespace {
                flow_indicators = true;
                block_indicators = true;
            }
        }

        // Check for line breaks, special, and unicode characters.
        if is_break(ch) {
            line_breaks = true;
        }
        if !(ch == '\n' || ('\x20'..='\x7E').contains(&ch)) {
            let printable_unicode = (ch == '\u{85}'
                || ('\u{A0}'..='\u{D7FF}').contains(&ch)
                || ('\u{E000}'..='\u{FFFD}').contains(&ch)
                || ('\u{10000}'..'\u{10FFFF}').contains(&ch))
                && ch != '\u{FEFF}';
            if printable_unicode {
                if !allow_unicode {
                    special_characters = true;
                }
            } else {
                special_characters = true;
            }
        }

        // Detect important whitespace combinations.
        if ch == ' ' {
            if index == 0 {
                leading_space = true;
            }
            if is_last {
                trailing_space = true;
            }
            if previous_break {
                break_space = true;
            }
            previous_space = true;
            previous_break = false;
        } else if is_break(ch) {
            if index == 0 {
                leading_break = true;
            }
            if is_last {
                trailing_break = true;
            }
            if previous_space {
                space_break = true;
            }
            previous_space = false;
            previous_break = true;
        } else {
            previous_space = false;
            previous_break = false;
        }

        // Prepare for the next character.
        index += 1;
        preceded_by_whitespace = is_space_or_break(ch);
    }

    // Let's decide what styles are allowed.
    let mut allow_flow_plain = true;
    let mut allow_block_plain = true;
    let mut allow_single_quoted = true;

    // Leading and trailing whitespaces are bad for plain scalars.
    if leading_space || leading_break || trailing_space || trailing_break {
        allow_flow_plain = false;
        allow_block_plain = false;
    }
    // Spaces at the beginning of a new line are only acceptable for block
    // scalars.
    if break_space {
        allow_flow_plain = false;
        allow_block_plain = false;
        allow_single_quoted = false;
    }
    // Spaces followed by breaks, as well as special character are only allowed
    // for double quoted scalars.
    if space_break || special_characters {
        allow_flow_plain = false;
        allow_block_plain = false;
        allow_single_quoted = false;
    }
    // Although the plain scalar writer supports breaks, we never emit
    // multiline plain scalars.
    if line_breaks {
        allow_flow_plain = false;
        allow_block_plain = false;
    }
    // Flow indicators are forbidden for flow plain scalars.
    if flow_indicators {
        allow_flow_plain = false;
    }
    // Block indicators are forbidden for block plain scalars.
    if block_indicators {
        allow_block_plain = false;
    }

    ScalarAnalysis {
        len: index,
        empty: false,
        multiline: line_breaks,
        allow_flow_plain,
        allow_block_plain,
        allow_single_quoted,
    }
}

/// The scalar styles `choose_scalar_style` can return here (no node carries a
/// `|` or `>` style).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Style {
    Plain,
    Single,
    Double,
}

/// A mapping key: a string (a record's key, the hot path) or any other value
/// (a `dict` with non-string keys, the elements of a `set`).
#[derive(Clone, Copy)]
enum Key<'v> {
    Str(&'v str),
    Value(&'v Value),
}

/// The `null` every `!!set` element maps to.
static NULL: Value = Value::Null;

/// The `root` / `sequence` / `mapping` / `simple_key` arguments of
/// `Emitter.expect_node` (`sequence_context` is never read, so it is not kept).
#[derive(Debug, Clone, Copy, Default)]
struct Context {
    root: bool,
    mapping: bool,
    simple_key: bool,
}

impl Context {
    const ROOT: Self = Self {
        root: true,
        mapping: false,
        simple_key: false,
    };
    const SEQUENCE: Self = Self {
        root: false,
        mapping: false,
        simple_key: false,
    };
    const MAPPING: Self = Self {
        root: false,
        mapping: true,
        simple_key: false,
    };
    const SIMPLE_KEY: Self = Self {
        root: false,
        mapping: true,
        simple_key: true,
    };
}

/// PyYAML's `Emitter` with `Serializer` / `Representer` folded in.
struct Emitter<'o> {
    out: &'o mut String,
    opts: DumpOptions,

    /// The current indentation level and the stack of previous indents.
    indents: Vec<Option<usize>>,
    indent: Option<usize>,
    flow_level: usize,

    // Contexts.
    root_context: bool,
    mapping_context: bool,
    simple_key_context: bool,

    // Characteristics of the last emitted character.
    column: usize,
    whitespace: bool,
    indention: bool,

    /// Whether the document requires an explicit document indicator.
    open_ended: bool,

    best_indent: usize,
    best_width: usize,
}

impl<'o> Emitter<'o> {
    fn new(out: &'o mut String, opts: DumpOptions) -> Self {
        Self {
            out,
            opts,
            indents: Vec::new(),
            indent: None,
            flow_level: 0,
            root_context: false,
            mapping_context: false,
            simple_key_context: false,
            column: 0,
            whitespace: true,
            indention: true,
            open_ended: false,
            best_indent: 2,
            best_width: 80,
        }
    }

    /// `yaml.dump(value)`: stream start, one implicit document, stream end.
    fn emit_document(&mut self, value: &Value) {
        // expect_document_start (first, implicit: `check_empty_document` is
        // false because the root scalar's tag is never `None`).
        self.expect_node(value, Context::ROOT);
        // expect_document_end
        self.write_indent();
        // expect_document_start, on the StreamEndEvent
        if self.open_ended {
            self.write_indicator("...", true, false, false);
            self.write_indent();
        }
    }

    fn increase_indent(&mut self, flow: bool, indentless: bool) {
        self.indents.push(self.indent);
        match self.indent {
            None => self.indent = Some(if flow { self.best_indent } else { 0 }),
            Some(indent) if !indentless => self.indent = Some(indent + self.best_indent),
            Some(_) => {}
        }
    }

    fn pop_indent(&mut self) {
        self.indent = self.indents.pop().flatten();
    }

    // ── Node handlers ────────────────────────────────────────────────────────

    fn expect_node(&mut self, value: &Value, ctx: Context) {
        match value {
            Value::List(items) => {
                self.set_context(ctx);
                self.expect_sequence(items);
            }
            // `Representer.represent_tuple`: a sequence tagged `!!python/tuple`.
            Value::Tuple(items) => {
                self.set_context(ctx);
                self.write_indicator("!!python/tuple", true, false, false);
                self.expect_sequence(items);
            }
            Value::Map(map) => {
                self.set_context(ctx);
                let entries = self.mapping_entries(map);
                self.expect_mapping(&entries);
            }
            // A dict whose keys are not all strings (`represent_dict`).
            Value::Dict(dict) => {
                self.set_context(ctx);
                let pairs: Vec<(&Value, &Value)> = dict.iter().collect();
                let entries = self.sorted_entries(pairs);
                self.expect_mapping(&entries);
            }
            // `SafeRepresenter.represent_set`: a `!!set` mapping of the elements to null.
            Value::Set(set) => {
                self.set_context(ctx);
                self.write_indicator("!!set", true, false, false);
                let pairs: Vec<(&Value, &Value)> = set.iter().map(|k| (k, &NULL)).collect();
                let entries = self.sorted_entries(pairs);
                self.expect_mapping(&entries);
            }
            scalar => {
                let (text, tag) = represent_scalar(scalar);
                let analysis = analyze_scalar(&text, self.opts.allow_unicode);
                self.expect_scalar(&text, tag, &analysis, ctx);
            }
        }
    }

    fn expect_sequence(&mut self, items: &[Value]) {
        if self.flow_level > 0 || self.opts.default_flow_style || items.is_empty() {
            self.expect_flow_sequence(items);
        } else {
            self.expect_block_sequence(items);
        }
    }

    fn expect_mapping(&mut self, entries: &[(Key<'_>, &Value)]) {
        if self.flow_level > 0 || self.opts.default_flow_style || entries.is_empty() {
            self.expect_flow_mapping(entries);
        } else {
            self.expect_block_mapping(entries);
        }
    }

    /// `represent_mapping` for items with arbitrary keys: `sorted(mapping)` when
    /// `sort_keys`, which Python abandons (keeping the insertion order) when two
    /// keys do not compare.
    fn sorted_entries<'v>(&self, pairs: Vec<(&'v Value, &'v Value)>) -> Vec<(Key<'v>, &'v Value)> {
        let mut pairs = pairs;
        if self.opts.sort_keys {
            let comparable = pairs.iter().enumerate().all(|(i, (a, _))| {
                pairs[i + 1..]
                    .iter()
                    .all(|(b, _)| crate::expr::py_order(a, b).is_some())
            });
            if comparable {
                pairs.sort_by(|(a, _), (b, _)| {
                    crate::expr::py_order(a, b)
                        .flatten()
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
            }
        }
        pairs.into_iter().map(|(k, v)| (Key::Value(k), v)).collect()
    }

    fn set_context(&mut self, ctx: Context) {
        self.root_context = ctx.root;
        self.mapping_context = ctx.mapping;
        self.simple_key_context = ctx.simple_key;
    }

    /// `represent_mapping`: the items, sorted when `sort_keys`.
    fn mapping_entries<'v>(
        &self,
        map: &'v indexmap::IndexMap<String, Value>,
    ) -> Vec<(Key<'v>, &'v Value)> {
        let mut entries: Vec<(&str, &Value)> = map.iter().map(|(k, v)| (k.as_str(), v)).collect();
        if self.opts.sort_keys {
            entries.sort_by(|a, b| a.0.cmp(b.0));
        }
        entries.into_iter().map(|(k, v)| (Key::Str(k), v)).collect()
    }

    /// `expect_node` for a scalar, then `process_tag` and `expect_scalar`.
    fn expect_scalar(&mut self, text: &str, tag: Tag, analysis: &ScalarAnalysis, ctx: Context) {
        self.set_context(ctx);
        // The Serializer's implicit flags: does a plain scalar with this text
        // read back as this tag, and is the tag the default (`str`) one.
        let implicit = (resolve_implicit(text) == tag, tag == Tag::Str);
        // process_tag
        let style = self.choose_scalar_style(analysis, implicit.0);
        let implicit_enough = if style == Style::Plain {
            implicit.0
        } else {
            implicit.1
        };
        if !implicit_enough {
            self.write_indicator(tag.prepared(), true, false, false);
        }
        // expect_scalar
        self.increase_indent(true, false);
        // process_scalar
        let split = !self.simple_key_context;
        match style {
            Style::Double => self.write_double_quoted(text, split),
            Style::Single => self.write_single_quoted(text, split),
            Style::Plain => self.write_plain(text, split),
        }
        self.pop_indent();
    }

    // ── Flow sequence and mapping ────────────────────────────────────────────

    fn expect_flow_sequence(&mut self, items: &[Value]) {
        self.write_indicator("[", true, true, false);
        self.flow_level += 1;
        self.increase_indent(true, false);
        for (i, item) in items.iter().enumerate() {
            if i > 0 {
                self.write_indicator(",", false, false, false);
            }
            if self.column > self.best_width {
                self.write_indent();
            }
            self.expect_node(item, Context::SEQUENCE);
        }
        self.pop_indent();
        self.flow_level -= 1;
        self.write_indicator("]", false, false, false);
    }

    fn expect_flow_mapping(&mut self, entries: &[(Key<'_>, &Value)]) {
        self.write_indicator("{", true, true, false);
        self.flow_level += 1;
        self.increase_indent(true, false);
        for (i, (key, value)) in entries.iter().enumerate() {
            if i > 0 {
                self.write_indicator(",", false, false, false);
            }
            if self.column > self.best_width {
                self.write_indent();
            }
            match key {
                Key::Str(key) => {
                    let analysis = analyze_scalar(key, self.opts.allow_unicode);
                    if check_simple_key(&analysis) {
                        self.expect_scalar(key, Tag::Str, &analysis, Context::SIMPLE_KEY);
                        self.write_indicator(":", false, false, false);
                    } else {
                        self.write_indicator("?", true, false, false);
                        self.expect_scalar(key, Tag::Str, &analysis, Context::MAPPING);
                        self.flow_mapping_value_indicator();
                    }
                }
                Key::Value(key) => {
                    if self.value_key_is_simple(key) {
                        self.expect_node(key, Context::SIMPLE_KEY);
                        self.write_indicator(":", false, false, false);
                    } else {
                        self.write_indicator("?", true, false, false);
                        self.expect_node(key, Context::MAPPING);
                        self.flow_mapping_value_indicator();
                    }
                }
            }
            self.expect_node(value, Context::MAPPING);
        }
        self.pop_indent();
        self.flow_level -= 1;
        self.write_indicator("}", false, false, false);
    }

    /// `expect_flow_mapping_value`'s `:` after a complex key.
    fn flow_mapping_value_indicator(&mut self) {
        if self.column > self.best_width {
            self.write_indent();
        }
        self.write_indicator(":", true, false, false);
    }

    /// `check_simple_key` for a key that is any [`Value`]: a scalar of fewer
    /// than 128 characters (counting its tag) that is neither empty nor
    /// multi-line, or an empty collection.
    fn value_key_is_simple(&self, key: &Value) -> bool {
        match key {
            Value::List(items) | Value::Tuple(items) => items.is_empty(),
            Value::Map(m) => m.is_empty(),
            Value::Dict(d) => d.is_empty(),
            Value::Set(s) => s.is_empty(),
            scalar => {
                let (text, tag) = represent_scalar(scalar);
                let analysis = analyze_scalar(&text, self.opts.allow_unicode);
                tag.prepared().len() + analysis.len < 128 && !analysis.empty && !analysis.multiline
            }
        }
    }

    // ── Block sequence and mapping ───────────────────────────────────────────

    fn expect_block_sequence(&mut self, items: &[Value]) {
        let indentless = self.mapping_context && !self.indention;
        self.increase_indent(false, indentless);
        for item in items {
            self.write_indent();
            self.write_indicator("-", true, false, true);
            self.expect_node(item, Context::SEQUENCE);
        }
        self.pop_indent();
    }

    fn expect_block_mapping(&mut self, entries: &[(Key<'_>, &Value)]) {
        self.increase_indent(false, false);
        for (key, value) in entries {
            self.write_indent();
            match key {
                Key::Str(key) => {
                    let analysis = analyze_scalar(key, self.opts.allow_unicode);
                    if check_simple_key(&analysis) {
                        self.expect_scalar(key, Tag::Str, &analysis, Context::SIMPLE_KEY);
                        // expect_block_mapping_simple_value
                        self.write_indicator(":", false, false, false);
                    } else {
                        self.write_indicator("?", true, false, true);
                        self.expect_scalar(key, Tag::Str, &analysis, Context::MAPPING);
                        // expect_block_mapping_value
                        self.write_indent();
                        self.write_indicator(":", true, false, true);
                    }
                }
                Key::Value(key) => {
                    if self.value_key_is_simple(key) {
                        self.expect_node(key, Context::SIMPLE_KEY);
                        self.write_indicator(":", false, false, false);
                    } else {
                        self.write_indicator("?", true, false, true);
                        self.expect_node(key, Context::MAPPING);
                        self.write_indent();
                        self.write_indicator(":", true, false, true);
                    }
                }
            }
            self.expect_node(value, Context::MAPPING);
        }
        self.pop_indent();
    }

    // ── Scalar style ─────────────────────────────────────────────────────────

    /// `choose_scalar_style` with `event.style` unset and `canonical` off.
    fn choose_scalar_style(&self, analysis: &ScalarAnalysis, implicit_plain: bool) -> Style {
        if implicit_plain
            && !(self.simple_key_context && (analysis.empty || analysis.multiline))
            && ((self.flow_level > 0 && analysis.allow_flow_plain)
                || (self.flow_level == 0 && analysis.allow_block_plain))
        {
            return Style::Plain;
        }
        if analysis.allow_single_quoted && !(self.simple_key_context && analysis.multiline) {
            return Style::Single;
        }
        Style::Double
    }

    // ── Writers ──────────────────────────────────────────────────────────────

    fn write_indicator(
        &mut self,
        indicator: &str,
        need_whitespace: bool,
        whitespace: bool,
        indention: bool,
    ) {
        if !(self.whitespace || !need_whitespace) {
            self.out.push(' ');
            self.column += 1;
        }
        self.out.push_str(indicator);
        self.whitespace = whitespace;
        self.indention = self.indention && indention;
        self.column += indicator.chars().count();
        self.open_ended = false;
    }

    fn write_indent(&mut self) {
        let indent = self.indent.unwrap_or(0);
        if !self.indention || self.column > indent || (self.column == indent && !self.whitespace) {
            self.write_line_break(None);
        }
        if self.column < indent {
            self.whitespace = true;
            self.out
                .extend(std::iter::repeat_n(' ', indent - self.column));
            self.column = indent;
        }
    }

    fn write_line_break(&mut self, data: Option<char>) {
        self.whitespace = true;
        self.indention = true;
        self.column = 0;
        self.out.push(data.unwrap_or('\n'));
    }

    /// Write `text`, advancing the column by its length in characters.
    fn write_text(&mut self, text: &str) {
        self.out.push_str(text);
        self.column += if text.is_ascii() {
            text.len()
        } else {
            text.chars().count()
        };
    }

    fn write_chars(&mut self, chars: &[char]) {
        self.out.extend(chars);
        self.column += chars.len();
    }

    fn write_single_quoted(&mut self, text: &str, split: bool) {
        self.write_indicator("'", true, false, false);
        if !text.contains([' ', '\'', '\n', '\u{85}', '\u{2028}', '\u{2029}']) {
            // No spaces, breaks or quotes: the loop below would write the
            // whole text in its final iteration.
            self.write_text(text);
            self.write_indicator("'", false, false, false);
            return;
        }
        let chars: Vec<char> = text.chars().collect();
        let len = chars.len();
        let mut spaces = false;
        let mut breaks = false;
        let mut start = 0;
        let mut end = 0;
        while end <= len {
            let ch = chars.get(end).copied();
            if spaces {
                if ch != Some(' ') {
                    if start + 1 == end
                        && self.column > self.best_width
                        && split
                        && start != 0
                        && end != len
                    {
                        self.write_indent();
                    } else {
                        self.write_chars(&chars[start..end]);
                    }
                    start = end;
                }
            } else if breaks {
                if ch.is_none_or(|c| !is_break(c)) {
                    if chars[start] == '\n' {
                        self.write_line_break(None);
                    }
                    for &br in &chars[start..end] {
                        self.write_line_break(if br == '\n' { None } else { Some(br) });
                    }
                    self.write_indent();
                    start = end;
                }
            } else if ch.is_none_or(|c| c == ' ' || is_break(c) || c == '\'') && start < end {
                self.write_chars(&chars[start..end]);
                start = end;
            }
            if ch == Some('\'') {
                self.out.push_str("''");
                self.column += 2;
                start = end + 1;
            }
            if let Some(c) = ch {
                spaces = c == ' ';
                breaks = is_break(c);
            }
            end += 1;
        }
        self.write_indicator("'", false, false, false);
    }

    fn write_double_quoted(&mut self, text: &str, split: bool) {
        self.write_indicator("\"", true, false, false);
        let chars: Vec<char> = text.chars().collect();
        let len = chars.len();
        let allow_unicode = self.opts.allow_unicode;
        let mut start = 0usize;
        let mut end = 0usize;
        while end <= len {
            let ch = chars.get(end).copied();
            let needs_escape = ch.is_none_or(|c| {
                matches!(
                    c,
                    '"' | '\\' | '\u{85}' | '\u{2028}' | '\u{2029}' | '\u{FEFF}'
                ) || !(('\x20'..='\x7E').contains(&c)
                    || (allow_unicode
                        && (('\u{A0}'..='\u{D7FF}').contains(&c)
                            || ('\u{E000}'..='\u{FFFD}').contains(&c))))
            });
            if needs_escape {
                if start < end {
                    self.write_chars(&chars[start..end]);
                    start = end;
                }
                if let Some(c) = ch {
                    let data = escape_char(c);
                    self.out.push_str(&data);
                    self.column += data.len();
                    start = end + 1;
                }
            }
            // `end - start` is -1 right after an escape, as in Python.
            if 0 < end
                && end + 1 < len
                && (ch == Some(' ') || start >= end)
                && (self.column + end) as isize - start as isize > self.best_width as isize
                && split
            {
                if start < end {
                    self.write_chars(&chars[start..end]);
                    start = end;
                }
                self.out.push('\\');
                self.column += 1;
                self.write_indent();
                self.whitespace = false;
                self.indention = false;
                if chars[start] == ' ' {
                    self.out.push('\\');
                    self.column += 1;
                }
            }
            end += 1;
        }
        self.write_indicator("\"", false, false, false);
    }

    fn write_plain(&mut self, text: &str, split: bool) {
        if self.root_context {
            self.open_ended = true;
        }
        if text.is_empty() {
            return;
        }
        if !self.whitespace {
            self.out.push(' ');
            self.column += 1;
        }
        self.whitespace = false;
        self.indention = false;
        if !text.contains(' ') {
            // Plain scalars never hold line breaks (`analyze_scalar` forbids
            // them), so without spaces the loop writes the text in one piece.
            self.write_text(text);
            return;
        }
        let chars: Vec<char> = text.chars().collect();
        let len = chars.len();
        let mut spaces = false;
        let mut start = 0;
        let mut end = 0;
        while end <= len {
            let ch = chars.get(end).copied();
            if spaces {
                if ch != Some(' ') {
                    if start + 1 == end && self.column > self.best_width && split {
                        self.write_indent();
                        self.whitespace = false;
                        self.indention = false;
                    } else {
                        self.write_chars(&chars[start..end]);
                    }
                    start = end;
                }
            } else if ch.is_none_or(|c| c == ' ') {
                self.write_chars(&chars[start..end]);
                start = end;
            }
            if let Some(c) = ch {
                spaces = c == ' ';
            }
            end += 1;
        }
    }
}

/// `check_simple_key` for a scalar key: a `!!str` tag adds five characters to
/// the length that must stay under 128.
fn check_simple_key(analysis: &ScalarAnalysis) -> bool {
    Tag::Str.prepared().len() + analysis.len < 128 && !analysis.empty && !analysis.multiline
}

/// The `write_double_quoted` escape for `ch` (`ESCAPE_REPLACEMENTS`, else
/// `\xXX`, `\uXXXX` or `\UXXXXXXXX`).
fn escape_char(ch: char) -> String {
    let named = match ch {
        '\0' => '0',
        '\x07' => 'a',
        '\x08' => 'b',
        '\x09' => 't',
        '\x0A' => 'n',
        '\x0B' => 'v',
        '\x0C' => 'f',
        '\x0D' => 'r',
        '\x1B' => 'e',
        '"' => '"',
        '\\' => '\\',
        '\u{85}' => 'N',
        '\u{A0}' => '_',
        '\u{2028}' => 'L',
        '\u{2029}' => 'P',
        _ => {
            let code = ch as u32;
            return if code <= 0xFF {
                format!("\\x{code:02X}")
            } else if code <= 0xFFFF {
                format!("\\u{code:04X}")
            } else {
                format!("\\U{code:08X}")
            };
        }
    };
    format!("\\{named}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(json: &str) -> Value {
        serde_json::from_str(json).unwrap()
    }

    fn block(value: &Value) -> String {
        dump_yaml(value, DumpOptions::default())
    }

    // Expected strings below were produced by upstream's `dump_yaml` (PyYAML
    // 6.0.3); the full differential corpus is `tests/pyyaml_differential.rs`.

    #[test]
    fn root_plain_scalars_get_a_document_end_marker() {
        assert_eq!(block(&v("\"abc\"")), "abc\n...\n");
        assert_eq!(block(&v("5")), "5\n...\n");
        assert_eq!(block(&Value::Null), "null\n...\n");
        assert_eq!(block(&Value::Float(f64::NAN)), ".nan\n...\n");
        // A quoted scalar needs no marker.
        assert_eq!(block(&v("\"yes\"")), "'yes'\n");
    }

    #[test]
    fn empty_containers_and_sequence_indentation() {
        assert_eq!(block(&v("{}")), "{}\n");
        assert_eq!(block(&v("[]")), "[]\n");
        assert_eq!(block(&v(r#"{"a": [], "b": {}}"#)), "a: []\nb: {}\n");
        // Sequences inside mappings are not indented; keys are sorted.
        assert_eq!(
            block(&v(r#"{"b": 1, "a": [{"x": 1}, 2]}"#)),
            "a:\n- x: 1\n- 2\nb: 1\n"
        );
    }

    #[test]
    fn multi_line_strings_are_single_quoted_with_blank_lines() {
        assert_eq!(block(&v("\"a\\nb\"")), "'a\n\n  b'\n");
    }

    #[test]
    fn long_keys_use_the_explicit_key_syntax() {
        let key = "k".repeat(130);
        let text = block(&v(&format!(r#"{{"{key}": 1}}"#)));
        assert_eq!(text, format!("? {key}\n: 1\n"));
    }

    #[test]
    fn flow_style_without_unicode_escapes_non_ascii() {
        let opts = DumpOptions {
            default_flow_style: true,
            allow_unicode: false,
            sort_keys: true,
        };
        let value = Value::List(vec![Value::Float(1e20), Value::Str("\u{e9}".into())]);
        assert_eq!(dump_yaml(&value, opts), "[1.0e+20, \"\\xE9\"]\n");
    }

    #[test]
    fn resolver_matches_pyyaml_implicit_types() {
        for (text, tag) in [
            ("yes", Tag::Bool),
            ("1e3", Tag::Str),
            ("1.0e+3", Tag::Float),
            ("0x1F", Tag::Int),
            ("1:30", Tag::Int),
            ("1:30.5", Tag::Float),
            ("2024-01-01", Tag::Timestamp),
            ("~", Tag::Null),
            ("", Tag::Null),
            ("<<", Tag::Merge),
            (".5", Tag::Float),
            ("-.inf", Tag::Float),
            ("abc", Tag::Str),
        ] {
            assert_eq!(resolve_implicit(text), tag, "{text:?}");
        }
    }

    /// Tuples, sets and dicts with non-string keys against the text upstream's
    /// `dump_yaml` (PyYAML's `Dumper`) writes for them; see
    /// `tools/gen_pyyaml_container_cases.py`.
    #[test]
    fn containers_dump_as_pyyaml_does() {
        let cases: serde_json::Value =
            serde_json::from_str(include_str!("../tests/data/pyyaml_containers.json")).unwrap();
        let mut failures = Vec::new();
        for case in cases.as_array().unwrap() {
            let expr = case["expr"].as_str().unwrap();
            let value = crate::expr::eval_expr(expr).unwrap();
            for (name, opts) in [
                ("block_sorted", DumpOptions::default()),
                (
                    "block_unsorted",
                    DumpOptions {
                        sort_keys: false,
                        ..DumpOptions::default()
                    },
                ),
                (
                    "flow",
                    DumpOptions {
                        default_flow_style: true,
                        allow_unicode: false,
                        sort_keys: true,
                    },
                ),
            ] {
                let want = case[name].as_str().unwrap();
                let got = dump_yaml(&value, opts);
                if got != want {
                    failures.push(format!("{expr} [{name}]:\n want {want:?}\n got  {got:?}"));
                }
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }
}
