//! Python's operators, subscripting and iteration over [`Py`], with
//! `asteval`'s `safe_*` limits (`astutils.py`).

use std::collections::VecDeque;
use std::rc::Rc;

use super::ast::{BinOp, CmpOp, UnaryOp};
use super::value::*;

/// `astutils.MAX_EXPONENT`.
pub(crate) const MAX_EXPONENT: i64 = 10000;
/// `astutils.MAX_STR_LEN` (`2 << 17`).
pub(crate) const MAX_STR_LEN: i64 = 2 << 17;
/// `astutils.MAX_SHIFT`.
pub(crate) const MAX_SHIFT: i64 = 1000;

fn int_overflow<T>() -> PyRes<T> {
    overflow_err("integer result out of range (big integers are not supported)")
}

fn zero_div<T>(msg: &str) -> PyRes<T> {
    Err(exc("ZeroDivisionError", msg))
}

fn is_int_like(v: &Py) -> Option<i64> {
    match v {
        Py::Bool(b) => Some(*b as i64),
        Py::Int(i) => Some(*i),
        _ => None,
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

fn pow_int(base: i64, exp: i64) -> PyRes<Py> {
    if exp < 0 {
        if base == 0 {
            return zero_div("0.0 cannot be raised to a negative power");
        }
        return Ok(Py::Float((base as f64).powf(exp as f64)));
    }
    let mut result: i64 = 1;
    let mut b = base;
    let mut e = exp;
    while e > 0 {
        if e & 1 == 1 {
            result = result.checked_mul(b).map_or_else(int_overflow, Ok)?;
        }
        e >>= 1;
        if e > 0 {
            b = b.checked_mul(b).map_or_else(int_overflow, Ok)?;
        }
    }
    Ok(Py::Int(result))
}

fn pow_float(x: f64, y: f64) -> PyRes<Py> {
    if x == 0.0 && y < 0.0 {
        return zero_div("0.0 cannot be raised to a negative power");
    }
    if x < 0.0 && y.is_finite() && y.fract() != 0.0 {
        let (a, b) = (Py::Float(x), Py::Float(y));
        return complex_binop(BinOp::Pow, (x, 0.0), (y, 0.0), &a, &b);
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
        && let (Some(x), Some(y)) = (complex_parts(a), complex_parts(b))
    {
        return complex_binop(op, x, y, a, b);
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
            (NumV::I(x), NumV::I(y)) => Py::Int(x.checked_add(y).map_or_else(int_overflow, Ok)?),
            (x, y) => Py::Float(to_f(x) + to_f(y)),
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

pub(crate) fn to_f(n: NumV) -> f64 {
    match n {
        NumV::I(i) => i as f64,
        NumV::F(f) => f,
    }
}

fn sub(a: &Py, b: &Py) -> PyRes<Py> {
    if let (Some(x), Some(y)) = (as_num(a), as_num(b)) {
        return Ok(match (x, y) {
            (NumV::I(x), NumV::I(y)) => Py::Int(x.checked_sub(y).map_or_else(int_overflow, Ok)?),
            (x, y) => Py::Float(to_f(x) - to_f(y)),
        });
    }
    if let (Py::Set(x), Py::Set(y)) = (a, b) {
        let (x, y) = (x.borrow(), y.borrow());
        let out: PySet = x
            .iter()
            .filter(|(k, _)| !y.contains_key(*k))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        return Ok(Py::Set(rf(out)));
    }
    unsupported_operand("-", a, b)
}

fn mult(a: &Py, b: &Py) -> PyRes<Py> {
    // safe_mult
    if let (Py::Str(s), Some(n)) = (a, is_int_like(b))
        && (s.chars().count() as i128) * (n as i128) > MAX_STR_LEN as i128
    {
        return runtime_err(format!(
            "String length exceeded, max string length is {MAX_STR_LEN}"
        ));
    }
    if let (Some(x), Some(y)) = (as_num(a), as_num(b)) {
        return Ok(match (x, y) {
            (NumV::I(x), NumV::I(y)) => Py::Int(x.checked_mul(y).map_or_else(int_overflow, Ok)?),
            (x, y) => Py::Float(to_f(x) * to_f(y)),
        });
    }
    let (seq, n) = match (a, is_int_like(a), b, is_int_like(b)) {
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
        let both_int = matches!((x, y), (NumV::I(_), NumV::I(_)));
        let d = to_f(y);
        if d == 0.0 {
            return zero_div(if both_int {
                "division by zero"
            } else {
                "float division by zero"
            });
        }
        return Ok(Py::Float(to_f(x) / d));
    }
    unsupported_operand("/", a, b)
}

fn floordiv(a: &Py, b: &Py) -> PyRes<Py> {
    if let (Some(x), Some(y)) = (as_num(a), as_num(b)) {
        return match (x, y) {
            (NumV::I(x), NumV::I(y)) => {
                if y == 0 {
                    return zero_div("integer division or modulo by zero");
                }
                let q = x.checked_div(y).map_or_else(int_overflow, Ok)?;
                let r = x % y;
                Ok(Py::Int(if r != 0 && ((r < 0) != (y < 0)) {
                    q - 1
                } else {
                    q
                }))
            }
            (x, y) => Ok(Py::Float(float_floordiv(to_f(x), to_f(y))?)),
        };
    }
    unsupported_operand("//", a, b)
}

fn modulo(a: &Py, b: &Py) -> PyRes<Py> {
    if let (Some(x), Some(y)) = (as_num(a), as_num(b)) {
        return match (x, y) {
            (NumV::I(x), NumV::I(y)) => {
                if y == 0 {
                    return zero_div("integer modulo by zero");
                }
                let r = x.checked_rem(y).unwrap_or(0);
                Ok(Py::Int(if r != 0 && ((r < 0) != (y < 0)) {
                    r + y
                } else {
                    r
                }))
            }
            (x, y) => Ok(Py::Float(float_mod(to_f(x), to_f(y))?)),
        };
    }
    if let Py::Str(fmt) = a {
        return super::fmt::percent_format(fmt, b).map(Py::str);
    }
    unsupported_operand("%", a, b)
}

fn pow(a: &Py, b: &Py) -> PyRes<Py> {
    // safe_pow: `isinstance(exp, numbers.Number)`.
    if let Some(e) = as_num(b) {
        let too_big = match e {
            NumV::I(i) => i > MAX_EXPONENT,
            NumV::F(f) => f > MAX_EXPONENT as f64,
        };
        if too_big {
            return runtime_err(format!("Invalid exponent, max exponent is {MAX_EXPONENT}"));
        }
    }
    if let (Some(x), Some(y)) = (as_num(a), as_num(b)) {
        return match (x, y) {
            (NumV::I(x), NumV::I(y)) => pow_int(x, y),
            (x, y) => pow_float(to_f(x), to_f(y)),
        };
    }
    unsupported_operand("** or pow()", a, b)
}

fn lshift(a: &Py, b: &Py) -> PyRes<Py> {
    // safe_lshift
    if let Some(e) = as_num(b) {
        let too_big = match e {
            NumV::I(i) => i > MAX_SHIFT,
            NumV::F(f) => f > MAX_SHIFT as f64,
        };
        if too_big {
            return runtime_err(format!("Invalid left shift, max left shift is {MAX_SHIFT}"));
        }
    }
    match (is_int_like(a), is_int_like(b)) {
        (Some(x), Some(n)) => {
            if n < 0 {
                return value_err("negative shift count");
            }
            if x == 0 {
                return Ok(Py::Int(0));
            }
            if n >= 63 {
                return int_overflow();
            }
            let r = x.checked_shl(n as u32).filter(|r| (r >> n) == x);
            r.map_or_else(int_overflow, |r| Ok(Py::Int(r)))
        }
        _ => unsupported_operand("<<", a, b),
    }
}

fn rshift(a: &Py, b: &Py) -> PyRes<Py> {
    match (is_int_like(a), is_int_like(b)) {
        (Some(x), Some(n)) => {
            if n < 0 {
                return value_err("negative shift count");
            }
            Ok(Py::Int(if n >= 64 {
                if x < 0 { -1 } else { 0 }
            } else {
                x >> n
            }))
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
    if let (Some(x), Some(y)) = (is_int_like(a), is_int_like(b)) {
        return Ok(Py::Int(match op {
            BinOp::BitAnd => x & y,
            BinOp::BitOr => x | y,
            _ => x ^ y,
        }));
    }
    if let (Py::Set(x), Py::Set(y)) = (a, b) {
        let (x, y) = (x.borrow(), y.borrow());
        let mut out = PySet::new();
        match op {
            BinOp::BitAnd => {
                for (k, v) in x.iter() {
                    if y.contains_key(k) {
                        out.insert(k.clone(), v.clone());
                    }
                }
            }
            BinOp::BitOr => {
                for (k, v) in x.iter().chain(y.iter()) {
                    set_add(&mut out, k.clone(), v.clone());
                }
            }
            _ => {
                for (k, v) in x.iter() {
                    if !y.contains_key(k) {
                        out.insert(k.clone(), v.clone());
                    }
                }
                for (k, v) in y.iter() {
                    if !x.contains_key(k) {
                        out.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        return Ok(Py::Set(rf(out)));
    }
    if let (BinOp::BitOr, Py::Dict(x), Py::Dict(y)) = (op, a, b) {
        let mut out = x.borrow().clone();
        for (k, v) in y.borrow().iter() {
            out.insert(k.clone(), v.clone());
        }
        return Ok(Py::Dict(rf(out)));
    }
    unsupported_operand(sym, a, b)
}

/// `op2func(op)(a)`.
pub(crate) fn unaryop(op: UnaryOp, a: &Py) -> PyRes<Py> {
    match op {
        UnaryOp::Not => Ok(Py::Bool(!a.truthy())),
        UnaryOp::USub if matches!(a, Py::Complex(..)) => {
            let Py::Complex(r, i) = a else { unreachable!() };
            Ok(Py::Complex(-r, -i))
        }
        UnaryOp::UAdd if matches!(a, Py::Complex(..)) => Ok(a.clone()),
        UnaryOp::USub => match a {
            Py::Bool(b) => Ok(Py::Int(-(*b as i64))),
            Py::Int(i) => i
                .checked_neg()
                .map_or_else(int_overflow, |v| Ok(Py::Int(v))),
            Py::Float(f) => Ok(Py::Float(-f)),
            _ => type_err(format!("bad operand type for unary -: '{}'", a.type_name())),
        },
        UnaryOp::UAdd => match a {
            Py::Bool(b) => Ok(Py::Int(*b as i64)),
            Py::Int(_) | Py::Float(_) => Ok(a.clone()),
            _ => type_err(format!("bad operand type for unary +: '{}'", a.type_name())),
        },
        UnaryOp::Invert => match is_int_like(a) {
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
        Py::Set(s) => {
            let k = hkey(item)?;
            Ok(s.borrow().contains_key(&k))
        }
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
        Py::Set(s) => Cursor::Items(s.borrow().values().cloned().collect()),
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
            d.borrow_mut().insert(k, (key.clone(), val));
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
        Py::Set(s) => Py::Set(rf(s
            .borrow()
            .iter()
            .map(|(h, v)| (h.clone(), deepcopy(v)))
            .collect())),
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

fn complex_binop(op: BinOp, x: (f64, f64), y: (f64, f64), a: &Py, b: &Py) -> PyRes<Py> {
    let c = |r: f64, i: f64| Ok(Py::Complex(r, i));
    match op {
        BinOp::Add => c(x.0 + y.0, x.1 + y.1),
        BinOp::Sub => c(x.0 - y.0, x.1 - y.1),
        BinOp::Mult => c(x.0 * y.0 - x.1 * y.1, x.0 * y.1 + x.1 * y.0),
        BinOp::Div => {
            let d = y.0 * y.0 + y.1 * y.1;
            if d == 0.0 {
                return zero_div("division by zero");
            }
            c((x.0 * y.0 + x.1 * y.1) / d, (x.1 * y.0 - x.0 * y.1) / d)
        }
        BinOp::Pow => {
            if y.0 > MAX_EXPONENT as f64 {
                return runtime_err(format!("Invalid exponent, max exponent is {MAX_EXPONENT}"));
            }
            if x == (0.0, 0.0) {
                return if y == (0.0, 0.0) {
                    c(1.0, 0.0)
                } else {
                    c(0.0, 0.0)
                };
            }
            let (r, theta) = (x.0.hypot(x.1), x.1.atan2(x.0));
            let (lr, li) = (r.ln(), theta);
            let (er, ei) = (y.0 * lr - y.1 * li, y.0 * li + y.1 * lr);
            let m = er.exp();
            if m.is_infinite() {
                return overflow_err("complex exponentiation");
            }
            c(m * ei.cos(), m * ei.sin())
        }
        BinOp::FloorDiv => unsupported_operand("//", a, b),
        BinOp::Mod => unsupported_operand("%", a, b),
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
