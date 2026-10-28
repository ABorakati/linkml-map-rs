//! The numeric tower of the restricted evaluator: `bool`, `int` (an `i64` fast
//! path and Python's arbitrary-precision integers), `float` and `complex`, under
//! simpleeval's operators and its `safe_power` / `safe_lshift` / `safe_rshift`
//! limits.
//!
//! [`try_native_binary`](super::eval) hands the operators here once both
//! operands are numbers. `Ok(None)` is the Python `TypeError` (a bitwise
//! operator on a float, `//` on a complex): the null-propagating wrapper's retry
//! path.

use std::cmp::Ordering;

use super::error::{ExprError, ExprResult};
use super::eval::py_str;
use super::parser::BinOp;
use crate::pynum::complex::{self, Complex64};
use crate::pynum::{self, BigInt, Bitwise, IntRef, NumError};
use crate::value::Value;
use num_traits::Signed;

// simpleeval's limits (`simpleeval.MAX_POWER`, `MAX_SHIFT`), enforced by its
// `safe_power` and `safe_lshift`/`safe_rshift` operators, which `LinkMLEvaluator`
// wraps. Exceeding one raises `NumberTooHigh`, an `InvalidExpression`: an
// [`ExprError::Eval`].
const MAX_POWER: i64 = 4_000_000;
const MAX_SHIFT: i64 = 10_000;

/// A numeric operand: what `bool`, `int`, `float` and `complex` operators see.
#[derive(Clone, Copy)]
pub(super) enum Arith<'a> {
    Int(IntRef<'a>),
    Float(f64),
    Complex(Complex64),
}

/// A value as an arithmetic operand (a `bool` is the integer 0 or 1).
pub(super) fn as_arith(v: &Value) -> Option<Arith<'_>> {
    match v {
        Value::Int(i) => Some(Arith::Int(IntRef::Small(*i))),
        Value::Bool(b) => Some(Arith::Int(IntRef::Small(i64::from(*b)))),
        Value::BigInt(n) => Some(Arith::Int(IntRef::Big(n))),
        Value::Float(f) => Some(Arith::Float(*f)),
        Value::Complex(z) => Some(Arith::Complex(**z)),
        _ => None,
    }
}

impl Arith<'_> {
    /// `float(x)`: an integer beyond the float range is an `OverflowError`.
    fn to_f64(self) -> ExprResult<f64> {
        match self {
            Arith::Int(n) => Ok(pynum::to_f64(n)?),
            Arith::Float(f) => Ok(f),
            Arith::Complex(_) => unreachable!("a complex has no float value"),
        }
    }

    /// `complex(x)` of a real operand.
    fn to_complex(self) -> ExprResult<Complex64> {
        match self {
            Arith::Complex(z) => Ok(z),
            real => Ok(Complex64::new(real.to_f64()?, 0.0)),
        }
    }

    /// `abs(x) > limit` for a limit that fits `i64` (`NaN` compares false).
    fn abs_exceeds(self, limit: i64) -> ExprResult<bool> {
        Ok(match self {
            Arith::Int(IntRef::Small(i)) => i.unsigned_abs() > limit as u64,
            // Outside `i64`, so beyond any limit.
            Arith::Int(IntRef::Big(_)) => true,
            Arith::Float(f) => f.abs() > limit as f64,
            Arith::Complex(z) => complex::abs(z)? > limit as f64,
        })
    }
}

impl From<NumError> for ExprError {
    fn from(e: NumError) -> Self {
        match e {
            // Python `ValueError`: a rejection the unrestricted fallback may catch.
            NumError::Value(m) => ExprError::Eval(m),
            // `ZeroDivisionError`, `OverflowError`, `MemoryError`: never caught.
            other => ExprError::Uncaught(other.to_string()),
        }
    }
}

/// `x op y` for two numbers, after the `safe_*` checks of simpleeval.
pub(super) fn native_binary(op: BinOp, left: &Value, right: &Value) -> ExprResult<Option<Value>> {
    let (Some(l), Some(r)) = (as_arith(left), as_arith(right)) else {
        return Ok(None);
    };
    // `safe_power`, `safe_lshift`, `safe_rshift`: operands are checked before the
    // operator runs, whatever their type.
    match op {
        BinOp::Pow => check_power_limits(l, r, left, right)?,
        BinOp::LShift | BinOp::RShift => check_shift_limits(op, l, r, left, right)?,
        _ => {}
    }
    match (l, r) {
        (Arith::Int(a), Arith::Int(b)) => int_binary(op, a, b),
        (Arith::Complex(_), _) | (_, Arith::Complex(_)) => {
            complex_binary(op, l.to_complex()?, r.to_complex()?)
        }
        _ => float_binary(op, l.to_f64()?, r.to_f64()?),
    }
}

/// `simpleeval.safe_power`: `abs(a) > MAX_POWER or abs(b) > MAX_POWER` raises
/// `NumberTooHigh`. Called with numeric operands only.
fn check_power_limits(a: Arith<'_>, b: Arith<'_>, left: &Value, right: &Value) -> ExprResult<()> {
    if a.abs_exceeds(MAX_POWER)? || b.abs_exceeds(MAX_POWER)? {
        return Err(ExprError::Eval(format!(
            "Sorry! I don't want to evaluate {} ** {}",
            py_str(left),
            py_str(right)
        )));
    }
    Ok(())
}

/// `simpleeval.safe_lshift`/`safe_rshift`: `abs(b) > MAX_SHIFT or abs(a) >
/// MAX_SHIFT_BASE` raises `NumberTooHigh` (`MAX_SHIFT_BASE` is
/// `int(sys.float_info.max)`, so only an infinite magnitude exceeds it). Called
/// with numeric operands only.
fn check_shift_limits(
    op: BinOp,
    a: Arith<'_>,
    b: Arith<'_>,
    left: &Value,
    right: &Value,
) -> ExprResult<()> {
    let base_too_big = match a {
        Arith::Int(IntRef::Small(_)) => false,
        Arith::Int(IntRef::Big(n)) => {
            pynum::cmp_big_f64(&n.abs(), f64::MAX) == Some(Ordering::Greater)
        }
        Arith::Float(f) => f.abs() > f64::MAX,
        Arith::Complex(z) => complex::abs(z)? > f64::MAX,
    };
    if b.abs_exceeds(MAX_SHIFT)? || base_too_big {
        return Err(ExprError::Eval(format!(
            "Sorry! I don't want to evaluate {} {} {}",
            py_str(left),
            super::eval::binop_symbol(op),
            py_str(right)
        )));
    }
    Ok(())
}

// --- int ---

/// `a op b` for two `i64`s when the result is a plain integer or float and no
/// limit or error is involved; `None` sends the operation down the general path
/// (overflow, division by zero, `**` and the shifts with their simpleeval limits).
#[inline]
pub(super) fn int_fast_path(op: BinOp, a: i64, b: i64) -> Option<Value> {
    match op {
        BinOp::Add => a.checked_add(b).map(Value::Int),
        BinOp::Sub => a.checked_sub(b).map(Value::Int),
        BinOp::Mul => a.checked_mul(b).map(Value::Int),
        BinOp::FloorDiv if b != 0 => python_floordiv_int(a, b).map(Value::Int),
        BinOp::Mod if b != 0 => Some(Value::Int(python_mod_int(a, b))),
        BinOp::Div if b != 0 => Some(Value::Float(int_truediv(a, b))),
        BinOp::BitAnd => Some(Value::Int(a & b)),
        BinOp::BitOr => Some(Value::Int(a | b)),
        BinOp::BitXor => Some(Value::Int(a ^ b)),
        _ => None,
    }
}

/// `a op b` for two floats where the result is a plain float (`None` for the
/// operators with limits, errors or a retry path).
#[inline]
pub(super) fn float_fast_path(op: BinOp, a: f64, b: f64) -> Option<Value> {
    match op {
        BinOp::Add => Some(Value::Float(a + b)),
        BinOp::Sub => Some(Value::Float(a - b)),
        BinOp::Mul => Some(Value::Float(a * b)),
        BinOp::Div if b != 0.0 => Some(Value::Float(a / b)),
        _ => None,
    }
}

/// What the `i64` fast path of an integer operator made of its operands.
enum Fast {
    Done(Value),
    /// The result needs more than 64 bits.
    Overflow,
}

fn int_binary(op: BinOp, a: IntRef<'_>, b: IntRef<'_>) -> ExprResult<Option<Value>> {
    if let (IntRef::Small(x), IntRef::Small(y)) = (a, b) {
        match small_int_binary(op, x, y)? {
            Some(Fast::Done(v)) => return Ok(Some(v)),
            Some(Fast::Overflow) => {}
            None => return Ok(None),
        }
    }
    big_int_binary(op, a, b)
}

/// An integer operator on two `i64`s. `None` is the retry path (`@`).
fn small_int_binary(op: BinOp, a: i64, b: i64) -> ExprResult<Option<Fast>> {
    let done = |v: Option<i64>| {
        Ok(Some(
            v.map_or(Fast::Overflow, |i| Fast::Done(Value::Int(i))),
        ))
    };
    match op {
        BinOp::Add => done(a.checked_add(b)),
        BinOp::Sub => done(a.checked_sub(b)),
        BinOp::Mul => done(a.checked_mul(b)),
        BinOp::Div => {
            // Python `/` always yields float.
            if b == 0 {
                return Err(ExprError::zero_division("division by zero"));
            }
            Ok(Some(Fast::Done(Value::Float(int_truediv(a, b)))))
        }
        BinOp::FloorDiv => {
            if b == 0 {
                return Err(ExprError::zero_division(
                    "integer division or modulo by zero",
                ));
            }
            // `-2**63 // -1` is 2**63.
            done(python_floordiv_int(a, b))
        }
        BinOp::Mod => {
            if b == 0 {
                return Err(ExprError::zero_division("integer modulo by zero"));
            }
            done(Some(python_mod_int(a, b)))
        }
        BinOp::Pow => {
            if b < 0 {
                // Python returns a float for negative integer exponents,
                // computed as `float(a) ** float(b)`.
                return Ok(Some(Fast::Done(float_pow(a as f64, b as f64)?)));
            }
            match u32::try_from(b) {
                Ok(exp) => done(a.checked_pow(exp)),
                Err(_) => Ok(Some(Fast::Overflow)),
            }
        }
        BinOp::LShift => {
            let count = shift_count(b)?;
            if a == 0 {
                return done(Some(0));
            }
            if count >= 64 {
                return Ok(Some(Fast::Overflow));
            }
            done(i64::try_from(i128::from(a) << count).ok())
        }
        BinOp::RShift => {
            let count = shift_count(b)?;
            // Shifting everything out leaves the sign.
            done(Some(a >> count.min(63)))
        }
        BinOp::BitAnd => done(Some(a & b)),
        BinOp::BitOr => done(Some(a | b)),
        BinOp::BitXor => done(Some(a ^ b)),
        // Refused before any operand is evaluated (see `syntax::matmul_refused`).
        BinOp::MatMul => Ok(None),
    }
}

/// The shift count of an `int << int` or `int >> int` that passed
/// [`check_shift_limits`]: a negative one is Python's own `ValueError`.
fn shift_count(b: i64) -> ExprResult<u32> {
    u32::try_from(b).map_err(|_| ExprError::Eval("negative shift count".into()))
}

/// An integer operator where an operand, or the result, is beyond `i64`.
fn big_int_binary(op: BinOp, a: IntRef<'_>, b: IntRef<'_>) -> ExprResult<Option<Value>> {
    let value = |n: BigInt| Ok(Some(Value::from_bigint(n)));
    match op {
        BinOp::Add => value(pynum::add(a, b)),
        BinOp::Sub => value(pynum::sub(a, b)),
        BinOp::Mul => value(pynum::mul(a, b)),
        BinOp::Div => Ok(Some(Value::Float(pynum::true_div(a, b)?))),
        BinOp::FloorDiv => value(pynum::floordiv(a, b)?),
        BinOp::Mod => value(pynum::modulo(a, b)?),
        BinOp::Pow => {
            if b.is_negative() {
                // `float(a) ** float(b)`; either conversion may overflow.
                let (x, y) = (pynum::to_f64(a)?, pynum::to_f64(b)?);
                return Ok(Some(float_pow(x, y)?));
            }
            value(pynum::pow(a, b)?)
        }
        BinOp::LShift | BinOp::RShift => {
            // `check_shift_limits` bounded the count by `MAX_SHIFT`.
            let count = b.to_i64().unwrap_or(i64::MAX);
            let count = u64::from(shift_count(count)?);
            if op == BinOp::LShift {
                value(pynum::lshift(a, count)?)
            } else {
                value(pynum::rshift(a, count))
            }
        }
        BinOp::BitAnd => value(pynum::bitwise(Bitwise::And, a, b)),
        BinOp::BitOr => value(pynum::bitwise(Bitwise::Or, a, b)),
        BinOp::BitXor => value(pynum::bitwise(Bitwise::Xor, a, b)),
        BinOp::MatMul => Ok(None),
    }
}

/// CPython's `int / int`: the correctly rounded quotient of the exact values.
///
/// Converting each operand to `f64` first rounds twice, which differs from
/// Python in the last place once an operand exceeds 2**53.
fn int_truediv(a: i64, b: i64) -> f64 {
    const EXACT: u64 = 1 << 53;
    if a.unsigned_abs() <= EXACT && b.unsigned_abs() <= EXACT || a == 0 {
        return a as f64 / b as f64;
    }
    // Normalise both magnitudes to 64 significant bits, divide in 128 bits and
    // fold the remainder into a sticky bit so `as f64` rounds the exact quotient.
    let (ua, ub) = (a.unsigned_abs(), b.unsigned_abs());
    let (la, lb) = (ua.leading_zeros(), ub.leading_zeros());
    let (na, nb) = (u128::from(ua << la), u128::from(ub << lb));
    let numerator = na << 64;
    let mut quotient = numerator / nb;
    if numerator % nb != 0 {
        quotient |= 1;
    }
    // quotient / 2**64 is in (0.5, 2); undo the normalising shifts.
    let magnitude = quotient as f64 * 2f64.powi(lb as i32 - la as i32 - 64);
    if (a < 0) != (b < 0) {
        -magnitude
    } else {
        magnitude
    }
}

/// Python floor division for ints (floors toward negative infinity); `None`
/// for `-2**63 // -1`, which is outside `i64`.
fn python_floordiv_int(a: i64, b: i64) -> Option<i64> {
    let q = a.checked_div(b)?;
    let r = a % b;
    Some(if (r != 0) && ((r < 0) != (b < 0)) {
        q - 1
    } else {
        q
    })
}

/// Python modulo for ints (result has sign of divisor).
fn python_mod_int(a: i64, b: i64) -> i64 {
    // `-2**63 % -1` is 0, where the plain `%` would overflow.
    let r = a.wrapping_rem(b);
    if r != 0 && (r < 0) != (b < 0) {
        r + b
    } else {
        r
    }
}

// --- float ---

/// CPython's `float ** float` (`float_pow`) for finite or infinite operands.
///
/// The `ZeroDivisionError` and `OverflowError` are not caught upstream
/// ([`ExprError::Uncaught`]); a negative base with a fractional exponent is a
/// `complex`.
fn float_pow(a: f64, b: f64) -> ExprResult<Value> {
    if a == 0.0 && b < 0.0 {
        return Err(ExprError::Uncaught(
            "0.0 cannot be raised to a negative power".into(),
        ));
    }
    if a < 0.0 && a.is_finite() && b.is_finite() && b.fract() != 0.0 {
        let z = complex::pow(Complex64::new(a, 0.0), Complex64::new(b, 0.0))?;
        return Ok(Value::from_complex(z));
    }
    let result = a.powf(b);
    if result.is_infinite() && a.is_finite() && b.is_finite() {
        return Err(ExprError::Uncaught(
            "(34, 'Numerical result out of range')".into(),
        ));
    }
    Ok(Value::Float(result))
}

fn float_binary(op: BinOp, a: f64, b: f64) -> ExprResult<Option<Value>> {
    let v = match op {
        BinOp::Add => Value::Float(a + b),
        BinOp::Sub => Value::Float(a - b),
        BinOp::Mul => Value::Float(a * b),
        BinOp::Div => {
            if b == 0.0 {
                return Err(ExprError::zero_division("float division by zero"));
            }
            Value::Float(a / b)
        }
        BinOp::FloorDiv => {
            if b == 0.0 {
                return Err(ExprError::zero_division("float floor division by zero"));
            }
            Value::Float(python_floordiv_float(a, b))
        }
        BinOp::Mod => {
            if b == 0.0 {
                return Err(ExprError::zero_division("float modulo by zero"));
            }
            Value::Float(python_mod_float(a, b))
        }
        BinOp::Pow => float_pow(a, b)?,
        // Bitwise/shift on floats is a TypeError in Python → retry path.
        BinOp::LShift
        | BinOp::RShift
        | BinOp::BitAnd
        | BinOp::BitOr
        | BinOp::BitXor
        | BinOp::MatMul => {
            return Ok(None);
        }
    };
    Ok(Some(v))
}

/// Numeric retry path: both operands coerced to f64.
pub(super) fn numeric_binary_f64(op: BinOp, a: f64, b: f64) -> ExprResult<Value> {
    match float_binary(op, a, b)? {
        Some(v) => Ok(v),
        None => Ok(Value::Null), // bitwise on floats
    }
}

/// CPython's `float % float` (`float_rem`): the result has the divisor's sign,
/// and a zero result is `copysign(0.0, b)`.
fn python_mod_float(a: f64, b: f64) -> f64 {
    let r = a % b;
    if r == 0.0 {
        return 0.0_f64.copysign(b);
    }
    if (r < 0.0) != (b < 0.0) { r + b } else { r }
}

/// CPython's `float // float` (`float_divmod`'s quotient), which differs from
/// `(a / b).floor()` (`1 // 0.1` is `9.0`).
fn python_floordiv_float(a: f64, b: f64) -> f64 {
    let modulus = a % b;
    let mut div = (a - modulus) / b;
    if modulus != 0.0 && (b < 0.0) != (modulus < 0.0) {
        div -= 1.0;
    }
    if div == 0.0 {
        return 0.0_f64.copysign(a / b);
    }
    let floored = div.floor();
    if div - floored > 0.5 {
        floored + 1.0
    } else {
        floored
    }
}

// --- complex ---

/// A binary operator where an operand is a `complex`. `//`, `%` and the bitwise
/// operators are a `TypeError` (the retry path).
fn complex_binary(op: BinOp, a: Complex64, b: Complex64) -> ExprResult<Option<Value>> {
    let value = |z: Complex64| Ok(Some(Value::from_complex(z)));
    match op {
        BinOp::Add => value(Complex64::new(a.re + b.re, a.im + b.im)),
        BinOp::Sub => value(Complex64::new(a.re - b.re, a.im - b.im)),
        BinOp::Mul => value(complex::prod(a, b)),
        BinOp::Div => value(complex::quot(a, b)?),
        BinOp::Pow => value(complex::pow(a, b)?),
        _ => Ok(None),
    }
}

// --- ordering ---

/// A numeric operand of an ordering comparison.
pub(super) enum Ordered<'a> {
    Int(IntRef<'a>),
    Float(f64),
}

/// Numeric view for ordering: bool, int, float are all numeric here (a
/// `complex` does not order).
pub(super) fn numeric_for_ordering(v: &Value) -> Option<Ordered<'_>> {
    match v {
        Value::Bool(b) => Some(Ordered::Int(IntRef::Small(i64::from(*b)))),
        Value::Int(i) => Some(Ordered::Int(IntRef::Small(*i))),
        Value::BigInt(n) => Some(Ordered::Int(IntRef::Big(n))),
        Value::Float(f) => Some(Ordered::Float(*f)),
        _ => None,
    }
}

/// The order of two numbers, exact between an `int` and a `float` as in CPython;
/// `None` for a NaN.
pub(super) fn cmp_ordered(a: &Ordered<'_>, b: &Ordered<'_>) -> Option<Ordering> {
    match (a, b) {
        (Ordered::Int(x), Ordered::Int(y)) => Some(pynum::cmp(*x, *y)),
        (Ordered::Float(x), Ordered::Float(y)) => x.partial_cmp(y),
        (Ordered::Int(x), Ordered::Float(y)) => pynum::cmp_int_f64(*x, *y),
        (Ordered::Float(x), Ordered::Int(y)) => pynum::cmp_int_f64(*y, *x).map(Ordering::reverse),
    }
}
