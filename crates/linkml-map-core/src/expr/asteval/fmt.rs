//! `format(value, spec)` (the format-spec mini-language, used by f-strings)
//! and printf-style `str % args`.

use super::value::*;
use crate::pyfmt::{py_float_repr, py_repr_str};

#[derive(Default)]
struct Spec {
    fill: Option<char>,
    align: Option<char>,
    sign: Option<char>,
    alt: bool,
    zero: bool,
    width: Option<usize>,
    grouping: Option<char>,
    precision: Option<usize>,
    ty: Option<char>,
}

fn parse_spec(spec: &str) -> PyRes<Spec> {
    let c: Vec<char> = spec.chars().collect();
    let mut i = 0;
    let mut s = Spec::default();
    if c.len() >= 2 && matches!(c[1], '<' | '>' | '=' | '^') {
        s.fill = Some(c[0]);
        s.align = Some(c[1]);
        i = 2;
    } else if !c.is_empty() && matches!(c[0], '<' | '>' | '=' | '^') {
        s.align = Some(c[0]);
        i = 1;
    }
    if i < c.len() && matches!(c[i], '+' | '-' | ' ') {
        s.sign = Some(c[i]);
        i += 1;
    }
    if i < c.len() && c[i] == 'z' {
        i += 1;
    }
    if i < c.len() && c[i] == '#' {
        s.alt = true;
        i += 1;
    }
    if i < c.len() && c[i] == '0' {
        s.zero = true;
        i += 1;
    }
    let mut w = String::new();
    while i < c.len() && c[i].is_ascii_digit() {
        w.push(c[i]);
        i += 1;
    }
    if !w.is_empty() {
        s.width = Some(
            w.parse()
                .map_err(|_| exc("ValueError", "Too many decimal digits in format string"))?,
        );
    }
    if i < c.len() && matches!(c[i], ',' | '_') {
        s.grouping = Some(c[i]);
        i += 1;
    }
    if i < c.len() && c[i] == '.' {
        i += 1;
        let mut p = String::new();
        while i < c.len() && c[i].is_ascii_digit() {
            p.push(c[i]);
            i += 1;
        }
        if p.is_empty() {
            return value_err("Format specifier missing precision");
        }
        s.precision = Some(
            p.parse()
                .map_err(|_| exc("ValueError", "Too many decimal digits in format string"))?,
        );
    }
    if i < c.len() {
        s.ty = Some(c[i]);
        i += 1;
    }
    if i < c.len() {
        return value_err("Invalid format specifier");
    }
    Ok(s)
}

fn pad(body: &str, sign_prefix: &str, s: &Spec, default_align: char, numeric: bool) -> String {
    let width = s.width.unwrap_or(0);
    let len = body.chars().count() + sign_prefix.chars().count();
    if len >= width {
        return format!("{sign_prefix}{body}");
    }
    let n = width - len;
    let (fill, align) = if s.zero && s.fill.is_none() && numeric {
        ('0', '=')
    } else {
        (s.fill.unwrap_or(' '), s.align.unwrap_or(default_align))
    };
    let fill_s = |k: usize| fill.to_string().repeat(k);
    match align {
        '<' => format!("{sign_prefix}{body}{}", fill_s(n)),
        '^' => format!("{}{sign_prefix}{body}{}", fill_s(n / 2), fill_s(n - n / 2)),
        '=' => format!("{sign_prefix}{}{body}", fill_s(n)),
        _ => format!("{}{sign_prefix}{body}", fill_s(n)),
    }
}

fn group(digits: &str, sep: char, size: usize) -> String {
    let n = digits.len();
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (n - i).is_multiple_of(size) {
            out.push(sep);
        }
        out.push(ch);
    }
    out
}

/// Exponent in Python's `e` form: at least two digits and an explicit sign.
fn fix_exp(rust: &str, upper: bool) -> String {
    let (mant, exp) = rust.split_once('e').unwrap_or((rust, "0"));
    let (sign, digits) = match exp.strip_prefix('-') {
        Some(d) => ('-', d),
        None => ('+', exp),
    };
    let e = if upper { 'E' } else { 'e' };
    format!("{mant}{e}{sign}{digits:0>2}")
}

fn float_body(x: f64, ty: char, prec: Option<usize>, alt: bool) -> String {
    let upper = ty.is_ascii_uppercase();
    if x.is_nan() {
        return if upper { "NAN" } else { "nan" }.into();
    }
    if x.is_infinite() {
        return if upper { "INF" } else { "inf" }.into();
    }
    let x = x.abs();
    match ty.to_ascii_lowercase() {
        'f' => {
            let s = format!("{x:.*}", prec.unwrap_or(6));
            if alt && prec == Some(0) {
                format!("{s}.")
            } else {
                s
            }
        }
        'e' => fix_exp(&format!("{x:.*e}", prec.unwrap_or(6)), upper),
        '%' => {
            let s = format!("{:.*}", prec.unwrap_or(6), x * 100.0);
            format!("{s}%")
        }
        'g' | 'n' => {
            let p = match prec {
                None => 6,
                Some(0) => 1,
                Some(p) => p,
            };
            let sci = format!("{x:.*e}", p - 1);
            let exp: i32 = sci
                .split_once('e')
                .map_or(0, |(_, e)| e.parse().unwrap_or(0));
            let mut s = if exp >= -4 && exp < p as i32 {
                format!("{x:.*}", (p as i32 - 1 - exp).max(0) as usize)
            } else {
                fix_exp(&sci, upper)
            };
            if !alt {
                s = strip_zeros(&s);
            }
            s
        }
        _ => py_float_repr(x),
    }
}

fn strip_zeros(s: &str) -> String {
    let (mant, rest) = match s.find(['e', 'E']) {
        Some(i) => (&s[..i], &s[i..]),
        None => (s, ""),
    };
    let mant = if mant.contains('.') {
        mant.trim_end_matches('0').trim_end_matches('.')
    } else {
        mant
    };
    format!("{mant}{rest}")
}

fn format_float_spec(x: f64, s: &Spec) -> PyRes<String> {
    let ty = s.ty.unwrap_or(' ');
    let body_core = match ty {
        ' ' => {
            let b = match s.precision {
                None => py_float_repr(x.abs()),
                Some(p) => {
                    // Like 'g' but with at least one digit after the point.
                    let g = float_body(x, 'g', Some(p.max(1)), false);
                    if g.contains(['.', 'e', 'n', 'i']) {
                        g
                    } else {
                        format!("{g}.0")
                    }
                }
            };
            if x.is_nan() { "nan".into() } else { b }
        }
        'e' | 'E' | 'f' | 'F' | 'g' | 'G' | 'n' | '%' => float_body(x, ty, s.precision, s.alt),
        other => {
            return value_err(format!(
                "Unknown format code '{other}' for object of type 'float'"
            ));
        }
    };
    let negative = x.is_sign_negative() && !x.is_nan();
    let sign_prefix = match (negative, s.sign) {
        (true, _) => "-",
        (false, Some('+')) => "+",
        (false, Some(' ')) => " ",
        _ => "",
    };
    let body = if let Some(sep) = s.grouping {
        let (int_part, rest) = match body_core.find(['.', 'e', 'E', '%']) {
            Some(i) => (&body_core[..i], &body_core[i..]),
            None => (&body_core[..], ""),
        };
        if int_part.chars().all(|c| c.is_ascii_digit()) {
            let mut digits = int_part.to_string();
            if s.zero && s.fill.is_none() {
                let width = s.width.unwrap_or(0);
                while group(&digits, sep, 3).chars().count()
                    + rest.chars().count()
                    + sign_prefix.chars().count()
                    < width
                {
                    digits.insert(0, '0');
                }
            }
            format!("{}{rest}", group(&digits, sep, 3))
        } else {
            body_core
        }
    } else {
        body_core
    };
    Ok(pad(&body, sign_prefix, s, '>', true))
}

fn format_int_spec(i: i64, s: &Spec, is_bool: bool) -> PyRes<String> {
    let ty = s.ty.unwrap_or('d');
    match ty {
        'e' | 'E' | 'f' | 'F' | 'g' | 'G' | '%' => return format_float_spec(i as f64, s),
        _ if s.precision.is_some() => {
            return value_err("Precision not allowed in integer format specifier");
        }
        'd' | 'n' | 'b' | 'o' | 'x' | 'X' | 'c' => {}
        other => {
            return value_err(format!(
                "Unknown format code '{other}' for object of type '{}'",
                if is_bool { "bool" } else { "int" }
            ));
        }
    }
    if ty == 'c' {
        let c = u32::try_from(i).ok().and_then(char::from_u32);
        return match c {
            Some(c) => Ok(pad(&c.to_string(), "", s, '<', false)),
            None => overflow_err("%c arg not in range(0x110000)"),
        };
    }
    let neg = i < 0;
    let mag = i.unsigned_abs();
    let mut body = match ty {
        'b' => format!("{mag:b}"),
        'o' => format!("{mag:o}"),
        'x' => format!("{mag:x}"),
        'X' => format!("{mag:X}"),
        _ => mag.to_string(),
    };
    let prefix = if s.alt {
        match ty {
            'b' => "0b",
            'o' => "0o",
            'x' => "0x",
            'X' => "0X",
            _ => "",
        }
    } else {
        ""
    };
    let sign = match (neg, s.sign) {
        (true, _) => "-",
        (false, Some('+')) => "+",
        (false, Some(' ')) => " ",
        _ => "",
    };
    let sign_prefix = format!("{sign}{prefix}");
    if let Some(sep) = s.grouping {
        let size = if matches!(ty, 'd' | 'n') { 3 } else { 4 };
        if s.zero && s.fill.is_none() {
            let width = s.width.unwrap_or(0);
            while group(&body, sep, size).chars().count() + sign_prefix.chars().count() < width {
                body.insert(0, '0');
            }
        }
        body = group(&body, sep, size);
    }
    Ok(pad(&body, &sign_prefix, s, '>', true))
}

/// `format(value, spec)`.
pub(crate) fn format_value(v: &Py, spec: &str) -> PyRes<String> {
    if spec.is_empty() {
        return Ok(to_str(v));
    }
    let s = parse_spec(spec)?;
    match v {
        Py::Str(text) => {
            match s.ty {
                None | Some('s') => {}
                Some(other) => {
                    return value_err(format!(
                        "Unknown format code '{other}' for object of type 'str'"
                    ));
                }
            }
            if s.sign.is_some() {
                return value_err("Sign not allowed in string format specifier");
            }
            if s.alt {
                return value_err("Alternate form (#) not allowed in string format specifier");
            }
            if s.grouping.is_some() {
                return value_err("Cannot specify ',' with 's'.");
            }
            if s.align == Some('=') {
                return value_err("'=' alignment not allowed in string format specifier");
            }
            let t: String = match s.precision {
                Some(p) => text.chars().take(p).collect(),
                None => text.to_string(),
            };
            Ok(pad(&t, "", &s, '<', false))
        }
        Py::Int(i) => format_int_spec(*i, &s, false),
        Py::Bool(b) => {
            if s.ty.is_none() {
                return Ok(pad(if *b { "True" } else { "False" }, "", &s, '<', false));
            }
            format_int_spec(*b as i64, &s, true)
        }
        Py::Float(f) => format_float_spec(*f, &s),
        other => type_err(format!(
            "unsupported format string passed to {}.__format__",
            other.type_name()
        )),
    }
}

// --- printf-style -------------------------------------------------------------

fn pct_err<T>(msg: impl Into<String>) -> PyRes<T> {
    type_err(msg)
}

/// `fmt % args`.
pub(crate) fn percent_format(fmt: &str, args: &Py) -> PyRes<String> {
    let chars: Vec<char> = fmt.chars().collect();
    let tuple_args: Vec<Py>;
    let mapping: Option<&Py> = match args {
        Py::Dict(_) => Some(args),
        _ => None,
    };
    let positional: &[Py] = match args {
        Py::Tuple(t) => {
            tuple_args = t.to_vec();
            &tuple_args
        }
        other => {
            tuple_args = vec![other.clone()];
            &tuple_args
        }
    };
    let mut next_arg = 0usize;
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c != '%' {
            out.push(c);
            i += 1;
            continue;
        }
        i += 1;
        let mut key: Option<String> = None;
        if chars.get(i) == Some(&'(') {
            let mut depth = 1;
            let mut j = i + 1;
            while j < chars.len() && depth > 0 {
                match chars[j] {
                    '(' => depth += 1,
                    ')' => depth -= 1,
                    _ => {}
                }
                j += 1;
            }
            if depth > 0 {
                return value_err("incomplete format key");
            }
            key = Some(chars[i + 1..j - 1].iter().collect());
            i = j;
        }
        let (mut left, mut plus, mut space, mut alt, mut zero) =
            (false, false, false, false, false);
        while let Some(&f) = chars.get(i) {
            match f {
                '-' => left = true,
                '+' => plus = true,
                ' ' => space = true,
                '#' => alt = true,
                '0' => zero = true,
                _ => break,
            }
            i += 1;
        }
        let take_arg = |next_arg: &mut usize| -> PyRes<Py> {
            if let (Some(k), Some(Py::Dict(d))) = (&key, mapping) {
                let hk = HKey::Str(k.as_str().into());
                return match d.borrow().get(&hk) {
                    Some((_, v)) => Ok(v.clone()),
                    None => Err(Box::new(ExcObj {
                        class: "KeyError",
                        args: vec![Py::str(k.as_str())],
                    })),
                };
            }
            if key.is_some() {
                return pct_err("format requires a mapping");
            }
            let v = positional.get(*next_arg).cloned();
            *next_arg += 1;
            v.map_or_else(|| pct_err("not enough arguments for format string"), Ok)
        };
        let mut width: Option<usize> = None;
        if chars.get(i) == Some(&'*') {
            let w = take_arg(&mut next_arg)?;
            let Py::Int(w) = w else {
                return pct_err("* wants int");
            };
            if w < 0 {
                left = true;
            }
            width = Some(w.unsigned_abs() as usize);
            i += 1;
        } else {
            let mut w = String::new();
            while let Some(d) = chars.get(i).filter(|d| d.is_ascii_digit()) {
                w.push(*d);
                i += 1;
            }
            if !w.is_empty() {
                width = w.parse().ok();
            }
        }
        let mut precision: Option<usize> = None;
        if chars.get(i) == Some(&'.') {
            i += 1;
            if chars.get(i) == Some(&'*') {
                let p = take_arg(&mut next_arg)?;
                let Py::Int(p) = p else {
                    return pct_err("* wants int");
                };
                precision = Some(p.max(0) as usize);
                i += 1;
            } else {
                let mut p = String::new();
                while let Some(d) = chars.get(i).filter(|d| d.is_ascii_digit()) {
                    p.push(*d);
                    i += 1;
                }
                precision = Some(p.parse().unwrap_or(0));
            }
        }
        while matches!(chars.get(i), Some('h' | 'l' | 'L')) {
            i += 1;
        }
        let Some(&conv) = chars.get(i) else {
            return value_err("incomplete format");
        };
        i += 1;
        if conv == '%' {
            out.push('%');
            continue;
        }
        let arg = take_arg(&mut next_arg)?;
        let spec = Spec {
            fill: None,
            align: Some(if left { '<' } else { '>' }),
            sign: if plus {
                Some('+')
            } else if space {
                Some(' ')
            } else {
                None
            },
            alt,
            zero: zero && !left,
            width,
            grouping: None,
            precision,
            ty: None,
        };
        let piece = match conv {
            's' | 'r' | 'a' => {
                let t = if conv == 's' {
                    to_str(&arg)
                } else {
                    repr(&arg)
                };
                let t: String = match precision {
                    Some(p) => t.chars().take(p).collect(),
                    None => t,
                };
                pad(
                    &t,
                    "",
                    &Spec {
                        zero: false,
                        ..spec
                    },
                    if left { '<' } else { '>' },
                    false,
                )
            }
            'd' | 'i' | 'u' => {
                let n = match &arg {
                    Py::Int(n) => *n,
                    Py::Bool(b) => *b as i64,
                    Py::Float(f) => f.trunc() as i64,
                    other => {
                        return pct_err(format!(
                            "%{conv} format: a real number is required, not {}",
                            other.type_name()
                        ));
                    }
                };
                format_int_spec(
                    n,
                    &Spec {
                        ty: Some('d'),
                        precision: None,
                        ..spec
                    },
                    false,
                )?
            }
            'o' | 'x' | 'X' => {
                let n = match &arg {
                    Py::Int(n) => *n,
                    Py::Bool(b) => *b as i64,
                    other => {
                        return pct_err(format!(
                            "%{conv} format: an integer is required, not {}",
                            other.type_name()
                        ));
                    }
                };
                format_int_spec(
                    n,
                    &Spec {
                        ty: Some(conv),
                        precision: None,
                        ..spec
                    },
                    false,
                )?
            }
            'e' | 'E' | 'f' | 'F' | 'g' | 'G' => {
                let x = match &arg {
                    Py::Int(n) => *n as f64,
                    Py::Bool(b) => *b as i64 as f64,
                    Py::Float(f) => *f,
                    other => {
                        return pct_err(format!("must be real number, not {}", other.type_name()));
                    }
                };
                format_float_spec(
                    x,
                    &Spec {
                        ty: Some(conv),
                        ..spec
                    },
                )?
            }
            'c' => match &arg {
                Py::Int(n) => match u32::try_from(*n).ok().and_then(char::from_u32) {
                    Some(ch) => pad(
                        &ch.to_string(),
                        "",
                        &Spec {
                            zero: false,
                            ..spec
                        },
                        '>',
                        false,
                    ),
                    None => return overflow_err("%c arg not in range(0x110000)"),
                },
                Py::Str(s) if s.chars().count() == 1 => pad(
                    s,
                    "",
                    &Spec {
                        zero: false,
                        ..spec
                    },
                    '>',
                    false,
                ),
                _ => return pct_err("%c requires int or char"),
            },
            other => {
                return value_err(format!(
                    "unsupported format character '{other}' (0x{:x}) at index {}",
                    other as u32,
                    i - 1
                ));
            }
        };
        out.push_str(&piece);
    }
    if mapping.is_none() && next_arg < positional.len() && !matches!(args, Py::Dict(_)) {
        return pct_err("not all arguments converted during string formatting");
    }
    let _ = py_repr_str;
    Ok(out)
}
