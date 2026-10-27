//! Attribute access and the methods of Python's built-in value types.

use std::rc::Rc;
use std::sync::Arc;

use super::funcs::{Caller, Kwargs, py_round, sort_items};
use super::ops::*;
use super::value::*;
use crate::expr::pystr;

const STR_METHODS: &[&str] = &[
    "capitalize", "casefold", "center", "count", "encode", "endswith", "expandtabs", "find",
    "format", "format_map", "index", "isalnum", "isalpha", "isascii", "isdecimal", "isdigit",
    "isidentifier", "islower", "isnumeric", "isprintable", "isspace", "istitle", "isupper",
    "join", "ljust", "lower", "lstrip", "maketrans", "partition", "removeprefix", "removesuffix",
    "replace", "rfind", "rindex", "rjust", "rpartition", "rsplit", "rstrip", "split", "splitlines",
    "startswith", "strip", "swapcase", "title", "translate", "upper", "zfill",
];
const LIST_METHODS: &[&str] = &[
    "append", "clear", "copy", "count", "extend", "index", "insert", "pop", "remove", "reverse",
    "sort",
];
const DICT_METHODS: &[&str] = &[
    "clear", "copy", "fromkeys", "get", "items", "keys", "pop", "popitem", "setdefault", "update",
    "values",
];
const SET_METHODS: &[&str] = &[
    "add", "clear", "copy", "difference", "difference_update", "discard", "intersection",
    "intersection_update", "isdisjoint", "issubset", "issuperset", "pop", "remove",
    "symmetric_difference", "symmetric_difference_update", "union", "update",
];
const TUPLE_METHODS: &[&str] = &["count", "index"];
const BYTES_METHODS: &[&str] = &["decode", "hex", "startswith", "endswith"];
const INT_METHODS: &[&str] = &[
    "as_integer_ratio", "bit_count", "bit_length", "conjugate", "is_integer",
];
const FLOAT_METHODS: &[&str] = &["as_integer_ratio", "conjugate", "is_integer"];
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
        Py::Int(_) | Py::Bool(_) => INT_METHODS,
        Py::Float(_) => FLOAT_METHODS,
        Py::Range(..) => RANGE_METHODS,
        Py::Exc(_) => EXC_METHODS,
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
        (Py::Int(_) | Py::Bool(_), "real" | "numerator") => {
            return Ok(Py::Int(match obj {
                Py::Int(i) => *i,
                Py::Bool(b) => *b as i64,
                _ => 0,
            }));
        }
        (Py::Int(_) | Py::Bool(_), "imag") => return Ok(Py::Int(0)),
        (Py::Int(_) | Py::Bool(_), "denominator") => return Ok(Py::Int(1)),
        (Py::Float(_), "real") => return Ok(obj.clone()),
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
            format!("'{}' object has no attribute '{name}' and no __dict__ for setting new attributes", other.type_name()),
        )),
    }
}

fn nargs(name: &str, args: &[Py], min: usize, max: usize) -> PyRes<()> {
    let n = args.len();
    if n < min || n > max {
        return type_err(if min == max {
            format!("{name}() takes exactly {min} argument{} ({n} given)", if min == 1 { "" } else { "s" })
        } else if n < min {
            format!("{name}() takes at least {min} argument{} ({n} given)", if min == 1 { "" } else { "s" })
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
    let norm = |v: Option<i64>, d: i64| -> i64 {
        match v {
            None => d,
            Some(x) if x < 0 => (x + len).max(0),
            Some(x) => x.min(len),
        }
    };
    (norm(start, 0), norm(end, len))
}

fn kwarg_or_pos(args: &[Py], kwargs: &Kwargs, pos: usize, name: &str) -> Option<Py> {
    args.get(pos)
        .cloned()
        .or_else(|| kwargs.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()))
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
        Py::Int(_) | Py::Bool(_) | Py::Float(_) => num_method(recv, name),
        Py::Exc(e) => match name {
            "with_traceback" => Ok(Py::Exc(e.clone())),
            _ => Ok(Py::None),
        },
        _ => Err(exc("AttributeError", format!("no method {name}"))),
    }
}

fn num_method(recv: &Py, name: &str) -> PyRes<Py> {
    match (recv, name) {
        (Py::Int(_) | Py::Bool(_), "conjugate") => Ok(Py::Int(int_of(recv)?)),
        (Py::Int(_) | Py::Bool(_), "bit_length") => {
            let i = int_of(recv)?;
            Ok(Py::Int(64 - i.unsigned_abs().leading_zeros() as i64))
        }
        (Py::Int(_) | Py::Bool(_), "bit_count") => Ok(Py::Int(int_of(recv)?.unsigned_abs().count_ones() as i64)),
        (Py::Int(_) | Py::Bool(_), "is_integer") => Ok(Py::Bool(true)),
        (Py::Int(_) | Py::Bool(_), "as_integer_ratio") => Ok(Py::tuple(vec![Py::Int(int_of(recv)?), Py::Int(1)])),
        (Py::Float(f), "conjugate") => Ok(Py::Float(*f)),
        (Py::Float(f), "is_integer") => Ok(Py::Bool(f.is_finite() && f.fract() == 0.0)),
        (Py::Float(f), "as_integer_ratio") => {
            if f.is_nan() {
                return value_err("cannot convert NaN to integer ratio");
            }
            if f.is_infinite() {
                return overflow_err("cannot convert Infinity to integer ratio");
            }
            let (mut num, mut den) = (*f, 1i64);
            let mut k = 0;
            while num.fract() != 0.0 && k < 1100 {
                num *= 2.0;
                den = den.checked_mul(2).map_or_else(
                    || overflow_err("integer result out of range (big integers are not supported)"),
                    Ok,
                )?;
                k += 1;
            }
            Ok(Py::tuple(vec![Py::Int(num as i64), Py::Int(den)]))
        }
        _ => Err(exc("AttributeError", format!("no method {name}"))),
    }
}

fn bytes_method(b: &Arc<[u8]>, name: &str, args: Vec<Py>) -> PyRes<Py> {
    match name {
        "decode" => String::from_utf8(b.to_vec())
            .map(Py::str)
            .map_err(|_| exc("UnicodeDecodeError", "'utf-8' codec can't decode bytes")),
        "hex" => Ok(Py::str(b.iter().map(|x| format!("{x:02x}")).collect::<String>())),
        "startswith" | "endswith" => {
            nargs(name, &args, 1, 1)?;
            let Py::Bytes(p) = &args[0] else {
                return type_err(format!("{name} first arg must be bytes or a tuple of bytes"));
            };
            Ok(Py::Bool(if name == "startswith" { b.starts_with(p) } else { b.ends_with(p) }))
        }
        _ => Err(exc("AttributeError", format!("no method {name}"))),
    }
}

fn seq_method(name: &str, items: Vec<Py>, args: Vec<Py>) -> PyRes<Py> {
    match name {
        "count" => {
            nargs(name, &args, 1, 1)?;
            Ok(Py::Int(items.iter().filter(|x| x.is(&args[0]) || py_eq(x, &args[0])).count() as i64))
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
            let key = kwargs.iter().find(|(k, _)| k == "key").map(|(_, v)| v.clone());
            let rev = kwargs.iter().find(|(k, _)| k == "reverse").map(|(_, v)| v.clone());
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
            Ok(d.borrow().get(&k).map(|(_, v)| v.clone()).unwrap_or_else(|| args.get(1).cloned().unwrap_or(Py::None)))
        }
        "items" => {
            nargs(name, &args, 0, 0)?;
            Ok(view(
                "dict_items",
                d.borrow().values().map(|(k, v)| Py::tuple(vec![k.clone(), v.clone()])).collect(),
            ))
        }
        "keys" => {
            nargs(name, &args, 0, 0)?;
            Ok(view("dict_keys", d.borrow().values().map(|(k, _)| k.clone()).collect()))
        }
        "values" => {
            nargs(name, &args, 0, 0)?;
            Ok(view("dict_values", d.borrow().values().map(|(_, v)| v.clone()).collect()))
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
                d.borrow_mut().insert(HKey::Str(k.as_str().into()), (Py::str(k.as_str()), v));
            }
            Ok(Py::None)
        }
        "fromkeys" => {
            nargs(name, &args, 1, 2)?;
            let mut out = PyDict::new();
            for k in to_vec(&args[0])? {
                out.insert(hkey(&k)?, (k, args.get(1).cloned().unwrap_or(Py::None)));
            }
            Ok(Py::Dict(rf(out)))
        }
        _ => {
            let _ = recv;
            Err(exc("AttributeError", format!("no method {name}")))
        }
    }
}

fn set_items(v: &Py) -> PyRes<PySet> {
    let mut out = PySet::new();
    match v {
        Py::Set(s) => return Ok(s.borrow().clone()),
        other => {
            for x in to_vec(other)? {
                out.insert(hkey(&x)?, x);
            }
        }
    }
    Ok(out)
}

fn set_method(s: &Rf<PySet>, name: &str, args: Vec<Py>) -> PyRes<Py> {
    match name {
        "add" => {
            nargs(name, &args, 1, 1)?;
            let k = hkey(&args[0])?;
            s.borrow_mut().entry(k).or_insert_with(|| args[0].clone());
            Ok(Py::None)
        }
        "clear" => {
            s.borrow_mut().clear();
            Ok(Py::None)
        }
        "copy" => Ok(Py::Set(rf(s.borrow().clone()))),
        "discard" | "remove" => {
            nargs(name, &args, 1, 1)?;
            let k = hkey(&args[0])?;
            let had = s.borrow_mut().shift_remove(&k).is_some();
            if !had && name == "remove" {
                return Err(Box::new(ExcObj {
                    class: "KeyError",
                    args: vec![args[0].clone()],
                }));
            }
            Ok(Py::None)
        }
        "pop" => match s.borrow_mut().pop() {
            Some((_, v)) => Ok(v),
            None => Err(exc("KeyError", "pop from an empty set")),
        },
        "union" | "update" => {
            let mut out = s.borrow().clone();
            for a in &args {
                for (k, v) in set_items(a)? {
                    out.insert(k, v);
                }
            }
            if name == "update" {
                *s.borrow_mut() = out;
                Ok(Py::None)
            } else {
                Ok(Py::Set(rf(out)))
            }
        }
        "intersection" | "intersection_update" => {
            let mut out = s.borrow().clone();
            for a in &args {
                let other = set_items(a)?;
                out.retain(|k, _| other.contains_key(k));
            }
            if name.ends_with("update") {
                *s.borrow_mut() = out;
                Ok(Py::None)
            } else {
                Ok(Py::Set(rf(out)))
            }
        }
        "difference" | "difference_update" => {
            let mut out = s.borrow().clone();
            for a in &args {
                let other = set_items(a)?;
                out.retain(|k, _| !other.contains_key(k));
            }
            if name.ends_with("update") {
                *s.borrow_mut() = out;
                Ok(Py::None)
            } else {
                Ok(Py::Set(rf(out)))
            }
        }
        "symmetric_difference" | "symmetric_difference_update" => {
            nargs(name, &args, 1, 1)?;
            let other = set_items(&args[0])?;
            let cur = s.borrow().clone();
            let mut out = PySet::new();
            for (k, v) in cur.iter() {
                if !other.contains_key(k) {
                    out.insert(k.clone(), v.clone());
                }
            }
            for (k, v) in other.iter() {
                if !cur.contains_key(k) {
                    out.insert(k.clone(), v.clone());
                }
            }
            if name.ends_with("update") {
                *s.borrow_mut() = out;
                Ok(Py::None)
            } else {
                Ok(Py::Set(rf(out)))
            }
        }
        "isdisjoint" | "issubset" | "issuperset" => {
            nargs(name, &args, 1, 1)?;
            let other = set_items(&args[0])?;
            let cur = s.borrow();
            Ok(Py::Bool(match name {
                "isdisjoint" => cur.keys().all(|k| !other.contains_key(k)),
                "issubset" => cur.keys().all(|k| other.contains_key(k)),
                _ => other.keys().all(|k| cur.contains_key(k)),
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
        (start..=last).rev().find(|&i| hay[i..i + needle.len()] == *needle)
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
                    return type_err(format!(
                        "must be str or None, not {}",
                        other.type_name()
                    ));
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
                if matches!(c, '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}') {
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
            let (a, b) = clamp_range(opt_int(args.get(1))?, opt_int(args.get(2))?, chars.len() as i64);
            let hay: String = if a <= b { chars[a as usize..b as usize].iter().collect() } else { String::new() };
            let test = |p: &Py| -> PyRes<bool> {
                match p {
                    Py::Str(p) => Ok(if name == "startswith" { hay.starts_with(&**p) } else { hay.ends_with(&**p) }),
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
            let (a, b) = clamp_range(opt_int(args.get(1))?, opt_int(args.get(2))?, hay.len() as i64);
            let r = if a > b { None } else { char_find(&hay, &needle, a as usize, b as usize, name.starts_with('r')) };
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
            let (a, b) = clamp_range(opt_int(args.get(1))?, opt_int(args.get(2))?, hay.len() as i64);
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
                Some(_) => return type_err("The fill character must be exactly one character long"),
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
            let found = if name == "partition" { sv.find(sep) } else { sv.rfind(sep) };
            Ok(match found {
                Some(i) => Py::tuple(vec![
                    Py::str(&sv[..i]),
                    Py::str(sep),
                    Py::str(&sv[i + sep.len()..]),
                ]),
                None if name == "partition" => Py::tuple(vec![Py::Str(s.clone()), Py::str(""), Py::str("")]),
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
                "isdigit" | "isdecimal" => !sv.is_empty() && sv.chars().all(pystr::is_decimal_digit),
                "isnumeric" => !sv.is_empty() && sv.chars().all(char::is_numeric),
                "isascii" => sv.is_ascii(),
                "isspace" => !sv.is_empty() && sv.chars().all(pystr::is_py_space),
                "isupper" => sv.chars().any(char::is_uppercase) && !sv.chars().any(char::is_lowercase),
                "islower" => sv.chars().any(char::is_lowercase) && !sv.chars().any(char::is_uppercase),
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
                    it.next().is_some_and(super::lex::is_id_start) && it.all(super::lex::is_id_continue)
                }
                _ => sv.chars().all(is_printable),
            };
            Ok(Py::Bool(r))
        }
        "encode" => Ok(Py::Bytes(sv.as_bytes().to_vec().into())),
        "expandtabs" => {
            nargs(name, &args, 0, 1)?;
            let tab = match args.first() {
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
        _ => Err(exc("NotImplementedError", format!("str.{name} is not supported"))),
    }
}

#[allow(dead_code)]
fn _unused() {
    let _ = py_round;
}
