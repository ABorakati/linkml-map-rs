//! Python's arbitrary-precision `int`, as the evaluators and the data paths need it.
//!
//! [`Value::Int`](crate::value::Value::Int) is the fast path. Anything outside
//! `i64` is a [`Value::BigInt`](crate::value::Value::BigInt), and every result
//! that fits `i64` again is normalised back to `Int` (see
//! [`Value::from_bigint`](crate::value::Value::from_bigint)), so a number has one
//! representation and `Int` stays what every existing path expects.
//!
//! The functions here are CPython's integer protocol on [`BigInt`]: the
//! operators (`//` and `%` floor, shifts, bitwise operators in two's
//! complement), `float(int)` with its `OverflowError`, correctly rounded
//! `int / int`, the exact `int`/`float` comparison of `float_richcompare`,
//! `int()` of a float, `hash()` and the string conversions with the
//! `sys.set_int_max_str_digits()` limit (4300 digits by default) that CPython 3.13
//! applies. Each takes the operands' integer value (an [`IntRef`], so an `i64`
//! needs no allocation) and reports Python's exception as a [`NumError`], which
//! the evaluator maps to its own error type.
//!
//! [`complex`] holds `complex`.

pub mod complex;

use std::borrow::Cow;
use std::cmp::Ordering;
use std::fmt;

pub use num_bigint::BigInt;
use num_bigint::{BigUint, Sign};
use num_integer::Integer;
use num_traits::{FromPrimitive, Signed, ToPrimitive, Zero};

/// CPython's default `sys.get_int_max_str_digits()`: the most decimal digits an
/// `int` converts to or from a `str`.
pub const MAX_STR_DIGITS: usize = 4300;

/// A Python exception raised by integer or complex arithmetic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NumError {
    /// `ZeroDivisionError`.
    ZeroDivision(&'static str),
    /// `OverflowError`.
    Overflow(&'static str),
    /// `ValueError`.
    Value(String),
    /// `MemoryError`: a result CPython could not allocate.
    Memory,
}

impl fmt::Display for NumError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NumError::ZeroDivision(m) | NumError::Overflow(m) => f.write_str(m),
            NumError::Value(m) => f.write_str(m),
            NumError::Memory => f.write_str("MemoryError"),
        }
    }
}

impl std::error::Error for NumError {}

/// `OverflowError` of `float(int)`.
pub const INT_TOO_LARGE_FOR_FLOAT: &str = "int too large to convert to float";
/// `OverflowError` of `int / int`.
pub const DIV_TOO_LARGE_FOR_FLOAT: &str = "integer division result too large for a float";

/// An integer operand: the `i64` fast path, or a borrowed big integer.
#[derive(Debug, Clone, Copy)]
pub enum IntRef<'a> {
    /// A value that fits `i64`.
    Small(i64),
    /// A value outside `i64` (or any [`BigInt`] a caller holds).
    Big(&'a BigInt),
}

impl<'a> IntRef<'a> {
    /// The value as a [`BigInt`], borrowed when it already is one.
    pub fn to_big(self) -> Cow<'a, BigInt> {
        match self {
            IntRef::Small(i) => Cow::Owned(BigInt::from(i)),
            IntRef::Big(b) => Cow::Borrowed(b),
        }
    }

    /// `n == 0`.
    pub fn is_zero(self) -> bool {
        match self {
            IntRef::Small(i) => i == 0,
            IntRef::Big(b) => b.is_zero(),
        }
    }

    /// `n < 0`.
    pub fn is_negative(self) -> bool {
        match self {
            IntRef::Small(i) => i < 0,
            IntRef::Big(b) => b.is_negative(),
        }
    }

    /// The value as `i64` when it fits.
    pub fn to_i64(self) -> Option<i64> {
        match self {
            IntRef::Small(i) => Some(i),
            IntRef::Big(b) => b.to_i64(),
        }
    }
}

impl From<i64> for IntRef<'_> {
    fn from(i: i64) -> Self {
        IntRef::Small(i)
    }
}

impl<'a> From<&'a BigInt> for IntRef<'a> {
    fn from(b: &'a BigInt) -> Self {
        IntRef::Big(b)
    }
}

// --- the arithmetic operators ---

/// `a + b`.
pub fn add(a: IntRef<'_>, b: IntRef<'_>) -> BigInt {
    &*a.to_big() + &*b.to_big()
}

/// `a - b`.
pub fn sub(a: IntRef<'_>, b: IntRef<'_>) -> BigInt {
    &*a.to_big() - &*b.to_big()
}

/// `a * b`.
pub fn mul(a: IntRef<'_>, b: IntRef<'_>) -> BigInt {
    &*a.to_big() * &*b.to_big()
}

/// `a // b` (floor division).
///
/// # Errors
/// `ZeroDivisionError` for a zero divisor.
pub fn floordiv(a: IntRef<'_>, b: IntRef<'_>) -> Result<BigInt, NumError> {
    if b.is_zero() {
        return Err(NumError::ZeroDivision("integer division or modulo by zero"));
    }
    Ok(a.to_big().div_floor(&b.to_big()))
}

/// `a % b` (the result has the divisor's sign).
///
/// # Errors
/// `ZeroDivisionError` for a zero divisor.
pub fn modulo(a: IntRef<'_>, b: IntRef<'_>) -> Result<BigInt, NumError> {
    if b.is_zero() {
        return Err(NumError::ZeroDivision("integer modulo by zero"));
    }
    Ok(a.to_big().mod_floor(&b.to_big()))
}

/// `-a`.
pub fn neg(a: IntRef<'_>) -> BigInt {
    -a.to_big().into_owned()
}

/// `abs(a)`.
pub fn abs(a: IntRef<'_>) -> BigInt {
    a.to_big().abs()
}

/// `~a`, which is `-a - 1`.
pub fn invert(a: IntRef<'_>) -> BigInt {
    -a.to_big().into_owned() - 1
}

/// `a & b`, `a | b` or `a ^ b` on the infinite two's-complement expansions.
pub fn bitwise(op: Bitwise, a: IntRef<'_>, b: IntRef<'_>) -> BigInt {
    let (a, b) = (a.to_big(), b.to_big());
    match op {
        Bitwise::And => &*a & &*b,
        Bitwise::Or => &*a | &*b,
        Bitwise::Xor => &*a ^ &*b,
    }
}

/// The bitwise operators of [`bitwise`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bitwise {
    /// `&`.
    And,
    /// `|`.
    Or,
    /// `^`.
    Xor,
}

/// `a ** b` for a non-negative exponent.
///
/// # Errors
/// `MemoryError` where CPython would run out of memory (an exponent beyond
/// `u32` with a base other than 0, 1 and -1).
pub fn pow(a: IntRef<'_>, b: IntRef<'_>) -> Result<BigInt, NumError> {
    debug_assert!(!b.is_negative());
    let base = a.to_big();
    let exp = b.to_big();
    if exp.is_zero() {
        return Ok(BigInt::from(1));
    }
    if base.is_zero() {
        return Ok(BigInt::zero());
    }
    if *base == BigInt::from(1) {
        return Ok(BigInt::from(1));
    }
    if *base == BigInt::from(-1) {
        return Ok(BigInt::from(if exp.is_even() { 1 } else { -1 }));
    }
    let Some(e) = exp.to_u32() else {
        return Err(NumError::Memory);
    };
    Ok(num_traits::pow::Pow::pow(&*base, e))
}

/// `a << n`.
///
/// # Errors
/// `MemoryError` for a shift count no allocation could satisfy.
pub fn lshift(a: IntRef<'_>, n: u64) -> Result<BigInt, NumError> {
    if a.is_zero() {
        return Ok(BigInt::zero());
    }
    // 2**36 bits is 8 GiB: where CPython raises `MemoryError` (or is killed).
    if n > 1 << 36 {
        return Err(NumError::Memory);
    }
    Ok(a.to_big().into_owned() << (n as usize))
}

/// `a >> n`: floors toward negative infinity, so shifting everything out of a
/// negative number leaves `-1`.
pub fn rshift(a: IntRef<'_>, n: u64) -> BigInt {
    let a = a.to_big();
    if n >= a.magnitude().bits() {
        return BigInt::from(if a.is_negative() { -1 } else { 0 });
    }
    &*a >> (n as usize)
}

/// The order of two integers.
pub fn cmp(a: IntRef<'_>, b: IntRef<'_>) -> Ordering {
    match (a, b) {
        (IntRef::Small(x), IntRef::Small(y)) => x.cmp(&y),
        (IntRef::Small(_), IntRef::Big(y)) => {
            // `y` is outside `i64`: its sign decides.
            if y.is_negative() {
                Ordering::Greater
            } else {
                Ordering::Less
            }
        }
        (IntRef::Big(x), IntRef::Small(_)) => {
            if x.is_negative() {
                Ordering::Less
            } else {
                Ordering::Greater
            }
        }
        (IntRef::Big(x), IntRef::Big(y)) => x.cmp(y),
    }
}

// --- conversions to and from float ---

/// `float(n)`, correctly rounded (half to even).
///
/// # Errors
/// `OverflowError` when the nearest float is out of range.
pub fn to_f64(n: IntRef<'_>) -> Result<f64, NumError> {
    match n {
        IntRef::Small(i) => Ok(i as f64),
        IntRef::Big(b) => big_to_f64(b).ok_or(NumError::Overflow(INT_TOO_LARGE_FOR_FLOAT)),
    }
}

/// `float(n)` of a big integer; `None` when it does not fit.
pub fn big_to_f64(n: &BigInt) -> Option<f64> {
    if let Some(i) = n.to_i64() {
        return Some(i as f64);
    }
    let magnitude = n.magnitude();
    let bits = magnitude.bits();
    if bits > 1024 {
        return None;
    }
    // The top 64 bits, with every lower bit folded into the lowest as a sticky
    // bit (well below the 53-bit rounding point), convert with one correct
    // rounding.
    let shift = bits - 64;
    let top = (magnitude >> shift as usize).to_u64().unwrap_or(u64::MAX);
    let sticky = magnitude.trailing_zeros().is_some_and(|tz| tz < shift);
    let rounded = (top | u64::from(sticky)) as f64;
    let value = rounded * 2f64.powi(shift as i32);
    if value.is_infinite() {
        return None;
    }
    Some(if n.is_negative() { -value } else { value })
}

/// `int(f)` for a finite float: the exact integer value, truncated toward zero.
pub fn float_to_bigint(f: f64) -> BigInt {
    debug_assert!(f.is_finite());
    BigInt::from_f64(f.trunc()).unwrap_or_default()
}

/// `a / b` of two integers: the exact quotient correctly rounded, as CPython's
/// `long_true_divide`.
///
/// # Errors
/// `ZeroDivisionError` for a zero divisor, `OverflowError` for a quotient out of
/// float range.
pub fn true_div(a: IntRef<'_>, b: IntRef<'_>) -> Result<f64, NumError> {
    if b.is_zero() {
        return Err(NumError::ZeroDivision("division by zero"));
    }
    if let (IntRef::Small(x), IntRef::Small(y)) = (a, b) {
        const EXACT: u64 = 1 << 53;
        if x.unsigned_abs() <= EXACT && y.unsigned_abs() <= EXACT {
            return Ok(x as f64 / y as f64);
        }
    }
    let negate = a.is_negative() != b.is_negative();
    let signed = |magnitude: f64| if negate { -magnitude } else { magnitude };
    if a.is_zero() {
        return Ok(signed(0.0));
    }
    let (a, b) = (a.to_big(), b.to_big());
    let (num, den) = (a.magnitude(), b.magnitude());
    // The quotient lies in (2**(diff-1), 2**(diff+1)).
    let diff = num.bits() as i64 - den.bits() as i64;
    if diff > 1025 {
        return Err(NumError::Overflow(DIV_TOO_LARGE_FOR_FLOAT));
    }
    // Scale so the integer quotient has 64 or 65 bits, keep a sticky bit for the
    // remainder, then round once to 53 bits (fewer for a subnormal result).
    let scale = 64 - diff;
    let (numerator, denominator) = if scale >= 0 {
        (Cow::Owned(num << scale as usize), Cow::Borrowed(den))
    } else {
        (Cow::Borrowed(num), Cow::Owned(den << (-scale) as usize))
    };
    let (quotient, remainder) = numerator.div_rem(&denominator);
    let sticky = !remainder.is_zero();
    let q = quotient.to_u128().unwrap_or(u128::MAX);
    let q_bits = 128 - q.leading_zeros() as i64;
    // Exponent of the unit in the last place of the result.
    let ulp_exp = (q_bits - 1 - scale - 52).max(-1074);
    let drop = ulp_exp + scale;
    if drop > q_bits {
        return Ok(signed(0.0));
    }
    let drop = drop as u32;
    let mut m = q >> drop;
    let rest = q & ((1u128 << drop) - 1);
    let half = 1u128 << (drop - 1);
    if rest > half || (rest == half && (sticky || m & 1 == 1)) {
        m += 1;
    }
    let value = ldexp(m as f64, ulp_exp as i32);
    if value.is_infinite() {
        return Err(NumError::Overflow(DIV_TOO_LARGE_FOR_FLOAT));
    }
    Ok(signed(value))
}

/// `m * 2**e` for an `m` below 2**54 whose product is representable (or
/// overflows to infinity).
fn ldexp(m: f64, e: i32) -> f64 {
    let half = e / 2;
    m * 2f64.powi(half) * 2f64.powi(e - half)
}

/// `float_richcompare` for an integer and a float: the exact order, or `None`
/// when the float is NaN.
pub fn cmp_int_f64(n: IntRef<'_>, f: f64) -> Option<Ordering> {
    match n {
        IntRef::Small(i) => crate::value::cmp_int_float(i, f),
        IntRef::Big(b) => cmp_big_f64(b, f),
    }
}

/// [`cmp_int_f64`] for a big integer.
pub fn cmp_big_f64(n: &BigInt, f: f64) -> Option<Ordering> {
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
    let floor = f.floor();
    // `floor` is an integer-valued float, so the conversion is exact.
    let floor_int = BigInt::from_f64(floor).unwrap_or_default();
    Some(n.cmp(&floor_int).then(if f > floor {
        Ordering::Less
    } else {
        Ordering::Equal
    }))
}

/// `a == f` for a big integer and a float.
pub fn big_eq_f64(n: &BigInt, f: f64) -> bool {
    cmp_big_f64(n, f) == Some(Ordering::Equal)
}

// --- strings ---

/// `str(n)`.
///
/// # Errors
/// `ValueError` when `n` has more than [`MAX_STR_DIGITS`] decimal digits.
pub fn int_to_string(n: IntRef<'_>) -> Result<String, NumError> {
    match n {
        IntRef::Small(i) => Ok(i.to_string()),
        IntRef::Big(b) => big_to_string(b),
    }
}

/// [`int_to_string`] for a big integer.
pub fn big_to_string(n: &BigInt) -> Result<String, NumError> {
    // 2**14400 has 4335 digits: no need to convert to know it is too long.
    if n.magnitude().bits() > 14_400 {
        return Err(too_many_digits_to_str());
    }
    let text = n.to_string();
    let digits = text.len() - usize::from(n.is_negative());
    if digits > MAX_STR_DIGITS {
        return Err(too_many_digits_to_str());
    }
    Ok(text)
}

fn too_many_digits_to_str() -> NumError {
    NumError::Value(format!(
        "Exceeds the limit ({MAX_STR_DIGITS} digits) for integer string conversion; \
         use sys.set_int_max_str_digits() to increase the limit"
    ))
}

/// The error `int(text)` raises for more than [`MAX_STR_DIGITS`] digits
/// (`digits` of them).
pub fn too_many_digits_from_str(digits: usize) -> NumError {
    NumError::Value(format!(
        "Exceeds the limit ({MAX_STR_DIGITS} digits) for integer string conversion: \
         value has {digits} digits; use sys.set_int_max_str_digits() to increase the limit"
    ))
}

/// The value of `digits` (ASCII digits valid for `radix`, no sign, no
/// underscores) in `radix`.
///
/// # Errors
/// `ValueError` when a non-power-of-two base has more than [`MAX_STR_DIGITS`]
/// digits, as `int(text, base)` does.
pub fn parse_digits(digits: &str, radix: u32) -> Result<BigInt, NumError> {
    if !radix.is_power_of_two() && digits.len() > MAX_STR_DIGITS {
        // Leading zeros count: CPython checks the digit run it scanned.
        return Err(too_many_digits_from_str(digits.len()));
    }
    Ok(BigUint::parse_bytes(digits.as_bytes(), radix)
        .map(BigInt::from)
        .unwrap_or_default())
}

/// A signed [`parse_digits`].
///
/// # Errors
/// As [`parse_digits`].
pub fn parse_signed_digits(negative: bool, digits: &str, radix: u32) -> Result<BigInt, NumError> {
    let magnitude = parse_digits(digits, radix)?;
    Ok(if negative { -magnitude } else { magnitude })
}

// --- hash ---

/// `hash(n)`: the value modulo the Mersenne prime 2**61 - 1, with the sign of
/// `n` and `-1` mapped to `-2`.
pub fn hash(n: IntRef<'_>) -> i64 {
    const MODULUS: i64 = (1 << 61) - 1;
    let n = n.to_big();
    let reduced = n.magnitude() % BigUint::from(MODULUS as u64);
    let magnitude = reduced.to_i64().unwrap_or(0);
    let signed = if n.is_negative() {
        -magnitude
    } else {
        magnitude
    };
    if signed == -1 { -2 } else { signed }
}

/// `n.bit_length()`.
pub fn bit_length(n: &BigInt) -> u64 {
    n.magnitude().bits()
}

/// `n.bit_count()`: the number of ones in the magnitude.
pub fn bit_count(n: &BigInt) -> u64 {
    n.magnitude().count_ones()
}

/// The sign of a big integer's value as an [`Ordering`] against zero.
pub fn sign_ordering(n: &BigInt) -> Ordering {
    match n.sign() {
        Sign::Minus => Ordering::Less,
        Sign::NoSign => Ordering::Equal,
        Sign::Plus => Ordering::Greater,
    }
}

#[cfg(test)]
mod tests;
