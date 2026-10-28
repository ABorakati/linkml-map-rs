//! Python's format-specification mini-language and `str.format`.
//!
//! f-string replacement fields with a format spec (`f"{x:>8.2f}"`) and the
//! `str.format` / `str.format_map` methods are `format(value, spec)` underneath,
//! so their output is CPython's (`Python/formatter_unicode.c`), not Rust's:
//! this module implements the spec parser and the `str`, `int` and `float`
//! formatters, and the `{}` template syntax.
//!
//! [`Value`] has no tuples, sets, dates or arbitrary objects, so `format()` of
//! those with a non-empty spec (a `TypeError` for every type but numbers and
//! strings, `date.__format__` aside) is an evaluation error here too.

use super::error::{ExprError, ExprResult};
use super::eval::{try_py_repr, try_py_str, type_name};
use crate::pynum::{self, BigInt};
use crate::value::Value;
use num_traits::{Signed, ToPrimitive};

/// The widest field or precision accepted, to bound the allocation a spec such
/// as `>99999999999` would make (CPython raises `MemoryError` or fills memory).
const MAX_WIDTH: usize = 10_000_000;

fn value_error(message: impl Into<String>) -> ExprError {
    ExprError::Eval(message.into())
}

/// A parsed format specification (`parse_internal_render_format_spec`).
#[derive(Debug, Clone)]
struct Spec {
    fill: char,
    align: Option<char>,
    sign: Option<char>,
    /// `z`: negative zero becomes positive zero after rounding.
    coerce_zero: bool,
    alternate: bool,
    width: usize,
    grouping: Option<char>,
    precision: Option<usize>,
    ty: Option<char>,
}

impl Spec {
    /// Parse `spec` for a value whose natural alignment is `default_align`.
    fn parse(spec: &str, default_align: char, type_name: &str) -> ExprResult<Self> {
        let chars: Vec<char> = spec.chars().collect();
        let is_align = |c: char| matches!(c, '<' | '>' | '=' | '^');
        let mut out = Self {
            fill: ' ',
            align: None,
            sign: None,
            coerce_zero: false,
            alternate: false,
            width: 0,
            grouping: None,
            precision: None,
            ty: None,
        };
        let mut pos = 0;
        let mut fill_given = false;
        if chars.len() >= 2 && is_align(chars[1]) {
            out.fill = chars[0];
            out.align = Some(chars[1]);
            fill_given = true;
            pos = 2;
        } else if !chars.is_empty() && is_align(chars[0]) {
            out.align = Some(chars[0]);
            pos = 1;
        }
        if let Some(&c @ ('+' | '-' | ' ')) = chars.get(pos) {
            out.sign = Some(c);
            pos += 1;
        }
        if chars.get(pos) == Some(&'z') {
            out.coerce_zero = true;
            pos += 1;
        }
        if chars.get(pos) == Some(&'#') {
            out.alternate = true;
            pos += 1;
        }
        if !fill_given && chars.get(pos) == Some(&'0') {
            out.fill = '0';
            if out.align.is_none() && default_align == '>' {
                out.align = Some('=');
            }
            pos += 1;
        }
        let digits = |pos: &mut usize| -> ExprResult<Option<usize>> {
            let start = *pos;
            while chars.get(*pos).is_some_and(char::is_ascii_digit) {
                *pos += 1;
            }
            if start == *pos {
                return Ok(None);
            }
            let text: String = chars[start..*pos].iter().collect();
            text.parse::<usize>()
                .map(Some)
                .map_err(|_| value_error("Too many decimal digits in format string"))
        };
        out.width = digits(&mut pos)?.unwrap_or(0);
        if let Some(&c @ (',' | '_')) = chars.get(pos) {
            out.grouping = Some(c);
            pos += 1;
        }
        if let Some(&c @ (',' | '_')) = chars.get(pos) {
            return Err(value_error(if Some(c) == out.grouping {
                format!("Cannot specify '{c}' with '{c}'.")
            } else {
                "Cannot specify both ',' and '_'.".to_owned()
            }));
        }
        if chars.get(pos) == Some(&'.') {
            pos += 1;
            out.precision = Some(
                digits(&mut pos)?
                    .ok_or_else(|| value_error("Format specifier missing precision"))?,
            );
        }
        match chars.len() - pos {
            0 => {}
            1 => out.ty = Some(chars[pos]),
            _ => {
                return Err(value_error(format!(
                    "Invalid format specifier '{spec}' for object of type '{type_name}'"
                )));
            }
        }
        if out.width > MAX_WIDTH || out.precision.is_some_and(|p| p > MAX_WIDTH) {
            return Err(value_error("format width or precision too large"));
        }
        if out.align.is_none() {
            out.align = Some(default_align);
        }
        Ok(out)
    }

    /// Pad `prefix` (sign and radix prefix) and `body` to the field width.
    fn pad(&self, prefix: &str, body: &str) -> String {
        let len = prefix.chars().count() + body.chars().count();
        let n = self.width.saturating_sub(len);
        let fill = |count: usize| self.fill.to_string().repeat(count);
        match self.align.unwrap_or('>') {
            '<' => format!("{prefix}{body}{}", fill(n)),
            '^' => format!("{}{prefix}{body}{}", fill(n / 2), fill(n - n / 2)),
            '=' => format!("{prefix}{}{body}", fill(n)),
            _ => format!("{}{prefix}{body}", fill(n)),
        }
    }
}

/// `format(value, spec)`.
///
/// # Errors
/// The `ValueError`s and `TypeError`s of CPython's formatters.
pub(super) fn format_value(value: &Value, spec: &str) -> ExprResult<String> {
    if spec.is_empty() {
        return try_py_str(value);
    }
    match value {
        Value::Str(s) => format_str(s, spec),
        Value::Bool(b) => format_int(&BigInt::from(*b), spec),
        Value::Int(i) => format_int(&BigInt::from(*i), spec),
        Value::BigInt(n) => format_int(n, spec),
        Value::Float(f) => format_float(*f, spec),
        Value::Complex(z) => format_complex(**z, spec),
        other => Err(value_error(format!(
            "unsupported format string passed to {}.__format__",
            python_type(other)
        ))),
    }
}

fn python_type(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::List(_) => "list",
        Value::Map(_) => "dict",
        other => type_name(other),
    }
}

// === str ===================================================================

fn format_str(s: &str, spec: &str) -> ExprResult<String> {
    let spec = Spec::parse(spec, '<', "str")?;
    if let Some(ty) = spec.ty.filter(|&t| t != 's') {
        return Err(value_error(format!(
            "Unknown format code '{ty}' for object of type 'str'"
        )));
    }
    if spec.sign.is_some() {
        return Err(value_error("Sign not allowed in string format specifier"));
    }
    if spec.coerce_zero {
        return Err(value_error(
            "Negative zero coercion (z) not allowed in format specifier",
        ));
    }
    if spec.alternate {
        return Err(value_error(
            "Alternate form (#) not allowed in string format specifier",
        ));
    }
    if spec.align == Some('=') {
        return Err(value_error(
            "'=' alignment not allowed in string format specifier",
        ));
    }
    if let Some(sep) = spec.grouping {
        return Err(value_error(format!("Cannot specify '{sep}' with 's'.")));
    }
    let body: String = match spec.precision {
        Some(p) => s.chars().take(p).collect(),
        None => s.to_owned(),
    };
    Ok(spec.pad("", &body))
}

// === int ===================================================================

/// Insert `sep` every `group` digits of `digits`, counting from the right.
fn group_digits(digits: &str, sep: char, group: usize) -> String {
    let n = digits.chars().count();
    let mut out = String::with_capacity(n + n / group);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (n - i).is_multiple_of(group) {
            out.push(sep);
        }
        out.push(c);
    }
    out
}

/// Zero-extend `digits` and group them so the field (with `prefix_len` columns
/// of sign and radix prefix before it) reaches `width`, as CPython's
/// `_PyUnicode_InsertThousandsGrouping` does for `0`-padded numbers.
fn zero_padded_grouped(
    digits: &str,
    sep: char,
    group: usize,
    width: usize,
    prefix_len: usize,
) -> String {
    let mut padded = digits.to_owned();
    let mut grouped = group_digits(&padded, sep, group);
    while grouped.chars().count() + prefix_len < width {
        padded.insert(0, '0');
        grouped = group_digits(&padded, sep, group);
    }
    // A separator never leads the number: CPython pads on with another zero.
    if grouped.starts_with(sep) {
        padded.insert(0, '0');
        grouped = group_digits(&padded, sep, group);
    }
    grouped
}

fn format_int(value: &BigInt, spec_text: &str) -> ExprResult<String> {
    let spec = Spec::parse(spec_text, '>', "int")?;
    let ty = spec.ty.unwrap_or('d');
    if matches!(ty, 'e' | 'E' | 'f' | 'F' | 'g' | 'G' | '%') {
        // `float(value)` raises `OverflowError` for an integer out of range.
        let float = pynum::big_to_f64(value)
            .ok_or_else(|| ExprError::Uncaught(pynum::INT_TOO_LARGE_FOR_FLOAT.into()))?;
        return format_float(float, spec_text);
    }
    if !matches!(ty, 'b' | 'o' | 'x' | 'X' | 'd' | 'n' | 'c') {
        return Err(value_error(format!(
            "Unknown format code '{ty}' for object of type 'int'"
        )));
    }
    if let Some(sep) = spec.grouping
        && (ty == 'c' || (sep == ',' && matches!(ty, 'b' | 'o' | 'x' | 'X')))
    {
        return Err(value_error(format!("Cannot specify '{sep}' with '{ty}'.")));
    }
    if spec.precision.is_some() {
        return Err(value_error(
            "Precision not allowed in integer format specifier",
        ));
    }
    if spec.coerce_zero {
        return Err(value_error(
            "Negative zero coercion (z) not allowed in format specifier",
        ));
    }
    if ty == 'c' {
        if spec.sign.is_some() {
            return Err(value_error(
                "Sign not allowed with integer format specifier 'c'",
            ));
        }
        if spec.alternate {
            return Err(value_error(
                "Alternate form (#) not allowed with integer format specifier 'c'",
            ));
        }
        let c = value
            .to_u32()
            .and_then(char::from_u32)
            .ok_or_else(|| value_error("%c arg not in range(0x110000)"))?;
        return Ok(spec.pad("", &c.to_string()));
    }
    let magnitude = value.magnitude();
    let digits = match ty {
        'b' => magnitude.to_str_radix(2),
        'o' => magnitude.to_str_radix(8),
        'x' => magnitude.to_str_radix(16),
        'X' => magnitude.to_str_radix(16).to_uppercase(),
        // Decimal conversion has CPython's digit limit.
        _ => pynum::big_to_string(&BigInt::from(magnitude.clone()))
            .map_err(|e| value_error(e.to_string()))?,
    };
    let mut prefix = String::new();
    if value.is_negative() {
        prefix.push('-');
    } else if let Some(sign @ ('+' | ' ')) = spec.sign {
        prefix.push(sign);
    }
    if spec.alternate {
        match ty {
            'b' => prefix.push_str("0b"),
            'o' => prefix.push_str("0o"),
            'x' => prefix.push_str("0x"),
            'X' => prefix.push_str("0X"),
            _ => {}
        }
    }
    let group = if matches!(ty, 'b' | 'o' | 'x' | 'X') {
        4
    } else {
        3
    };
    let body = match spec.grouping {
        None => digits,
        Some(sep) if spec.fill == '0' && spec.align == Some('=') => {
            zero_padded_grouped(&digits, sep, group, spec.width, prefix.chars().count())
        }
        Some(sep) => group_digits(&digits, sep, group),
    };
    Ok(spec.pad(&prefix, &body))
}

// === float =================================================================

/// `abs` in `f` notation with `precision` decimals.
fn fixed(abs: f64, precision: usize) -> String {
    format!("{abs:.precision$}")
}

/// `abs` in `e` notation: `d.ddde+XX` with at least two exponent digits.
fn exponent(abs: f64, precision: usize, upper: bool) -> String {
    let text = format!("{abs:.precision$e}");
    let (mantissa, exp) = text.split_once('e').expect("exponent form");
    let exp: i32 = exp.parse().expect("integer exponent");
    let e = if upper { 'E' } else { 'e' };
    let sign = if exp < 0 { '-' } else { '+' };
    format!("{mantissa}{e}{sign}{:02}", exp.abs())
}

/// `abs` in `g` notation with `precision` significant digits.
fn general(abs: f64, precision: usize, upper: bool, alternate: bool, add_dot_0: bool) -> String {
    let p = precision.max(1);
    let probe = format!("{abs:.prec$e}", prec = p - 1);
    let exp: i32 = probe
        .split_once('e')
        .and_then(|(_, e)| e.parse().ok())
        .expect("exponent form");
    // With `add_dot_0` (a spec with a precision but no type) CPython switches to
    // exponent notation one digit earlier: `format(100.0, '.3')` is `'1e+02'`.
    let limit = if add_dot_0 { p as i32 - 1 } else { p as i32 };
    let use_fixed = abs == 0.0 || (-4..limit).contains(&exp);
    let mut text = if use_fixed {
        let decimals = if abs == 0.0 {
            p - 1
        } else {
            (p as i32 - 1 - exp) as usize
        };
        fixed(abs, decimals)
    } else {
        exponent(abs, p - 1, upper)
    };
    if !alternate {
        // Drop trailing zeros of the mantissa, and a bare decimal point.
        let (mantissa, rest) = match text.find(['e', 'E']) {
            Some(i) => text.split_at(i),
            None => (text.as_str(), ""),
        };
        let mantissa = if mantissa.contains('.') {
            mantissa.trim_end_matches('0').trim_end_matches('.')
        } else {
            mantissa
        };
        text = format!("{mantissa}{rest}");
    } else if !text.contains('.') && !text.contains(['e', 'E']) {
        text.push('.');
    }
    if add_dot_0 && !text.contains(['.', 'e', 'E']) {
        text.push_str(".0");
    }
    text
}

fn format_float(value: f64, spec_text: &str) -> ExprResult<String> {
    let spec = Spec::parse(spec_text, '>', "float")?;
    let ty = spec.ty;
    if let Some(t) = ty
        && !matches!(t, 'e' | 'E' | 'f' | 'F' | 'g' | 'G' | 'n' | '%')
    {
        return Err(value_error(format!(
            "Unknown format code '{t}' for object of type 'float'"
        )));
    }
    if let Some(sep) = spec.grouping
        && ty == Some('n')
    {
        return Err(value_error(format!("Cannot specify '{sep}' with 'n'.")));
    }
    let upper = matches!(ty, Some('E' | 'F' | 'G'));
    let mut abs = value.abs();
    let negative = value.is_sign_negative() && !value.is_nan();
    let (mut text, number) = if value.is_finite() {
        if ty == Some('%') {
            abs *= 100.0;
        }
        let prec = spec.precision.unwrap_or(6);
        let text = match ty {
            Some('f' | 'F' | '%') => fixed(abs, prec),
            Some('e' | 'E') => exponent(abs, prec, upper),
            Some('g' | 'G' | 'n') => general(abs, prec, upper, spec.alternate, false),
            _ => match spec.precision {
                // No type: `repr`, or `g` that still shows a `.0` when given a precision.
                None => crate::pyfmt::py_float_repr(abs),
                Some(p) => general(abs, p, false, spec.alternate, true),
            },
        };
        (text, true)
    } else {
        let word = if value.is_nan() { "nan" } else { "inf" };
        let word = if upper {
            word.to_uppercase()
        } else {
            word.to_owned()
        };
        (word, false)
    };
    if ty == Some('%') && number {
        text.push('%');
    }
    let is_negative_zero = negative
        && number
        && text
            .trim_end_matches('%')
            .chars()
            .all(|c| matches!(c, '0' | '.'));
    let show_minus = negative && !(spec.coerce_zero && is_negative_zero);
    let mut prefix = String::new();
    if show_minus {
        prefix.push('-');
    } else if let Some(sign @ ('+' | ' ')) = spec.sign {
        prefix.push(sign);
    }
    if let (Some(sep), true) = (spec.grouping, number) {
        // Group the integer part only.
        let end = text.find(['.', 'e', 'E', '%']).unwrap_or(text.len());
        let (int_part, rest) = text.split_at(end);
        let grouped = if spec.fill == '0' && spec.align == Some('=') {
            let tail = rest.chars().count();
            zero_padded_grouped(
                int_part,
                sep,
                3,
                spec.width.saturating_sub(tail),
                prefix.chars().count(),
            )
        } else {
            group_digits(int_part, sep, 3)
        };
        text = format!("{grouped}{rest}");
    }
    Ok(spec.pad(&prefix, &text))
}

// === complex ===============================================================

/// The digits of one finite part of a complex number (`abs` of it) for type
/// `ty` (`None` is `repr`).
fn complex_part_text(
    abs: f64,
    ty: Option<char>,
    precision: Option<usize>,
    alternate: bool,
) -> String {
    let upper = matches!(ty, Some('E' | 'F' | 'G'));
    let mut text = match ty {
        Some('f' | 'F') => fixed(abs, precision.unwrap_or(6)),
        Some('e' | 'E') => exponent(abs, precision.unwrap_or(6), upper),
        Some('g' | 'G' | 'n') => general(abs, precision.unwrap_or(6), upper, alternate, false),
        // No type: `repr` without its `.0`, or `g` when given a precision.
        _ => match precision {
            None => {
                let repr = crate::pyfmt::py_float_repr(abs);
                repr.strip_suffix(".0").map_or(repr.clone(), str::to_owned)
            }
            Some(p) => general(abs, p, false, alternate, false),
        },
    };
    // `#` keeps a decimal point that would otherwise be dropped.
    if alternate && !text.contains('.') && !matches!(ty, Some('g' | 'G' | 'n')) {
        match text.find(['e', 'E']) {
            Some(at) => text.insert(at, '.'),
            None => text.push('.'),
        }
    }
    text
}

/// `format(z, spec)` for a `complex` (`format_complex_internal`).
pub(super) fn format_complex(
    z: crate::pynum::complex::Complex64,
    spec_text: &str,
) -> ExprResult<String> {
    let spec = Spec::parse(spec_text, '>', "complex")?;
    if spec.fill == '0' {
        return Err(value_error(
            "Zero padding is not allowed in complex format specifier",
        ));
    }
    if spec.align == Some('=') {
        return Err(value_error(
            "'=' alignment flag is not allowed in complex format specifier",
        ));
    }
    if let Some(t) = spec.ty
        && !matches!(t, 'e' | 'E' | 'f' | 'F' | 'g' | 'G' | 'n')
    {
        return Err(value_error(format!(
            "Unknown format code '{t}' for object of type 'complex'"
        )));
    }
    let ty = spec.ty;
    // With no type: like `str(z)`, a positive-zero real part is left out and
    // anything else puts the number in parentheses.
    let skip_re = ty.is_none() && z.re == 0.0 && z.re.is_sign_positive();
    let add_parens = ty.is_none() && !skip_re;
    let upper = matches!(ty, Some('E' | 'F' | 'G'));
    let part = |value: f64, always_sign: bool| -> String {
        let negative = value.is_sign_negative() && !value.is_nan();
        let mut text = if value.is_finite() {
            complex_part_text(value.abs(), ty, spec.precision, spec.alternate)
        } else if value.is_nan() {
            if upper { "NAN" } else { "nan" }.to_owned()
        } else if upper {
            "INF".to_owned()
        } else {
            "inf".to_owned()
        };
        let mantissa = text.split(['e', 'E']).next().unwrap_or("");
        let zero = value.is_finite() && mantissa.chars().all(|c| matches!(c, '0' | '.'));
        let minus = negative && !(spec.coerce_zero && zero);
        if let Some(sep) = spec.grouping.filter(|_| ty.is_some() && value.is_finite()) {
            let end = text.find(['.', 'e', 'E']).unwrap_or(text.len());
            let (int_part, rest) = text.split_at(end);
            text = format!("{}{rest}", group_digits(int_part, sep, 3));
        }
        let sign = if minus {
            "-"
        } else if always_sign {
            "+"
        } else {
            match spec.sign {
                Some('+') => "+",
                Some(' ') => " ",
                _ => "",
            }
        };
        format!("{sign}{text}")
    };
    let mut body = String::new();
    if add_parens {
        body.push('(');
    }
    if !skip_re {
        body.push_str(&part(z.re, false));
    }
    body.push_str(&part(z.im, !skip_re));
    body.push('j');
    if add_parens {
        body.push(')');
    }
    Ok(spec.pad("", &body))
}

// === str.format ===========================================================

/// `str.format(*args, **kwargs)`: `template` with `{}` fields filled from
/// `args` and `kwargs`.
///
/// # Errors
/// CPython's `ValueError`, `IndexError`, `KeyError` and `TypeError` cases.
pub(super) fn str_format(
    template: &str,
    args: &[Value],
    kwargs: &[(String, Value)],
) -> ExprResult<String> {
    let mut auto = AutoNumber::Unset;
    format_template(template, args, kwargs, &mut auto, 2)
}

#[derive(Clone, Copy, PartialEq)]
enum AutoNumber {
    Unset,
    Auto(usize),
    Manual,
}

/// `ascii(text)`: `text` (a `repr`) with every non-ASCII character escaped.
pub(super) fn ascii_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        let cp = c as u32;
        if cp < 0x80 {
            out.push(c);
        } else if cp < 0x100 {
            out.push_str(&format!("\\x{cp:02x}"));
        } else if cp < 0x10000 {
            out.push_str(&format!("\\u{cp:04x}"));
        } else {
            out.push_str(&format!("\\U{cp:08x}"));
        }
    }
    out
}

fn format_template(
    template: &str,
    args: &[Value],
    kwargs: &[(String, Value)],
    auto: &mut AutoNumber,
    depth: u32,
) -> ExprResult<String> {
    if depth == 0 {
        return Err(value_error("Max string recursion exceeded"));
    }
    let mut out = String::with_capacity(template.len());
    let chars: Vec<char> = template.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '{' if chars.get(i + 1) == Some(&'{') => {
                out.push('{');
                i += 2;
            }
            '}' if chars.get(i + 1) == Some(&'}') => {
                out.push('}');
                i += 2;
            }
            '}' => return Err(value_error("Single '}' encountered in format string")),
            '{' => {
                // The field runs to the matching `}`, nested braces included.
                let start = i + 1;
                let mut level = 1;
                let mut j = start;
                while j < chars.len() {
                    match chars[j] {
                        '{' => level += 1,
                        '}' => {
                            level -= 1;
                            if level == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    j += 1;
                }
                if level != 0 {
                    return Err(value_error(if start >= chars.len() {
                        "Single '{' encountered in format string"
                    } else {
                        "expected '}' before end of string"
                    }));
                }
                let field: String = chars[start..j].iter().collect();
                out.push_str(&render_field(&field, args, kwargs, auto, depth)?);
                i = j + 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    Ok(out)
}

fn render_field(
    field: &str,
    args: &[Value],
    kwargs: &[(String, Value)],
    auto: &mut AutoNumber,
    depth: u32,
) -> ExprResult<String> {
    // field_name [! conversion] [: spec]; the name may hold `[..]` with `!`/`:`.
    let mut name_end = field.len();
    let mut in_bracket = false;
    for (i, c) in field.char_indices() {
        match c {
            '[' => in_bracket = true,
            ']' => in_bracket = false,
            '!' | ':' if !in_bracket => {
                name_end = i;
                break;
            }
            _ => {}
        }
    }
    let (name, rest) = field.split_at(name_end);
    let mut conversion = None;
    let mut spec_text = "";
    let mut rest = rest;
    if let Some(after) = rest.strip_prefix('!') {
        let mut it = after.chars();
        conversion = it.next();
        rest = it.as_str();
        if conversion.is_none() {
            return Err(value_error(
                "end of string while looking for conversion specifier",
            ));
        }
        if !rest.is_empty() && !rest.starts_with(':') {
            return Err(value_error("expected ':' after conversion specifier"));
        }
    }
    if let Some(spec) = rest.strip_prefix(':') {
        spec_text = spec;
    }
    let value = lookup_field(name, args, kwargs, auto)?;
    let value = match conversion {
        None => value,
        Some('r') => Value::Str(try_py_repr(&value)?),
        Some('s') => Value::Str(try_py_str(&value)?),
        Some('a') => Value::Str(ascii_escape(&try_py_repr(&value)?)),
        Some(c) => {
            return Err(value_error(format!("Unknown conversion specifier {c}")));
        }
    };
    let spec = if spec_text.contains('{') {
        format_template(spec_text, args, kwargs, auto, depth - 1)?
    } else {
        spec_text.to_owned()
    };
    format_value(&value, &spec)
}

fn lookup_field(
    name: &str,
    args: &[Value],
    kwargs: &[(String, Value)],
    auto: &mut AutoNumber,
) -> ExprResult<Value> {
    let first_end = name.find(['.', '[']).unwrap_or(name.len());
    let (first, mut rest) = name.split_at(first_end);
    let mut value = if first.is_empty() {
        let index = match *auto {
            AutoNumber::Manual => {
                return Err(value_error(
                    "cannot switch from manual field specification to automatic field numbering",
                ));
            }
            AutoNumber::Unset => 0,
            AutoNumber::Auto(n) => n,
        };
        *auto = AutoNumber::Auto(index + 1);
        positional(args, index)?
    } else if first.bytes().all(|b| b.is_ascii_digit()) {
        if matches!(*auto, AutoNumber::Auto(_)) {
            return Err(value_error(
                "cannot switch from automatic field numbering to manual field specification",
            ));
        }
        *auto = AutoNumber::Manual;
        let index = first
            .parse::<usize>()
            .map_err(|_| value_error("Too many decimal digits in format string"))?;
        positional(args, index)?
    } else {
        kwargs
            .iter()
            .rev()
            .find(|(k, _)| k == first)
            .map(|(_, v)| v.clone())
            .ok_or_else(|| ExprError::Uncaught(format!("KeyError: '{first}'")))?
    };
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix('.') {
            let end = after.find(['.', '[']).unwrap_or(after.len());
            let (attr, tail) = after.split_at(end);
            if attr.is_empty() {
                return Err(value_error("Empty attribute in format string"));
            }
            value = super::pymethods::format_getattr(&value, attr)?;
            rest = tail;
        } else if let Some(after) = rest.strip_prefix('[') {
            let end = after
                .find(']')
                .ok_or_else(|| value_error("Missing ']' in format string"))?;
            let key = &after[..end];
            if key.is_empty() {
                return Err(value_error("Empty attribute in format string"));
            }
            value = index_field(&value, key)?;
            rest = &after[end + 1..];
            if !rest.is_empty() && !rest.starts_with(['.', '[']) {
                return Err(value_error(
                    "Only '.' or '[' may follow ']' in format field specifier",
                ));
            }
        } else {
            return Err(value_error(
                "Only '.' or '[' may follow ']' in format field specifier",
            ));
        }
    }
    Ok(value)
}

fn positional(args: &[Value], index: usize) -> ExprResult<Value> {
    args.get(index).cloned().ok_or_else(|| {
        ExprError::Uncaught(format!(
            "IndexError: Replacement index {index} out of range for positional args tuple"
        ))
    })
}

/// `value[key]` for a `[key]` in a replacement field: digits are an integer
/// index, anything else a string key.
fn index_field(value: &Value, key: &str) -> ExprResult<Value> {
    let numeric = key.bytes().all(|b| b.is_ascii_digit());
    match (value, numeric) {
        (Value::List(items), true) => key
            .parse::<usize>()
            .ok()
            .and_then(|i| items.get(i))
            .cloned()
            .ok_or_else(|| ExprError::Uncaught("IndexError: list index out of range".into())),
        (Value::Str(s), true) => key
            .parse::<usize>()
            .ok()
            .and_then(|i| s.chars().nth(i))
            .map(|c| Value::Str(c.to_string()))
            .ok_or_else(|| ExprError::Uncaught("IndexError: string index out of range".into())),
        (Value::Map(m), false) => m
            .get(key)
            .cloned()
            .ok_or_else(|| ExprError::Uncaught(format!("KeyError: '{key}'"))),
        (Value::Map(_), true) => Err(ExprError::Uncaught(format!("KeyError: {key}"))),
        (Value::List(_), false) => Err(value_error(
            "list indices must be integers or slices, not str",
        )),
        (other, _) => Err(value_error(format!(
            "'{}' object is not subscriptable",
            type_name(other)
        ))),
    }
}
