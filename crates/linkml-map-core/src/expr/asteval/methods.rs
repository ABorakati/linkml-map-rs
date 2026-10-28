//! Attribute access and the methods of Python's built-in value types.

use std::rc::Rc;
use std::sync::Arc;

use super::funcs::{Caller, Kwargs, py_round, sort_items};
use super::ops::*;
use super::value::*;
use crate::expr::pystr;
use crate::pynum::{self, BigInt};

const STR_METHODS: &[&str] = &[
    "capitalize",
    "casefold",
    "center",
    "count",
    "encode",
    "endswith",
    "expandtabs",
    "find",
    "format",
    "format_map",
    "index",
    "isalnum",
    "isalpha",
    "isascii",
    "isdecimal",
    "isdigit",
    "isidentifier",
    "islower",
    "isnumeric",
    "isprintable",
    "isspace",
    "istitle",
    "isupper",
    "join",
    "ljust",
    "lower",
    "lstrip",
    "maketrans",
    "partition",
    "removeprefix",
    "removesuffix",
    "replace",
    "rfind",
    "rindex",
    "rjust",
    "rpartition",
    "rsplit",
    "rstrip",
    "split",
    "splitlines",
    "startswith",
    "strip",
    "swapcase",
    "title",
    "translate",
    "upper",
    "zfill",
];
const LIST_METHODS: &[&str] = &[
    "append", "clear", "copy", "count", "extend", "index", "insert", "pop", "remove", "reverse",
    "sort",
];
const DICT_METHODS: &[&str] = &[
    "clear",
    "copy",
    "fromkeys",
    "get",
    "items",
    "keys",
    "pop",
    "popitem",
    "setdefault",
    "update",
    "values",
];
const SET_METHODS: &[&str] = &[
    "add",
    "clear",
    "copy",
    "difference",
    "difference_update",
    "discard",
    "intersection",
    "intersection_update",
    "isdisjoint",
    "issubset",
    "issuperset",
    "pop",
    "remove",
    "symmetric_difference",
    "symmetric_difference_update",
    "union",
    "update",
];
const TUPLE_METHODS: &[&str] = &["count", "index"];
const BYTES_METHODS: &[&str] = &["decode", "hex", "startswith", "endswith"];
const INT_METHODS: &[&str] = &[
    "as_integer_ratio",
    "bit_count",
    "bit_length",
    "conjugate",
    "is_integer",
];
const COMPLEX_METHODS: &[&str] = &["conjugate"];
const FLOAT_METHODS: &[&str] = &["as_integer_ratio", "conjugate", "hex", "is_integer"];
const RANGE_METHODS: &[&str] = &["count", "index"];
const EXC_METHODS: &[&str] = &["add_note", "with_traceback"];

fn table(v: &Py) -> &'static [&'static str] {
    match v {
        Py::Str(_) => STR_METHODS,
        Py::List(_) => LIST_METHODS,
        Py::Dict(_) => DICT_METHODS,
        Py::Set(_) => SET_METHODS,
        Py::Tuple(_) => TUPLE_METHODS,
        Py::Bytes(_) => BYTES_METHODS,
        Py::Int(_) | Py::BigInt(_) | Py::Bool(_) => INT_METHODS,
        Py::Float(_) => FLOAT_METHODS,
        Py::Complex(..) => COMPLEX_METHODS,
        Py::Range(..) => RANGE_METHODS,
        Py::Exc(_) => EXC_METHODS,
        Py::Type("dict") => &["fromkeys"],
        Py::Type("str") => &["maketrans"],
        _ => &[],
    }
}

/// `getattr(obj, name)` once the unsafe-attribute checks have passed. A
/// `DynObj` answers a missing attribute with `None` (`__getattr__`); every
/// other type raises `AttributeError`.
pub(crate) fn get_attr(obj: &Py, name: &str) -> PyRes<Py> {
    if let Py::Obj(o) = obj {
        return Ok(o.borrow().get(name).cloned().unwrap_or(Py::None));
    }
    // Data attributes.
    match (obj, name) {
        (Py::Int(_) | Py::BigInt(_) | Py::Bool(_), "real" | "numerator") => {
            return Ok(match obj {
                Py::Bool(b) => Py::Int(*b as i64),
                other => other.clone(),
            });
        }
        (Py::Int(_) | Py::BigInt(_) | Py::Bool(_), "imag") => return Ok(Py::Int(0)),
        (Py::Int(_) | Py::BigInt(_) | Py::Bool(_), "denominator") => return Ok(Py::Int(1)),
        (Py::Float(_), "real") => return Ok(obj.clone()),
        (Py::Complex(r, _), "real") => return Ok(Py::Float(*r)),
        (Py::Complex(_, i), "imag") => return Ok(Py::Float(*i)),
        (Py::Float(_), "imag") => return Ok(Py::Float(0.0)),
        (Py::Range(a, _, _), "start") => return Ok(Py::Int(*a)),
        (Py::Range(_, b, _), "stop") => return Ok(Py::Int(*b)),
        (Py::Range(_, _, c), "step") => return Ok(Py::Int(*c)),
        (Py::Slice(s), "start") => return Ok(s.0.clone()),
        (Py::Slice(s), "stop") => return Ok(s.1.clone()),
        (Py::Slice(s), "step") => return Ok(s.2.clone()),
        (Py::Exc(e), "args") => return Ok(Py::tuple(e.args.clone())),
        (Py::Proc(p), "name") => return Ok(Py::str(p.name.as_str())),
        (Py::Proc(_), "lineno") => return Ok(Py::Int(0)),
        _ => {}
    }
    if let Some(m) = table(obj).iter().find(|m| **m == name) {
        return Ok(Py::Method(Rc::new((obj.clone(), m))));
    }
    Err(exc(
        "AttributeError",
        format!("'{}' object has no attribute '{name}'", obj.type_name()),
    ))
}

/// `setattr(obj, name, val)` after the unsafe checks.
pub(crate) fn set_attr(obj: &Py, name: &str, val: Py) -> PyRes<()> {
    match obj {
        Py::Obj(o) => {
            o.borrow_mut().insert(name.to_string(), val);
            Ok(())
        }
        Py::Proc(_) => type_err("procedure is read-only"),
        other => Err(exc(
            "AttributeError",
            format!(
                "'{}' object has no attribute '{name}' and no __dict__ for setting new attributes",
                other.type_name()
            ),
        )),
    }
}

fn nargs(name: &str, args: &[Py], min: usize, max: usize) -> PyRes<()> {
    let n = args.len();
    if n < min || n > max {
        return type_err(if min == max {
            format!(
                "{name}() takes exactly {min} argument{} ({n} given)",
                if min == 1 { "" } else { "s" }
            )
        } else if n < min {
            format!(
                "{name}() takes at least {min} argument{} ({n} given)",
                if min == 1 { "" } else { "s" }
            )
        } else {
            format!("{name}() takes at most {max} arguments ({n} given)")
        });
    }
    Ok(())
}

fn want_str<'a>(name: &str, v: &'a Py, what: &str) -> PyRes<&'a str> {
    match v {
        Py::Str(s) => Ok(s),
        other => type_err(format!(
            "{name}() {what} must be str, not {}",
            other.type_name()
        )),
    }
}

fn int_of(v: &Py) -> PyRes<i64> {
    match v {
        Py::Int(i) => Ok(*i),
        Py::Bool(b) => Ok(*b as i64),
        // A slice-style bound beyond `i64` clamps.
        Py::BigInt(n) => Ok(if n.sign() == num_bigint::Sign::Minus {
            i64::MIN
        } else {
            i64::MAX
        }),
        other => type_err(format!(
            "'{}' object cannot be interpreted as an integer",
            other.type_name()
        )),
    }
}

fn opt_int(v: Option<&Py>) -> PyRes<Option<i64>> {
    match v {
        None | Some(Py::None) => Ok(None),
        Some(x) => int_of(x).map(Some),
    }
}

/// Resolve `start`/`end` slice arguments against a length (char counts).
fn clamp_range(start: Option<i64>, end: Option<i64>, len: i64) -> (i64, i64) {
    // `ADJUST_INDICES`: a start beyond the length stays (so the range is empty
    // and nothing matches, not even the empty string).
    let norm = |v: Option<i64>, d: i64, cap: bool| -> i64 {
        match v {
            None => d,
            Some(x) if x < 0 => (x + len).max(0),
            Some(x) if cap => x.min(len),
            Some(x) => x,
        }
    };
    (norm(start, 0, false), norm(end, len, true))
}

fn kwarg_or_pos(args: &[Py], kwargs: &Kwargs, pos: usize, name: &str) -> Option<Py> {
    args.get(pos).cloned().or_else(|| {
        kwargs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    })
}

/// Call a bound built-in method.
pub(crate) fn call_method(
    c: &mut dyn Caller,
    recv: &Py,
    name: &'static str,
    args: Vec<Py>,
    kwargs: Kwargs,
) -> PyRes<Py> {
    match recv {
        Py::Str(s) => str_method(s, name, args, kwargs),
        Py::List(l) => list_method(c, recv, l, name, args, kwargs),
        Py::Dict(d) => dict_method(recv, d, name, args, kwargs),
        Py::Set(s) => set_method(s, name, args),
        Py::Tuple(t) => seq_method(name, t.to_vec(), args),
        Py::Range(..) => seq_method(name, to_vec(recv)?, args),
        Py::Bytes(b) => bytes_method(b, name, args),
        Py::Int(_) | Py::BigInt(_) | Py::Bool(_) | Py::Float(_) | Py::Complex(..) => {
            num_method(recv, name)
        }
        Py::Exc(e) => match name {
            "with_traceback" => Ok(Py::Exc(e.clone())),
            _ => Ok(Py::None),
        },
        Py::Type("dict") => {
            // `dict.fromkeys(iterable, value=None)`.
            nargs(name, &args, 1, 2)?;
            let mut out = PyDict::new();
            for k in to_vec(&args[0])? {
                let h = hkey(&k)?;
                dict_set(&mut out, h, k, args.get(1).cloned().unwrap_or(Py::None));
            }
            Ok(Py::Dict(rf(out)))
        }
        Py::Type("str") => maketrans(args),
        _ => Err(exc("AttributeError", format!("no method {name}"))),
    }
}

/// `str.maketrans(x[, y[, z]])`.
fn maketrans(args: Vec<Py>) -> PyRes<Py> {
    let ord = |c: char| Py::Int(i64::from(u32::from(c)));
    let mut out = PyDict::new();
    let mut put = |k: Py, v: Py| -> PyRes<()> {
        let h = hkey(&k)?;
        dict_set(&mut out, h, k, v);
        Ok(())
    };
    match args.as_slice() {
        [Py::Dict(d)] => {
            let pairs: Vec<(Py, Py)> = d.borrow().values().cloned().collect();
            for (k, v) in pairs {
                let key = match &k {
                    Py::Str(s) => {
                        let mut chars = s.chars();
                        match (chars.next(), chars.next()) {
                            (Some(c), None) => ord(c),
                            _ => {
                                return value_err(
                                    "string keys in translate table must be of length 1",
                                );
                            }
                        }
                    }
                    Py::Int(_) | Py::Bool(_) => k.clone(),
                    _ => return type_err("keys in translate table must be strings or integers"),
                };
                put(key, v)?;
            }
        }
        [_] => return type_err("if you give only one argument to maketrans it must be a dict"),
        [Py::Str(x), Py::Str(y), rest @ ..] if rest.len() <= 1 => {
            let (xs, ys): (Vec<char>, Vec<char>) = (x.chars().collect(), y.chars().collect());
            if xs.len() != ys.len() {
                return value_err("the first two maketrans arguments must have equal length");
            }
            for (a, b) in xs.iter().zip(&ys) {
                put(ord(*a), ord(*b))?;
            }
            match rest {
                [] => {}
                [Py::Str(z)] => {
                    for c in z.chars() {
                        put(ord(c), Py::None)?;
                    }
                }
                _ => return type_err("maketrans() argument 3 must be str"),
            }
        }
        [_, _] | [_, _, _] => return type_err("maketrans() arguments must be str"),
        other => {
            return type_err(format!(
                "maketrans expected 1 to 3 arguments, got {}",
                other.len()
            ));
        }
    }
    Ok(Py::Dict(rf(out)))
}

/// `str.translate(table)`: `table[ord(c)]` for each character; a lookup error
/// leaves the character, `None` deletes it, an int or a string replaces it.
fn translate(s: &str, table: &Py) -> PyRes<String> {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        let key = Py::Int(i64::from(u32::from(c)));
        match getitem(table, &key) {
            Ok(Py::None) => {}
            Ok(Py::Str(r)) => out.push_str(&r),
            Ok(Py::Int(n)) => out.push(
                u32::try_from(n)
                    .ok()
                    .and_then(char::from_u32)
                    .ok_or_else(|| {
                        exc("TypeError", "character mapping must be in range(0x110000)")
                    })?,
            ),
            Ok(_) => return type_err("character mapping must return integer, None or str"),
            Err(e) if e.class == "LookupError" || exc_subclass(e.class, "LookupError") => {
                out.push(c);
            }
            Err(e) => return Err(e),
        }
    }
    Ok(out)
}

fn num_method(recv: &Py, name: &str) -> PyRes<Py> {
    let int_value = |v: &Py| match v {
        Py::Bool(b) => Some(BigInt::from(*b)),
        Py::Int(i) => Some(BigInt::from(*i)),
        Py::BigInt(n) => Some((**n).clone()),
        _ => None,
    };
    match (recv, name) {
        (Py::Complex(r, i), "conjugate") => Ok(Py::Complex(*r, -*i)),
        (Py::Int(_) | Py::BigInt(_) | Py::Bool(_), "conjugate") => {
            Ok(Py::from_big(int_value(recv).unwrap_or_default()))
        }
        (Py::Int(_) | Py::BigInt(_) | Py::Bool(_), "bit_length") => Ok(Py::from_big(BigInt::from(
            pynum::bit_length(&int_value(recv).unwrap_or_default()),
        ))),
        (Py::Int(_) | Py::BigInt(_) | Py::Bool(_), "bit_count") => Ok(Py::from_big(BigInt::from(
            pynum::bit_count(&int_value(recv).unwrap_or_default()),
        ))),
        (Py::Int(_) | Py::BigInt(_) | Py::Bool(_), "is_integer") => Ok(Py::Bool(true)),
        (Py::Int(_) | Py::BigInt(_) | Py::Bool(_), "as_integer_ratio") => Ok(Py::tuple(vec![
            Py::from_big(int_value(recv).unwrap_or_default()),
            Py::Int(1),
        ])),
        (Py::Float(f), "conjugate") => Ok(Py::Float(*f)),
        (Py::Float(f), "hex") => Ok(Py::str(float_hex(*f))),
        (Py::Float(f), "is_integer") => Ok(Py::Bool(f.is_finite() && f.fract() == 0.0)),
        (Py::Float(f), "as_integer_ratio") => {
            if f.is_nan() {
                return value_err("cannot convert NaN to integer ratio");
            }
            if f.is_infinite() {
                return overflow_err("cannot convert Infinity to integer ratio");
            }
            // The exact fraction: the mantissa over a power of two, in lowest terms.
            let bits = f.to_bits();
            let raw_exp = ((bits >> 52) & 0x7ff) as i32;
            let fraction = bits & ((1u64 << 52) - 1);
            if *f == 0.0 {
                return Ok(Py::tuple(vec![Py::Int(0), Py::Int(1)]));
            }
            let (mut mantissa, mut exp) = if raw_exp == 0 {
                (fraction, -1074)
            } else {
                (fraction | (1u64 << 52), raw_exp - 1075)
            };
            let trailing = mantissa.trailing_zeros() as i32;
            mantissa >>= trailing;
            exp += trailing;
            let numerator = BigInt::from(mantissa) * if *f < 0.0 { -1 } else { 1 };
            if exp >= 0 {
                Ok(Py::tuple(vec![
                    Py::from_big(numerator << exp as usize),
                    Py::Int(1),
                ]))
            } else {
                Ok(Py::tuple(vec![
                    Py::from_big(numerator),
                    Py::from_big(BigInt::from(1) << (-exp) as usize),
                ]))
            }
        }
        _ => Err(exc("AttributeError", format!("no method {name}"))),
    }
}

fn bytes_method(b: &Arc<[u8]>, name: &str, args: Vec<Py>) -> PyRes<Py> {
    match name {
        "decode" => String::from_utf8(b.to_vec())
            .map(Py::str)
            .map_err(|_| exc("UnicodeDecodeError", "'utf-8' codec can't decode bytes")),
        "hex" => Ok(Py::str(
            b.iter().map(|x| format!("{x:02x}")).collect::<String>(),
        )),
        "startswith" | "endswith" => {
            nargs(name, &args, 1, 1)?;
            let Py::Bytes(p) = &args[0] else {
                return type_err(format!(
                    "{name} first arg must be bytes or a tuple of bytes"
                ));
            };
            Ok(Py::Bool(if name == "startswith" {
                b.starts_with(p)
            } else {
                b.ends_with(p)
            }))
        }
        _ => Err(exc("AttributeError", format!("no method {name}"))),
    }
}

fn seq_method(name: &str, items: Vec<Py>, args: Vec<Py>) -> PyRes<Py> {
    match name {
        "count" => {
            nargs(name, &args, 1, 1)?;
            Ok(Py::Int(
                items
                    .iter()
                    .filter(|x| x.is(&args[0]) || py_eq(x, &args[0]))
                    .count() as i64,
            ))
        }
        "index" => {
            nargs(name, &args, 1, 3)?;
            let len = items.len() as i64;
            let (s, e) = clamp_range(opt_int(args.get(1))?, opt_int(args.get(2))?, len);
            for i in s..e {
                if items[i as usize].is(&args[0]) || py_eq(&items[i as usize], &args[0]) {
                    return Ok(Py::Int(i));
                }
            }
            value_err(format!("{} is not in list", repr(&args[0])))
        }
        _ => Err(exc("AttributeError", format!("no method {name}"))),
    }
}

fn list_method(
    c: &mut dyn Caller,
    recv: &Py,
    l: &Rf<Vec<Py>>,
    name: &str,
    args: Vec<Py>,
    kwargs: Kwargs,
) -> PyRes<Py> {
    match name {
        "append" => {
            nargs(name, &args, 1, 1)?;
            l.borrow_mut().push(args[0].clone());
            Ok(Py::None)
        }
        "clear" => {
            nargs(name, &args, 0, 0)?;
            l.borrow_mut().clear();
            Ok(Py::None)
        }
        "copy" => {
            nargs(name, &args, 0, 0)?;
            Ok(Py::list(l.borrow().clone()))
        }
        "extend" => {
            nargs(name, &args, 1, 1)?;
            let items = to_vec(&args[0])?;
            l.borrow_mut().extend(items);
            Ok(Py::None)
        }
        "insert" => {
            nargs(name, &args, 2, 2)?;
            let i = int_of(&args[0])?;
            let mut v = l.borrow_mut();
            let len = v.len() as i64;
            let idx = if i < 0 { (i + len).max(0) } else { i.min(len) };
            v.insert(idx as usize, args[1].clone());
            Ok(Py::None)
        }
        "pop" => {
            nargs(name, &args, 0, 1)?;
            let mut v = l.borrow_mut();
            if v.is_empty() {
                return Err(exc("IndexError", "pop from empty list"));
            }
            let i = match args.first() {
                Some(x) => int_of(x)?,
                None => -1,
            };
            let idx = if i < 0 { i + v.len() as i64 } else { i };
            if idx < 0 || idx >= v.len() as i64 {
                return Err(exc("IndexError", "pop index out of range"));
            }
            Ok(v.remove(idx as usize))
        }
        "remove" => {
            nargs(name, &args, 1, 1)?;
            let pos = {
                let v = l.borrow().clone();
                v.iter().position(|x| x.is(&args[0]) || py_eq(x, &args[0]))
            };
            match pos {
                Some(i) => {
                    l.borrow_mut().remove(i);
                    Ok(Py::None)
                }
                None => value_err("list.remove(x): x not in list"),
            }
        }
        "reverse" => {
            nargs(name, &args, 0, 0)?;
            l.borrow_mut().reverse();
            Ok(Py::None)
        }
        "sort" => {
            nargs(name, &args, 0, 0)?;
            for (k, _) in &kwargs {
                if k != "key" && k != "reverse" {
                    return type_err(format!("sort() got an unexpected keyword argument '{k}'"));
                }
            }
            let items = l.borrow().clone();
            let key = kwargs
                .iter()
                .find(|(k, _)| k == "key")
                .map(|(_, v)| v.clone());
            let rev = kwargs
                .iter()
                .find(|(k, _)| k == "reverse")
                .map(|(_, v)| v.clone());
            let sorted = sort_items(c, items, key.as_ref(), rev.as_ref())?;
            *l.borrow_mut() = sorted;
            Ok(Py::None)
        }
        "count" | "index" => seq_method(name, l.borrow().clone(), args),
        _ => {
            let _ = recv;
            Err(exc("AttributeError", format!("no method {name}")))
        }
    }
}

fn dict_method(recv: &Py, d: &Rf<PyDict>, name: &str, args: Vec<Py>, kwargs: Kwargs) -> PyRes<Py> {
    let view = |kind: &'static str, items: Vec<Py>| {
        Py::Iter(rf(PyIter {
            kind,
            items: items.into(),
        }))
    };
    match name {
        "clear" => {
            nargs(name, &args, 0, 0)?;
            d.borrow_mut().clear();
            Ok(Py::None)
        }
        "copy" => {
            nargs(name, &args, 0, 0)?;
            Ok(Py::Dict(rf(d.borrow().clone())))
        }
        "get" => {
            nargs(name, &args, 1, 2)?;
            let k = hkey(&args[0])?;
            Ok(d.borrow()
                .get(&k)
                .map(|(_, v)| v.clone())
                .unwrap_or_else(|| args.get(1).cloned().unwrap_or(Py::None)))
        }
        "items" => {
            nargs(name, &args, 0, 0)?;
            Ok(view(
                "dict_items",
                d.borrow()
                    .values()
                    .map(|(k, v)| Py::tuple(vec![k.clone(), v.clone()]))
                    .collect(),
            ))
        }
        "keys" => {
            nargs(name, &args, 0, 0)?;
            Ok(view(
                "dict_keys",
                d.borrow().values().map(|(k, _)| k.clone()).collect(),
            ))
        }
        "values" => {
            nargs(name, &args, 0, 0)?;
            Ok(view(
                "dict_values",
                d.borrow().values().map(|(_, v)| v.clone()).collect(),
            ))
        }
        "pop" => {
            nargs(name, &args, 1, 2)?;
            let k = hkey(&args[0])?;
            match d.borrow_mut().shift_remove(&k) {
                Some((_, v)) => Ok(v),
                None => match args.get(1) {
                    Some(dflt) => Ok(dflt.clone()),
                    None => Err(Box::new(ExcObj {
                        class: "KeyError",
                        args: vec![args[0].clone()],
                    })),
                },
            }
        }
        "popitem" => {
            nargs(name, &args, 0, 0)?;
            match d.borrow_mut().pop() {
                Some((_, (k, v))) => Ok(Py::tuple(vec![k, v])),
                None => Err(exc("KeyError", "popitem(): dictionary is empty")),
            }
        }
        "setdefault" => {
            nargs(name, &args, 1, 2)?;
            let k = hkey(&args[0])?;
            let dflt = args.get(1).cloned().unwrap_or(Py::None);
            let mut m = d.borrow_mut();
            Ok(m.entry(k).or_insert((args[0].clone(), dflt)).1.clone())
        }
        "update" => {
            nargs(name, &args, 0, 1)?;
            if let Some(src) = args.first() {
                let tmp = super::funcs::call_type_dict(vec![src.clone()], vec![])?;
                if let Py::Dict(o) = tmp {
                    for (k, v) in o.borrow().iter() {
                        d.borrow_mut().insert(k.clone(), v.clone());
                    }
                }
            }
            for (k, v) in kwargs {
                dict_set(
                    &mut d.borrow_mut(),
                    HKey::Str(k.as_str().into()),
                    Py::str(k.as_str()),
                    v,
                );
            }
            Ok(Py::None)
        }
        "fromkeys" => {
            nargs(name, &args, 1, 2)?;
            let mut out = PyDict::new();
            for k in to_vec(&args[0])? {
                {
                    let h = hkey(&k)?;
                    dict_set(&mut out, h, k, args.get(1).cloned().unwrap_or(Py::None));
                }
            }
            Ok(Py::Dict(rf(out)))
        }
        _ => {
            let _ = recv;
            Err(exc("AttributeError", format!("no method {name}")))
        }
    }
}

/// How a set method's argument is seen: a set (iterated and probed as such) or
/// the elements of any other iterable.
enum SetArg {
    Set(Rf<PySet>),
    Items(Vec<Py>),
}

fn set_arg(v: &Py) -> PyRes<SetArg> {
    Ok(match v {
        Py::Set(s) => SetArg::Set(s.clone()),
        other => SetArg::Items(to_vec(other)?),
    })
}

fn type_error(message: String) -> Box<ExcObj> {
    exc("TypeError", message)
}

fn set_method(s: &Rf<PySet>, name: &str, args: Vec<Py>) -> PyRes<Py> {
    match name {
        "add" => {
            nargs(name, &args, 1, 1)?;
            s.borrow_mut().add(args[0].clone()).map_err(type_error)?;
            Ok(Py::None)
        }
        "clear" => {
            s.borrow_mut().clear();
            Ok(Py::None)
        }
        "copy" => Ok(Py::Set(rf(s.borrow().copy()))),
        "discard" | "remove" => {
            nargs(name, &args, 1, 1)?;
            let had = s.borrow_mut().discard(&args[0]).map_err(type_error)?;
            if !had && name == "remove" {
                return Err(Box::new(ExcObj {
                    class: "KeyError",
                    args: vec![args[0].clone()],
                }));
            }
            Ok(Py::None)
        }
        "pop" => match s.borrow_mut().pop() {
            Some(v) => Ok(v),
            None => Err(exc("KeyError", "pop from an empty set")),
        },
        "union" => {
            let mut out = s.borrow().copy();
            for a in &args {
                match set_arg(a)? {
                    SetArg::Set(o) if Rc::ptr_eq(s, &o) => {}
                    SetArg::Set(o) => out.merge(&o.borrow()),
                    SetArg::Items(items) => out.update_items(items).map_err(type_error)?,
                }
            }
            Ok(Py::Set(rf(out)))
        }
        "update" => {
            for a in &args {
                match set_arg(a)? {
                    SetArg::Set(o) if Rc::ptr_eq(s, &o) => {}
                    SetArg::Set(o) => s.borrow_mut().merge(&o.borrow()),
                    SetArg::Items(items) => {
                        s.borrow_mut().update_items(items).map_err(type_error)?
                    }
                }
            }
            Ok(Py::None)
        }
        "intersection" | "intersection_update" => {
            // `set_intersection_multi`: the first argument meets `s` itself, each
            // further one the running result.
            let mut out: Option<PySet> = None;
            for a in &args {
                let next = {
                    let borrowed = s.borrow();
                    let left: &PySet = out.as_ref().unwrap_or(&borrowed);
                    match set_arg(a)? {
                        SetArg::Set(o) if out.is_none() && Rc::ptr_eq(s, &o) => left.copy(),
                        SetArg::Set(o) => left.intersection(&o.borrow()),
                        SetArg::Items(items) => {
                            left.intersection_items(items).map_err(type_error)?
                        }
                    }
                };
                out = Some(next);
            }
            let out = out.unwrap_or_else(|| s.borrow().copy());
            if name.ends_with("update") {
                *s.borrow_mut() = out;
                Ok(Py::None)
            } else {
                Ok(Py::Set(rf(out)))
            }
        }
        "difference" => {
            // `set_difference_multi`: the first argument is subtracted from `s`
            // itself, the rest are removed from the result in place.
            let mut out: Option<PySet> = None;
            for a in &args {
                let arg = set_arg(a)?;
                match out.as_mut() {
                    None => {
                        let borrowed = s.borrow();
                        out = Some(match arg {
                            SetArg::Set(o) if Rc::ptr_eq(s, &o) => PySet::new(),
                            SetArg::Set(o) => borrowed.difference(&o.borrow()),
                            SetArg::Items(items) => {
                                borrowed.difference_items(items).map_err(type_error)?
                            }
                        });
                    }
                    Some(result) => match arg {
                        SetArg::Set(o) => result.difference_update(&o.borrow()),
                        SetArg::Items(items) => {
                            result.difference_update_items(items).map_err(type_error)?;
                        }
                    },
                }
            }
            Ok(Py::Set(rf(out.unwrap_or_else(|| s.borrow().copy()))))
        }
        "difference_update" => {
            for a in &args {
                match set_arg(a)? {
                    SetArg::Set(o) if Rc::ptr_eq(s, &o) => s.borrow_mut().clear(),
                    SetArg::Set(o) => s.borrow_mut().difference_update(&o.borrow()),
                    SetArg::Items(items) => {
                        s.borrow_mut()
                            .difference_update_items(items)
                            .map_err(type_error)?;
                    }
                }
            }
            Ok(Py::None)
        }
        "symmetric_difference" | "symmetric_difference_update" => {
            nargs(name, &args, 1, 1)?;
            let out = match set_arg(&args[0])? {
                SetArg::Set(o) if Rc::ptr_eq(s, &o) => PySet::new(),
                SetArg::Set(o) => s.borrow().symmetric_difference(&o.borrow()),
                SetArg::Items(items) => s
                    .borrow()
                    .symmetric_difference_items(items)
                    .map_err(type_error)?,
            };
            if name.ends_with("update") {
                *s.borrow_mut() = out;
                Ok(Py::None)
            } else {
                Ok(Py::Set(rf(out)))
            }
        }
        "isdisjoint" | "issubset" | "issuperset" => {
            nargs(name, &args, 1, 1)?;
            let other = match set_arg(&args[0])? {
                SetArg::Set(o) => o.borrow().clone(),
                SetArg::Items(items) => PySet::from_items(items).map_err(type_error)?,
            };
            let cur = s.borrow();
            Ok(Py::Bool(match name {
                "isdisjoint" => cur.is_disjoint(&other),
                "issubset" => cur.is_subset(&other),
                _ => other.is_subset(&cur),
            }))
        }
        _ => Err(exc("AttributeError", format!("no method {name}"))),
    }
}

// --- str ---------------------------------------------------------------------

fn char_find(hay: &[char], needle: &[char], start: usize, end: usize, rev: bool) -> Option<usize> {
    if needle.len() > end.saturating_sub(start) {
        return None;
    }
    let last = end - needle.len();
    if rev {
        (start..=last)
            .rev()
            .find(|&i| hay[i..i + needle.len()] == *needle)
    } else {
        (start..=last).find(|&i| hay[i..i + needle.len()] == *needle)
    }
}

fn is_printable(c: char) -> bool {
    !(c.is_control() || (c.is_whitespace() && c != ' '))
}

fn str_method(s: &Arc<str>, name: &'static str, args: Vec<Py>, kwargs: Kwargs) -> PyRes<Py> {
    let sv: &str = s;
    let st = |x: String| Ok(Py::str(x));
    match name {
        "upper" => {
            nargs(name, &args, 0, 0)?;
            st(pystr::upper(sv))
        }
        "lower" | "casefold" => {
            nargs(name, &args, 0, 0)?;
            st(pystr::lower(sv))
        }
        "title" => {
            nargs(name, &args, 0, 0)?;
            st(pystr::title(sv))
        }
        "capitalize" => {
            nargs(name, &args, 0, 0)?;
            st(pystr::capitalize(sv))
        }
        "swapcase" => {
            nargs(name, &args, 0, 0)?;
            st(sv
                .chars()
                .flat_map(|c| {
                    if c.is_uppercase() {
                        c.to_lowercase().collect::<Vec<_>>()
                    } else if c.is_lowercase() {
                        c.to_uppercase().collect::<Vec<_>>()
                    } else {
                        vec![c]
                    }
                })
                .collect())
        }
        "strip" | "lstrip" | "rstrip" => {
            nargs(name, &args, 0, 1)?;
            let chars = match args.first() {
                None | Some(Py::None) => None,
                Some(Py::Str(c)) => Some(c.to_string()),
                Some(other) => {
                    return type_err(format!(
                        "{name} arg must be None or str, not {}",
                        other.type_name()
                    ));
                }
            };
            st(pystr::strip(sv, chars.as_deref(), name != "rstrip", name != "lstrip").to_string())
        }
        "split" | "rsplit" => {
            nargs(name, &args, 0, 2)?;
            let sep = kwarg_or_pos(&args, &kwargs, 0, "sep");
            let maxsplit = match kwarg_or_pos(&args, &kwargs, 1, "maxsplit") {
                Some(v) => int_of(&v)?,
                None => -1,
            };
            let parts: Vec<String> = match sep {
                None | Some(Py::None) => {
                    if name == "split" {
                        pystr::split_whitespace(sv, maxsplit)
                    } else {
                        let rev: String = sv.chars().rev().collect();
                        let mut p = pystr::split_whitespace(&rev, maxsplit);
                        p.reverse();
                        p.into_iter().map(|x| x.chars().rev().collect()).collect()
                    }
                }
                Some(Py::Str(sep)) => {
                    if sep.is_empty() {
                        return value_err("empty separator");
                    }
                    if name == "split" {
                        pystr::split_sep(sv, &sep, maxsplit).unwrap_or_default()
                    } else if maxsplit < 0 {
                        sv.split(&*sep).map(str::to_string).collect()
                    } else {
                        let mut p: Vec<String> = sv
                            .rsplitn(maxsplit as usize + 1, &*sep)
                            .map(str::to_string)
                            .collect();
                        p.reverse();
                        p
                    }
                }
                Some(other) => {
                    return type_err(format!("must be str or None, not {}", other.type_name()));
                }
            };
            Ok(Py::list(parts.into_iter().map(Py::str).collect()))
        }
        "splitlines" => {
            nargs(name, &args, 0, 1)?;
            let keep = args.first().is_some_and(Py::truthy);
            let mut out = Vec::new();
            let mut cur = String::new();
            let mut chars = sv.chars().peekable();
            while let Some(c) = chars.next() {
                if matches!(
                    c,
                    '\n' | '\r'
                        | '\x0b'
                        | '\x0c'
                        | '\x1c'
                        | '\x1d'
                        | '\x1e'
                        | '\u{85}'
                        | '\u{2028}'
                        | '\u{2029}'
                ) {
                    let mut eol = c.to_string();
                    if c == '\r' && chars.peek() == Some(&'\n') {
                        eol.push('\n');
                        chars.next();
                    }
                    if keep {
                        cur.push_str(&eol);
                    }
                    out.push(Py::str(std::mem::take(&mut cur)));
                } else {
                    cur.push(c);
                }
            }
            if !cur.is_empty() {
                out.push(Py::str(cur));
            }
            Ok(Py::list(out))
        }
        "join" => {
            nargs(name, &args, 1, 1)?;
            let items = to_vec(&args[0])?;
            let mut out = String::new();
            for (i, x) in items.iter().enumerate() {
                match x {
                    Py::Str(p) => {
                        if i > 0 {
                            out.push_str(sv);
                        }
                        out.push_str(p);
                    }
                    other => {
                        return type_err(format!(
                            "sequence item {i}: expected str instance, {} found",
                            other.type_name()
                        ));
                    }
                }
            }
            st(out)
        }
        "replace" => {
            nargs(name, &args, 2, 3)?;
            let old = want_str(name, &args[0], "argument 1")?;
            let new = want_str(name, &args[1], "argument 2")?;
            let count = match args.get(2) {
                Some(v) => int_of(v)?,
                None => kwargs
                    .iter()
                    .find(|(k, _)| k == "count")
                    .map(|(_, v)| int_of(v))
                    .transpose()?
                    .unwrap_or(-1),
            };
            if count < 0 {
                st(sv.replace(old, new))
            } else {
                st(sv.replacen(old, new, count as usize))
            }
        }
        "startswith" | "endswith" => {
            nargs(name, &args, 1, 3)?;
            let chars: Vec<char> = sv.chars().collect();
            let (a, b) = clamp_range(
                opt_int(args.get(1))?,
                opt_int(args.get(2))?,
                chars.len() as i64,
            );
            let hay: String = if a <= b {
                chars[a as usize..b as usize].iter().collect()
            } else {
                String::new()
            };
            let test = |p: &Py| -> PyRes<bool> {
                match p {
                    Py::Str(_) if a > b => Ok(false),
                    Py::Str(p) => Ok(if name == "startswith" {
                        hay.starts_with(&**p)
                    } else {
                        hay.ends_with(&**p)
                    }),
                    other => type_err(format!(
                        "tuple for {name} must only contain str, not {}",
                        other.type_name()
                    )),
                }
            };
            match &args[0] {
                Py::Tuple(t) => {
                    for p in t.iter() {
                        if test(p)? {
                            return Ok(Py::Bool(true));
                        }
                    }
                    Ok(Py::Bool(false))
                }
                Py::Str(_) => Ok(Py::Bool(test(&args[0])?)),
                other => type_err(format!(
                    "{name} first arg must be str or a tuple of str, not {}",
                    other.type_name()
                )),
            }
        }
        "find" | "rfind" | "index" | "rindex" => {
            nargs(name, &args, 1, 3)?;
            let needle: Vec<char> = want_str(name, &args[0], "argument")?.chars().collect();
            let hay: Vec<char> = sv.chars().collect();
            let (a, b) = clamp_range(
                opt_int(args.get(1))?,
                opt_int(args.get(2))?,
                hay.len() as i64,
            );
            let r = if a > b {
                None
            } else {
                char_find(&hay, &needle, a as usize, b as usize, name.starts_with('r'))
            };
            match (r, name.ends_with("index")) {
                (Some(i), _) => Ok(Py::Int(i as i64)),
                (None, false) => Ok(Py::Int(-1)),
                (None, true) => value_err("substring not found"),
            }
        }
        "count" => {
            nargs(name, &args, 1, 3)?;
            let needle: Vec<char> = want_str(name, &args[0], "argument")?.chars().collect();
            let hay: Vec<char> = sv.chars().collect();
            let (a, b) = clamp_range(
                opt_int(args.get(1))?,
                opt_int(args.get(2))?,
                hay.len() as i64,
            );
            if a > b {
                return Ok(Py::Int(0));
            }
            if needle.is_empty() {
                return Ok(Py::Int(b - a + 1));
            }
            let (mut i, mut n) = (a as usize, 0);
            while let Some(p) = char_find(&hay, &needle, i, b as usize, false) {
                n += 1;
                i = p + needle.len();
            }
            Ok(Py::Int(n))
        }
        "center" | "ljust" | "rjust" => {
            nargs(name, &args, 1, 2)?;
            let w = int_of(&args[0])?;
            let fill = match args.get(1) {
                None => ' ',
                Some(Py::Str(f)) if f.chars().count() == 1 => f.chars().next().unwrap_or(' '),
                Some(_) => {
                    return type_err("The fill character must be exactly one character long");
                }
            };
            let len = sv.chars().count() as i64;
            if w <= len {
                return Ok(Py::Str(s.clone()));
            }
            let total = (w - len) as usize;
            let (l, r) = match name {
                "ljust" => (0, total),
                "rjust" => (total, 0),
                _ => {
                    let left = total / 2 + (total & (w as usize) & 1);
                    (left, total - left)
                }
            };
            st(format!(
                "{}{sv}{}",
                fill.to_string().repeat(l),
                fill.to_string().repeat(r)
            ))
        }
        "maketrans" => maketrans(args),
        "translate" => {
            nargs(name, &args, 1, 1)?;
            st(translate(sv, &args[0])?)
        }
        "zfill" => {
            nargs(name, &args, 1, 1)?;
            let w = int_of(&args[0])?;
            let len = sv.chars().count() as i64;
            if w <= len {
                return Ok(Py::Str(s.clone()));
            }
            let (sign, rest) = match sv.chars().next() {
                Some(c @ ('+' | '-')) => (c.to_string(), &sv[1..]),
                _ => (String::new(), sv),
            };
            st(format!("{sign}{}{rest}", "0".repeat((w - len) as usize)))
        }
        "partition" | "rpartition" => {
            nargs(name, &args, 1, 1)?;
            let sep = want_str(name, &args[0], "argument")?;
            if sep.is_empty() {
                return value_err("empty separator");
            }
            let found = if name == "partition" {
                sv.find(sep)
            } else {
                sv.rfind(sep)
            };
            Ok(match found {
                Some(i) => Py::tuple(vec![
                    Py::str(&sv[..i]),
                    Py::str(sep),
                    Py::str(&sv[i + sep.len()..]),
                ]),
                None if name == "partition" => {
                    Py::tuple(vec![Py::Str(s.clone()), Py::str(""), Py::str("")])
                }
                None => Py::tuple(vec![Py::str(""), Py::str(""), Py::Str(s.clone())]),
            })
        }
        "removeprefix" | "removesuffix" => {
            nargs(name, &args, 1, 1)?;
            let p = want_str(name, &args[0], "argument")?;
            st(if name == "removeprefix" {
                sv.strip_prefix(p).unwrap_or(sv).to_string()
            } else {
                sv.strip_suffix(p).unwrap_or(sv).to_string()
            })
        }
        "isalpha" | "isalnum" | "isdigit" | "isdecimal" | "isnumeric" | "isascii" | "isspace"
        | "isupper" | "islower" | "istitle" | "isidentifier" | "isprintable" => {
            nargs(name, &args, 0, 0)?;
            let r = match name {
                "isalpha" => !sv.is_empty() && sv.chars().all(char::is_alphabetic),
                "isalnum" => !sv.is_empty() && sv.chars().all(char::is_alphanumeric),
                "isdigit" | "isdecimal" => {
                    !sv.is_empty() && sv.chars().all(pystr::is_decimal_digit)
                }
                "isnumeric" => !sv.is_empty() && sv.chars().all(char::is_numeric),
                "isascii" => sv.is_ascii(),
                "isspace" => !sv.is_empty() && sv.chars().all(pystr::is_py_space),
                "isupper" => {
                    sv.chars().any(char::is_uppercase) && !sv.chars().any(char::is_lowercase)
                }
                "islower" => {
                    sv.chars().any(char::is_lowercase) && !sv.chars().any(char::is_uppercase)
                }
                "istitle" => {
                    let mut prev_cased = false;
                    let mut any = false;
                    let mut ok = true;
                    for c in sv.chars() {
                        if c.is_uppercase() {
                            if prev_cased {
                                ok = false;
                            }
                            prev_cased = true;
                            any = true;
                        } else if c.is_lowercase() {
                            if !prev_cased {
                                ok = false;
                            }
                            prev_cased = true;
                            any = true;
                        } else {
                            prev_cased = false;
                        }
                    }
                    any && ok
                }
                "isidentifier" => {
                    let mut it = sv.chars();
                    it.next().is_some_and(super::lex::is_id_start)
                        && it.all(super::lex::is_id_continue)
                }
                _ => sv.chars().all(is_printable),
            };
            Ok(Py::Bool(r))
        }
        "encode" => {
            nargs(name, &args, 0, 2)?;
            let enc = kwarg_or_pos(&args, &kwargs, 0, "encoding");
            if let Some(e) = &enc {
                let Py::Str(e) = e else {
                    return type_err(format!(
                        "encode() argument 'encoding' must be str, not {}",
                        e.type_name()
                    ));
                };
                let norm = e.to_ascii_lowercase().replace('_', "-");
                if !matches!(norm.as_str(), "utf-8" | "utf8" | "u8") {
                    return Err(exc("LookupError", format!("unknown encoding: {e}")));
                }
            }
            if let Some(e) = kwarg_or_pos(&args, &kwargs, 1, "errors")
                && !matches!(e, Py::Str(_))
            {
                return type_err(format!(
                    "encode() argument 'errors' must be str, not {}",
                    e.type_name()
                ));
            }
            Ok(Py::Bytes(sv.as_bytes().to_vec().into()))
        }
        "expandtabs" => {
            nargs(name, &args, 0, 1)?;
            let tab = match kwarg_or_pos(&args, &kwargs, 0, "tabsize").as_ref() {
                Some(v) => int_of(v)?.max(0) as usize,
                None => 8,
            };
            let mut out = String::new();
            let mut col = 0usize;
            for c in sv.chars() {
                match c {
                    '\t' => {
                        if tab > 0 {
                            let n = tab - col % tab;
                            out.push_str(&" ".repeat(n));
                            col += n;
                        }
                    }
                    '\n' | '\r' => {
                        out.push(c);
                        col = 0;
                    }
                    _ => {
                        out.push(c);
                        col += 1;
                    }
                }
            }
            st(out)
        }
        _ => Err(exc(
            "NotImplementedError",
            format!("str.{name} is not supported"),
        )),
    }
}

#[allow(dead_code)]
fn _unused() {
    let _ = py_round;
}

/// `float.hex()`.
fn float_hex(x: f64) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf" } else { "-inf" }.into();
    }
    let sign = if x.is_sign_negative() { "-" } else { "" };
    if x == 0.0 {
        return format!("{sign}0x0.0p+0");
    }
    let bits = x.to_bits();
    let exp = ((bits >> 52) & 0x7ff) as i32;
    let frac = bits & ((1u64 << 52) - 1);
    let (lead, e) = if exp == 0 {
        (0, -1022)
    } else {
        (1, exp - 1023)
    };
    format!(
        "{sign}0x{lead}.{frac:013x}p{}{}",
        if e < 0 { "-" } else { "+" },
        e.abs()
    )
}
