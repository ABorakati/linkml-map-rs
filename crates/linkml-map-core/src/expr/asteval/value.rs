//! The Python values `asteval` computes with, and their basic protocols
//! (truthiness, equality, ordering, hashing, `repr`/`str`).
//!
//! Mutable containers have reference semantics, as in Python, so a statement
//! block can alias and mutate lists and dicts. They live only for the duration
//! of one evaluation; [`Py`] is deliberately not `Send`.

use std::cell::RefCell;
use std::cmp::Ordering;
use std::rc::Rc;
use std::sync::Arc;

use indexmap::IndexMap;

use super::ast::FuncDef;
use crate::pyfmt::{py_float_repr, py_repr_str};
use crate::value::{Value, cmp_int_float};

pub(crate) type Rf<T> = Rc<RefCell<T>>;

pub(crate) fn rf<T>(v: T) -> Rf<T> {
    Rc::new(RefCell::new(v))
}

/// A hashable dictionary/set key (Python's `hash`/`==` identity of values:
/// `True`, `1` and `1.0` are one key).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum HKey {
    None,
    Int(i64),
    Float(u64),
    Str(Arc<str>),
    Bytes(Arc<[u8]>),
    Tuple(Vec<HKey>),
    /// Hashable by identity (objects, functions, types, ranges by value are
    /// tuples instead).
    Id(usize),
    Name(&'static str),
}

pub(crate) type PyDict = IndexMap<HKey, (Py, Py)>;
pub(crate) type PySet = IndexMap<HKey, Py>;

/// An exception instance.
#[derive(Debug, Clone)]
pub(crate) struct ExcObj {
    pub class: &'static str,
    pub args: Vec<Py>,
}

/// A user-defined function (`asteval.astutils.Procedure`).
#[derive(Debug)]
pub(crate) struct Procedure {
    pub name: String,
    pub def: Arc<FuncDef>,
    /// Leading parameters without defaults.
    pub argnames: Vec<String>,
    /// `(name, evaluated default)` of the parameters with defaults.
    pub kwargs: Vec<(String, Py)>,
    pub is_lambda: bool,
}

/// What an iterator object holds. Python's lazy `map`/`zip`/`filter`/
/// `enumerate`/`reversed`/`iter` objects are materialised up front.
#[derive(Debug)]
pub(crate) struct PyIter {
    pub kind: &'static str,
    pub items: std::collections::VecDeque<Py>,
}

#[derive(Debug, Clone)]
pub(crate) enum Py {
    None,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(Arc<str>),
    Bytes(Arc<[u8]>),
    List(Rf<Vec<Py>>),
    Tuple(Rc<[Py]>),
    Dict(Rf<PyDict>),
    Set(Rf<PySet>),
    /// `range(start, stop, step)`.
    Range(i64, i64, i64),
    Slice(Rc<(Py, Py, Py)>),
    Iter(Rf<PyIter>),
    /// A `DynObj`: the source object (`src`) and its nested objects.
    Obj(Rf<IndexMap<String, Py>>),
    /// A built-in function.
    Builtin(&'static str),
    /// A built-in class (`int`, `str`, `ValueError`, ...).
    Type(&'static str),
    /// A bound built-in method: receiver and method name.
    Method(Rc<(Py, &'static str)>),
    Proc(Rc<Procedure>),
    Exc(Rc<ExcObj>),
    /// An `ast` node object (what `break`/`continue` evaluate to).
    Ast(&'static str),
    Ellipsis,
}

pub(crate) type PyRes<T> = Result<T, Box<ExcObj>>;

/// Build an exception to raise.
pub(crate) fn exc(class: &'static str, msg: impl Into<String>) -> Box<ExcObj> {
    Box::new(ExcObj {
        class,
        args: vec![Py::Str(msg.into().into())],
    })
}

pub(crate) fn type_err<T>(msg: impl Into<String>) -> PyRes<T> {
    Err(exc("TypeError", msg))
}

pub(crate) fn value_err<T>(msg: impl Into<String>) -> PyRes<T> {
    Err(exc("ValueError", msg))
}

pub(crate) fn runtime_err<T>(msg: impl Into<String>) -> PyRes<T> {
    Err(exc("RuntimeError", msg))
}

pub(crate) fn overflow_err<T>(msg: impl Into<String>) -> PyRes<T> {
    Err(exc("OverflowError", msg))
}

impl Py {
    pub(crate) fn str(s: impl Into<Arc<str>>) -> Py {
        Py::Str(s.into())
    }

    pub(crate) fn list(v: Vec<Py>) -> Py {
        Py::List(rf(v))
    }

    pub(crate) fn tuple(v: Vec<Py>) -> Py {
        Py::Tuple(v.into())
    }

    pub(crate) fn is_none(&self) -> bool {
        matches!(self, Py::None)
    }

    /// `type(x).__name__`.
    pub(crate) fn type_name(&self) -> String {
        match self {
            Py::None => "NoneType".into(),
            Py::Bool(_) => "bool".into(),
            Py::Int(_) => "int".into(),
            Py::Float(_) => "float".into(),
            Py::Str(_) => "str".into(),
            Py::Bytes(_) => "bytes".into(),
            Py::List(_) => "list".into(),
            Py::Tuple(_) => "tuple".into(),
            Py::Dict(_) => "dict".into(),
            Py::Set(_) => "set".into(),
            Py::Range(..) => "range".into(),
            Py::Slice(_) => "slice".into(),
            Py::Iter(it) => it.borrow().kind.into(),
            Py::Obj(_) => "DynObj".into(),
            Py::Builtin("type" | "open" | "uuid5" | "slot") => "function".into(),
            Py::Builtin("print") => "method".into(),
            Py::Builtin(_) => "builtin_function_or_method".into(),
            Py::Type(_) => "type".into(),
            Py::Method(_) => "builtin_function_or_method".into(),
            Py::Proc(_) => "Procedure".into(),
            Py::Exc(e) => e.class.into(),
            Py::Ast(k) => (*k).into(),
            Py::Ellipsis => "ellipsis".into(),
        }
    }

    /// `bool(x)`.
    pub(crate) fn truthy(&self) -> bool {
        match self {
            Py::None => false,
            Py::Bool(b) => *b,
            Py::Int(i) => *i != 0,
            Py::Float(f) => *f != 0.0,
            Py::Str(s) => !s.is_empty(),
            Py::Bytes(b) => !b.is_empty(),
            Py::List(l) => !l.borrow().is_empty(),
            Py::Tuple(t) => !t.is_empty(),
            Py::Dict(d) => !d.borrow().is_empty(),
            Py::Set(s) => !s.borrow().is_empty(),
            Py::Range(a, b, c) => range_len(*a, *b, *c) > 0,
            _ => true,
        }
    }

    /// Identity (`is`).
    pub(crate) fn is(&self, other: &Py) -> bool {
        match (self, other) {
            (Py::None, Py::None) | (Py::Ellipsis, Py::Ellipsis) => true,
            (Py::Bool(a), Py::Bool(b)) => a == b,
            (Py::Int(a), Py::Int(b)) => a == b,
            (Py::Float(a), Py::Float(b)) => a.to_bits() == b.to_bits(),
            (Py::Str(a), Py::Str(b)) => {
                Arc::ptr_eq(a, b) || (a.len() <= 1 && a == b) || (a.is_empty() && b.is_empty())
            }
            (Py::Bytes(a), Py::Bytes(b)) => Arc::ptr_eq(a, b),
            (Py::List(a), Py::List(b)) => Rc::ptr_eq(a, b),
            (Py::Tuple(a), Py::Tuple(b)) => Rc::ptr_eq(a, b) || (a.is_empty() && b.is_empty()),
            (Py::Dict(a), Py::Dict(b)) => Rc::ptr_eq(a, b),
            (Py::Set(a), Py::Set(b)) => Rc::ptr_eq(a, b),
            (Py::Iter(a), Py::Iter(b)) => Rc::ptr_eq(a, b),
            (Py::Obj(a), Py::Obj(b)) => Rc::ptr_eq(a, b),
            (Py::Proc(a), Py::Proc(b)) => Rc::ptr_eq(a, b),
            (Py::Exc(a), Py::Exc(b)) => Rc::ptr_eq(a, b),
            (Py::Slice(a), Py::Slice(b)) => Rc::ptr_eq(a, b),
            (Py::Method(a), Py::Method(b)) => Rc::ptr_eq(a, b),
            (Py::Builtin(a), Py::Builtin(b)) | (Py::Type(a), Py::Type(b)) => a == b,
            (Py::Ast(a), Py::Ast(b)) => a == b,
            (Py::Range(..), Py::Range(..)) => false,
            _ => false,
        }
    }

    /// A stable fake address for `id()` and default reprs.
    pub(crate) fn addr(&self) -> usize {
        match self {
            Py::List(a) => Rc::as_ptr(a) as *const u8 as usize,
            Py::Tuple(a) => Rc::as_ptr(a) as *const u8 as usize,
            Py::Dict(a) => Rc::as_ptr(a) as *const u8 as usize,
            Py::Set(a) => Rc::as_ptr(a) as *const u8 as usize,
            Py::Obj(a) => Rc::as_ptr(a) as *const u8 as usize,
            Py::Proc(a) => Rc::as_ptr(a) as *const u8 as usize,
            Py::Exc(a) => Rc::as_ptr(a) as *const u8 as usize,
            Py::Iter(a) => Rc::as_ptr(a) as *const u8 as usize,
            Py::Str(a) => a.as_ptr() as usize,
            Py::Int(i) => 0x7f00_0000_0000 + (*i as usize).wrapping_mul(32),
            _ => 0x7f00_dead_0000,
        }
    }
}

pub(crate) fn range_len(start: i64, stop: i64, step: i64) -> i64 {
    if step > 0 {
        if start >= stop {
            0
        } else {
            ((stop as i128 - start as i128 - 1) / step as i128 + 1) as i64
        }
    } else if start <= stop {
        0
    } else {
        ((start as i128 - stop as i128 - 1) / (-(step as i128)) + 1) as i64
    }
}

// --- exception classes ------------------------------------------------------

/// `(class, parent)` of every exception class `asteval` exposes (and the few
/// internal ones its machinery raises).
const EXC_PARENTS: &[(&str, &str)] = &[
    ("BaseException", ""),
    ("Exception", "BaseException"),
    ("KeyboardInterrupt", "BaseException"),
    ("SystemExit", "BaseException"),
    ("GeneratorExit", "BaseException"),
    ("ArithmeticError", "Exception"),
    ("AssertionError", "Exception"),
    ("AttributeError", "Exception"),
    ("BufferError", "Exception"),
    ("EOFError", "Exception"),
    ("ImportError", "Exception"),
    ("LookupError", "Exception"),
    ("MemoryError", "Exception"),
    ("NameError", "Exception"),
    ("OSError", "Exception"),
    ("ReferenceError", "Exception"),
    ("RuntimeError", "Exception"),
    ("StopIteration", "Exception"),
    ("SyntaxError", "Exception"),
    ("SystemError", "Exception"),
    ("TypeError", "Exception"),
    ("ValueError", "Exception"),
    ("Warning", "Exception"),
    ("FloatingPointError", "ArithmeticError"),
    ("OverflowError", "ArithmeticError"),
    ("ZeroDivisionError", "ArithmeticError"),
    ("IndexError", "LookupError"),
    ("KeyError", "LookupError"),
    ("UnboundLocalError", "NameError"),
    ("FileNotFoundError", "OSError"),
    ("NotImplementedError", "RuntimeError"),
    ("RecursionError", "RuntimeError"),
    ("IndentationError", "SyntaxError"),
    ("UnicodeError", "ValueError"),
    ("UnicodeDecodeError", "UnicodeError"),
    ("UnicodeEncodeError", "UnicodeError"),
    ("UnicodeTranslateError", "UnicodeError"),
    ("BytesWarning", "Warning"),
    ("DeprecationWarning", "Warning"),
    ("ImportWarning", "Warning"),
    ("RuntimeWarning", "Warning"),
    ("SyntaxWarning", "Warning"),
    ("UnicodeWarning", "Warning"),
];

/// Resolve a builtin name to an exception class (`EnvironmentError` and
/// `IOError` are aliases of `OSError`).
pub(crate) fn exc_class(name: &str) -> Option<&'static str> {
    let name = match name {
        "EnvironmentError" | "IOError" => "OSError",
        n => n,
    };
    EXC_PARENTS
        .iter()
        .find(|(c, _)| *c == name)
        .map(|(c, _)| *c)
}

/// `issubclass(class, base)` for exception classes.
pub(crate) fn exc_subclass(class: &str, base: &str) -> bool {
    let mut cur = class;
    loop {
        if cur == base {
            return true;
        }
        match EXC_PARENTS.iter().find(|(c, _)| *c == cur) {
            Some((_, parent)) if !parent.is_empty() => cur = parent,
            _ => return false,
        }
    }
}

/// The exception classes `asteval` puts in its symbol table.
pub(crate) const EXC_BUILTINS: [&str; 43] = [
    "ArithmeticError",
    "AssertionError",
    "AttributeError",
    "BaseException",
    "BufferError",
    "BytesWarning",
    "DeprecationWarning",
    "EOFError",
    "EnvironmentError",
    "Exception",
    "FloatingPointError",
    "IOError",
    "ImportError",
    "ImportWarning",
    "IndentationError",
    "IndexError",
    "KeyError",
    "KeyboardInterrupt",
    "LookupError",
    "MemoryError",
    "NameError",
    "NotImplementedError",
    "OSError",
    "OverflowError",
    "ReferenceError",
    "RuntimeError",
    "RuntimeWarning",
    "StopIteration",
    "SyntaxError",
    "SyntaxWarning",
    "SystemError",
    "TypeError",
    "UnboundLocalError",
    "UnicodeDecodeError",
    "UnicodeEncodeError",
    "UnicodeError",
    "UnicodeTranslateError",
    "UnicodeWarning",
    "ValueError",
    "Warning",
    "ZeroDivisionError",
    "GeneratorExit",
    "SystemExit",
];

impl ExcObj {
    /// `str(exc)`.
    pub(crate) fn message(&self) -> String {
        match self.args.len() {
            0 => String::new(),
            1 => {
                if self.class == "KeyError" {
                    repr(&self.args[0])
                } else {
                    to_str(&self.args[0])
                }
            }
            _ => repr(&Py::tuple(self.args.clone())),
        }
    }
}

// --- hashing ------------------------------------------------------------------

/// `hash`-key of a value, or `TypeError: unhashable type`.
pub(crate) fn hkey(v: &Py) -> PyRes<HKey> {
    Ok(match v {
        Py::None => HKey::None,
        Py::Bool(b) => HKey::Int(*b as i64),
        Py::Int(i) => HKey::Int(*i),
        Py::Float(f) => {
            if f.fract() == 0.0 && f.abs() < 9.2e18 {
                HKey::Int(*f as i64)
            } else {
                HKey::Float(f.to_bits())
            }
        }
        Py::Str(s) => HKey::Str(s.clone()),
        Py::Bytes(b) => HKey::Bytes(b.clone()),
        Py::Tuple(t) => HKey::Tuple(t.iter().map(hkey).collect::<PyRes<_>>()?),
        Py::Range(a, b, c) => HKey::Tuple(vec![
            HKey::Name("range"),
            HKey::Int(*a),
            HKey::Int(*b),
            HKey::Int(*c),
        ]),
        Py::Builtin(n) | Py::Type(n) => HKey::Name(n),
        Py::Ellipsis => HKey::Name("Ellipsis"),
        Py::List(_) | Py::Dict(_) | Py::Set(_) => {
            return type_err(format!("unhashable type: '{}'", v.type_name()));
        }
        Py::Slice(s) => HKey::Tuple(vec![hkey(&s.0)?, hkey(&s.1)?, hkey(&s.2)?]),
        other => HKey::Id(other.addr()),
    })
}

// --- equality and ordering --------------------------------------------------

fn num_of(v: &Py) -> Option<NumV> {
    match v {
        Py::Bool(b) => Some(NumV::I(*b as i64)),
        Py::Int(i) => Some(NumV::I(*i)),
        Py::Float(f) => Some(NumV::F(*f)),
        _ => None,
    }
}

#[derive(Clone, Copy)]
pub(crate) enum NumV {
    I(i64),
    F(f64),
}

pub(crate) fn as_num(v: &Py) -> Option<NumV> {
    num_of(v)
}

pub(crate) fn cmp_num(a: NumV, b: NumV) -> Option<Ordering> {
    match (a, b) {
        (NumV::I(x), NumV::I(y)) => Some(x.cmp(&y)),
        (NumV::F(x), NumV::F(y)) => x.partial_cmp(&y),
        (NumV::I(x), NumV::F(y)) => cmp_int_float(x, y),
        (NumV::F(x), NumV::I(y)) => cmp_int_float(y, x).map(Ordering::reverse),
    }
}

/// `a == b`.
pub(crate) fn py_eq(a: &Py, b: &Py) -> bool {
    if let (Some(x), Some(y)) = (num_of(a), num_of(b)) {
        return cmp_num(x, y) == Some(Ordering::Equal);
    }
    match (a, b) {
        (Py::None, Py::None) | (Py::Ellipsis, Py::Ellipsis) => true,
        (Py::Str(x), Py::Str(y)) => x == y,
        (Py::Bytes(x), Py::Bytes(y)) => x == y,
        (Py::List(x), Py::List(y)) => {
            if Rc::ptr_eq(x, y) {
                return true;
            }
            let (x, y) = (x.borrow(), y.borrow());
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(p, q)| p.is(q) || py_eq(p, q))
        }
        (Py::Tuple(x), Py::Tuple(y)) => {
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(p, q)| p.is(q) || py_eq(p, q))
        }
        (Py::Dict(x), Py::Dict(y)) => {
            if Rc::ptr_eq(x, y) {
                return true;
            }
            let (x, y) = (x.borrow(), y.borrow());
            x.len() == y.len()
                && x.iter()
                    .all(|(k, (_, v))| y.get(k).is_some_and(|(_, w)| v.is(w) || py_eq(v, w)))
        }
        (Py::Set(x), Py::Set(y)) => {
            if Rc::ptr_eq(x, y) {
                return true;
            }
            let (x, y) = (x.borrow(), y.borrow());
            x.len() == y.len() && x.keys().all(|k| y.contains_key(k))
        }
        (Py::Range(a1, b1, c1), Py::Range(a2, b2, c2)) => {
            let (l1, l2) = (range_len(*a1, *b1, *c1), range_len(*a2, *b2, *c2));
            l1 == l2 && (l1 == 0 || (a1 == a2 && (l1 == 1 || c1 == c2)))
        }
        (Py::Slice(x), Py::Slice(y)) => py_eq(&x.0, &y.0) && py_eq(&x.1, &y.1) && py_eq(&x.2, &y.2),
        _ => a.is(b),
    }
}

/// Rich comparison `a < b` family. `Ok(None)` is "unordered" (NaN).
fn py_cmp_opt(a: &Py, b: &Py, sym: &str) -> PyRes<Option<Ordering>> {
    if let (Some(x), Some(y)) = (num_of(a), num_of(b)) {
        return Ok(cmp_num(x, y));
    }
    match (a, b) {
        (Py::Str(x), Py::Str(y)) => Ok(Some(x.as_bytes().cmp(y.as_bytes()))),
        (Py::Bytes(x), Py::Bytes(y)) => Ok(Some(x.cmp(y))),
        (Py::List(x), Py::List(y)) => {
            let (x, y) = (x.borrow().clone(), y.borrow().clone());
            seq_cmp(&x, &y, sym)
        }
        (Py::Tuple(x), Py::Tuple(y)) => seq_cmp(x, y, sym),
        _ => type_err(format!(
            "'{sym}' not supported between instances of '{}' and '{}'",
            a.type_name(),
            b.type_name()
        )),
    }
}

fn seq_cmp(x: &[Py], y: &[Py], sym: &str) -> PyRes<Option<Ordering>> {
    for (p, q) in x.iter().zip(y.iter()) {
        if !(p.is(q) || py_eq(p, q)) {
            return py_cmp_opt(p, q, sym);
        }
    }
    Ok(Some(x.len().cmp(&y.len())))
}

fn set_cmp(x: &PySet, y: &PySet) -> Option<Ordering> {
    let sub = x.keys().all(|k| y.contains_key(k));
    let sup = y.keys().all(|k| x.contains_key(k));
    match (sub, sup) {
        (true, true) => Some(Ordering::Equal),
        (true, false) => Some(Ordering::Less),
        (false, true) => Some(Ordering::Greater),
        _ => None,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Rel {
    Lt,
    Le,
    Gt,
    Ge,
}

pub(crate) fn py_rel(a: &Py, b: &Py, rel: Rel) -> PyRes<bool> {
    let sym = match rel {
        Rel::Lt => "<",
        Rel::Le => "<=",
        Rel::Gt => ">",
        Rel::Ge => ">=",
    };
    let ord = if let (Py::Set(x), Py::Set(y)) = (a, b) {
        set_cmp(&x.borrow(), &y.borrow())
    } else {
        py_cmp_opt(a, b, sym)?
    };
    Ok(match ord {
        None => false,
        Some(o) => match rel {
            Rel::Lt => o == Ordering::Less,
            Rel::Le => o != Ordering::Greater,
            Rel::Gt => o == Ordering::Greater,
            Rel::Ge => o != Ordering::Less,
        },
    })
}

// --- repr and str -------------------------------------------------------------

pub(crate) fn repr(v: &Py) -> String {
    let mut out = String::new();
    let mut seen = Vec::new();
    write_repr(v, &mut out, &mut seen, true);
    out
}

pub(crate) fn to_str(v: &Py) -> String {
    match v {
        Py::Str(s) => s.to_string(),
        Py::Exc(e) => e.message(),
        Py::Obj(o) => {
            // DynObj.__str__: str(vars(self))
            let mut out = String::new();
            let mut seen = Vec::new();
            write_map(&o.borrow(), &mut out, &mut seen);
            out
        }
        other => repr(other),
    }
}

fn write_map(m: &IndexMap<String, Py>, out: &mut String, seen: &mut Vec<usize>) {
    out.push('{');
    for (i, (k, v)) in m.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&py_repr_str(k));
        out.push_str(": ");
        write_repr(v, out, seen, true);
    }
    out.push('}');
}

pub(crate) fn bytes_repr(b: &[u8]) -> String {
    let quote = if b.contains(&b'\'') && !b.contains(&b'"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::from("b");
    out.push(quote);
    for &c in b {
        match c {
            b'\\' => out.push_str("\\\\"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            b'\t' => out.push_str("\\t"),
            c if c as char == quote => {
                out.push('\\');
                out.push(c as char);
            }
            0x20..=0x7e => out.push(c as char),
            c => out.push_str(&format!("\\x{c:02x}")),
        }
    }
    out.push(quote);
    out
}

fn write_repr(v: &Py, out: &mut String, seen: &mut Vec<usize>, _top: bool) {
    match v {
        Py::None => out.push_str("None"),
        Py::Bool(true) => out.push_str("True"),
        Py::Bool(false) => out.push_str("False"),
        Py::Int(i) => out.push_str(&i.to_string()),
        Py::Float(f) => out.push_str(&py_float_repr(*f)),
        Py::Str(s) => out.push_str(&py_repr_str(s)),
        Py::Bytes(b) => out.push_str(&bytes_repr(b)),
        Py::List(l) => {
            let id = v.addr();
            if seen.contains(&id) {
                out.push_str("[...]");
                return;
            }
            seen.push(id);
            out.push('[');
            for (i, x) in l.borrow().iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_repr(x, out, seen, false);
            }
            out.push(']');
            seen.pop();
        }
        Py::Tuple(t) => {
            out.push('(');
            for (i, x) in t.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_repr(x, out, seen, false);
            }
            if t.len() == 1 {
                out.push(',');
            }
            out.push(')');
        }
        Py::Dict(d) => {
            let id = v.addr();
            if seen.contains(&id) {
                out.push_str("{...}");
                return;
            }
            seen.push(id);
            out.push('{');
            for (i, (k, val)) in d.borrow().values().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_repr(k, out, seen, false);
                out.push_str(": ");
                write_repr(val, out, seen, false);
            }
            out.push('}');
            seen.pop();
        }
        Py::Set(s) => {
            let s = s.borrow();
            if s.is_empty() {
                out.push_str("set()");
                return;
            }
            out.push('{');
            for (i, x) in s.values().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_repr(x, out, seen, false);
            }
            out.push('}');
        }
        Py::Range(a, b, c) => {
            if *c == 1 {
                out.push_str(&format!("range({a}, {b})"));
            } else {
                out.push_str(&format!("range({a}, {b}, {c})"));
            }
        }
        Py::Slice(s) => {
            out.push_str("slice(");
            write_repr(&s.0, out, seen, false);
            out.push_str(", ");
            write_repr(&s.1, out, seen, false);
            out.push_str(", ");
            write_repr(&s.2, out, seen, false);
            out.push(')');
        }
        Py::Iter(it) => out.push_str(&format!(
            "<{} object at 0x{:x}>",
            it.borrow().kind,
            v.addr()
        )),
        Py::Obj(_) => out.push_str(&format!(
            "<linkml_map.utils.dynamic_object.DynObj object at 0x{:x}>",
            v.addr()
        )),
        Py::Builtin(n) => out.push_str(&format!("<built-in function {n}>")),
        Py::Type(n) => out.push_str(&format!("<class '{n}'>")),
        Py::Method(m) => out.push_str(&format!(
            "<built-in method {} of {} object at 0x{:x}>",
            m.1,
            m.0.type_name(),
            m.0.addr()
        )),
        Py::Proc(p) => {
            let mut sig = p.argnames.join(", ");
            if p.def.params.vararg.is_some() {
                sig.push('*');
                sig.push_str(p.def.params.vararg.as_deref().unwrap_or(""));
            }
            if !p.kwargs.is_empty() {
                if !sig.is_empty() {
                    sig.push_str(", ");
                }
                let kw: Vec<String> = p
                    .kwargs
                    .iter()
                    .map(|(k, d)| format!("{k}={}", repr(d)))
                    .collect();
                sig.push_str(&kw.join(", "));
                if let Some(kw) = &p.def.params.kwarg {
                    sig.push_str(&format!(", **{kw}"));
                }
            }
            out.push_str(&format!("<Procedure {}({sig})>", p.name));
        }
        Py::Exc(e) => {
            out.push_str(e.class);
            out.push('(');
            for (i, a) in e.args.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_repr(a, out, seen, false);
            }
            out.push(')');
        }
        Py::Ast(k) => out.push_str(&format!("<ast.{k} object at 0x{:x}>", v.addr())),
        Py::Ellipsis => out.push_str("Ellipsis"),
    }
}

// --- conversion from and to the engine's Value ------------------------------

/// A binding value as Python data. A map becomes a `DynObj`, as the engine's
/// `dynamic_object` builds from the source row.
pub(crate) fn from_value(v: &Value) -> Py {
    match v {
        Value::Null => Py::None,
        Value::Bool(b) => Py::Bool(*b),
        Value::Int(i) => Py::Int(*i),
        Value::Float(f) => Py::Float(*f),
        Value::Str(s) => Py::str(s.as_str()),
        Value::List(l) => Py::list(l.iter().map(from_value).collect()),
        Value::Map(m) => Py::Obj(rf(m
            .iter()
            .map(|(k, v)| (k.clone(), from_value(v)))
            .collect())),
        Value::Decimal(d) => Py::str(d.to_string()),
        Value::Temporal(t) => Py::str(t.to_string()),
    }
}

/// `target`'s final value as an engine [`Value`]. Tuples and sets become
/// lists, objects with no engine counterpart become their `str()`.
pub(crate) fn to_value(v: &Py, depth: usize) -> Value {
    if depth > 200 {
        return Value::Null;
    }
    match v {
        Py::None => Value::Null,
        Py::Bool(b) => Value::Bool(*b),
        Py::Int(i) => Value::Int(*i),
        Py::Float(f) => Value::Float(*f),
        Py::Str(s) => Value::Str(s.to_string()),
        Py::List(l) => Value::List(l.borrow().iter().map(|x| to_value(x, depth + 1)).collect()),
        Py::Tuple(t) => Value::List(t.iter().map(|x| to_value(x, depth + 1)).collect()),
        Py::Set(s) => Value::List(
            s.borrow()
                .values()
                .map(|x| to_value(x, depth + 1))
                .collect(),
        ),
        Py::Dict(d) => Value::Map(
            d.borrow()
                .values()
                .map(|(k, v)| (to_str(k), to_value(v, depth + 1)))
                .collect(),
        ),
        Py::Obj(o) => Value::Map(
            o.borrow()
                .iter()
                .map(|(k, v)| (k.clone(), to_value(v, depth + 1)))
                .collect(),
        ),
        other => Value::Str(repr(other)),
    }
}

/// An engine value as plain Python data (maps become dicts).
pub(crate) fn plain_from_value(v: &crate::value::Value) -> Py {
    match v {
        Value::Map(m) => {
            let mut d = PyDict::new();
            for (k, x) in m {
                d.insert(
                    HKey::Str(k.as_str().into()),
                    (Py::str(k.as_str()), plain_from_value(x)),
                );
            }
            Py::Dict(rf(d))
        }
        Value::List(l) => Py::list(l.iter().map(plain_from_value).collect()),
        other => from_value(other),
    }
}

/// `d[key] = val`: an existing key keeps its original object (`d[1.0] = x` on
/// `{1: y}` leaves the key `1`).
pub(crate) fn dict_set(d: &mut PyDict, h: HKey, key: Py, val: Py) {
    match d.get_mut(&h) {
        Some(entry) => entry.1 = val,
        None => {
            d.insert(h, (key, val));
        }
    }
}

/// `s.add(x)`: an equal element already present stays.
pub(crate) fn set_add(s: &mut PySet, h: HKey, v: Py) {
    s.entry(h).or_insert(v);
}

/// `math.frexp`: mantissa in `[0.5, 1)` and the binary exponent.
pub(crate) fn frexp(x: f64) -> (f64, i32) {
    if x == 0.0 || !x.is_finite() {
        return (x, 0);
    }
    let bits = x.to_bits();
    let exp = ((bits >> 52) & 0x7ff) as i32;
    if exp == 0 {
        let (m, e) = frexp(x * 2f64.powi(54));
        return (m, e - 54);
    }
    let m = f64::from_bits((bits & !(0x7ffu64 << 52)) | (1022u64 << 52));
    (m, exp - 1022)
}
