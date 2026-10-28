//! `asteval`'s initial symbol table: Python's built-in functions and types
//! (the `FROM_PY` list), the `math` functions (`FROM_MATH`), `type`/`open`
//! (`LOCALFUNCS`) and the exception classes.

use std::collections::VecDeque;
use std::rc::Rc;

use super::dirs;
use super::ops::*;
use super::value::*;
use crate::expr::builtins;
use crate::pyfmt::py_float_repr;
use crate::pynum::{self, BigInt, IntRef};
use crate::value::Value;
use num_traits::Signed;

/// What built-ins need from the interpreter: calling a Python callable.
pub(crate) trait Caller {
    fn call_value(&mut self, f: &Py, args: Vec<Py>, kwargs: Vec<(String, Py)>) -> PyRes<Py>;
}

pub(crate) type Kwargs = Vec<(String, Py)>;

const BUILTIN_FUNCS: &[&str] = &[
    "abs",
    "all",
    "any",
    "bin",
    "chr",
    "dir",
    "divmod",
    "format",
    "hash",
    "hex",
    "id",
    "isinstance",
    "len",
    "max",
    "min",
    "oct",
    "ord",
    "repr",
    "round",
    "sorted",
    "sum",
    "type",
    "open",
    "print",
    // math
    "acos",
    "acosh",
    "asin",
    "asinh",
    "atan",
    "atan2",
    "atanh",
    "ceil",
    "copysign",
    "cos",
    "cosh",
    "degrees",
    "exp",
    "fabs",
    "factorial",
    "floor",
    "fmod",
    "frexp",
    "fsum",
    "hypot",
    "isinf",
    "isnan",
    "ldexp",
    "log",
    "log10",
    "log1p",
    "modf",
    "pow",
    "radians",
    "sin",
    "sinh",
    "sqrt",
    "tan",
    "tanh",
    "trunc",
    "uuid5",
];

const BUILTIN_TYPES: &[&str] = &[
    "bool",
    "bytearray",
    "bytes",
    "complex",
    "dict",
    "enumerate",
    "filter",
    "float",
    "frozenset",
    "int",
    "list",
    "map",
    "range",
    "reversed",
    "set",
    "slice",
    "str",
    "tuple",
    "zip",
];

/// The value of `name` in a fresh `asteval` symbol table (before user
/// symbols): the built-ins, math functions and constants.
pub(crate) fn lookup_builtin(name: &str) -> Option<Py> {
    if let Some(n) = BUILTIN_FUNCS.iter().find(|n| **n == name) {
        return Some(Py::Builtin(n));
    }
    if let Some(n) = BUILTIN_TYPES.iter().find(|n| **n == name) {
        return Some(Py::Type(n));
    }
    match name {
        "pi" => return Some(Py::Float(std::f64::consts::PI)),
        "e" => return Some(Py::Float(std::f64::consts::E)),
        _ => {}
    }
    if EXC_BUILTINS.contains(&name) {
        return exc_class(name).map(Py::Type);
    }
    None
}

fn arity<T>(name: &str, got: usize, min: usize, max: usize) -> PyRes<T>
where
    T: Default,
{
    let _ = (name, got, min, max);
    Ok(T::default())
}

fn check_args(name: &str, args: &[Py], min: usize, max: usize) -> PyRes<()> {
    let n = args.len();
    if n < min || n > max {
        let msg = if name == "type" && (n != 1) {
            if n == 0 {
                "_type() missing 1 required positional argument: 'x'".to_string()
            } else {
                format!("_type() takes 1 positional argument but {n} were given")
            }
        } else if is_math(name) && min == max {
            format!(
                "math.{name}() takes exactly {} argument{} ({n} given)",
                if min == 1 {
                    "one".to_string()
                } else {
                    min.to_string()
                },
                if min == 1 { "" } else { "s" }
            )
        } else if min == max {
            format!(
                "{name}() takes exactly {} argument{} ({n} given)",
                if min == 1 {
                    "one".to_string()
                } else {
                    min.to_string()
                },
                if min == 1 { "" } else { "s" }
            )
        } else if n < min {
            format!(
                "{name} expected at least {min} argument{}, got {n}",
                if min == 1 { "" } else { "s" }
            )
        } else {
            format!(
                "{name} expected at most {max} argument{}, got {n}",
                if max == 1 { "" } else { "s" }
            )
        };
        return type_err(msg);
    }
    let _ = arity::<()>;
    Ok(())
}

fn is_math(name: &str) -> bool {
    matches!(
        name,
        "acos"
            | "acosh"
            | "asin"
            | "asinh"
            | "atan"
            | "atan2"
            | "atanh"
            | "ceil"
            | "copysign"
            | "cos"
            | "cosh"
            | "degrees"
            | "exp"
            | "fabs"
            | "factorial"
            | "floor"
            | "fmod"
            | "frexp"
            | "fsum"
            | "hypot"
            | "isinf"
            | "isnan"
            | "ldexp"
            | "log"
            | "log10"
            | "log1p"
            | "modf"
            | "pow"
            | "radians"
            | "sin"
            | "sinh"
            | "sqrt"
            | "tan"
            | "tanh"
            | "trunc"
    )
}

fn no_kwargs(name: &str, kwargs: &Kwargs) -> PyRes<()> {
    if let Some((k, _)) = kwargs.first() {
        return type_err(format!("{name}() takes no keyword arguments (got '{k}')"));
    }
    Ok(())
}

fn kw<'a>(kwargs: &'a Kwargs, name: &str) -> Option<&'a Py> {
    kwargs.iter().find(|(k, _)| k == name).map(|(_, v)| v)
}

fn int_arg(v: &Py) -> PyRes<i64> {
    match v {
        Py::Int(i) => Ok(*i),
        Py::Bool(b) => Ok(*b as i64),
        Py::BigInt(_) => overflow_err("Python int too large to convert to C ssize_t"),
        other => type_err(format!(
            "'{}' object cannot be interpreted as an integer",
            other.type_name()
        )),
    }
}

fn real_arg(v: &Py) -> PyRes<f64> {
    match v {
        Py::Int(i) => Ok(*i as f64),
        Py::BigInt(n) => pynum::big_to_f64(n)
            .map_or_else(|| overflow_err(pynum::INT_TOO_LARGE_FOR_FLOAT), Ok),
        Py::Bool(b) => Ok(*b as i64 as f64),
        Py::Float(f) => Ok(*f),
        other => type_err(format!("must be real number, not {}", other.type_name())),
    }
}

fn eval_err_to_exc(e: crate::expr::error::ExprError, class: &'static str) -> Box<ExcObj> {
    let msg = match e {
        crate::expr::error::ExprError::Eval(m) | crate::expr::error::ExprError::Uncaught(m) => m,
        other => other.to_string(),
    };
    exc(class, msg)
}

/// Call a built-in function.
pub(crate) fn call_builtin(
    c: &mut dyn Caller,
    name: &'static str,
    args: Vec<Py>,
    kwargs: Kwargs,
) -> PyRes<Py> {
    match name {
        "abs" => {
            no_kwargs(name, &kwargs)?;
            check_args(name, &args, 1, 1)?;
            match &args[0] {
                Py::Complex(r, i) => pynum::complex::abs(pynum::complex::Complex64::new(*r, *i))
                    .map(Py::Float)
                    .map_err(super::ops::num_err),
                Py::Bool(b) => Ok(Py::Int(*b as i64)),
                Py::Int(i) => Ok(i.checked_abs().map_or_else(
                    || Py::from_big(pynum::abs(IntRef::Small(*i))),
                    Py::Int,
                )),
                Py::BigInt(n) => Ok(Py::from_big(pynum::abs(IntRef::Big(n)))),
                Py::Float(f) => Ok(Py::Float(f.abs())),
                other => type_err(format!(
                    "bad operand type for abs(): '{}'",
                    other.type_name()
                )),
            }
        }
        "all" | "any" => {
            no_kwargs(name, &kwargs)?;
            check_args(name, &args, 1, 1)?;
            let mut it = iterate(&args[0])?;
            let want_all = name == "all";
            while let Some(x) = it.next() {
                if x.truthy() != want_all {
                    return Ok(Py::Bool(!want_all));
                }
            }
            Ok(Py::Bool(want_all))
        }
        "bin" | "hex" | "oct" => {
            no_kwargs(name, &kwargs)?;
            check_args(name, &args, 1, 1)?;
            let i = match &args[0] {
                Py::Int(i) => BigInt::from(*i),
                Py::Bool(b) => BigInt::from(*b),
                Py::BigInt(n) => (**n).clone(),
                other => {
                    return type_err(format!(
                        "'{}' object cannot be interpreted as an integer",
                        other.type_name()
                    ));
                }
            };
            let (p, body) = match name {
                "bin" => ("0b", i.magnitude().to_str_radix(2)),
                "hex" => ("0x", i.magnitude().to_str_radix(16)),
                _ => ("0o", i.magnitude().to_str_radix(8)),
            };
            Ok(Py::str(format!(
                "{}{p}{body}",
                if i.is_negative() { "-" } else { "" }
            )))
        }
        "chr" => {
            no_kwargs(name, &kwargs)?;
            check_args(name, &args, 1, 1)?;
            let i = int_arg(&args[0])?;
            match u32::try_from(i).ok().and_then(char::from_u32) {
                Some(ch) => Ok(Py::str(ch.to_string())),
                None if (0xD800..0xE000).contains(&i) => {
                    value_err("chr() surrogates are not supported")
                }
                None => value_err("chr() arg not in range(0x110000)"),
            }
        }
        "ord" => {
            no_kwargs(name, &kwargs)?;
            check_args(name, &args, 1, 1)?;
            match &args[0] {
                Py::Str(s) if s.chars().count() == 1 => {
                    Ok(Py::Int(s.chars().next().map_or(0, |c| c as i64)))
                }
                Py::Str(s) => type_err(format!(
                    "ord() expected a character, but string of length {} found",
                    s.chars().count()
                )),
                Py::Bytes(b) if b.len() == 1 => Ok(Py::Int(b[0] as i64)),
                other => type_err(format!(
                    "ord() expected string of length 1, but {} found",
                    other.type_name()
                )),
            }
        }
        "divmod" => {
            no_kwargs(name, &kwargs)?;
            check_args(name, &args, 2, 2)?;
            let q = binop(super::ast::BinOp::FloorDiv, &args[0], &args[1]);
            let r = binop(super::ast::BinOp::Mod, &args[0], &args[1]);
            match (q, r) {
                (Ok(q), Ok(r)) => Ok(Py::tuple(vec![q, r])),
                (Err(e), _) | (_, Err(e)) => {
                    if e.class == "ZeroDivisionError"
                        && (matches!(args[0], Py::Float(_)) || matches!(args[1], Py::Float(_)))
                    {
                        zero_div_msg("float divmod()")
                    } else if e.class == "TypeError" {
                        type_err(format!(
                            "unsupported operand type(s) for divmod(): '{}' and '{}'",
                            args[0].type_name(),
                            args[1].type_name()
                        ))
                    } else {
                        Err(e)
                    }
                }
            }
        }
        "format" => {
            no_kwargs(name, &kwargs)?;
            check_args(name, &args, 1, 2)?;
            let spec = match args.get(1) {
                None => String::new(),
                Some(Py::Str(s)) => s.to_string(),
                Some(other) => {
                    return type_err(format!(
                        "format() argument 2 must be str, not {}",
                        other.type_name()
                    ));
                }
            };
            super::fmt::format_value(&args[0], &spec).map(Py::str)
        }
        "hash" => {
            no_kwargs(name, &kwargs)?;
            check_args(name, &args, 1, 1)?;
            py_hash(&args[0]).map(Py::Int)
        }
        "id" => {
            no_kwargs(name, &kwargs)?;
            check_args(name, &args, 1, 1)?;
            Ok(Py::Int(args[0].addr() as i64))
        }
        "isinstance" => {
            no_kwargs(name, &kwargs)?;
            check_args(name, &args, 2, 2)?;
            isinstance(&args[0], &args[1]).map(Py::Bool)
        }
        "len" => {
            no_kwargs(name, &kwargs)?;
            check_args(name, &args, 1, 1)?;
            py_len(&args[0]).map(Py::Int)
        }
        "max" | "min" => min_max(c, name, args, kwargs),
        "repr" => {
            no_kwargs(name, &kwargs)?;
            check_args(name, &args, 1, 1)?;
            Ok(Py::str(repr_checked(&args[0])?))
        }
        "round" => {
            check_args(name, &args, 1, 2)?;
            let nd = match (args.get(1), kw(&kwargs, "ndigits")) {
                (Some(Py::None), _) | (None, None) | (None, Some(Py::None)) => None,
                (Some(v), _) | (None, Some(v)) => Some(int_arg(v)?),
            };
            py_round(&args[0], nd)
        }
        "sorted" => {
            check_args(name, &args, 1, 1)?;
            let items = to_vec(&args[0])?;
            for (k, _) in &kwargs {
                if k != "key" && k != "reverse" {
                    return type_err(format!("sort() got an unexpected keyword argument '{k}'"));
                }
            }
            sort_items(c, items, kw(&kwargs, "key"), kw(&kwargs, "reverse")).map(Py::list)
        }
        "sum" => py_sum(c, args, kwargs),
        "type" => {
            no_kwargs(name, &kwargs)?;
            check_args(name, &args, 1, 1)?;
            Ok(Py::str(args[0].type_name()))
        }
        "open" => {
            let fname = args.first().map(to_str).unwrap_or_default();
            Err(exc(
                "FileNotFoundError",
                format!(
                    "[Errno 2] No such file or directory: {}",
                    crate::pyfmt::py_repr_str(&fname)
                ),
            ))
        }
        "print" => Ok(Py::None),
        "dir" => {
            // `dir()` lists the locals of `Interpreter.on_call`.
            if args.is_empty() {
                return Ok(Py::list(
                    [
                        "args", "func", "keywords", "kwargs", "node", "self", "starargs",
                    ]
                    .into_iter()
                    .map(Py::str)
                    .collect(),
                ));
            }
            check_args(name, &args, 1, 1)?;
            let names: &[&str] = match &args[0] {
                Py::None => dirs::DIR_NONE,
                Py::Bool(_) => dirs::DIR_BOOL,
                Py::Int(_) => dirs::DIR_INT,
                Py::Float(_) => dirs::DIR_FLOAT,
                Py::Str(_) => dirs::DIR_STR,
                Py::Bytes(_) => dirs::DIR_BYTES,
                Py::List(_) => dirs::DIR_LIST,
                Py::Tuple(_) => dirs::DIR_TUPLE,
                Py::Dict(_) => dirs::DIR_DICT,
                Py::Set(_) => dirs::DIR_SET,
                Py::Range(..) => dirs::DIR_RANGE,
                Py::Slice(_) => dirs::DIR_SLICE,
                Py::Ellipsis => dirs::DIR_ELLIPSIS,
                Py::Exc(_) => dirs::DIR_EXC,
                Py::Type(_) => dirs::DIR_TYPE,
                Py::Builtin(_) | Py::Method(_) => dirs::DIR_FUNC_BUILTIN,
                Py::Obj(o) => {
                    let mut v: Vec<String> =
                        dirs::DIR_OBJ.iter().map(|s| (*s).to_string()).collect();
                    v.extend(o.borrow().keys().cloned());
                    v.sort();
                    v.dedup();
                    return Ok(Py::list(v.into_iter().map(Py::str).collect()));
                }
                _ => dirs::DIR_OBJ,
            };
            Ok(Py::list(names.iter().map(|s| Py::str(*s)).collect()))
        }
        "uuid5" => {
            no_kwargs(name, &kwargs)?;
            check_args(name, &args, 2, 2)?;
            let (Py::Str(ns), Py::Str(nm)) = (&args[0], &args[1]) else {
                return match (&args[0], &args[1]) {
                    (Py::Str(_), other) => {
                        type_err(format!("can't concat {} to bytes", other.type_name()))
                    }
                    (other, _) => Err(exc(
                        "AttributeError",
                        format!("'{}' object has no attribute 'encode'", other.type_name()),
                    )),
                };
            };
            let derived = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, ns.as_bytes());
            Ok(Py::str(
                uuid::Uuid::new_v5(&derived, nm.as_bytes()).to_string(),
            ))
        }
        _ => math_func(name, &args, &kwargs),
    }
}

fn py_hash(v: &Py) -> PyRes<i64> {
    const MOD: i128 = (1 << 61) - 1;
    fn hash_int(i: i128) -> i64 {
        let neg = i < 0;
        let h = (i.abs() % MOD) as i64;
        let h = if neg { -h } else { h };
        if h == -1 { -2 } else { h }
    }
    // `_Py_HashDouble`
    fn hash_float(x: f64) -> i64 {
        if x.is_nan() {
            return 0;
        }
        if x.is_infinite() {
            return if x > 0.0 { 314159 } else { -314159 };
        }
        const BITS: i32 = 61;
        if x == 0.0 {
            return 0;
        }
        let (mut m, mut e) = {
            let (m, e) = frexp(x.abs());
            (m, e)
        };
        let mut hx: u64 = 0;
        while m != 0.0 {
            hx = ((hx << 28) & ((1u64 << BITS) - 1)) | (hx >> (BITS - 28));
            m *= 268_435_456.0; // 2**28
            e -= 28;
            let y = m as u64;
            m -= y as f64;
            hx += y;
            if hx >= (1u64 << BITS) {
                hx -= 1u64 << BITS;
            }
        }
        let e = if e >= 0 {
            e % BITS
        } else {
            BITS - 1 - ((-1 - e) % BITS)
        };
        hx = ((hx << e) & ((1u64 << BITS) - 1)) | (hx >> (BITS - e));
        let mut h = hx as i64;
        if x < 0.0 {
            h = -h;
        }
        if h == -1 { -2 } else { h }
    }
    Ok(match v {
        Py::None => 0xFCA8_6420,
        Py::Bool(b) => *b as i64,
        Py::Int(i) => hash_int(*i as i128),
        Py::BigInt(n) => pynum::hash(IntRef::Big(n)),
        Py::Complex(r, i) => {
            // `complex.__hash__`: `hash(real) + 1000003 * hash(imag)`.
            let h = hash_float(*r).wrapping_add(hash_float(*i).wrapping_mul(1_000_003));
            if h == -1 { -2 } else { h }
        }
        Py::Float(f) => hash_float(*f),
        Py::Str(s) if s.is_empty() => 0,
        Py::Str(s) => {
            let mut h: u64 = 0xcbf2_9ce4_8422_2325;
            for b in s.bytes() {
                h ^= b as u64;
                h = h.wrapping_mul(0x100_0000_01b3);
            }
            h as i64
        }
        Py::Tuple(t) => {
            // The xxHash-based tuple hash of CPython 3.8+.
            const P1: u64 = 11_400_714_785_074_694_791;
            const P2: u64 = 14_029_467_366_897_019_727;
            const P5: u64 = 2_870_177_450_012_600_261;
            let mut acc: u64 = P5;
            for x in t.iter() {
                let lane = py_hash(x)? as u64;
                acc = acc.wrapping_add(lane.wrapping_mul(P2));
                acc = acc.rotate_left(31);
                acc = acc.wrapping_mul(P1);
            }
            acc = acc.wrapping_add((t.len() as u64) ^ (P5 ^ 3_527_539));
            if acc == u64::MAX {
                return Ok(1_546_275_796);
            }
            acc as i64
        }
        other => {
            hkey(other)?;
            other.addr() as i64
        }
    })
}

pub(crate) fn isinstance(obj: &Py, cls: &Py) -> PyRes<bool> {
    match cls {
        Py::Type(t) => Ok(instance_of(obj, t)),
        Py::Tuple(items) => {
            for x in items.iter() {
                if isinstance(obj, x)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        _ => type_err("isinstance() arg 2 must be a type, a tuple of types, or a union"),
    }
}

fn instance_of(obj: &Py, t: &str) -> bool {
    match (obj, t) {
        (_, "object") => true,
        (Py::Type(_), "type") => true,
        (Py::Bool(_), "bool" | "int") => true,
        (Py::Int(_) | Py::BigInt(_), "int") => true,
        (Py::Float(_), "float") => true,
        (Py::Complex(..), "complex") => true,
        (Py::Str(_), "str") => true,
        (Py::Bytes(_), "bytes") => true,
        (Py::List(_), "list") => true,
        (Py::Tuple(_), "tuple") => true,
        (Py::Dict(_), "dict") => true,
        (Py::Set(_), "set" | "frozenset") => true,
        (Py::Range(..), "range") => true,
        (Py::Slice(_), "slice") => true,
        (Py::Iter(it), t) => it.borrow().kind == t,
        (Py::Exc(e), t) => exc_subclass(e.class, t),
        _ => false,
    }
}

fn min_max(c: &mut dyn Caller, name: &str, args: Vec<Py>, kwargs: Kwargs) -> PyRes<Py> {
    let want_max = name == "max";
    let mut key: Option<Py> = None;
    let mut default: Option<Py> = None;
    for (k, v) in kwargs {
        match k.as_str() {
            "key" => key = if v.is_none() { None } else { Some(v) },
            "default" => default = Some(v),
            other => {
                return type_err(format!(
                    "{name}() got an unexpected keyword argument '{other}'"
                ));
            }
        }
    }
    let items: Vec<Py> = match args.len() {
        0 => return type_err(format!("{name} expected at least 1 argument, got 0")),
        1 => to_vec(&args[0])?,
        _ => {
            if default.is_some() {
                return type_err(format!(
                    "Cannot specify a default for {name}() with multiple positional arguments"
                ));
            }
            args
        }
    };
    if items.is_empty() {
        return match default {
            Some(d) => Ok(d),
            None => value_err(format!("{name}() iterable argument is empty")),
        };
    }
    let mut best = items[0].clone();
    let mut best_key = match &key {
        Some(f) => c.call_value(f, vec![best.clone()], vec![])?,
        None => best.clone(),
    };
    for x in items.into_iter().skip(1) {
        let k = match &key {
            Some(f) => c.call_value(f, vec![x.clone()], vec![])?,
            None => x.clone(),
        };
        let better = if want_max {
            py_rel(&k, &best_key, Rel::Gt)?
        } else {
            py_rel(&k, &best_key, Rel::Lt)?
        };
        if better {
            best = x;
            best_key = k;
        }
    }
    Ok(best)
}

pub(crate) fn py_round(x: &Py, nd: Option<i64>) -> PyRes<Py> {
    match (x, nd) {
        (Py::Int(_) | Py::BigInt(_), None) => Ok(x.clone()),
        (Py::Bool(b), None) => Ok(Py::Int(*b as i64)),
        (Py::Int(i), Some(n)) => Ok(round_to_py(builtins::round_int(IntRef::Small(*i), n))),
        (Py::BigInt(i), Some(n)) => Ok(round_to_py(builtins::round_int(IntRef::Big(i), n))),
        (Py::Bool(b), Some(n)) => Ok(round_to_py(builtins::round_int(
            IntRef::Small(i64::from(*b)),
            n,
        ))),
        (Py::Float(f), None) => float_to_int(f.round_ties_even()),
        (Py::Float(f), Some(n)) => builtins::round_float(*f, n)
            .map(Py::Float)
            .map_err(|e| eval_err_to_exc(e, "OverflowError")),
        (other, _) => type_err(format!(
            "type {} doesn't define __round__ method",
            other.type_name()
        )),
    }
}

fn round_to_py(v: Value) -> Py {
    match v {
        Value::Int(i) => Py::Int(i),
        Value::BigInt(n) => Py::BigInt(Rc::new(*n)),
        _ => Py::Int(0),
    }
}

pub(crate) fn float_to_int(f: f64) -> PyRes<Py> {
    if f.is_nan() {
        return value_err("cannot convert float NaN to integer");
    }
    if f.is_infinite() {
        return overflow_err("cannot convert float infinity to integer");
    }
    if (-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&f) {
        Ok(Py::Int(f as i64))
    } else {
        Ok(Py::from_big(pynum::float_to_bigint(f)))
    }
}

/// Sort with an optional key function; `list.sort`/`sorted` semantics (stable,
/// `<` only).
pub(crate) fn sort_items(
    c: &mut dyn Caller,
    items: Vec<Py>,
    key: Option<&Py>,
    reverse: Option<&Py>,
) -> PyRes<Vec<Py>> {
    let key = key.filter(|k| !k.is_none());
    let mut keyed: Vec<(Py, Py)> = Vec::with_capacity(items.len());
    for x in items {
        let k = match key {
            Some(f) => c.call_value(f, vec![x.clone()], vec![])?,
            None => x.clone(),
        };
        keyed.push((k, x));
    }
    let rev = reverse.is_some_and(Py::truthy);
    // Merge sort with a fallible `<`.
    let mut err: Option<Box<ExcObj>> = None;
    if rev {
        keyed.reverse();
    }
    let mut less = |a: &(Py, Py), b: &(Py, Py)| -> bool {
        if err.is_some() {
            return false;
        }
        match py_rel(&a.0, &b.0, Rel::Lt) {
            Ok(v) => v,
            Err(e) => {
                err = Some(e);
                false
            }
        }
    };
    keyed = merge_sort(keyed, &mut less, rev);
    if rev {
        keyed.reverse();
    }
    if let Some(e) = err {
        return Err(e);
    }
    Ok(keyed.into_iter().map(|(_, v)| v).collect())
}

fn merge_sort<T: Clone>(v: Vec<T>, less: &mut impl FnMut(&T, &T) -> bool, _rev: bool) -> Vec<T> {
    if v.len() <= 1 {
        return v;
    }
    let mid = v.len() / 2;
    let right = merge_sort(v[mid..].to_vec(), less, _rev);
    let left = merge_sort(v[..mid].to_vec(), less, _rev);
    let mut out = Vec::with_capacity(left.len() + right.len());
    let (mut i, mut j) = (0, 0);
    while i < left.len() && j < right.len() {
        if less(&right[j], &left[i]) {
            out.push(right[j].clone());
            j += 1;
        } else {
            out.push(left[i].clone());
            i += 1;
        }
    }
    out.extend_from_slice(&left[i..]);
    out.extend_from_slice(&right[j..]);
    out
}

fn py_sum(c: &mut dyn Caller, args: Vec<Py>, kwargs: Kwargs) -> PyRes<Py> {
    let _ = c;
    let mut start: Option<Py> = None;
    for (k, v) in &kwargs {
        if k == "start" {
            start = Some(v.clone());
        } else {
            return type_err(format!("sum() got an unexpected keyword argument '{k}'"));
        }
    }
    if args.is_empty() {
        return type_err("sum() takes at least 1 positional argument (0 given)");
    }
    if args.len() > 2 || (args.len() == 2 && start.is_some()) {
        return type_err(format!(
            "sum() takes at most 2 arguments ({} given)",
            args.len()
        ));
    }
    let start = start.or_else(|| args.get(1).cloned()).unwrap_or(Py::Int(0));
    let mut it = iterate(&args[0])?;
    match &start {
        Py::Str(_) => return type_err("sum() can't sum strings [use ''.join(seq) instead]"),
        Py::Bytes(_) => return type_err("sum() can't sum bytes [use b''.join(seq) instead]"),
        _ => {}
    }
    let mut acc = start;
    // CPython's fast paths: exact ints, then floats with Neumaier compensation.
    if matches!(acc, Py::Int(_) | Py::Bool(_) | Py::BigInt(_)) {
        let mut total = match &acc {
            Py::Int(i) => BigInt::from(*i),
            Py::Bool(b) => BigInt::from(*b),
            Py::BigInt(n) => (**n).clone(),
            _ => BigInt::default(),
        };
        loop {
            let Some(item) = it.next() else {
                return Ok(Py::from_big(total));
            };
            match &item {
                Py::Int(_) | Py::Bool(_) | Py::BigInt(_) => {
                    let v = match &item {
                        Py::Int(i) => BigInt::from(*i),
                        Py::Bool(b) => BigInt::from(*b),
                        Py::BigInt(n) => (**n).clone(),
                        _ => BigInt::default(),
                    };
                    total += v;
                }
                Py::Float(_) => {
                    // `float(total)` for the accumulator (CPython adds the exact
                    // sum of the integers so far).
                    acc = Py::Float(pynum::to_f64(IntRef::Big(&total)).map_err(super::ops::num_err)?);
                    // fall into the float loop with this item pending
                    return sum_floats(it, acc, Some(item));
                }
                _ => {
                    acc = binop(super::ast::BinOp::Add, &Py::from_big(total), &item)?;
                    break;
                }
            }
        }
    }
    if matches!(acc, Py::Float(_)) {
        return sum_floats(it, acc, None);
    }
    while let Some(item) = it.next() {
        acc = binop(super::ast::BinOp::Add, &acc, &item)?;
    }
    Ok(acc)
}

fn sum_floats(mut it: Cursor, acc: Py, pending: Option<Py>) -> PyRes<Py> {
    let Py::Float(mut f_result) = acc else {
        return Ok(acc);
    };
    let mut comp = 0.0_f64;
    let mut next = pending;
    loop {
        let item = match next.take() {
            Some(x) => x,
            None => match it.next() {
                Some(x) => x,
                None => break,
            },
        };
        match &item {
            Py::Float(x) => {
                let x = *x;
                let t = f_result + x;
                if f_result.abs() >= x.abs() {
                    comp += (f_result - t) + x;
                } else {
                    comp += (x - t) + f_result;
                }
                f_result = t;
            }
            Py::Int(i) => f_result += *i as f64,
            Py::Bool(b) => f_result += *b as i64 as f64,
            _ => {
                if comp != 0.0 && comp.is_finite() {
                    f_result += comp;
                }
                let mut acc = Py::Float(f_result);
                acc = binop(super::ast::BinOp::Add, &acc, &item)?;
                while let Some(item) = it.next() {
                    acc = binop(super::ast::BinOp::Add, &acc, &item)?;
                }
                return Ok(acc);
            }
        }
    }
    if comp != 0.0 && comp.is_finite() {
        f_result += comp;
    }
    Ok(Py::Float(f_result))
}

// --- math ---------------------------------------------------------------------

fn zero_div_msg<T>(msg: &str) -> PyRes<T> {
    Err(exc("ZeroDivisionError", msg))
}

fn domain<T>() -> PyRes<T> {
    value_err("math domain error")
}

fn range_err<T>() -> PyRes<T> {
    overflow_err("math range error")
}

fn math_func(name: &'static str, args: &[Py], kwargs: &Kwargs) -> PyRes<Py> {
    no_kwargs(name, kwargs)?;
    let one = |args: &[Py]| -> PyRes<f64> {
        check_args(name, args, 1, 1)?;
        real_arg(&args[0])
    };
    let f1 = |args: &[Py], f: fn(f64) -> f64, dom: fn(f64) -> bool| -> PyRes<Py> {
        let x = one(args)?;
        if !dom(x) && !x.is_nan() {
            return domain();
        }
        let r = f(x);
        if r.is_infinite() && x.is_finite() {
            return range_err();
        }
        Ok(Py::Float(r))
    };
    match name {
        "acos" => f1(args, f64::acos, |x| (-1.0..=1.0).contains(&x)),
        "asin" => f1(args, f64::asin, |x| (-1.0..=1.0).contains(&x)),
        "atan" => f1(args, f64::atan, |_| true),
        "acosh" => f1(args, f64::acosh, |x| x >= 1.0),
        "asinh" => f1(args, f64::asinh, |_| true),
        "atanh" => f1(args, f64::atanh, |x| x > -1.0 && x < 1.0),
        "cos" => f1(args, f64::cos, |x| x.is_finite()),
        "sin" => f1(args, f64::sin, |x| x.is_finite()),
        "tan" => f1(args, f64::tan, |x| x.is_finite()),
        "cosh" => f1(args, f64::cosh, |_| true),
        "sinh" => f1(args, f64::sinh, |_| true),
        "tanh" => f1(args, f64::tanh, |_| true),
        "exp" => f1(args, f64::exp, |_| true),
        "sqrt" => f1(args, f64::sqrt, |x| x >= 0.0),
        "log1p" => f1(args, f64::ln_1p, |x| x > -1.0),
        "degrees" => f1(args, f64::to_degrees, |_| true),
        "radians" => f1(args, f64::to_radians, |_| true),
        "fabs" => f1(args, f64::abs, |_| true),
        "log10" => {
            let x = one(args)?;
            if x <= 0.0 {
                return domain();
            }
            Ok(Py::Float(x.log10()))
        }
        "log" => {
            check_args(name, args, 1, 2)?;
            let x = real_arg(&args[0])?;
            if x <= 0.0 {
                return domain();
            }
            match args.get(1) {
                None => Ok(Py::Float(x.ln())),
                Some(b) => {
                    let b = real_arg(b)?;
                    if b <= 0.0 {
                        return domain();
                    }
                    let d = b.ln();
                    if d == 0.0 {
                        return zero_div_math();
                    }
                    Ok(Py::Float(x.ln() / d))
                }
            }
        }
        "atan2" | "copysign" | "fmod" | "pow" => {
            check_args(name, args, 2, 2)?;
            let (x, y) = (real_arg(&args[0])?, real_arg(&args[1])?);
            match name {
                "atan2" => Ok(Py::Float(x.atan2(y))),
                "copysign" => Ok(Py::Float(x.copysign(y))),
                "fmod" => {
                    if y == 0.0 && !x.is_nan() || x.is_infinite() && !y.is_nan() {
                        return domain();
                    }
                    Ok(Py::Float(x % y))
                }
                _ => {
                    if x == 0.0 && y < 0.0 && y.is_finite() {
                        return domain();
                    }
                    if x < 0.0 && x.is_finite() && y.is_finite() && y.fract() != 0.0 {
                        return domain();
                    }
                    let r = x.powf(y);
                    if r.is_infinite() && x.is_finite() && y.is_finite() {
                        return range_err();
                    }
                    Ok(Py::Float(r))
                }
            }
        }
        "ceil" | "floor" | "trunc" => {
            check_args(name, args, 1, 1)?;
            match &args[0] {
                Py::Int(_) | Py::BigInt(_) => Ok(args[0].clone()),
                Py::Bool(b) => Ok(Py::Int(*b as i64)),
                Py::Float(f) => float_to_int(match name {
                    "ceil" => f.ceil(),
                    "floor" => f.floor(),
                    _ => f.trunc(),
                }),
                other if name == "trunc" => type_err(format!(
                    "type {} doesn't define __trunc__ method",
                    other.type_name()
                )),
                other => type_err(format!("must be real number, not {}", other.type_name())),
            }
        }
        "factorial" => {
            check_args(name, args, 1, 1)?;
            let n = match &args[0] {
                Py::Int(i) => *i,
                Py::Bool(b) => *b as i64,
                Py::BigInt(n) if n.is_negative() => {
                    return value_err("factorial() not defined for negative values");
                }
                Py::BigInt(_) => {
                    return overflow_err("factorial() argument should not exceed 9223372036854775807");
                }
                Py::Float(_) => {
                    return type_err("'float' object cannot be interpreted as an integer");
                }
                other => {
                    return type_err(format!(
                        "'{}' object cannot be interpreted as an integer",
                        other.type_name()
                    ));
                }
            };
            if n < 0 {
                return value_err("factorial() not defined for negative values");
            }
            // Python computes the exact product; 21! is already beyond `i64`.
            let mut r = BigInt::from(1);
            for k in 2..=n {
                r *= k;
            }
            Ok(Py::from_big(r))
        }
        "frexp" => {
            let x = one(args)?;
            if x == 0.0 || !x.is_finite() {
                return Ok(Py::tuple(vec![Py::Float(x), Py::Int(0)]));
            }
            let (m, e) = frexp(x);
            Ok(Py::tuple(vec![Py::Float(m), Py::Int(e as i64)]))
        }
        "ldexp" => {
            check_args(name, args, 2, 2)?;
            let x = real_arg(&args[0])?;
            let i = int_arg(&args[1])?;
            let r = x * 2f64.powi(i.clamp(-2200, 2200) as i32);
            if r.is_infinite() && x.is_finite() {
                return range_err();
            }
            Ok(Py::Float(r))
        }
        "modf" => {
            let x = one(args)?;
            if x.is_infinite() {
                return Ok(Py::tuple(vec![
                    Py::Float(0.0_f64.copysign(x)),
                    Py::Float(x),
                ]));
            }
            let i = x.trunc();
            Ok(Py::tuple(vec![
                Py::Float((x - i).copysign(x)),
                Py::Float(i),
            ]))
        }
        "fsum" => {
            check_args(name, args, 1, 1)?;
            let mut it = iterate(&args[0])?;
            let mut xs = Vec::new();
            while let Some(x) = it.next() {
                xs.push(real_arg(&x)?);
            }
            fsum(&xs).map(Py::Float)
        }
        "hypot" => {
            let mut s = 0.0f64;
            let mut inf = false;
            let mut nan = false;
            for a in args {
                let x = real_arg(a)?;
                inf |= x.is_infinite();
                nan |= x.is_nan();
                s = s.hypot(x);
            }
            if inf {
                return Ok(Py::Float(f64::INFINITY));
            }
            if nan {
                return Ok(Py::Float(f64::NAN));
            }
            if s.is_infinite() {
                return range_err();
            }
            Ok(Py::Float(s))
        }
        "isinf" => Ok(Py::Bool(one(args)?.is_infinite())),
        "isnan" => Ok(Py::Bool(one(args)?.is_nan())),
        _ => Err(exc(
            "NotImplementedError",
            format!("{name} is not supported"),
        )),
    }
}

fn zero_div_math<T>() -> PyRes<T> {
    Err(exc("ZeroDivisionError", "float division by zero"))
}

/// `math.fsum`: exactly rounded sum (Shewchuk).
fn fsum(xs: &[f64]) -> PyRes<f64> {
    let mut partials: Vec<f64> = Vec::new();
    let (mut special, mut inf_sum) = (0.0_f64, 0.0_f64);
    let mut has_special = false;
    for &x0 in xs {
        let mut x = x0;
        if !x.is_finite() {
            has_special = true;
            special += x;
            if x.is_infinite() {
                inf_sum += x;
            }
            continue;
        }
        let mut i = 0;
        for j in 0..partials.len() {
            let mut y = partials[j];
            if x.abs() < y.abs() {
                std::mem::swap(&mut x, &mut y);
            }
            let hi = x + y;
            let lo = y - (hi - x);
            if lo != 0.0 {
                partials[i] = lo;
                i += 1;
            }
            x = hi;
        }
        partials.truncate(i);
        partials.push(x);
    }
    if has_special {
        if special.is_nan() && inf_sum.is_nan() {
            return value_err("-inf + inf in fsum");
        }
        return Ok(special);
    }
    let mut hi = 0.0;
    let mut n = partials.len();
    if n > 0 {
        n -= 1;
        hi = partials[n];
        let mut lo = 0.0;
        while n > 0 {
            let x = hi;
            n -= 1;
            let y = partials[n];
            hi = x + y;
            let yr = hi - x;
            lo = y - yr;
            if lo != 0.0 {
                break;
            }
        }
        if n > 0 && ((lo < 0.0 && partials[n - 1] < 0.0) || (lo > 0.0 && partials[n - 1] > 0.0)) {
            let y = lo * 2.0;
            let x = hi + y;
            let yr = x - hi;
            if y == yr {
                hi = x;
            }
        }
    }
    Ok(hi)
}

// --- type constructors -------------------------------------------------------

/// Call a built-in class: `int(x)`, `str(x)`, `ValueError('m')`, ...
pub(crate) fn call_type(
    c: &mut dyn Caller,
    name: &'static str,
    args: Vec<Py>,
    kwargs: Kwargs,
) -> PyRes<Py> {
    if let Some(class) = exc_class(name) {
        return Ok(Py::Exc(Rc::new(ExcObj { class, args })));
    }
    match name {
        "int" => {
            no_kwargs_except(name, &kwargs, &["base"])?;
            check_args(name, &args, 0, 2)?;
            let base = args.get(1).or_else(|| kw(&kwargs, "base"));
            let Some(x) = args.first() else {
                return Ok(Py::Int(0));
            };
            if let Some(b) = base {
                let b = int_arg(b)?;
                return match x {
                    Py::Str(s) => parse_int_py(s, b),
                    Py::Bytes(bytes) => parse_int_py(&String::from_utf8_lossy(bytes), b),
                    _ => type_err("int() can't convert non-string with explicit base"),
                };
            }
            match x {
                Py::Int(_) | Py::BigInt(_) => Ok(x.clone()),
                Py::Bool(b) => Ok(Py::Int(*b as i64)),
                Py::Float(f) => float_to_int(f.trunc()),
                Py::Str(s) => parse_int_py(s, 10),
                Py::Bytes(b) => parse_int_py(&String::from_utf8_lossy(b), 10),
                other => type_err(format!(
                    "int() argument must be a string, a bytes-like object or a real number, not '{}'",
                    other.type_name()
                )),
            }
        }
        "float" => {
            no_kwargs(name, &kwargs)?;
            check_args(name, &args, 0, 1)?;
            match args.first() {
                None => Ok(Py::Float(0.0)),
                Some(Py::Float(f)) => Ok(Py::Float(*f)),
                Some(Py::Int(i)) => Ok(Py::Float(*i as f64)),
                Some(Py::BigInt(n)) => pynum::big_to_f64(n).map(Py::Float).map_or_else(
                    || overflow_err(pynum::INT_TOO_LARGE_FOR_FLOAT),
                    Ok,
                ),
                Some(Py::Bool(b)) => Ok(Py::Float(*b as i64 as f64)),
                Some(Py::Str(s)) => crate::expr::pystr::float_from_str(s)
                    .map(Py::Float)
                    .map_or_else(
                        || {
                            value_err(format!(
                                "could not convert string to float: {}",
                                crate::pyfmt::py_repr_str(s)
                            ))
                        },
                        Ok,
                    ),
                Some(other) => type_err(format!(
                    "float() argument must be a string or a real number, not '{}'",
                    other.type_name()
                )),
            }
        }
        "str" => {
            check_args(name, &args, 0, 3)?;
            Ok(match args.first() {
                None => Py::str(""),
                Some(Py::Bytes(b)) if args.len() > 1 => {
                    Py::str(String::from_utf8_lossy(b).to_string())
                }
                Some(v) => Py::str(to_str_checked(v)?),
            })
        }
        "bool" => {
            no_kwargs(name, &kwargs)?;
            check_args(name, &args, 0, 1)?;
            Ok(Py::Bool(args.first().is_some_and(Py::truthy)))
        }
        "list" => {
            no_kwargs(name, &kwargs)?;
            check_args(name, &args, 0, 1)?;
            Ok(Py::list(match args.first() {
                Some(x) => to_vec(x)?,
                None => Vec::new(),
            }))
        }
        "tuple" => {
            no_kwargs(name, &kwargs)?;
            check_args(name, &args, 0, 1)?;
            Ok(match args.first() {
                Some(Py::Tuple(t)) => Py::Tuple(t.clone()),
                Some(x) => Py::tuple(to_vec(x)?),
                None => Py::tuple(Vec::new()),
            })
        }
        "set" | "frozenset" => {
            no_kwargs(name, &kwargs)?;
            check_args(name, &args, 0, 1)?;
            let mut s = PySet::new();
            if let Some(x) = args.first() {
                for item in to_vec(x)? {
                    set_add(&mut s, hkey(&item)?, item);
                }
            }
            Ok(Py::Set(rf(s)))
        }
        "dict" => build_dict(args, kwargs),
        "range" => {
            no_kwargs(name, &kwargs)?;
            check_args(name, &args, 1, 3)?;
            let nums: Vec<i64> = args.iter().map(int_arg).collect::<PyRes<_>>()?;
            let (a, b, s) = match nums.as_slice() {
                [stop] => (0, *stop, 1),
                [a, b] => (*a, *b, 1),
                [a, b, s] => (*a, *b, *s),
                _ => unreachable!("arity checked"),
            };
            if s == 0 {
                return value_err("range() arg 3 must not be zero");
            }
            Ok(Py::Range(a, b, s))
        }
        "slice" => {
            no_kwargs(name, &kwargs)?;
            check_args(name, &args, 1, 3)?;
            let (a, b, s) = match args.as_slice() {
                [stop] => (Py::None, stop.clone(), Py::None),
                [a, b] => (a.clone(), b.clone(), Py::None),
                [a, b, s] => (a.clone(), b.clone(), s.clone()),
                _ => unreachable!("arity checked"),
            };
            Ok(Py::Slice(Rc::new((a, b, s))))
        }
        "enumerate" => {
            check_args(name, &args, 1, 2)?;
            let start = match (args.get(1), kw(&kwargs, "start")) {
                (Some(v), _) | (None, Some(v)) => int_arg(v)?,
                _ => 0,
            };
            let items: VecDeque<Py> = to_vec(&args[0])?
                .into_iter()
                .enumerate()
                .map(|(i, x)| Py::tuple(vec![Py::Int(start + i as i64), x]))
                .collect();
            Ok(Py::Iter(rf(PyIter {
                kind: "enumerate",
                items,
            })))
        }
        "zip" => {
            let strict = kw(&kwargs, "strict").is_some_and(Py::truthy);
            let mut lists = Vec::new();
            for a in &args {
                lists.push(to_vec(a)?);
            }
            let n = lists.iter().map(Vec::len).min().unwrap_or(0);
            if strict && lists.iter().any(|l| l.len() != n) {
                return value_err("zip() argument 2 is shorter or longer than argument 1");
            }
            let items: VecDeque<Py> = (0..n)
                .map(|i| Py::tuple(lists.iter().map(|l| l[i].clone()).collect()))
                .collect();
            Ok(Py::Iter(rf(PyIter { kind: "zip", items })))
        }
        "map" => {
            no_kwargs(name, &kwargs)?;
            if args.len() < 2 {
                return type_err("map() must have at least two arguments.");
            }
            let f = args[0].clone();
            let mut lists = Vec::new();
            for a in &args[1..] {
                lists.push(to_vec(a)?);
            }
            let n = lists.iter().map(Vec::len).min().unwrap_or(0);
            let mut items = VecDeque::new();
            for i in 0..n {
                let call_args: Vec<Py> = lists.iter().map(|l| l[i].clone()).collect();
                items.push_back(c.call_value(&f, call_args, vec![])?);
            }
            Ok(Py::Iter(rf(PyIter { kind: "map", items })))
        }
        "filter" => {
            no_kwargs(name, &kwargs)?;
            check_args(name, &args, 2, 2)?;
            let f = args[0].clone();
            let mut items = VecDeque::new();
            for x in to_vec(&args[1])? {
                let keep = if f.is_none() {
                    x.truthy()
                } else {
                    c.call_value(&f, vec![x.clone()], vec![])?.truthy()
                };
                if keep {
                    items.push_back(x);
                }
            }
            Ok(Py::Iter(rf(PyIter {
                kind: "filter",
                items,
            })))
        }
        "reversed" => {
            no_kwargs(name, &kwargs)?;
            check_args(name, &args, 1, 1)?;
            let items: VecDeque<Py> = match &args[0] {
                Py::List(_)
                | Py::Tuple(_)
                | Py::Str(_)
                | Py::Range(..)
                | Py::Bytes(_)
                | Py::Dict(_) => to_vec(&args[0])?.into_iter().rev().collect(),
                other => {
                    return type_err(format!("'{}' object is not reversible", other.type_name()));
                }
            };
            Ok(Py::Iter(rf(PyIter {
                kind: "reversed",
                items,
            })))
        }
        "bytes" | "bytearray" => {
            check_args(name, &args, 0, 3)?;
            Ok(Py::Bytes(match args.first() {
                None => Vec::new().into(),
                Some(Py::Int(n)) if *n >= 0 => vec![0u8; (*n).min(1 << 24) as usize].into(),
                Some(Py::Str(s)) => s.as_bytes().to_vec().into(),
                Some(Py::Bytes(b)) => b.clone(),
                Some(x) => {
                    let mut out = Vec::new();
                    for v in to_vec(x)? {
                        let i = int_arg(&v)?;
                        if !(0..256).contains(&i) {
                            return value_err("bytes must be in range(0, 256)");
                        }
                        out.push(i as u8);
                    }
                    out.into()
                }
            }))
        }
        "complex" => {
            no_kwargs_except(name, &kwargs, &["real", "imag"])?;
            check_args(name, &args, 0, 2)?;
            let part =
                |v: Option<&Py>| -> PyRes<(f64, f64)> {
                    match v {
                    None => Ok((0.0, 0.0)),
                    Some(Py::Str(_)) => value_err("complex() string arguments are not supported"),
                    Some(x) => complex_parts(x).map_or_else(
                        || type_err(format!(
                            "complex() first argument must be a string or a number, not '{}'",
                            x.type_name()
                        )),
                        Ok,
                    ),
                }
                };
            let (a, b) = (
                part(args.first().or_else(|| kw(&kwargs, "real")))?,
                part(args.get(1).or_else(|| kw(&kwargs, "imag")))?,
            );
            // complex(a, b) = a + b*1j
            Ok(Py::Complex(a.0 - b.1, a.1 + b.0))
        }
        _ => type_err(format!("cannot create '{name}' instances")),
    }
}

fn no_kwargs_except(name: &str, kwargs: &Kwargs, allowed: &[&str]) -> PyRes<()> {
    for (k, _) in kwargs {
        if !allowed.contains(&k.as_str()) {
            return type_err(format!("'{k}' is an invalid keyword argument for {name}()"));
        }
    }
    Ok(())
}

fn parse_int_py(s: &str, base: i64) -> PyRes<Py> {
    match builtins::parse_int(s, base) {
        Ok(Value::Int(i)) => Ok(Py::Int(i)),
        Ok(Value::BigInt(n)) => Ok(Py::BigInt(Rc::new(*n))),
        Ok(_) => Ok(Py::Int(0)),
        Err(e) => {
            let msg = match &e {
                crate::expr::error::ExprError::Eval(m) => m.clone(),
                other => other.to_string(),
            };
            if msg.contains("out of range") {
                overflow_err(msg)
            } else {
                value_err(msg)
            }
        }
    }
}

fn build_dict(args: Vec<Py>, kwargs: Kwargs) -> PyRes<Py> {
    if args.len() > 1 {
        return type_err(format!(
            "dict expected at most 1 argument, got {}",
            args.len()
        ));
    }
    let mut d = PyDict::new();
    if let Some(src) = args.first() {
        match src {
            Py::Dict(o) => {
                for (k, v) in o.borrow().iter() {
                    d.insert(k.clone(), v.clone());
                }
            }
            other => {
                let items = to_vec(other).map_err(|_| {
                    exc(
                        "TypeError",
                        format!("'{}' object is not iterable", other.type_name()),
                    )
                })?;
                for (i, item) in items.into_iter().enumerate() {
                    let pair = to_vec(&item).map_err(|_| {
                        exc(
                            "TypeError",
                            format!("cannot convert dictionary update sequence element #{i} to a sequence"),
                        )
                    })?;
                    if pair.len() != 2 {
                        return value_err(format!(
                            "dictionary update sequence element #{i} has length {}; 2 is required",
                            pair.len()
                        ));
                    }
                    dict_set(&mut d, hkey(&pair[0])?, pair[0].clone(), pair[1].clone());
                }
            }
        }
    }
    for (k, v) in kwargs {
        dict_set(&mut d, HKey::Str(k.as_str().into()), Py::str(k.as_str()), v);
    }
    Ok(Py::Dict(rf(d)))
}

#[allow(dead_code)]
fn _unused() {
    let _ = py_float_repr;
}

/// `dict(...)` for `dict.update`.
pub(crate) fn call_type_dict(args: Vec<Py>, kwargs: Kwargs) -> PyRes<Py> {
    build_dict(args, kwargs)
}
