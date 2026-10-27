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
    exp: i64,
}

/// Python's `decimal.MAX_EMAX`: the largest adjusted exponent a `Decimal` can be
/// constructed with.
const MAX_EMAX: i128 = 999_999_999_999_999_999;
/// Python's `decimal.MIN_ETINY`: the smallest exponent a `Decimal` can be
/// constructed with.
const MIN_ETINY: i128 = -1_999_999_999_999_999_997;

/// The default context's `prec`, `Emax` and `Etiny` (`Emin - prec + 1`), which
/// operations such as [`Decimal::normalize`] apply (construction does not).
const CONTEXT_PRECISION: usize = 28;
const CONTEXT_EMAX: i64 = 999_999;
const CONTEXT_ETINY: i64 = -999_999 - 27;

/// The longest fixed-point spelling [`Decimal::to_fixed_string`] will build.
/// Python would try to allocate it (and raise `MemoryError` or exhaust memory
/// for the largest exponents); this keeps a stray `1E+999999999` from doing so.
const MAX_FIXED_LEN: i64 = 1 << 28;

/// `decimal.Overflow`: the result of a context operation has an adjusted
/// exponent above `Emax` (999999 in the default context).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecimalOverflow;

impl fmt::Display for DecimalOverflow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[<class 'decimal.Overflow'>]")
    }
}

impl std::error::Error for DecimalOverflow {}

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
    /// exponent are accepted.
    /// `nan`/`snan`/`inf`/`infinity` (any case, optionally signed) are valid
    /// Python literals but not finite, so they report
    /// [`DecimalError::NotFinite`].
    ///
    /// The exponent range is Python's: construction is exact and ignores the
    /// context, so `Decimal("1e999999999999999999")` is valid and one more digit
    /// of exponent is `InvalidOperation` ([`DecimalError::Invalid`]).
    ///
    /// Like `Decimal(str)` (`numeric_as_ascii` in `_decimal.c`), the text is
    /// stripped of `str.isspace()` whitespace (U+001C to U+001F included), every
    /// `_` is dropped wherever it stands (`Decimal("1_")` is 1) and Unicode
    /// decimal digits are read as ASCII ones (`Decimal("١٢")` is 12).
    pub fn parse_python(text: &str) -> Result<Self, DecimalError> {
        let normalised = crate::expr::pystr::decimal_text(text);
        let t = normalised.as_ref();
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
        let int_digits = ascii_digits_only(int_part)?;
        let frac_digits = ascii_digits_only(frac_part)?;
        if int_digits.is_empty() && frac_digits.is_empty() {
            return Err(DecimalError::Invalid);
        }
        let exp: i128 = match exponent {
            None => 0,
            Some(e) => {
                let (eneg, edigits) = match e.strip_prefix('-') {
                    Some(rest) => (true, rest),
                    None => (false, e.strip_prefix('+').unwrap_or(e)),
                };
                let edigits = ascii_digits_only(edigits)?;
                if edigits.is_empty() {
                    return Err(DecimalError::Invalid);
                }
                // Leading zeros are fine (`1e0001`); more than 30 digits cannot
                // be in range whatever they are.
                let edigits = edigits.trim_start_matches('0');
                if edigits.len() > 30 {
                    return Err(DecimalError::Invalid);
                }
                let magnitude: i128 = if edigits.is_empty() {
                    0
                } else {
                    edigits.parse().map_err(|_| DecimalError::Invalid)?
                };
                if eneg { -magnitude } else { magnitude }
            }
        } - frac_digits.len() as i128;
        let mut digits = int_digits;
        digits.push_str(&frac_digits);
        let coeff_len = digits.trim_start_matches('0').len().max(1) as i128;
        // `Decimal(text)` is exact; only the absolute limits of the library apply.
        if exp < MIN_ETINY || exp + coeff_len - 1 > MAX_EMAX {
            return Err(DecimalError::Invalid);
        }
        let exp = i64::try_from(exp).map_err(|_| DecimalError::Invalid)?;
        Ok(Self::from_parts(negative, &digits, exp))
    }

    fn from_parts(negative: bool, digits: &str, exp: i64) -> Self {
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
        let exp = self.exp;
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
    ///
    /// PARITY: Python builds the whole string, however long, and raises
    /// `MemoryError` (or runs out of memory) for the largest exponents. A
    /// spelling longer than 2**28 characters is not built here:
    /// [`Decimal::try_to_fixed_string`] reports `None` and this method falls back
    /// to the scientific spelling.
    pub fn to_fixed_string(&self) -> String {
        self.try_to_fixed_string()
            .unwrap_or_else(|| self.to_sci_string())
    }

    /// [`Decimal::to_fixed_string`], or `None` when the spelling would be longer
    /// than 2**28 characters.
    pub fn try_to_fixed_string(&self) -> Option<String> {
        let n = self.coeff.len() as i64;
        let exp = self.exp;
        let sign = if self.negative { "-" } else { "" };
        let body = if exp >= 0 {
            if self.is_zero() {
                // PARITY: format(Decimal('0E+2'), 'f') is '0' (libmpdec).
                "0".to_owned()
            } else if exp.saturating_add(n) > MAX_FIXED_LEN {
                return None;
            } else {
                format!("{}{}", self.coeff, "0".repeat(exp as usize))
            }
        } else {
            let point = n + exp;
            if point > 0 {
                let (int, frac) = self.coeff.split_at(point as usize);
                format!("{int}.{frac}")
            } else if (-point).saturating_add(n) > MAX_FIXED_LEN {
                return None;
            } else {
                format!("0.{}{}", "0".repeat((-point) as usize), self.coeff)
            }
        };
        Some(format!("{sign}{body}"))
    }

    /// The value as the nearest `f64` (Python `float(Decimal)`).
    pub fn to_f64(&self) -> f64 {
        self.to_sci_string().parse().unwrap_or(f64::NAN)
    }

    /// Python `Decimal.normalize()` under the default context (`prec=28`,
    /// `Emax=999999`, `Emin=-999999`, `ROUND_HALF_EVEN`): the coefficient is
    /// rounded to 28 significant digits and, below `Etiny` (-1000026), to a
    /// multiple of `10**Etiny`, then trailing zeros are folded into the
    /// exponent. A zero normalises to `0` (with its sign).
    ///
    /// # Errors
    /// [`DecimalOverflow`] when the adjusted exponent exceeds `Emax`, which
    /// raises `decimal.Overflow` in Python (its default context traps it).
    pub fn normalize(&self) -> Result<Self, DecimalOverflow> {
        if self.is_zero() {
            return Ok(self.trimmed());
        }
        let mut coeff = self.coeff.clone().into_bytes();
        let mut exp = self.exp;
        if exp + coeff.len() as i64 - 1 > CONTEXT_EMAX {
            return Err(DecimalOverflow);
        }
        // Digits to drop: down to the precision, and down to Etiny.
        let to_precision = coeff.len().saturating_sub(CONTEXT_PRECISION) as i64;
        let to_etiny = (CONTEXT_ETINY - exp).max(0);
        let drop = to_precision.max(to_etiny);
        if drop > 0 {
            (coeff, exp) = round_half_even(coeff, exp, drop);
            if exp + coeff.len() as i64 - 1 > CONTEXT_EMAX {
                return Err(DecimalOverflow);
            }
        }
        Ok(Self {
            negative: self.negative,
            coeff: String::from_utf8(coeff).unwrap_or_default(),
            exp,
        }
        .trimmed())
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
            exp: self.exp + (self.coeff.len() - stripped.len()) as i64,
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

/// Drop the last `drop` digits of `coeff` (whose exponent is `exp`), rounding
/// half to even; returns the new coefficient and exponent. Dropping every
/// digit leaves `0` or, past the half, `1`.
fn round_half_even(coeff: Vec<u8>, exp: i64, drop: i64) -> (Vec<u8>, i64) {
    let len = coeff.len() as i64;
    let new_exp = exp + drop;
    if drop > len {
        // Less than half of one unit in the last place: rounds to zero.
        return (b"0".to_vec(), new_exp);
    }
    let keep = (len - drop) as usize;
    let (kept, dropped) = coeff.split_at(keep);
    let first = dropped[0];
    let rest_nonzero = dropped[1..].iter().any(|&b| b != b'0');
    let last_odd = kept.last().is_some_and(|&b| (b - b'0') % 2 == 1);
    let round_up = match first.cmp(&b'5') {
        Ordering::Greater => true,
        Ordering::Less => false,
        Ordering::Equal => rest_nonzero || last_odd,
    };
    let mut digits: Vec<u8> = if kept.is_empty() {
        vec![b'0']
    } else {
        kept.to_vec()
    };
    if round_up {
        let mut i = digits.len();
        loop {
            if i == 0 {
                // Carried out of the top digit: 99..9 + 1 = 100..0.
                digits.insert(0, b'1');
                break;
            }
            i -= 1;
            if digits[i] == b'9' {
                digits[i] = b'0';
            } else {
                digits[i] += 1;
                break;
            }
        }
    }
    (digits, new_exp)
}

/// `s` itself when it is all ASCII digits (possibly none).
fn ascii_digits_only(s: &str) -> Result<String, DecimalError> {
    if s.bytes().all(|b| b.is_ascii_digit()) {
        Ok(s.to_owned())
    } else {
        Err(DecimalError::Invalid)
    }
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
        let adj = |d: &Self| d.exp + d.coeff.len() as i64;
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
            "abc",
            "",
            "  ",
            "0x10",
            "True",
            "None",
            "1e",
            "e5",
            ".",
            "_",
            "1 _",
            "_ 1",
            "1\u{0}",
            "\u{a0}1\u{b2}",
        ] {
            assert_eq!(
                Decimal::parse_python(bad),
                Err(DecimalError::Invalid),
                "{bad:?}"
            );
        }
        // `numeric_as_ascii` drops every underscore, strips `str.isspace()` padding
        // (U+001C to U+001F too) and reads Unicode decimal digits.
        for (text, want) in [
            ("1__0", "10"),
            ("_1", "1"),
            ("1_", "1"),
            ("\u{1c}3816.78\u{1f}", "3816.78"),
            ("\t-_009.829e1", "-98.29"),
            ("\u{663}", "3"),
            ("-\u{ff16}.\u{661}", "-6.1"),
            ("1_e5", "1E+5"),
        ] {
            assert_eq!(
                Decimal::parse_python(text).unwrap().to_sci_string(),
                want,
                "{text:?}"
            );
        }
        assert_eq!(Decimal::parse_python("i_nf"), Err(DecimalError::NotFinite));
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
    fn normalize_rounds_to_28_digits_half_even() {
        let n = |t: &str| d(t).normalize().unwrap().to_fixed_string();
        assert_eq!(n("52.30"), "52.3");
        assert_eq!(n("5.00"), "5");
        assert_eq!(n("1E+3"), "1000");
        assert_eq!(n("1000"), "1000");
        // 29 significant digits: 28 kept, the 29th decides.
        assert_eq!(
            n("1.2345678901234567890123456789"),
            "1.234567890123456789012345679"
        );
        assert_eq!(
            n("1.2345678901234567890123456781"),
            "1.234567890123456789012345678"
        );
        // Exact tie rounds to the even digit.
        assert_eq!(
            n("1.2345678901234567890123456785"),
            "1.234567890123456789012345678"
        );
        assert_eq!(
            n("1.2345678901234567890123456775"),
            "1.234567890123456789012345678"
        );
        // A tie broken by further non-zero digits rounds up.
        assert_eq!(
            n("1.23456789012345678901234567851"),
            "1.234567890123456789012345679"
        );
        // Carry out of the top digit.
        assert_eq!(n("9.9999999999999999999999999999"), "10");
        assert_eq!(n("-0.10"), "-0.1");
        assert_eq!(
            n("12345678901234567890123456789"),
            "12345678901234567890123456790"
        );
    }

    // Expected values from CPython 3.13 (`decimal` 2.5.1, default context).
    #[test]
    fn construction_accepts_python_exponent_range() {
        for (text, sci) in [
            ("10e2147483647", "1.0E+2147483648"),
            ("1e999999999999999999", "1E+999999999999999999"),
            ("100e999999999999999997", "1.00E+999999999999999999"),
            ("1e-1999999999999999997", "1E-1999999999999999997"),
            ("0e999999999999999999", "0E+999999999999999999"),
            ("1e0000000000000000000000000000000001", "1E+1"),
        ] {
            let parsed = d(text);
            assert_eq!(parsed.to_sci_string(), sci, "str(Decimal({text:?}))");
            // The spelling reads back as the same value.
            assert_eq!(d(sci), parsed, "{text}");
        }
        for text in [
            "1e1000000000000000000",
            "10e999999999999999999",
            "1e-1999999999999999998",
            "0e1000000000000000000",
            "1e99999999999999999999999999999999999999",
        ] {
            assert_eq!(
                Decimal::parse_python(text),
                Err(DecimalError::Invalid),
                "{text}"
            );
        }
    }

    #[test]
    fn extreme_exponents_order_and_compare_without_wrapping() {
        let huge = d("10e2147483647");
        assert!(huge > Decimal::from_i64(1));
        assert!(d("1e-2147483649") < d("1e-2147483648"));
        assert_eq!(d("1e2147483648"), d("10e2147483647"));
        assert_ne!(d("1e2147483648"), d("1e2147483649"));
    }

    #[test]
    fn normalize_applies_the_default_context_limits() {
        let n = |t: &str| d(t).normalize().map(|x| x.to_sci_string());
        assert_eq!(n("1e999999"), Ok("1E+999999".to_string()));
        assert_eq!(n("10e999998"), Ok("1E+999999".to_string()));
        assert_eq!(n("1e1000000"), Err(DecimalOverflow));
        assert_eq!(n("10e2147483647"), Err(DecimalOverflow));
        assert_eq!(
            n("123456789012345678901234567895e999990"),
            Err(DecimalOverflow)
        );
        // Below Etiny (-1000026) the coefficient is rounded half to even.
        assert_eq!(n("1e-1000000"), Ok("1E-1000000".to_string()));
        assert_eq!(n("15e-1000027"), Ok("2E-1000026".to_string()));
        assert_eq!(n("25e-1000027"), Ok("2E-1000026".to_string()));
        assert_eq!(n("14e-1000027"), Ok("1E-1000026".to_string()));
        assert_eq!(n("1e-1000027"), Ok("0".to_string()));
        assert_eq!(n("1e-999999999999999999"), Ok("0".to_string()));
        assert_eq!(n("0e1000000"), Ok("0".to_string()));
    }

    #[test]
    fn a_huge_fixed_spelling_is_not_built() {
        assert_eq!(d("1e999999999999999999").try_to_fixed_string(), None);
        assert_eq!(d("1e-999999999999999999").try_to_fixed_string(), None);
        // The infallible spelling falls back to scientific notation.
        assert_eq!(
            d("1e999999999999999999").to_fixed_string(),
            "1E+999999999999999999"
        );
        assert_eq!(d("0e999999999999999999").to_fixed_string(), "0");
    }

    #[test]
    fn to_f64_round_trips() {
        assert_eq!(d("52.30").to_f64(), 52.3);
        assert_eq!(d("1E-7").to_f64(), 1e-7);
    }
}
