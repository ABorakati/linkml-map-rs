//! Python `str` method semantics used by the string built-ins.
//!
//! The scalar string functions in upstream `eval_utils._SCALAR_FUNCTIONS` are the
//! unbound `str` methods themselves (`str.upper`, `str.title`, `str.split`, ...),
//! so their behaviour is CPython's, not Rust's. Where the two differ this module
//! reproduces CPython (Objects/unicodeobject.c): the whitespace set, `title`
//! and `capitalize` (full titlecase mapping, `Cased`-driven word starts and the
//! final-sigma rule), and whitespace-run `split`.
//!
//! PARITY: the Unicode data in [`super::pytables`] (case mappings, `Cased`,
//! `Case_Ignorable`, decimal digits, whitespace, printable characters) is read
//! from CPython 3.13 (Unicode 15.1) by `tools/gen_pytables.py`, not taken from
//! Rust's `char`, whose Unicode version is newer. Regenerate it to follow a
//! newer Python.

use std::borrow::Cow;

use super::pytables::{
    CASE_IGNORABLE, CASED, CASEFOLD_RUNS, CASEFOLD_SPECIAL, DECIMAL_DIGITS, LOWER_RUNS,
    LOWER_SPECIAL, PY_SPACE, TITLE_EXCEPTIONS, UPPER_RUNS, UPPER_SPECIAL,
};

/// Whether `cp` falls inside one of the sorted, inclusive `ranges`.
pub(crate) fn in_ranges(ranges: &[(u32, u32)], cp: u32) -> bool {
    ranges
        .binary_search_by(|&(lo, hi)| {
            if cp < lo {
                std::cmp::Ordering::Greater
            } else if cp > hi {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

/// `str.isspace()` for one character: the set `str.strip()`/`str.split()` use by
/// default. Wider than Rust's `char::is_whitespace` (it adds U+001C..U+001F).
pub(crate) fn is_py_space(c: char) -> bool {
    in_ranges(PY_SPACE, c as u32)
}

/// `Py_ISSPACE`: the ASCII whitespace `int()` and `float()` strip once the text
/// has been through [`ascii_digits`]. Narrower than [`is_py_space`]: U+001C to
/// U+001F are `str.isspace()` but not `Py_ISSPACE`, so `float("\x1c1")` is an
/// error.
pub(super) fn is_c_space(c: char) -> bool {
    matches!(c, ' ' | '\t'..='\r')
}

/// General category `Nd`: what `\d` matches in a Python `str` pattern.
pub(super) fn is_decimal_digit(c: char) -> bool {
    c.is_ascii_digit() || (!c.is_ascii() && in_ranges(DECIMAL_DIGITS, c as u32))
}

/// The decimal value (0-9) of a Unicode `Nd` character.
fn decimal_value(c: char) -> Option<u32> {
    if let Some(d) = c.to_digit(10) {
        return Some(d);
    }
    // Every `Nd` block runs 0-9 from its first code point, and the table merges
    // adjacent blocks at multiples of ten.
    DECIMAL_DIGITS
        .iter()
        .find(|&&(lo, hi)| (lo..=hi).contains(&(c as u32)))
        .map(|&(lo, _)| (c as u32 - lo) % 10)
}

/// `_PyUnicode_TransformDecimalAndSpaceToASCII`, which `int()` and `float()` apply
/// to a string first: Unicode decimal digits become ASCII digits, Unicode
/// whitespace becomes a space, and any other non-ASCII character becomes `?`.
pub(crate) fn ascii_digits(s: &str) -> Cow<'_, str> {
    if s.is_ascii() {
        return Cow::Borrowed(s);
    }
    Cow::Owned(
        s.chars()
            .map(|c| match c {
                _ if c.is_ascii() => c,
                _ if is_py_space(c) => ' ',
                _ => decimal_value(c)
                    .and_then(|d| char::from_digit(d, 10))
                    .unwrap_or('?'),
            })
            .collect(),
    )
}

/// Whether every `_` of `s` sits between two ASCII digits, the only place
/// PEP 515 allows one in a numeric literal (and in `int()`/`float()` text).
fn underscores_between_digits(s: &str) -> bool {
    let bytes = s.as_bytes();
    bytes.iter().enumerate().all(|(i, &b)| {
        b != b'_'
            || (i > 0
                && bytes[i - 1].is_ascii_digit()
                && bytes.get(i + 1).is_some_and(u8::is_ascii_digit))
    })
}

/// `float(text)` for a `str`: `PyFloat_FromString`. The text is first reduced to
/// ASCII ([`ascii_digits`]), surrounding `Py_ISSPACE` whitespace is stripped and
/// single underscores between digits are allowed; what is left must be a decimal
/// or scientific literal or an `inf`/`infinity`/`nan` spelling.
pub(crate) fn float_from_str(text: &str) -> Option<f64> {
    let normalised = ascii_digits(text);
    let trimmed = normalised.trim_matches(is_c_space);
    if trimmed.contains('_') {
        if !underscores_between_digits(trimmed) {
            return None;
        }
        return trimmed.replace('_', "").parse().ok();
    }
    trimmed.parse().ok()
}

/// `int(text)` for a `str` in base 10 whose value fits an `i64` (`None` for an
/// invalid literal and for a big integer): the surrounding whitespace, sign and
/// underscore rules of [`float_from_str`].
pub(crate) fn int_from_str(text: &str) -> Option<i64> {
    let normalised = ascii_digits(text);
    let trimmed = normalised.trim_matches(is_c_space);
    if trimmed.contains('_') {
        if !underscores_between_digits(trimmed) {
            return None;
        }
        return trimmed.replace('_', "").parse().ok();
    }
    trimmed.parse().ok()
}

/// `int(text)` for a `str` in base 10 as a value of any size (`None` for an
/// invalid literal and for more than 4300 digits, both a `ValueError`).
pub fn int_value_from_str(text: &str) -> Option<crate::value::Value> {
    if let Some(i) = int_from_str(text) {
        return Some(crate::value::Value::Int(i));
    }
    let digits = int_literal(text)?;
    let (negative, body) = match digits.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, digits.as_str()),
    };
    crate::pynum::parse_signed_digits(negative, body, 10)
        .ok()
        .map(crate::value::Value::from_bigint)
}

/// The digits of `int(text)` for a `str` in base 10, of any size: an optional `-`
/// followed by ASCII digits (`None` for an invalid literal). The surrounding
/// whitespace, Unicode digit and underscore rules are those of [`float_from_str`].
pub fn int_literal(text: &str) -> Option<String> {
    let normalised = ascii_digits(text);
    let trimmed = normalised.trim_matches(is_c_space);
    let cleaned = if trimmed.contains('_') {
        if !underscores_between_digits(trimmed) {
            return None;
        }
        Cow::Owned(trimmed.replace('_', ""))
    } else {
        Cow::Borrowed(trimmed)
    };
    let body = cleaned.strip_prefix(['+', '-']).unwrap_or(&cleaned);
    if body.is_empty() || !body.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(cleaned.strip_prefix('+').unwrap_or(&cleaned).to_owned())
}

/// The text `Decimal(text)` parses (`numeric_as_ascii` in `_decimal.c`): the
/// surrounding `str.isspace()` whitespace is stripped (U+001C to U+001F
/// included), then every `_` is dropped wherever it stands and Unicode decimal
/// digits become ASCII ones. Other non-ASCII characters become `?`.
pub(crate) fn decimal_text(text: &str) -> Cow<'_, str> {
    let trimmed = text.trim_matches(is_py_space);
    if trimmed.is_ascii() && !trimmed.contains('_') {
        return Cow::Borrowed(trimmed);
    }
    Cow::Owned(
        trimmed
            .chars()
            .filter(|&c| c != '_')
            .map(|c| match c {
                _ if c.is_ascii() => c,
                _ if is_py_space(c) => ' ',
                _ => decimal_value(c)
                    .and_then(|d| char::from_digit(d, 10))
                    .unwrap_or('?'),
            })
            .collect(),
    )
}

/// Unicode `Cased` (`Lowercase` + `Uppercase` + titlecase letters).
pub(crate) fn is_cased(c: char) -> bool {
    in_ranges(CASED, c as u32)
}

fn is_case_ignorable(c: char) -> bool {
    in_ranges(CASE_IGNORABLE, c as u32)
}

/// Apply a `(first, last, stride, delta)` run table (see `tools/gen_pytables.py`).
fn map_run(runs: &[(u32, u32, u32, i32)], c: char) -> char {
    let cp = c as u32;
    let after = runs.partition_point(|&(first, ..)| first <= cp);
    if let Some(&(first, last, stride, delta)) = after.checked_sub(1).map(|i| &runs[i])
        && cp <= last
        && (cp - first).is_multiple_of(stride)
    {
        return char::from_u32(cp.wrapping_add_signed(delta)).unwrap_or(c);
    }
    c
}

/// Append `str.upper()` of the single character `c`.
pub(crate) fn push_upper(out: &mut String, c: char) {
    if c.is_ascii() {
        out.push(c.to_ascii_uppercase());
    } else if let Ok(i) = UPPER_SPECIAL.binary_search_by_key(&(c as u32), |&(cp, _)| cp) {
        out.push_str(UPPER_SPECIAL[i].1);
    } else {
        out.push(map_run(UPPER_RUNS, c));
    }
}

/// Append `str.lower()` of the single character `c`, without the final-sigma
/// context (see [`push_lower`]).
pub(crate) fn push_lower_char(out: &mut String, c: char) {
    if c.is_ascii() {
        out.push(c.to_ascii_lowercase());
    } else if let Ok(i) = LOWER_SPECIAL.binary_search_by_key(&(c as u32), |&(cp, _)| cp) {
        out.push_str(LOWER_SPECIAL[i].1);
    } else {
        out.push(map_run(LOWER_RUNS, c));
    }
}

/// Append `str.casefold()` of the single character `c`.
pub(crate) fn push_casefold(out: &mut String, c: char) {
    if c.is_ascii() {
        out.push(c.to_ascii_lowercase());
    } else if let Ok(i) = CASEFOLD_SPECIAL.binary_search_by_key(&(c as u32), |&(cp, _)| cp) {
        out.push_str(CASEFOLD_SPECIAL[i].1);
    } else {
        out.push(map_run(CASEFOLD_RUNS, c));
    }
}

/// `str.upper()`.
pub(super) fn upper(s: &str) -> String {
    if s.is_ascii() {
        return s.to_ascii_uppercase();
    }
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        push_upper(&mut out, c);
    }
    out
}

/// `str.lower()`, including the final-sigma rule.
pub(super) fn lower(s: &str) -> String {
    if s.is_ascii() {
        return s.to_ascii_lowercase();
    }
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    for i in 0..chars.len() {
        push_lower(&mut out, &chars, i);
    }
    out
}

/// Append `_PyUnicode_ToTitleFull(c)`.
fn push_title(out: &mut String, c: char) {
    match TITLE_EXCEPTIONS.binary_search_by_key(&(c as u32), |&(cp, _)| cp) {
        Ok(i) => out.push_str(TITLE_EXCEPTIONS[i].1),
        Err(_) => push_upper(out, c),
    }
}

/// Append CPython's `lower_ucs4` for `chars[i]`: the full lowercase mapping, with
/// U+03A3 (capital sigma) becoming final sigma in the Final_Sigma context.
fn push_lower(out: &mut String, chars: &[char], i: usize) {
    let c = chars[i];
    if c != '\u{3a3}' {
        push_lower_char(out, c);
        return;
    }
    // \p{cased} \p{case-ignorable}* SIGMA !(\p{case-ignorable}* \p{cased})
    let before = chars[..i].iter().rev().find(|&&p| !is_case_ignorable(p));
    let mut final_sigma = before.is_some_and(|&p| is_cased(p));
    if final_sigma {
        let after = chars[i + 1..].iter().find(|&&n| !is_case_ignorable(n));
        final_sigma = !after.is_some_and(|&n| is_cased(n));
    }
    out.push(if final_sigma { '\u{3c2}' } else { '\u{3c3}' });
}

/// `str.title()`: a character that does not follow a cased character is
/// titlecased, every other one is lowercased. Digits, apostrophes and uncased
/// letters all end a word (`"they're".title()` is `"They'Re"`).
pub(super) fn title(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut previous_is_cased = false;
    for (i, &c) in chars.iter().enumerate() {
        if previous_is_cased {
            push_lower(&mut out, &chars, i);
        } else {
            push_title(&mut out, c);
        }
        previous_is_cased = is_cased(c);
    }
    out
}

/// `str.capitalize()`: the first character titlecased, the rest lowercased.
pub(super) fn capitalize(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    for (i, &c) in chars.iter().enumerate() {
        if i == 0 {
            push_title(&mut out, c);
        } else {
            push_lower(&mut out, &chars, i);
        }
    }
    out
}

/// `str.strip(chars)` / `lstrip` / `rstrip`; `chars` of `None` strips
/// whitespace as `str.isspace()` defines it.
pub(super) fn strip<'a>(s: &'a str, chars: Option<&str>, left: bool, right: bool) -> &'a str {
    let matches = |c: char| match chars {
        None => is_py_space(c),
        Some(set) => set.contains(c),
    };
    let mut s = s;
    if left {
        s = s.trim_start_matches(matches);
    }
    if right {
        s = s.trim_end_matches(matches);
    }
    s
}

/// `str.split()` with no separator: runs of whitespace separate fields and
/// leading/trailing whitespace is dropped. After `maxsplit` splits the remainder
/// keeps its trailing whitespace (`"  a  b  c  ".split(None, 1)` is
/// `["a", "b  c  "]`). A negative `maxsplit` means no limit.
pub(super) fn split_whitespace(s: &str, maxsplit: i64) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = s;
    let mut splits = 0_i64;
    loop {
        rest = rest.trim_start_matches(is_py_space);
        if rest.is_empty() {
            break;
        }
        if maxsplit >= 0 && splits >= maxsplit {
            out.push(rest.to_string());
            break;
        }
        let end = rest.find(is_py_space).unwrap_or(rest.len());
        out.push(rest[..end].to_string());
        rest = &rest[end..];
        splits += 1;
    }
    out
}

/// `str.split(sep, maxsplit)`; `None` for an empty separator (Python raises
/// `ValueError: empty separator`). A negative `maxsplit` means no limit.
pub(super) fn split_sep(s: &str, sep: &str, maxsplit: i64) -> Option<Vec<String>> {
    if sep.is_empty() {
        return None;
    }
    Some(if maxsplit < 0 {
        s.split(sep).map(str::to_string).collect()
    } else {
        let parts = usize::try_from(maxsplit)
            .unwrap_or(usize::MAX)
            .saturating_add(1);
        s.splitn(parts, sep).map(str::to_string).collect()
    })
}

// --- inflection (the `to_snake` / `to_camel` / `to_pascal` back end) ---

/// `inflection.underscore(word)`:
///
/// ```python
/// word = re.sub(r"([A-Z]+)([A-Z][a-z])", r"\1_\2", word)
/// word = re.sub(r"([a-z\d])([A-Z])", r"\1_\2", word)
/// word = word.replace("-", "_")
/// return word.lower()
/// ```
pub(super) fn underscore(word: &str) -> String {
    let chars: Vec<char> = word.chars().collect();
    // Pass 1: a run of two or more capitals followed by a lowercase letter
    // splits before the run's last capital (`HTTPResponse` -> `HTTP_Response`).
    let mut pass1 = String::with_capacity(word.len() + 4);
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_ascii_uppercase() {
            let mut j = i;
            while j < chars.len() && chars[j].is_ascii_uppercase() {
                j += 1;
            }
            if j - i >= 2 && j < chars.len() && chars[j].is_ascii_lowercase() {
                pass1.extend(&chars[i..j - 1]);
                pass1.push('_');
                pass1.push(chars[j - 1]);
            } else {
                pass1.extend(&chars[i..j]);
            }
            i = j;
        } else {
            pass1.push(chars[i]);
            i += 1;
        }
    }
    // Pass 2: a lowercase letter or decimal digit directly followed by a capital.
    let mut pass2 = String::with_capacity(pass1.len() + 4);
    let mut prev: Option<char> = None;
    for c in pass1.chars() {
        if c.is_ascii_uppercase()
            && prev.is_some_and(|p| p.is_ascii_lowercase() || is_decimal_digit(p))
        {
            pass2.push('_');
        }
        pass2.push(if c == '-' { '_' } else { c });
        prev = Some(c);
    }
    lower(&pass2)
}

/// `inflection.camelize(string)` (`uppercase_first_letter=True`):
/// `re.sub(r"(?:^|_)(.)", lambda m: m.group(1).upper(), string)`. `.` does not
/// match a newline, so `^` and `_` followed by one are left alone.
pub(super) fn camelize(string: &str) -> String {
    let chars: Vec<char> = string.chars().collect();
    let mut out = String::with_capacity(string.len());
    let mut i = 0;
    while i < chars.len() {
        if i == 0 && chars[0] != '\n' {
            push_upper(&mut out, chars[0]);
            i = 1;
        } else if chars[i] == '_' && chars.get(i + 1).is_some_and(|&n| n != '\n') {
            push_upper(&mut out, chars[i + 1]);
            i += 2;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// `inflection.camelize(string, uppercase_first_letter=False)`:
/// `string[0].lower() + camelize(string)[1:]`. `None` for the empty string,
/// where Python raises `IndexError`.
pub(super) fn camelize_lower(string: &str) -> Option<String> {
    let first = string.chars().next()?;
    let mut out = String::new();
    push_lower_char(&mut out, first);
    out.extend(camelize(string).chars().skip(1));
    Some(out)
}
