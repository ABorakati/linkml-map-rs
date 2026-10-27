//! An exact decimal number mirroring Python's `decimal.Decimal` text semantics.
//!
//! Upstream coerces `range: decimal` values with `Decimal(str(v))` and keeps
//! them exact through every writer (`utils/serialization.py`). Neither
//! `f64`, `rust_decimal` (96-bit mantissa) nor `serde_json`'s
//! `arbitrary_precision` (which breaks `#[serde(untagged)]` enums) reproduces
//! that, so this is a small hand-written representation: a sign, a string of
//! ASCII coefficient digits and a base-10 exponent. Trailing zeros and the
//! exponent survive exactly as in Python (`52.30` stays `5230E-2`).
//!
//! Only what the transformer needs is implemented: parsing, the two Python
//! spellings (`str(d)` and `format(d, 'f')`), numeric equality / ordering and
//! conversions from the other [`crate::value::Value`] scalars.

use std::cmp::Ordering;
use std::fmt;

/// An exact, finite decimal number.
///
/// Equality and ordering are numeric, as in Python (`Decimal("52.30") ==
/// Decimal("52.3")`, `Decimal("-0") == Decimal("0")`); the text spellings keep
/// the digits exactly as parsed.
#[derive(Debug, Clone)]
pub struct Decimal {
    negative: bool,
    /// ASCII digits without leading zeros; `"0"` for a zero coefficient.
    coeff: String,
    exp: i32,
}

/// Why a string is not a finite [`Decimal`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecimalError {
    /// Not a decimal literal (Python `InvalidOperation`).
    Invalid,
    /// A valid literal that is not finite: `NaN`, `sNaN` or an infinity.
    NotFinite,
}

impl Decimal {
    /// Parse text the way Python's `Decimal(str)` does.
    ///
    /// Surrounding whitespace is stripped; an optional sign, digits with an
    /// optional `.` (`5.` and `.5` are valid) and an optional `e|E[+-]digits`
    /// exponent are accepted; a single `_` is allowed between digits.
    /// `nan`/`snan`/`inf`/`infinity` (any case, optionally signed) are valid
    /// Python literals but not finite, so they report
    /// [`DecimalError::NotFinite`].
    ///
    /// PARITY: Python also accepts Unicode decimal digits (`Decimal("١٢")`);
    /// only ASCII digits are accepted here. An exponent outside `i32` is
    /// rejected as invalid.
    pub fn parse_python(text: &str) -> Result<Self, DecimalError> {
        let t = text.trim();
        let (negative, body) = match t.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, t.strip_prefix('+').unwrap_or(t)),
        };
        let lower = body.to_ascii_lowercase();
        if matches!(lower.as_str(), "inf" | "infinity") {
            return Err(DecimalError::NotFinite);
        }
        // `nan` / `snan`, each optionally followed by diagnostic digits.
        let nan_tail = lower
            .strip_prefix("snan")
            .or_else(|| lower.strip_prefix("nan"));
        if let Some(tail) = nan_tail
            && tail.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(DecimalError::NotFinite);
        }

        let (mantissa, exponent) = match lower.find('e') {
            Some(i) => (&lower[..i], Some(&lower[i + 1..])),
            None => (lower.as_str(), None),
        };
        let (int_part, frac_part) = match mantissa.split_once('.') {
            Some((i, f)) => (i, f),
            None => (mantissa, ""),
        };
        let int_digits = strip_underscores(int_part)?;
        let frac_digits = strip_underscores(frac_part)?;
        if int_digits.is_empty() && frac_digits.is_empty() {
            return Err(DecimalError::Invalid);
        }
        let mut exp: i64 = match exponent {
            None => 0,
            Some(e) => {
                let (eneg, edigits) = match e.strip_prefix('-') {
                    Some(rest) => (true, rest),
                    None => (false, e.strip_prefix('+').unwrap_or(e)),
                };
                let edigits = strip_underscores(edigits)?;
                if edigits.is_empty() {
                    return Err(DecimalError::Invalid);
                }
                let magnitude: i64 = edigits.parse().map_err(|_| DecimalError::Invalid)?;
                if eneg { -magnitude } else { magnitude }
            }
        };
        exp -= frac_digits.len() as i64;
        let exp = i32::try_from(exp).map_err(|_| DecimalError::Invalid)?;
        let mut digits = int_digits;
        digits.push_str(&frac_digits);
        Ok(Self::from_parts(negative, &digits, exp))
    }

    fn from_parts(negative: bool, digits: &str, exp: i32) -> Self {
        let trimmed = digits.trim_start_matches('0');
        let coeff = if trimmed.is_empty() { "0" } else { trimmed };
        Self {
            negative,
            coeff: coeff.to_owned(),
            exp,
        }
    }

    /// Exact conversion of an integer (`Decimal(int)`).
    pub fn from_i64(i: i64) -> Self {
        Self::from_parts(i < 0, &i.unsigned_abs().to_string(), 0)
    }

    /// `Decimal(str(f))`: the digits of Python's `repr(float)`, so `1.0` keeps
    /// its trailing zero and `1e22` is `1E+22`.
    ///
    /// # Errors
    /// [`DecimalError::NotFinite`] for NaN and the infinities.
    pub fn from_f64_repr(f: f64) -> Result<Self, DecimalError> {
        Self::parse_python(&crate::pyfmt::py_float_repr(f))
    }

    /// `Decimal(f)`: the exact binary value of a float (not its printed digits).
    ///
    /// Only used to compare a decimal with a float exactly, as Python does.
    fn from_f64_exact(f: f64) -> Option<Self> {
        if !f.is_finite() {
            return None;
        }
        // A finite f64 has at most 1074 fractional decimal digits; Rust's
        // fixed formatter prints the exact expansion.
        let text = format!("{f:.1074}");
        let d = Self::parse_python(&text).ok()?;
        Some(d.trimmed())
    }

    /// Whether the value is zero (`Decimal("0.00")` is falsy).
    pub fn is_zero(&self) -> bool {
        self.coeff == "0"
    }

    /// Whether the sign bit is set (`-0` included).
    pub fn is_negative(&self) -> bool {
        self.negative
    }

    /// Python `str(d)` (to-scientific-string): plain notation for modest
    /// exponents, `d.dddE+X` otherwise (`1E-7`, `1E+3`, `0E+2`).
    pub fn to_sci_string(&self) -> String {
        let n = self.coeff.len() as i64;
        let exp = i64::from(self.exp);
        let adjusted = exp + n - 1;
        let sign = if self.negative { "-" } else { "" };
        let body = if exp <= 0 && adjusted >= -6 {
            if exp == 0 {
                self.coeff.clone()
            } else {
                let point = n + exp; // digits before the point
                if point > 0 {
                    let (int, frac) = self.coeff.split_at(point as usize);
                    format!("{int}.{frac}")
                } else {
                    format!("0.{}{}", "0".repeat((-point) as usize), self.coeff)
                }
            }
        } else {
            let mantissa = if n > 1 {
                format!("{}.{}", &self.coeff[..1], &self.coeff[1..])
            } else {
                self.coeff.clone()
            };
            format!(
                "{mantissa}E{}{}",
                if adjusted < 0 { '-' } else { '+' },
                adjusted.abs()
            )
        };
        format!("{sign}{body}")
    }

    /// Python `format(d, 'f')`: fixed-point notation with every digit kept
    /// (`1E-7` is `0.0000001`, `1E+3` is `1000`).
    pub fn to_fixed_string(&self) -> String {
        let n = self.coeff.len() as i64;
        let exp = i64::from(self.exp);
        let sign = if self.negative { "-" } else { "" };
        let body = if exp >= 0 {
            if self.is_zero() {
                // PARITY: format(Decimal('0E+2'), 'f') is '0' (libmpdec).
                "0".to_owned()
            } else {
                format!("{}{}", self.coeff, "0".repeat(exp as usize))
            }
        } else {
            let point = n + exp;
            if point > 0 {
                let (int, frac) = self.coeff.split_at(point as usize);
                format!("{int}.{frac}")
            } else {
                format!("0.{}{}", "0".repeat((-point) as usize), self.coeff)
            }
        };
        format!("{sign}{body}")
    }

    /// The value as the nearest `f64` (Python `float(Decimal)`).
    pub fn to_f64(&self) -> f64 {
        self.to_sci_string().parse().unwrap_or(f64::NAN)
    }

    /// This value with trailing coefficient zeros folded into the exponent.
    fn trimmed(&self) -> Self {
        if self.is_zero() {
            return Self::from_parts(self.negative, "0", 0);
        }
        let stripped = self.coeff.trim_end_matches('0');
        Self {
            negative: self.negative,
            coeff: stripped.to_owned(),
            exp: self.exp + (self.coeff.len() - stripped.len()) as i32,
        }
    }

    /// Numeric comparison with a float, exactly as Python does it
    /// (`Decimal("0.1") == 0.1` is false). `None` for NaN.
    pub fn cmp_f64(&self, f: f64) -> Option<Ordering> {
        if f.is_nan() {
            return None;
        }
        if f.is_infinite() {
            return Some(if f > 0.0 {
                Ordering::Less
            } else {
                Ordering::Greater
            });
        }
        Some(self.cmp(&Self::from_f64_exact(f)?))
    }
}

fn strip_underscores(s: &str) -> Result<String, DecimalError> {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'0'..=b'9' => out.push(b as char),
            b'_' => {
                let between_digits = i > 0
                    && bytes[i - 1].is_ascii_digit()
                    && bytes.get(i + 1).is_some_and(u8::is_ascii_digit);
                if !between_digits {
                    return Err(DecimalError::Invalid);
                }
            }
            _ => return Err(DecimalError::Invalid),
        }
    }
    Ok(out)
}

impl fmt::Display for Decimal {
    /// Python `str(Decimal)`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_sci_string())
    }
}

impl PartialEq for Decimal {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Decimal {}

impl PartialOrd for Decimal {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Decimal {
    fn cmp(&self, other: &Self) -> Ordering {
        let (a, b) = (self.trimmed(), other.trimmed());
        match (a.is_zero(), b.is_zero()) {
            (true, true) => return Ordering::Equal,
            (true, false) => {
                return if b.negative {
                    Ordering::Greater
                } else {
                    Ordering::Less
                };
            }
            (false, true) => {
                return if a.negative {
                    Ordering::Less
                } else {
                    Ordering::Greater
                };
            }
            (false, false) => {}
        }
        if a.negative != b.negative {
            return if a.negative {
                Ordering::Less
            } else {
                Ordering::Greater
            };
        }
        let adj = |d: &Self| i64::from(d.exp) + d.coeff.len() as i64;
        let magnitude = adj(&a).cmp(&adj(&b)).then_with(|| {
            let width = a.coeff.len().max(b.coeff.len());
            let pad = |d: &Self| format!("{:0<width$}", d.coeff);
            pad(&a).cmp(&pad(&b))
        });
        if a.negative {
            magnitude.reverse()
        } else {
            magnitude
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Decimal {
        Decimal::parse_python(s).unwrap_or_else(|e| panic!("{s:?}: {e:?}"))
    }

    // Upstream test_decimal_range.py / triage section 3.5 (Python-verified).
    #[test]
    fn parse_and_fixed_spelling_table() {
        let cases = [
            ("52.3", "52.3"),
            ("52.30", "52.30"),
            ("52", "52"),
            ("-0.05", "-0.05"),
            ("0.123456789012345678901", "0.123456789012345678901"),
            (" 5 ", "5"),
            ("+5", "5"),
            ("1_000", "1000"),
            ("5.", "5"),
            (".5", "0.5"),
            ("1e5", "100000"),
            ("1E-7", "0.0000001"),
            ("1e+3", "1000"),
            ("-0", "-0"),
            ("0.00", "0.00"),
            ("00012.50", "12.50"),
            ("0e-3", "0.000"),
            ("0E+2", "0"),
        ];
        for (src, want) in cases {
            assert_eq!(d(src).to_fixed_string(), want, "format({src:?}, 'f')");
        }
    }

    #[test]
    fn sci_spelling_table() {
        let cases = [
            ("52.30", "52.30"),
            ("52", "52"),
            ("1E-7", "1E-7"),
            ("0.0000001", "1E-7"),
            ("1e3", "1E+3"),
            ("0.000001", "0.000001"),
            ("0.00", "0.00"),
            ("-0", "-0"),
            ("0E+2", "0E+2"),
            ("1.5e10", "1.5E+10"),
            ("12.50e1", "125.0"),
            ("0.123456789012345678901", "0.123456789012345678901"),
        ];
        for (src, want) in cases {
            assert_eq!(d(src).to_sci_string(), want, "str(Decimal({src:?}))");
        }
    }

    #[test]
    fn invalid_and_non_finite() {
        for bad in [
            "abc", "", "  ", "0x10", "True", "None", "1e", "e5", ".", "1__0", "_1", "1_",
        ] {
            assert_eq!(
                Decimal::parse_python(bad),
                Err(DecimalError::Invalid),
                "{bad:?}"
            );
        }
        for nf in ["NaN", "Infinity", "-inf", "+INF", "sNaN", "nan12"] {
            assert_eq!(
                Decimal::parse_python(nf),
                Err(DecimalError::NotFinite),
                "{nf:?}"
            );
        }
    }

    #[test]
    fn from_floats_use_python_repr_digits() {
        let cases = [
            (0.1, "0.1", "0.1"),
            (1.0, "1.0", "1.0"),
            (1e-7, "1E-7", "0.0000001"),
            (1e16, "1E+16", "10000000000000000"),
            (1e22, "1E+22", "10000000000000000000000"),
            (0.1 + 0.2, "0.30000000000000004", "0.30000000000000004"),
        ];
        for (f, sci, fixed) in cases {
            let dec = Decimal::from_f64_repr(f).unwrap();
            assert_eq!(dec.to_sci_string(), sci, "str({f:e})");
            assert_eq!(dec.to_fixed_string(), fixed, "format({f:e}, 'f')");
        }
        assert_eq!(
            Decimal::from_f64_repr(f64::NAN),
            Err(DecimalError::NotFinite)
        );
        assert_eq!(Decimal::from_i64(-7).to_sci_string(), "-7");
        assert_eq!(
            Decimal::from_i64(i64::MIN).to_sci_string(),
            i64::MIN.to_string()
        );
    }

    #[test]
    fn equality_and_order_are_numeric() {
        assert_eq!(d("52.30"), d("52.3"));
        assert_eq!(d("-0"), d("0.000"));
        assert_eq!(d("1E+3"), d("1000"));
        assert_ne!(d("1"), d("1.0001"));
        assert!(d("-5") < d("-4.9"));
        assert!(d("0.001") > d("0"));
        assert!(d("-0.001") < d("0"));
        assert!(d("10") > d("9.99"));
        assert!(d("1E+3") > d("999.999"));
        assert!(d("-1E+3") < d("-999.999"));
        assert!(!d("0.5").is_zero() && d("0.00").is_zero());
    }

    #[test]
    fn float_comparison_is_exact_like_python() {
        // Decimal('0.1') != 0.1: the float is 0.1000000000000000055...
        assert_eq!(d("0.1").cmp_f64(0.1), Some(Ordering::Less));
        assert_eq!(d("0.5").cmp_f64(0.5), Some(Ordering::Equal));
        assert_eq!(d("2").cmp_f64(2.0), Some(Ordering::Equal));
        assert_eq!(d("1").cmp_f64(f64::NAN), None);
        assert_eq!(d("1").cmp_f64(f64::INFINITY), Some(Ordering::Less));
    }

    #[test]
    fn to_f64_round_trips() {
        assert_eq!(d("52.30").to_f64(), 52.3);
        assert_eq!(d("1E-7").to_f64(), 1e-7);
    }
}
