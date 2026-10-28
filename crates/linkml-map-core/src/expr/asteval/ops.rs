//! Python's operators, subscripting and iteration over [`Py`], with
//! `asteval`'s `safe_*` limits (`astutils.py`).

use std::collections::VecDeque;
use std::rc::Rc;

use super::ast::{BinOp, CmpOp, UnaryOp};
use super::value::*;
use crate::pynum::complex::{self, Complex64};
use crate::pynum::{self, BigInt, Bitwise, IntRef, NumError};
use num_traits::Signed;

/// `astutils.MAX_EXPONENT`.
pub(crate) const MAX_EXPONENT: i64 = 10000;
/// `astutils.MAX_STR_LEN` (`2 << 17`).
pub(crate) const MAX_STR_LEN: i64 = 2 << 17;
/// `astutils.MAX_SHIFT`.
pub(crate) const MAX_SHIFT: i64 = 1000;

fn zero_div<T>(msg: &str) -> PyRes<T> {
    Err(exc("ZeroDivisionError", msg))
}

/// An integer or bool as an index or count; an integer beyond `i64` saturates
/// (which clamps a slice bound as Python does and still fails an item index).
fn is_int_like(v: &Py) -> Option<i64> {
    match v {
        Py::Bool(b) => Some(*b as i64),
        Py::Int(i) => Some(*i),
        Py::BigInt(n) => Some(if n.sign() == num_bigint::Sign::Minus {
            i64::MIN
        } else {
            i64::MAX
        }),
        _ => None,
    }
}

/// An integer or bool as an arithmetic operand.
fn int_ref(v: &Py) -> Option<IntRef<'_>> {
    match v {
        Py::Bool(b) => Some(IntRef::Small(*b as i64)),
        Py::Int(i) => Some(IntRef::Small(*i)),
        Py::BigInt(n) => Some(IntRef::Big(n)),
        _ => None,
    }
}

/// The Python exception of a [`NumError`].
pub(crate) fn num_err(e: NumError) -> Box<ExcObj> {
    match e {
        NumError::ZeroDivision(m) => exc("ZeroDivisionError", m),
        NumError::Overflow(m) => exc("OverflowError", m),
        NumError::Value(m) => exc("ValueError", m),
        NumError::Memory => exc("MemoryError", ""),
    }
}

fn unsupported_operand<T>(sym: &str, a: &Py, b: &Py) -> PyRes<T> {
    type_err(format!(
        "unsupported operand type(s) for {sym}: '{}' and '{}'",
        a.type_name(),
        b.type_name()
    ))
}

/// A float modulo with Python's sign convention.
pub(crate) fn float_mod(x: f64, y: f64) -> PyRes<f64> {
    if y == 0.0 {
        return zero_div("float modulo by zero");
    }
    let mut m = x % y;
    if m != 0.0 {
        if (y < 0.0) != (m < 0.0) {
            m += y;
        }
    } else {
        m = 0.0_f64.copysign(y);
    }
    Ok(m)
}

/// `float.__floordiv__`.
pub(crate) fn float_floordiv(x: f64, y: f64) -> PyRes<f64> {
    if y == 0.0 {
        return zero_div("float floor division by zero");
    }
    let mut m = x % y;
    let mut div = (x - m) / y;
    if m != 0.0 && (y < 0.0) != (m < 0.0) {
        div -= 1.0;
    }
    let floordiv = if div != 0.0 {
        let f = div.floor();
        if div - f > 0.5 { f + 1.0 } else { f }
    } else {
        0.0_f64.copysign(x / y)
    };
    let _ = &mut m;
    Ok(floordiv)
}

fn pow_int(base: IntRef<'_>, exp: IntRef<'_>) -> PyRes<Py> {
    if exp.is_negative() {
        if base.is_zero() {
            return zero_div("0.0 cannot be raised to a negative power");
        }
        // `float(base) ** float(exp)`; an operand beyond float range overflows.
        let (x, y) = (
            pynum::to_f64(base).map_err(num_err)?,
            pynum::to_f64(exp).map_err(num_err)?,
        );
        return pow_float(x, y);
    }
    if let (IntRef::Small(b), IntRef::Small(e)) = (base, exp)
        && let Ok(e) = u32::try_from(e)
        && let Some(r) = b.checked_pow(e)
    {
        return Ok(Py::Int(r));
    }
    pynum::pow(base, exp).map(Py::from_big).map_err(num_err)
}

fn pow_float(x: f64, y: f64) -> PyRes<Py> {
    if x == 0.0 && y < 0.0 {
        return zero_div("0.0 cannot be raised to a negative power");
    }
    if x < 0.0 && y.is_finite() && y.fract() != 0.0 {
        return complex::pow(Complex64::new(x, 0.0), Complex64::new(y, 0.0))
            .map(|z| Py::Complex(z.re, z.im))
            .map_err(num_err);
    }
    let r = x.powf(y);
    if r.is_infinite() && x.is_finite() && y.is_finite() {
        return overflow_err("(34, 'Numerical result out of range')");
    }
    Ok(Py::Float(r))
}

fn repeat_len(len: usize, n: i64) -> PyRes<usize> {
    if n <= 0 {
        return Ok(0);
    }
    let total = (len as i128) * (n as i128);
    if total > (1 << 28) {
        return Err(exc("MemoryError", ""));
    }
    Ok(n as usize)
}

/// `op2func(op)(a, b)` for the arithmetic operators, including the `safe_*`
/// wrappers.
pub(crate) fn binop(op: BinOp, a: &Py, b: &Py) -> PyRes<Py> {
    if (matches!(a, Py::Complex(..)) || matches!(b, Py::Complex(..)))
        && let (Some(x), Some(y)) = (complex_operand(a), complex_operand(b))
    {
        return complex_binop(op, x?, y?, a, b);
    }
    match op {
        BinOp::Add => add(a, b),
        BinOp::Sub => sub(a, b),
        BinOp::Mult => mult(a, b),
        BinOp::Div => div(a, b),
        BinOp::FloorDiv => floordiv(a, b),
        BinOp::Mod => modulo(a, b),
        BinOp::Pow => pow(a, b),
        BinOp::LShift => lshift(a, b),
        BinOp::RShift => rshift(a, b),
        BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor => bitop(op, a, b),
        BinOp::MatMult => unsupported_operand("@", a, b),
    }
}

fn add(a: &Py, b: &Py) -> PyRes<Py> {
    // safe_add
    if let (Py::Str(x), Py::Str(y)) = (a, b) {
        if (x.chars().count() + y.chars().count()) as i64 > MAX_STR_LEN {
            return runtime_err(format!(
                "String length exceeded, max string length is {MAX_STR_LEN}"
            ));
        }
        let mut s = String::with_capacity(x.len() + y.len());
        s.push_str(x);
        s.push_str(y);
        return Ok(Py::str(s));
    }
    if let (Some(x), Some(y)) = (as_num(a), as_num(b)) {
        return Ok(match (x, y) {
            (NumV::I(x), NumV::I(y)) => match (x, y) {
                (IntRef::Small(p), IntRef::Small(q)) => p
                    .checked_add(q)
                    .map_or_else(|| Py::from_big(pynum::add(x, y)), Py::Int),
                _ => Py::from_big(pynum::add(x, y)),
            },
            (x, y) => Py::Float(to_f(x)? + to_f(y)?),
        });
    }
    match (a, b) {
        (Py::List(x), Py::List(y)) => {
            let mut v = x.borrow().clone();
            v.extend(y.borrow().iter().cloned());
            Ok(Py::list(v))
        }
        (Py::Tuple(x), Py::Tuple(y)) => {
            let mut v = x.to_vec();
            v.extend(y.iter().cloned());
            Ok(Py::tuple(v))
        }
        (Py::Bytes(x), Py::Bytes(y)) => {
            let mut v = x.to_vec();
            v.extend(y.iter());
            Ok(Py::Bytes(v.into()))
        }
        (Py::Str(_), _) => type_err(format!(
            "can only concatenate str (not \"{}\") to str",
            b.type_name()
        )),
        (Py::List(_), _) => type_err(format!(
            "can only concatenate list (not \"{}\") to list",
            b.type_name()
        )),
        (Py::Tuple(_), _) => type_err(format!(
            "can only concatenate tuple (not \"{}\") to tuple",
            b.type_name()
        )),
        _ => unsupported_operand("+", a, b),
    }
}

/// `float(n)`: an integer beyond the float range is an `OverflowError`.
pub(crate) fn to_f(n: NumV) -> PyRes<f64> {
    match n {
        NumV::I(i) => pynum::to_f64(i).map_err(num_err),
        NumV::F(f) => Ok(f),
    }
}

/// A set operator on two set objects; `x op x` is the same object, which
/// CPython treats specially (a copy for `|` and `&`).
fn set_binary(x: &Rf<PySet>, y: &Rf<PySet>, op: impl Fn(&PySet, &PySet) -> PySet) -> PySet {
    let (xb, yb) = (x.borrow(), y.borrow());
    if Rc::ptr_eq(x, y) {
        // The same reference twice: `op` sees one object.
        return op(&xb, &xb);
    }
    op(&xb, &yb)
}

fn sub(a: &Py, b: &Py) -> PyRes<Py> {
    if let (Some(x), Some(y)) = (as_num(a), as_num(b)) {
        return Ok(match (x, y) {
            (NumV::I(x), NumV::I(y)) => match (x, y) {
                (IntRef::Small(p), IntRef::Small(q)) => p
                    .checked_sub(q)
                    .map_or_else(|| Py::from_big(pynum::sub(x, y)), Py::Int),
                _ => Py::from_big(pynum::sub(x, y)),
            },
            (x, y) => Py::Float(to_f(x)? - to_f(y)?),
        });
    }
    if let (Py::Set(x), Py::Set(y)) = (a, b) {
        let out = set_binary(x, y, |x, y| x.difference(y));
        return Ok(Py::Set(rf(out)));
    }
    unsupported_operand("-", a, b)
}

fn mult(a: &Py, b: &Py) -> PyRes<Py> {
    // safe_mult
    if let (Py::Str(s), Some(n)) = (a, int_ref(b))
        && (BigInt::from(s.chars().count()) * &*n.to_big()) > BigInt::from(MAX_STR_LEN)
    {
        return runtime_err(format!(
            "String length exceeded, max string length is {MAX_STR_LEN}"
        ));
    }
    if let (Some(x), Some(y)) = (as_num(a), as_num(b)) {
        return Ok(match (x, y) {
            (NumV::I(x), NumV::I(y)) => match (x, y) {
                (IntRef::Small(p), IntRef::Small(q)) => p
                    .checked_mul(q)
                    .map_or_else(|| Py::from_big(pynum::mul(x, y)), Py::Int),
                _ => Py::from_big(pynum::mul(x, y)),
            },
            (x, y) => Py::Float(to_f(x)? * to_f(y)?),
        });
    }
    let (seq, n) = match (a, int_ref(a), b, int_ref(b)) {
        (_, _, _, Some(n)) if is_seq(a) => (a, n),
        (_, Some(n), _, _) if is_seq(b) => (b, n),
        _ => {
            if is_seq(a) {
                return type_err(format!(
                    "can't multiply sequence by non-int of type '{}'",
                    b.type_name()
                ));
            }
            if is_seq(b) {
                return type_err(format!(
                    "can't multiply sequence by non-int of type '{}'",
                    a.type_name()
                ));
            }
            return unsupported_operand("*", a, b);
        }
    };
    // A count that is no `ssize_t` cannot repeat anything.
    let Some(n) = n.to_i64() else {
        return overflow_err("cannot fit 'int' into an index-sized integer");
    };
    Ok(match seq {
        Py::Str(s) => {
            let k = repeat_len(s.len(), n)?;
            Py::str(s.repeat(k))
        }
        Py::Bytes(s) => {
            let k = repeat_len(s.len(), n)?;
            Py::Bytes(s.repeat(k).into())
        }
        Py::List(l) => {
            let l = l.borrow();
            let k = repeat_len(l.len(), n)?;
            let mut out = Vec::with_capacity(l.len() * k);
            for _ in 0..k {
                out.extend(l.iter().cloned());
            }
            Py::list(out)
        }
        Py::Tuple(t) => {
            let k = repeat_len(t.len(), n)?;
            let mut out = Vec::with_capacity(t.len() * k);
            for _ in 0..k {
                out.extend(t.iter().cloned());
            }
            Py::tuple(out)
        }
        _ => unreachable!("is_seq"),
    })
}

fn is_seq(v: &Py) -> bool {
    matches!(v, Py::Str(_) | Py::Bytes(_) | Py::List(_) | Py::Tuple(_))
}

fn div(a: &Py, b: &Py) -> PyRes<Py> {
    if let (Some(x), Some(y)) = (as_num(a), as_num(b)) {
        if let (NumV::I(p), NumV::I(q)) = (x, y) {
            return pynum::true_div(p, q).map(Py::Float).map_err(num_err);
        }
        let (n, d) = (to_f(x)?, to_f(y)?);
        if d == 0.0 {
            return zero_div("float division by zero");
        }
        return Ok(Py::Float(n / d));
    }
    unsupported_operand("/", a, b)
}

fn floordiv(a: &Py, b: &Py) -> PyRes<Py> {
    if let (Some(x), Some(y)) = (as_num(a), as_num(b)) {
        return match (x, y) {
            (NumV::I(x), NumV::I(y)) => {
                if let (IntRef::Small(p), IntRef::Small(q)) = (x, y)
                    && q != 0
                    && let Some(d) = p.checked_div(q)
                {
                    let r = p % q;
                    return Ok(Py::Int(if r != 0 && ((r < 0) != (q < 0)) {
                        d - 1
                    } else {
                        d
                    }));
                }
                pynum::floordiv(x, y).map(Py::from_big).map_err(num_err)
            }
            (x, y) => Ok(Py::Float(float_floordiv(to_f(x)?, to_f(y)?)?)),
        };
    }
    unsupported_operand("//", a, b)
}

fn modulo(a: &Py, b: &Py) -> PyRes<Py> {
    if let (Some(x), Some(y)) = (as_num(a), as_num(b)) {
        return match (x, y) {
            (NumV::I(x), NumV::I(y)) => {
                if let (IntRef::Small(p), IntRef::Small(q)) = (x, y)
                    && q != 0
                {
                    let r = p.checked_rem(q).unwrap_or(0);
                    return Ok(Py::Int(if r != 0 && ((r < 0) != (q < 0)) {
                        r + q
                    } else {
                        r
                    }));
                }
                pynum::modulo(x, y).map(Py::from_big).map_err(num_err)
            }
            (x, y) => Ok(Py::Float(float_mod(to_f(x)?, to_f(y)?)?)),
        };
    }
    if let Py::Str(fmt) = a {
        return super::fmt::percent_format(fmt, b).map(Py::str);
    }
    unsupported_operand("%", a, b)
}

fn pow(a: &Py, b: &Py) -> PyRes<Py> {
    // PARITY: asteval `astutils.safe_pow` (`isinstance(exp, numbers.Number)`
    // then `exp > MAX_EXPONENT`), so a complex exponent is the `TypeError` of
    // `complex > int`; `safe_lshift` does the same with its count.
    if let Py::Complex(..) = b {
        return type_err("'>' not supported between instances of 'complex' and 'int'");
    }
    if let Some(e) = as_num(b) {
        let too_big = match e {
            // An integer beyond `i64` is positive or negative: only the former is too big.
            NumV::I(IntRef::Small(i)) => i > MAX_EXPONENT,
            NumV::I(IntRef::Big(n)) => !n.is_negative(),
            NumV::F(f) => f > MAX_EXPONENT as f64,
        };
        if too_big {
            return runtime_err(format!("Invalid exponent, max exponent is {MAX_EXPONENT}"));
        }
    }
    if let (Some(x), Some(y)) = (as_num(a), as_num(b)) {
        return match (x, y) {
            (NumV::I(x), NumV::I(y)) => pow_int(x, y),
            (x, y) => pow_float(to_f(x)?, to_f(y)?),
        };
    }
    unsupported_operand("** or pow()", a, b)
}

fn lshift(a: &Py, b: &Py) -> PyRes<Py> {
    // safe_lshift
    if let Py::Complex(..) = b {
        return type_err("'>' not supported between instances of 'complex' and 'int'");
    }
    if let Some(e) = as_num(b) {
        let too_big = match e {
            NumV::I(IntRef::Small(i)) => i > MAX_SHIFT,
            NumV::I(IntRef::Big(n)) => !n.is_negative(),
            NumV::F(f) => f > MAX_SHIFT as f64,
        };
        if too_big {
            return runtime_err(format!("Invalid left shift, max left shift is {MAX_SHIFT}"));
        }
    }
    match (int_ref(a), int_ref(b)) {
        (Some(x), Some(n)) => {
            if n.is_negative() {
                return value_err("negative shift count");
            }
            if let (IntRef::Small(v), IntRef::Small(c)) = (x, n)
                && c < 63
                && let Some(r) = v.checked_shl(c as u32).filter(|r| (r >> c) == v)
            {
                return Ok(Py::Int(r));
            }
            // The count is at most `MAX_SHIFT`.
            let count = n.to_i64().unwrap_or(i64::MAX) as u64;
            pynum::lshift(x, count).map(Py::from_big).map_err(num_err)
        }
        _ => unsupported_operand("<<", a, b),
    }
}

fn rshift(a: &Py, b: &Py) -> PyRes<Py> {
    match (int_ref(a), int_ref(b)) {
        (Some(x), Some(n)) => {
            if n.is_negative() {
                return value_err("negative shift count");
            }
            // A count beyond `i64` shifts everything out.
            let count = n.to_i64().map_or(u64::MAX, |c| c as u64);
            Ok(Py::from_big(pynum::rshift(x, count)))
        }
        _ => unsupported_operand(">>", a, b),
    }
}

fn bitop(op: BinOp, a: &Py, b: &Py) -> PyRes<Py> {
    let sym = match op {
        BinOp::BitAnd => "&",
        BinOp::BitOr => "|",
        _ => "^",
    };
    if let (Py::Bool(x), Py::Bool(y)) = (a, b) {
        return Ok(Py::Bool(match op {
            BinOp::BitAnd => x & y,
            BinOp::BitOr => x | y,
            _ => x ^ y,
        }));
    }
    if let (Some(x), Some(y)) = (int_ref(a), int_ref(b)) {
        if let (IntRef::Small(p), IntRef::Small(q)) = (x, y) {
            return Ok(Py::Int(match op {
                BinOp::BitAnd => p & q,
                BinOp::BitOr => p | q,
                _ => p ^ q,
            }));
        }
        let kind = match op {
            BinOp::BitAnd => Bitwise::And,
            BinOp::BitOr => Bitwise::Or,
            _ => Bitwise::Xor,
        };
        return Ok(Py::from_big(pynum::bitwise(kind, x, y)));
    }
    if let (Py::Set(x), Py::Set(y)) = (a, b) {
        let out = match op {
            BinOp::BitAnd => set_binary(x, y, |x, y| x.intersection(y)),
            BinOp::BitOr => set_binary(x, y, |x, y| x.union(y)),
            _ => set_binary(x, y, |x, y| x.symmetric_difference(y)),
        };
        return Ok(Py::Set(rf(out)));
    }
    if let (BinOp::BitOr, Py::Dict(x), Py::Dict(y)) = (op, a, b) {
        let mut out = x.borrow().clone();
        for (h, (k, v)) in y.borrow().iter() {
            dict_set(&mut out, h.clone(), k.clone(), v.clone());
        }
        return Ok(Py::Dict(rf(out)));
    }
    // A dict view (`d.keys() | other`) combines with any set or iterable as a set.
    if is_dict_view(a) || is_dict_view(b) {
        let (x, y) = (view_as_set(a)?, view_as_set(b)?);
        if let (Some(x), Some(y)) = (x, y) {
            let out = match op {
                BinOp::BitAnd => x.intersection(&y),
                BinOp::BitOr => x.union(&y),
                _ => x.symmetric_difference(&y),
            };
            return Ok(Py::Set(rf(out)));
        }
    }
    unsupported_operand(sym, a, b)
}

fn is_dict_view(v: &Py) -> bool {
    matches!(v, Py::Iter(it) if matches!(it.borrow().kind, "dict_keys" | "dict_items"))
}

/// A set, or a keys/items view, as a set; `None` for anything else (a view
/// combines with another view or a set; other iterables are not set operands
/// of the infix operators).
fn view_as_set(v: &Py) -> PyRes<Option<PySet>> {
    Ok(match v {
        Py::Set(s) => Some(s.borrow().clone()),
        view if is_dict_view(view) => {
            Some(PySet::from_items(to_vec(view)?).map_err(|m| exc("TypeError", m))?)
        }
        _ => None,
    })
}

/// `op2func(op)(a)`.
pub(crate) fn unaryop(op: UnaryOp, a: &Py) -> PyRes<Py> {
    match op {
        UnaryOp::Not => Ok(Py::Bool(!a.truthy())),
        UnaryOp::USub if matches!(a, Py::Complex(..)) => {
            let Py::Complex(r, i) = a else { unreachable!() };
            Ok(Py::Complex(-r, -i))
        }
        UnaryOp::USub if matches!(a, Py::BigInt(_)) => Ok(Py::from_big(pynum::neg(
            int_ref(a).unwrap_or(IntRef::Small(0)),
        ))),
        UnaryOp::Invert if matches!(a, Py::BigInt(_)) => Ok(Py::from_big(pynum::invert(
            int_ref(a).unwrap_or(IntRef::Small(0)),
        ))),
        UnaryOp::UAdd if matches!(a, Py::Complex(..)) => Ok(a.clone()),
        UnaryOp::USub => match a {
            Py::Bool(b) => Ok(Py::Int(-(*b as i64))),
            Py::Int(i) => Ok(i
                .checked_neg()
                .map_or_else(|| Py::from_big(pynum::neg(IntRef::Small(*i))), Py::Int)),
            Py::Float(f) => Ok(Py::Float(-f)),
            _ => type_err(format!("bad operand type for unary -: '{}'", a.type_name())),
        },
        UnaryOp::UAdd => match a {
            Py::Bool(b) => Ok(Py::Int(*b as i64)),
            Py::Int(_) | Py::BigInt(_) | Py::Float(_) => Ok(a.clone()),
            _ => type_err(format!("bad operand type for unary +: '{}'", a.type_name())),
        },
        UnaryOp::Invert => match is_int_like(a).filter(|_| !matches!(a, Py::BigInt(_))) {
            Some(i) => Ok(Py::Int(!i)),
            None => type_err(format!("bad operand type for unary ~: '{}'", a.type_name())),
        },
    }
}

/// `op2func(cmp)(a, b)`.
pub(crate) fn compare(op: CmpOp, a: &Py, b: &Py) -> PyRes<bool> {
    Ok(match op {
        CmpOp::Eq => py_eq(a, b),
        CmpOp::NotEq => !py_eq(a, b),
        CmpOp::Lt => py_rel(a, b, Rel::Lt)?,
        CmpOp::LtE => py_rel(a, b, Rel::Le)?,
        CmpOp::Gt => py_rel(a, b, Rel::Gt)?,
        CmpOp::GtE => py_rel(a, b, Rel::Ge)?,
        CmpOp::Is => a.is(b),
        CmpOp::IsNot => !a.is(b),
        CmpOp::In => contains(b, a)?,
        CmpOp::NotIn => !contains(b, a)?,
    })
}

/// `item in container`.
pub(crate) fn contains(container: &Py, item: &Py) -> PyRes<bool> {
    match container {
        Py::Str(s) => match item {
            Py::Str(sub) => Ok(s.contains(&**sub)),
            other => type_err(format!(
                "'in <string>' requires string as left operand, not {}",
                other.type_name()
            )),
        },
        Py::Bytes(b) => match item {
            Py::Bytes(sub) => Ok(sub.is_empty() || b.windows(sub.len()).any(|w| w == &**sub)),
            Py::Int(i) if (0..256).contains(i) => Ok(b.contains(&(*i as u8))),
            Py::Int(_) => value_err("byte must be in range(0, 256)"),
            other => type_err(format!(
                "a bytes-like object is required, not '{}'",
                other.type_name()
            )),
        },
        Py::List(l) => {
            let items = l.borrow().clone();
            Ok(items.iter().any(|x| x.is(item) || py_eq(x, item)))
        }
        Py::Tuple(t) => Ok(t.iter().any(|x| x.is(item) || py_eq(x, item))),
        Py::Dict(d) => {
            let k = hkey(item)?;
            Ok(d.borrow().contains_key(&k))
        }
        // A set needle is looked up as the equal `frozenset`: hashable there, and
        // equal to no element (a set cannot hold a set).
        Py::Set(_) if matches!(item, Py::Set(_)) => Ok(false),
        Py::Set(s) => s.borrow().contains(item).map_err(|m| exc("TypeError", m)),
        // PARITY: a map may be the `DynObj` or the plain dict upstream builds; reads
        // that only the dict supports work (see `getitem`).
        Py::Obj(m) => Ok(matches!(item, Py::Str(k) if m.borrow().contains_key(&**k))),
        Py::Range(a, b, c) => Ok(match item {
            Py::Int(_) | Py::Bool(_) => {
                let x = is_int_like(item).unwrap_or(0);
                range_contains(*a, *b, *c, x)
            }
            Py::Float(f) => f.fract() == 0.0 && range_contains(*a, *b, *c, *f as i64),
            _ => false,
        }),
        Py::Iter(it) => {
            let view = it.borrow().kind.starts_with("dict_");
            let mut it = it.borrow_mut();
            if view {
                return Ok(it.items.iter().any(|x| x.is(item) || py_eq(x, item)));
            }
            while let Some(x) = it.items.pop_front() {
                if x.is(item) || py_eq(&x, item) {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        other => type_err(format!(
            "argument of type '{}' is not iterable",
            other.type_name()
        )),
    }
}

fn range_contains(start: i64, stop: i64, step: i64, x: i64) -> bool {
    if step > 0 {
        x >= start && x < stop && (x - start) % step == 0
    } else {
        x <= start && x > stop && (start - x) % (-step) == 0
    }
}

// --- iteration --------------------------------------------------------------

/// A live iteration over a value (`for x in value`).
pub(crate) enum Cursor {
    List(Rf<Vec<Py>>, usize),
    Items(VecDeque<Py>),
    Chars(Vec<char>, usize),
    Range(i64, i64, i64),
    Consume(Rf<PyIter>),
}

impl Cursor {
    pub(crate) fn next(&mut self) -> Option<Py> {
        match self {
            Cursor::List(l, i) => {
                let v = l.borrow().get(*i).cloned();
                if v.is_some() {
                    *i += 1;
                }
                v
            }
            Cursor::Items(q) => q.pop_front(),
            Cursor::Chars(c, i) => {
                let v = c.get(*i).map(|ch| Py::str(ch.to_string()));
                if v.is_some() {
                    *i += 1;
                }
                v
            }
            Cursor::Range(cur, stop, step) => {
                if (*step > 0 && *cur < *stop) || (*step < 0 && *cur > *stop) {
                    let v = *cur;
                    *cur = cur.saturating_add(*step);
                    Some(Py::Int(v))
                } else {
                    None
                }
            }
            Cursor::Consume(it) => it.borrow_mut().items.pop_front(),
        }
    }
}

/// `iter(value)`.
pub(crate) fn iterate(v: &Py) -> PyRes<Cursor> {
    Ok(match v {
        Py::List(l) => Cursor::List(l.clone(), 0),
        Py::Tuple(t) => Cursor::Items(t.iter().cloned().collect()),
        Py::Str(s) => Cursor::Chars(s.chars().collect(), 0),
        Py::Bytes(b) => Cursor::Items(b.iter().map(|&x| Py::Int(x as i64)).collect()),
        Py::Dict(d) => Cursor::Items(d.borrow().values().map(|(k, _)| k.clone()).collect()),
        Py::Obj(m) => Cursor::Items(m.borrow().keys().map(|k| Py::str(k.as_str())).collect()),
        Py::Set(s) => Cursor::Items(s.borrow().iter().cloned().collect()),
        Py::Range(a, b, c) => Cursor::Range(*a, *b, *c),
        Py::Iter(it) => {
            if it.borrow().kind.starts_with("dict_") {
                Cursor::Items(it.borrow().items.clone())
            } else {
                Cursor::Consume(it.clone())
            }
        }
        other => {
            return type_err(format!("'{}' object is not iterable", other.type_name()));
        }
    })
}

/// All items of an iterable.
pub(crate) fn to_vec(v: &Py) -> PyRes<Vec<Py>> {
    let mut c = iterate(v)?;
    let mut out = Vec::new();
    while let Some(x) = c.next() {
        out.push(x);
        if out.len() > (1 << 24) {
            return Err(exc("MemoryError", ""));
        }
    }
    Ok(out)
}

/// `len(value)`.
pub(crate) fn py_len(v: &Py) -> PyRes<i64> {
    Ok(match v {
        Py::Str(s) => s.chars().count() as i64,
        Py::Bytes(b) => b.len() as i64,
        Py::List(l) => l.borrow().len() as i64,
        Py::Tuple(t) => t.len() as i64,
        Py::Dict(d) => d.borrow().len() as i64,
        Py::Obj(m) => m.borrow().len() as i64,
        Py::Set(s) => s.borrow().len() as i64,
        Py::Range(a, b, c) => range_len(*a, *b, *c),
        Py::Iter(it) if it.borrow().kind.starts_with("dict_") => it.borrow().items.len() as i64,
        other => {
            return type_err(format!(
                "object of type '{}' has no len()",
                other.type_name()
            ));
        }
    })
}

// --- subscripts -------------------------------------------------------------

/// `slice.indices(len)`: `(start, stop, step)` and the resulting length.
fn slice_indices(s: &(Py, Py, Py), len: i64) -> PyRes<(i64, i64, i64)> {
    let part = |p: &Py| -> PyRes<Option<i64>> {
        match p {
            Py::None => Ok(None),
            other => match is_int_like(other) {
                Some(i) => Ok(Some(i)),
                None => {
                    type_err("slice indices must be integers or None or have an __index__ method")
                }
            },
        }
    };
    let step = part(&s.2)?.unwrap_or(1);
    if step == 0 {
        return value_err("slice step cannot be zero");
    }
    let (lower_default, upper_default) = if step < 0 { (len - 1, -1) } else { (0, len) };
    let clamp = |v: Option<i64>, default: i64| -> i64 {
        match v {
            None => default,
            Some(mut x) => {
                if x < 0 {
                    x += len;
                    if x < 0 {
                        x = if step < 0 { -1 } else { 0 };
                    }
                } else if x >= len {
                    x = if step < 0 { len - 1 } else { len };
                }
                x
            }
        }
    };
    let start = clamp(part(&s.0)?, lower_default);
    let stop = clamp(part(&s.1)?, upper_default);
    Ok((start, stop, step))
}

fn slice_positions(start: i64, stop: i64, step: i64) -> Vec<i64> {
    let n = range_len(start, stop, step);
    (0..n).map(|k| start + k * step).collect()
}

fn index_of(key: &Py, what: &str) -> PyRes<i64> {
    match is_int_like(key) {
        Some(i) => Ok(i),
        None => type_err(format!(
            "{what} indices must be integers or slices, not {}",
            key.type_name()
        )),
    }
}

/// `obj[key]`.
pub(crate) fn getitem(obj: &Py, key: &Py) -> PyRes<Py> {
    match obj {
        Py::List(l) => {
            if let Py::Slice(s) = key {
                let l = l.borrow();
                let (a, b, c) = slice_indices(s, l.len() as i64)?;
                return Ok(Py::list(
                    slice_positions(a, b, c)
                        .into_iter()
                        .map(|i| l[i as usize].clone())
                        .collect(),
                ));
            }
            let i = index_of(key, "list")?;
            let l = l.borrow();
            let idx = if i < 0 { i + l.len() as i64 } else { i };
            if idx < 0 || idx >= l.len() as i64 {
                return Err(exc("IndexError", "list index out of range"));
            }
            Ok(l[idx as usize].clone())
        }
        Py::Tuple(t) => {
            if let Py::Slice(s) = key {
                let (a, b, c) = slice_indices(s, t.len() as i64)?;
                return Ok(Py::tuple(
                    slice_positions(a, b, c)
                        .into_iter()
                        .map(|i| t[i as usize].clone())
                        .collect(),
                ));
            }
            let i = index_of(key, "tuple")?;
            let idx = if i < 0 { i + t.len() as i64 } else { i };
            if idx < 0 || idx >= t.len() as i64 {
                return Err(exc("IndexError", "tuple index out of range"));
            }
            Ok(t[idx as usize].clone())
        }
        Py::Str(s) => {
            let chars: Vec<char> = s.chars().collect();
            if let Py::Slice(sl) = key {
                let (a, b, c) = slice_indices(sl, chars.len() as i64)?;
                return Ok(Py::str(
                    slice_positions(a, b, c)
                        .into_iter()
                        .map(|i| chars[i as usize])
                        .collect::<String>(),
                ));
            }
            let i = match is_int_like(key) {
                Some(i) => i,
                None => {
                    return type_err(format!(
                        "string indices must be integers, not '{}'",
                        key.type_name()
                    ));
                }
            };
            let idx = if i < 0 { i + chars.len() as i64 } else { i };
            if idx < 0 || idx >= chars.len() as i64 {
                return Err(exc("IndexError", "string index out of range"));
            }
            Ok(Py::str(chars[idx as usize].to_string()))
        }
        Py::Bytes(b) => {
            if let Py::Slice(sl) = key {
                let (a, bb, c) = slice_indices(sl, b.len() as i64)?;
                let v: Vec<u8> = slice_positions(a, bb, c)
                    .into_iter()
                    .map(|i| b[i as usize])
                    .collect();
                return Ok(Py::Bytes(v.into()));
            }
            let i = index_of(key, "byte")?;
            let idx = if i < 0 { i + b.len() as i64 } else { i };
            if idx < 0 || idx >= b.len() as i64 {
                return Err(exc("IndexError", "index out of range"));
            }
            Ok(Py::Int(b[idx as usize] as i64))
        }
        Py::Range(a, b, c) => {
            let len = range_len(*a, *b, *c);
            if let Py::Slice(sl) = key {
                let (s0, s1, s2) = slice_indices(sl, len)?;
                let (na, nb, nc) = (a + s0 * c, a + s1 * c, c * s2);
                return Ok(Py::Range(na, nb, nc));
            }
            let i = index_of(key, "range")?;
            let idx = if i < 0 { i + len } else { i };
            if idx < 0 || idx >= len {
                return Err(exc("IndexError", "range object index out of range"));
            }
            Ok(Py::Int(a + idx * c))
        }
        Py::Dict(d) => {
            let k = hkey(key)?;
            match d.borrow().get(&k) {
                Some((_, v)) => Ok(v.clone()),
                None => Err(Box::new(ExcObj {
                    class: "KeyError",
                    args: vec![key.clone()],
                })),
            }
        }
        // PARITY: see `contains`: a map subscripts like the dict it may be.
        Py::Obj(m) => match key {
            Py::Str(k) => m.borrow().get(&**k).cloned().ok_or_else(|| {
                Box::new(ExcObj {
                    class: "KeyError",
                    args: vec![key.clone()],
                })
            }),
            _ => Err(Box::new(ExcObj {
                class: "KeyError",
                args: vec![key.clone()],
            })),
        },
        Py::Iter(it) if it.borrow().kind.starts_with("dict_") => {
            type_err(format!("'{}' object is not subscriptable", obj.type_name()))
        }
        other => type_err(format!(
            "'{}' object is not subscriptable",
            other.type_name()
        )),
    }
}

/// `obj[key] = val`.
pub(crate) fn setitem(obj: &Py, key: &Py, val: Py) -> PyRes<()> {
    match obj {
        Py::List(l) => {
            if let Py::Slice(s) = key {
                let items =
                    to_vec(&val).map_err(|_| exc("TypeError", "can only assign an iterable"))?;
                let len = l.borrow().len() as i64;
                let (a, b, c) = slice_indices(s, len)?;
                let mut list = l.borrow_mut();
                if c == 1 {
                    let b = b.max(a);
                    let tail = list.split_off(b as usize);
                    list.truncate(a as usize);
                    list.extend(items);
                    list.extend(tail);
                    return Ok(());
                }
                let pos = slice_positions(a, b, c);
                if pos.len() != items.len() {
                    return value_err(format!(
                        "attempt to assign sequence of size {} to extended slice of size {}",
                        items.len(),
                        pos.len()
                    ));
                }
                for (p, v) in pos.into_iter().zip(items) {
                    list[p as usize] = v;
                }
                return Ok(());
            }
            let i = index_of(key, "list")?;
            let mut l = l.borrow_mut();
            let idx = if i < 0 { i + l.len() as i64 } else { i };
            if idx < 0 || idx >= l.len() as i64 {
                return Err(exc("IndexError", "list assignment index out of range"));
            }
            l[idx as usize] = val;
            Ok(())
        }
        Py::Dict(d) => {
            let k = hkey(key)?;
            // An existing equal key keeps its original object (`d[1.0] = x` on `{1: y}`).
            dict_set(&mut d.borrow_mut(), k, key.clone(), val);
            Ok(())
        }
        other => type_err(format!(
            "'{}' object does not support item assignment",
            other.type_name()
        )),
    }
}

/// `del obj[key]`.
pub(crate) fn delitem(obj: &Py, key: &Py) -> PyRes<()> {
    match obj {
        Py::List(l) => {
            if let Py::Slice(s) = key {
                let len = l.borrow().len() as i64;
                let (a, b, c) = slice_indices(s, len)?;
                let mut pos = slice_positions(a, b, c);
                pos.sort_unstable();
                let mut list = l.borrow_mut();
                for p in pos.into_iter().rev() {
                    list.remove(p as usize);
                }
                return Ok(());
            }
            let i = index_of(key, "list")?;
            let mut l = l.borrow_mut();
            let idx = if i < 0 { i + l.len() as i64 } else { i };
            if idx < 0 || idx >= l.len() as i64 {
                return Err(exc("IndexError", "list assignment index out of range"));
            }
            l.remove(idx as usize);
            Ok(())
        }
        Py::Dict(d) => {
            let k = hkey(key)?;
            if d.borrow_mut().shift_remove(&k).is_none() {
                return Err(Box::new(ExcObj {
                    class: "KeyError",
                    args: vec![key.clone()],
                }));
            }
            Ok(())
        }
        other => type_err(format!(
            "'{}' object doesn't support item deletion",
            other.type_name()
        )),
    }
}

/// `copy.deepcopy` for the values `asteval` snapshots in comprehensions.
pub(crate) fn deepcopy(v: &Py) -> Py {
    match v {
        Py::List(l) => Py::list(l.borrow().iter().map(deepcopy).collect()),
        Py::Tuple(t) => Py::tuple(t.iter().map(deepcopy).collect()),
        Py::Dict(d) => Py::Dict(rf(d
            .borrow()
            .iter()
            .map(|(h, (k, v))| (h.clone(), (deepcopy(k), deepcopy(v))))
            .collect())),
        Py::Set(s) => Py::Set(rf(s.borrow().map_items(deepcopy))),
        Py::Obj(o) => Py::Obj(rf(o
            .borrow()
            .iter()
            .map(|(k, v)| (k.clone(), deepcopy(v)))
            .collect())),
        Py::Iter(it) => {
            let i = it.borrow();
            Py::Iter(rf(PyIter {
                kind: i.kind,
                items: i.items.iter().map(deepcopy).collect(),
            }))
        }
        Py::Exc(e) => Py::Exc(Rc::new(ExcObj {
            class: e.class,
            args: e.args.iter().map(deepcopy).collect(),
        })),
        other => other.clone(),
    }
}

/// A number as a complex operand (an integer beyond the float range overflows).
fn complex_operand(v: &Py) -> Option<PyRes<Complex64>> {
    match v {
        Py::Complex(r, i) => Some(Ok(Complex64::new(*r, *i))),
        other => as_num(other).map(|n| to_f(n).map(|f| Complex64::new(f, 0.0))),
    }
}

fn complex_binop(op: BinOp, x: Complex64, y: Complex64, a: &Py, b: &Py) -> PyRes<Py> {
    let c = |z: Complex64| Ok(Py::Complex(z.re, z.im));
    match op {
        BinOp::Add => c(Complex64::new(x.re + y.re, x.im + y.im)),
        BinOp::Sub => c(Complex64::new(x.re - y.re, x.im - y.im)),
        BinOp::Mult => c(complex::prod(x, y)),
        BinOp::Div => c(complex::quot(x, y).map_err(num_err)?),
        BinOp::Pow => {
            // `safe_pow` compares the exponent with `MAX_EXPONENT`: a complex one
            // is a `TypeError`, a real one is checked.
            if let Py::Complex(..) = b {
                return type_err("'>' not supported between instances of 'complex' and 'int'");
            }
            if y.re > MAX_EXPONENT as f64 {
                return runtime_err(format!("Invalid exponent, max exponent is {MAX_EXPONENT}"));
            }
            c(complex::pow(x, y).map_err(num_err)?)
        }
        BinOp::FloorDiv => unsupported_operand("//", a, b),
        BinOp::Mod => unsupported_operand("%", a, b),
        BinOp::LShift if matches!(b, Py::Complex(..)) => {
            type_err("'>' not supported between instances of 'complex' and 'int'")
        }
        BinOp::LShift if y.re > MAX_SHIFT as f64 => {
            runtime_err(format!("Invalid left shift, max left shift is {MAX_SHIFT}"))
        }
        _ => {
            let sym = match op {
                BinOp::LShift => "<<",
                BinOp::RShift => ">>",
                BinOp::BitAnd => "&",
                BinOp::BitOr => "|",
                BinOp::BitXor => "^",
                _ => "@",
            };
            unsupported_operand(sym, a, b)
        }
    }
}
