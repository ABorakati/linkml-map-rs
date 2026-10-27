//! Python `str` method semantics used by the string built-ins.
//!
//! The scalar string functions in upstream `eval_utils._SCALAR_FUNCTIONS` are the
//! unbound `str` methods themselves (`str.upper`, `str.title`, `str.split`, ...),
//! so their behaviour is CPython's, not Rust's. Where the two differ this module
//! reproduces CPython (Objects/unicodeobject.c): the whitespace set, `title`
//! and `capitalize` (full titlecase mapping, `Cased`-driven word starts and the
//! final-sigma rule), and whitespace-run `split`.
//!
//! PARITY: the Unicode data in [`super::pytables`] comes from CPython 3.13
//! (Unicode 15.1); `upper`/`lower` use Rust's own tables, so characters added in
//! later Unicode versions are the only possible difference.

use std::borrow::Cow;

use super::pytables::{
    CASE_IGNORABLE, DECIMAL_DIGITS, PY_SPACE, TITLE_EXCEPTIONS, TITLECASE_LETTERS,
};

/// Whether `cp` falls inside one of the sorted, inclusive `ranges`.
fn in_ranges(ranges: &[(u32, u32)], cp: u32) -> bool {
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
pub(super) fn is_py_space(c: char) -> bool {
    in_ranges(PY_SPACE, c as u32)
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
pub(super) fn ascii_digits(s: &str) -> Cow<'_, str> {
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

/// Unicode `Cased` (`Lowercase` + `Uppercase` + titlecase letters).
fn is_cased(c: char) -> bool {
    c.is_lowercase() || c.is_uppercase() || TITLECASE_LETTERS.binary_search(&(c as u32)).is_ok()
}

fn is_case_ignorable(c: char) -> bool {
    in_ranges(CASE_IGNORABLE, c as u32)
}

/// `str.upper()`.
pub(super) fn upper(s: &str) -> String {
    s.to_uppercase()
}

/// `str.lower()` (including the final-sigma rule, which `str::to_lowercase`
/// implements too).
pub(super) fn lower(s: &str) -> String {
    s.to_lowercase()
}

/// Append `_PyUnicode_ToTitleFull(c)`.
fn push_title(out: &mut String, c: char) {
    match TITLE_EXCEPTIONS.binary_search_by_key(&(c as u32), |&(cp, _)| cp) {
        Ok(i) => out.push_str(TITLE_EXCEPTIONS[i].1),
        Err(_) => out.extend(c.to_uppercase()),
    }
}

/// Append CPython's `lower_ucs4` for `chars[i]`: the full lowercase mapping, with
/// U+03A3 (capital sigma) becoming final sigma in the Final_Sigma context.
fn push_lower(out: &mut String, chars: &[char], i: usize) {
    let c = chars[i];
    if c != '\u{3a3}' {
        out.extend(c.to_lowercase());
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
    pass2.to_lowercase()
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
            out.extend(chars[0].to_uppercase());
            i = 1;
        } else if chars[i] == '_' && chars.get(i + 1).is_some_and(|&n| n != '\n') {
            out.extend(chars[i + 1].to_uppercase());
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
    let mut out: String = first.to_lowercase().collect();
    out.extend(camelize(string).chars().skip(1));
    Some(out)
}
