//! Python-faithful text formatting helpers (`repr` of floats and strings).
//!
//! Error messages and string coercions that upstream builds with Python's
//! `repr()` / `str()` need the exact Python spelling, which Rust's `{}` and
//! `{:?}` do not give (`1.0` prints as `1`, `1e16` as `10000000000000000`,
//! and `{:?}` escapes differently from Python).

/// Python `repr(float)` / `str(float)`.
///
/// Shortest round-trip digits; fixed notation when the decimal point sits in
/// `(-4, 16]`, otherwise `d.ddde+XX` with at least two exponent digits.
///
/// `1.0` → `1.0`, `1e16` → `1e+16`, `1e-5` → `1e-05`, `1.5e22` → `1.5e+22`.
pub fn py_float_repr(f: f64) -> String {
    if f.is_nan() {
        return "nan".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf" } else { "-inf" }.into();
    }
    let (negative, digits, decpt) = float_digits(f);
    let sign = if negative { "-" } else { "" };
    let n = digits.len() as i32;
    let body = if decpt > -4 && decpt <= 16 {
        if decpt <= 0 {
            format!("0.{}{digits}", "0".repeat((-decpt) as usize))
        } else if decpt >= n {
            format!("{digits}{}.0", "0".repeat((decpt - n) as usize))
        } else {
            format!(
                "{}.{}",
                &digits[..decpt as usize],
                &digits[decpt as usize..]
            )
        }
    } else {
        let exp = decpt - 1;
        let mantissa = if n > 1 {
            format!("{}.{}", &digits[..1], &digits[1..])
        } else {
            digits.clone()
        };
        format!(
            "{mantissa}e{}{:02}",
            if exp < 0 { '-' } else { '+' },
            exp.abs()
        )
    };
    format!("{sign}{body}")
}

/// Shortest round-trip decimal digits of a finite float.
///
/// Returns `(negative, digits, decpt)` where the value is
/// `0.<digits> × 10^decpt` (Python `float_repr_style` convention). `digits`
/// has no trailing zeros (a single `"0"` for zero).
pub(crate) fn float_digits(f: f64) -> (bool, String, i32) {
    let sci = format!("{:e}", f.abs());
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp10: i32 = exp.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let digits = digits.trim_end_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    let digits = even_on_tie(f.abs(), digits, exp10 + 1);
    (f.is_sign_negative(), digits, exp10 + 1)
}

/// Resolve a shortest-digits tie the way CPython does.
///
/// When the exact value lies halfway between two candidates of the shortest
/// length (`900719925474099.25` between `...099.2` and `...099.3`), CPython's
/// `dtoa` takes the one with an even last digit, where Rust's formatter may
/// take the odd one. A tie needs a shortest form of at least 15 digits and an
/// exact expansion one digit longer, which only doubles in `[2**12, 1e18)` can
/// have (their fractions need at most 40 digits), so nothing else is examined.
/// `decpt` is the decimal point position of `digits` (`0.<digits> x 10^decpt`).
fn even_on_tie(f: f64, digits: &str, decpt: i32) -> String {
    let last = digits.as_bytes()[digits.len() - 1];
    let odd = (last - b'0') % 2 == 1;
    if !odd || last == b'9' || digits.len() < 15 || !(4096.0..1e18).contains(&f) {
        return digits.to_owned();
    }
    let exact: String = format!("{f:.40}").chars().filter(|c| *c != '.').collect();
    let exact = exact.trim_end_matches('0');
    if exact.len() != digits.len() + 1 || !exact.ends_with('5') {
        return digits.to_owned();
    }
    // The even candidate is the neighbour of the odd one: above it when
    // `digits` is the truncation of the exact value, below it otherwise.
    let neighbour_digit = if exact.starts_with(digits) {
        last + 1
    } else {
        last - 1
    };
    let neighbour = format!(
        "{}{}",
        &digits[..digits.len() - 1],
        char::from(neighbour_digit)
    );
    // Both candidates are equidistant from `f`; use the neighbour only if it
    // also reads back as `f` (the rounding interval can be lopsided).
    let reads_back = format!("{neighbour}e{}", decpt - neighbour.len() as i32)
        .parse::<f64>()
        .is_ok_and(|value| value == f);
    if reads_back {
        neighbour
    } else {
        digits.to_owned()
    }
}

/// Python `repr(str)`: single quotes, switching to double quotes when the
/// string contains a single quote and no double quote; backslash, the active
/// quote and control characters are escaped.
///
/// `abc` → `'abc'`, `it's` → `"it's"`, a newline → `\n`.
pub fn py_repr_str(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_repr_matches_python() {
        let cases: &[(f64, &str)] = &[
            (0.0, "0.0"),
            (-0.0, "-0.0"),
            (1.0, "1.0"),
            (5.0, "5.0"),
            (0.1, "0.1"),
            (0.1 + 0.2, "0.30000000000000004"),
            (172.0, "172.0"),
            (1e15, "1000000000000000.0"),
            (1e16, "1e+16"),
            (1.5e16, "1.5e+16"),
            (1e22, "1e+22"),
            (123456789012345680.0, "1.2345678901234568e+17"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (1e-7, "1e-07"),
            (-2.5e-7, "-2.5e-07"),
            (52.3, "52.3"),
            (f64::INFINITY, "inf"),
            (f64::NEG_INFINITY, "-inf"),
        ];
        for (f, want) in cases {
            assert_eq!(py_float_repr(*f), *want, "repr({f:e})");
        }
        assert_eq!(py_float_repr(f64::NAN), "nan");
    }

    #[test]
    fn str_repr_matches_python() {
        assert_eq!(py_repr_str("maybe"), "'maybe'");
        assert_eq!(py_repr_str(""), "''");
        assert_eq!(py_repr_str("it's"), "\"it's\"");
        assert_eq!(py_repr_str("say \"hi\""), "'say \"hi\"'");
        assert_eq!(py_repr_str("both ' and \""), "'both \\' and \"'");
        assert_eq!(py_repr_str("back\\slash"), "'back\\\\slash'");
        assert_eq!(py_repr_str("tab\there"), "'tab\\there'");
    }
}
