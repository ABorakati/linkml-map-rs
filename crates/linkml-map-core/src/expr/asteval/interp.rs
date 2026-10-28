//! A literal port of `asteval.Interpreter` (1.0.x): the `run` dispatcher with
//! its error list and interrupt flags, `raise_exception`, and every `on_*`
//! handler, including their quirks (see the `PARITY` notes).

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use super::ast::*;
use super::funcs::{Caller, Kwargs, call_builtin, call_type, lookup_builtin};
use super::methods::{call_method, get_attr, set_attr};
use super::ops::*;
use super::value::*;

/// Python's recursion limit, less the frames a caller typically has on the
/// stack already.
const MAX_FRAMES: usize = 960;
/// A runaway-loop guard. Upstream has none (an infinite loop hangs).
const MAX_STEPS: u64 = 30_000_000;
/// Native stack the interpreter may use before it reports a `RecursionError`
/// (worker threads have as little as 2 MiB, and a deep Python call chain nests
/// many Rust frames per Python frame).
const STACK_BUDGET: usize = 1024 * 1024;

/// `astutils.RESERVED_WORDS`.
const RESERVED_WORDS: [&str; 41] = [
    "False",
    "None",
    "True",
    "and",
    "as",
    "assert",
    "async",
    "await",
    "break",
    "class",
    "continue",
    "def",
    "del",
    "elif",
    "else",
    "except",
    "finally",
    "for",
    "from",
    "global",
    "if",
    "import",
    "in",
    "is",
    "lambda",
    "nonlocal",
    "not",
    "or",
    "pass",
    "raise",
    "return",
    "try",
    "while",
    "with",
    "yield",
    "exec",
    "eval",
    "execfile",
    "__import__",
    "__package__",
    "__fstring__",
];

/// `astutils.UNSAFE_ATTRS`.
const UNSAFE_ATTRS: [&str; 25] = [
    "__subclasses__",
    "__bases__",
    "__globals__",
    "__code__",
    "__reduce__",
    "__reduce_ex__",
    "__mro__",
    "__closure__",
    "__func__",
    "__self__",
    "__module__",
    "__dict__",
    "__class__",
    "__call__",
    "__get__",
    "__getattribute__",
    "__subclasshook__",
    "__new__",
    "__init__",
    "func_globals",
    "func_code",
    "func_closure",
    "im_class",
    "im_func",
    "im_self",
];

fn unsafe_attr(name: &str) -> bool {
    UNSAFE_ATTRS.contains(&name)
        || matches!(
            name,
            "gi_code" | "gi_frame" | "f_locals" | "__asteval__" | "mro"
        )
        || (name.starts_with("__") && name.ends_with("__"))
}

/// `astutils.valid_symbol_name`.
fn valid_symbol_name(name: &str) -> bool {
    if RESERVED_WORDS.contains(&name) {
        return false;
    }
    let mut it = name.chars();
    match it.next() {
        Some(c) if super::lex::is_id_start(c) => it.all(super::lex::is_id_continue),
        _ => false,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Interrupt {
    Break,
    Continue,
    Return,
    Raise,
}

/// `ExceptionHolder`.
#[derive(Debug, Clone)]
pub(crate) struct Holder {
    pub exc: Option<&'static str>,
    pub msg: String,
    /// `sys.exc_info()` at creation; `None` is `(None, None, None)`.
    pub exc_info: Option<Rc<ExcObj>>,
    pub pos: Option<Pos>,
}

/// A statement or expression node about to be run.
#[derive(Clone, Copy)]
enum Node<'a> {
    E(&'a Expr),
    S(&'a Stmt),
    M(&'a Module),
}

pub(crate) struct Interp<'a> {
    sym: HashMap<String, Py>,
    removed: HashSet<String>,
    pub(crate) error: Vec<Holder>,
    error_msg: Option<String>,
    interrupt: Option<Interrupt>,
    retval: Option<Py>,
    calldepth: usize,
    pub(crate) source: &'a str,
    frames: usize,
    steps: u64,
    pub(crate) aborted: bool,
    /// Address of a local near the interpreter's entry, to bound native stack use.
    stack_base: usize,
    slot_values: Option<&'a indexmap::IndexMap<String, crate::value::Value>>,
}

/// The sentinel `ReturnedNone` (`on_return` of a `None`).
fn returned_none() -> Py {
    Py::Ast("Empty")
}

fn is_returned_none(v: &Py) -> bool {
    matches!(v, Py::Ast("Empty"))
}

impl<'a> Interp<'a> {
    /// `Interpreter(usersyms={"src": src, "target": None, "uuid5": _uuid5, **functions})`.
    pub(crate) fn new(
        source: &'a str,
        src: Py,
        slot_values: Option<&'a indexmap::IndexMap<String, crate::value::Value>>,
    ) -> Self {
        let mut sym = HashMap::new();
        sym.insert("src".to_string(), src);
        sym.insert("target".to_string(), Py::None);
        sym.insert("uuid5".to_string(), Py::Builtin("uuid5"));
        if slot_values.is_some() {
            sym.insert("slot".to_string(), Py::Builtin("slot"));
        }
        Interp {
            sym,
            removed: HashSet::new(),
            error: Vec::new(),
            error_msg: None,
            interrupt: None,
            retval: None,
            calldepth: 0,
            source,
            frames: 0,
            steps: 0,
            aborted: false,
            stack_base: {
                let marker = 0u8;
                std::ptr::addr_of!(marker) as usize
            },
            slot_values,
        }
    }

    pub(crate) fn symbol(&self, name: &str) -> Option<Py> {
        if let Some(v) = self.sym.get(name) {
            return Some(v.clone());
        }
        if self.removed.contains(name) {
            return None;
        }
        lookup_builtin(name)
    }

    fn set_symbol(&mut self, name: &str, v: Py) {
        self.removed.remove(name);
        self.sym.insert(name.to_string(), v);
    }

    fn has_symbol(&self, name: &str) -> bool {
        self.sym.contains_key(name)
            || (!self.removed.contains(name) && lookup_builtin(name).is_some())
    }

    // --- errors ---------------------------------------------------------------

    /// `Interpreter.raise_exception`: record an `ExceptionHolder` and return the
    /// exception to raise. `in_except` is the exception being handled at the
    /// call site (`sys.exc_info()`), if the Python call is inside an `except`.
    fn raise_exception(
        &mut self,
        pos: Option<Pos>,
        exc: Option<&'static str>,
        msg: &str,
        in_except: Option<&Rc<ExcObj>>,
    ) -> Box<ExcObj> {
        let mut h_exc = exc;
        if h_exc.is_none() {
            h_exc = in_except.map(|e| e.class);
        }
        let mut h_msg = msg.to_string();
        if h_msg.is_empty()
            && let Some(e) = in_except
        {
            h_msg = e.message();
        }
        self.interrupt = Some(Interrupt::Raise);
        self.error.push(Holder {
            exc: h_exc,
            msg: h_msg,
            exc_info: in_except.cloned(),
            pos,
        });
        if self.error_msg.is_none() {
            self.error_msg = Some(msg.to_string());
        }
        // `err` is the holder just created, until a pop replaces it.
        let mut err_idx: Option<usize> = Some(self.error.len() - 1);
        let mut err_detached: Option<Holder> = None;
        let mut exc = exc;
        if exc.is_none() {
            exc = self.error.last().and_then(|h| h.exc);
            if exc.is_none() {
                while exc.is_none() && !self.error.is_empty() {
                    let e = self.error.pop().expect("non-empty");
                    exc = e.exc;
                    err_idx = None;
                    err_detached = Some(e);
                }
            }
        }
        let exc = exc.unwrap_or("Exception");
        let err_msg_len = match (err_idx, &err_detached) {
            (Some(i), _) => self.error.get(i).map_or(0, |h| h.msg.len()),
            (None, Some(h)) => h.msg.len(),
            _ => 0,
        };
        let error_msg = self.error_msg.clone().unwrap_or_default();
        if err_msg_len == 0 && error_msg.is_empty() && self.error.len() > 1 {
            let popped = self.error.pop().expect("len > 1");
            let class = popped.exc.unwrap_or("Exception");
            return Box::new(ExcObj {
                class,
                args: vec![Py::str(popped.msg.as_str())],
            });
        }
        if err_msg_len == 0
            && let Some(i) = err_idx
            && let Some(h) = self.error.get_mut(i)
        {
            h.msg = error_msg.clone();
        }
        Box::new(ExcObj {
            class: exc,
            args: vec![Py::str(error_msg)],
        })
    }

    fn remove_duplicate_errors(&mut self) {
        if self.error.is_empty() {
            return;
        }
        let mut out = vec![self.error[0].clone()];
        for err in &self.error[1..] {
            let last = out.last().expect("non-empty");
            if (err.exc != last.exc || err.msg != last.msg) && !err.msg.is_empty() {
                out.push(err.clone());
            }
        }
        self.error = out;
    }

    /// `ExceptionHolder.get_error()`'s text for the first error.
    pub(crate) fn error_detail(&self, h: &Holder) -> String {
        let exc_name = h.exc.unwrap_or("UnknownError");
        let mut out: Vec<String> = Vec::new();
        let code: Vec<&str> = self.source.split('\n').collect();
        match h.pos {
            Some(p) if p.line >= 1 => {
                let lo = (p.line - 1) as usize;
                let hi = (p.end_line as usize).min(code.len());
                if lo < hi {
                    out.push(code[lo..hi].join("\n"));
                } else {
                    out.push(String::new());
                }
                if p.col > 0 {
                    out.push(format!("{}^^^^", " ".repeat(p.col as usize)));
                }
            }
            _ => out.push(self.source.to_string()),
        }
        out.push(format!("{exc_name}: {}", h.msg));
        out.join("\n")
    }

    // --- run ------------------------------------------------------------------

    fn tick(&mut self) -> PyRes<()> {
        self.steps += 1;
        if self.steps > MAX_STEPS {
            self.aborted = true;
        }
        if self.aborted {
            return Err(exc("RuntimeError", "evaluation step limit exceeded"));
        }
        Ok(())
    }

    fn interrupt_node(&self) -> Option<Py> {
        match self.interrupt {
            Some(Interrupt::Break) => Some(Py::Ast("Break")),
            Some(Interrupt::Continue) => Some(Py::Ast("Continue")),
            _ => None,
        }
    }

    /// `Interpreter.run` for one node.
    fn run(&mut self, node: Option<Node<'_>>, with_raise: bool) -> PyRes<Py> {
        self.tick()?;
        if !self.error.is_empty() {
            return Ok(Py::None);
        }
        if let Some(rv) = &self.retval {
            return Ok(rv.clone());
        }
        if let Some(n) = self.interrupt_node() {
            return Ok(n);
        }
        let Some(node) = node else {
            return Ok(Py::None);
        };
        // Python frames: `run` and the handler.
        let here = 0u8;
        let used = self
            .stack_base
            .saturating_sub(std::ptr::addr_of!(here) as usize);
        if self.frames + 2 > MAX_FRAMES || used > STACK_BUDGET {
            let e = exc("RecursionError", "maximum recursion depth exceeded");
            let rc = Rc::new(*e);
            // The error is raised by the interpreter while entering the frame, in
            // the caller's `except` machinery.
            return Err(Box::new((*rc).clone()));
        }
        self.frames += 2;
        let r = match node {
            Node::E(e) => self.handle_expr(e),
            Node::S(s) => self.handle_stmt(s),
            Node::M(m) => self.on_module(m),
        };
        self.frames -= 2;
        match r {
            Ok(ret) => Ok(match ret {
                // `if isinstance(ret, enumerate): ret = list(ret)`
                Py::Iter(it) if it.borrow().kind == "enumerate" => {
                    Py::list(it.borrow().items.iter().cloned().collect())
                }
                other => other,
            }),
            Err(e) => {
                if self.aborted {
                    return Err(e);
                }
                if with_raise {
                    let rc = Rc::new(*e);
                    Err(self.raise_exception(None, None, "", Some(&rc)))
                } else {
                    // Swallowed (try bodies and the top-level run).
                    if self.error.len() > 2 {
                        self.remove_duplicate_errors();
                    }
                    Ok(Py::None)
                }
            }
        }
    }

    fn run_e(&mut self, e: &Expr) -> PyRes<Py> {
        self.run(Some(Node::E(e)), true)
    }

    fn run_s(&mut self, s: &Stmt) -> PyRes<Py> {
        self.run(Some(Node::S(s)), true)
    }

    fn run_opt(&mut self, e: Option<&Expr>) -> PyRes<Py> {
        self.run(e.map(Node::E), true)
    }

    /// `Interpreter.eval` of a parsed module; the result is ignored. Errors are
    /// swallowed (`show_errors` only prints them).
    pub(crate) fn eval_module(&mut self, m: &Module) {
        self.error.clear();
        self.error_msg = None;
        let _ = self.run(Some(Node::M(m)), false);
    }

    fn on_module(&mut self, m: &Module) -> PyRes<Py> {
        let mut out = Py::None;
        for s in &m.body {
            out = self.run_s(s)?;
        }
        Ok(out)
    }

    // --- expressions ------------------------------------------------------------

    fn handle_expr(&mut self, e: &Expr) -> PyRes<Py> {
        match e {
            Expr::Constant(c) => self.on_constant(c),
            Expr::Name(id, pos) => self.on_name(id, *pos),
            Expr::List(elts) => self.on_list(elts),
            Expr::Tuple(elts) => self.on_list(elts).map(|l| match l {
                Py::List(l) => Py::tuple(l.borrow().clone()),
                other => other,
            }),
            Expr::Set(elts) => self.on_set(elts),
            Expr::Dict(items) => self.on_dict(items),
            Expr::Attribute(value, attr) => self.on_attribute(value, attr),
            Expr::Subscript(value, slice) => self.on_subscript(value, slice),
            Expr::Slice(lo, hi, st) => self.on_slice(lo.as_deref(), hi.as_deref(), st.as_deref()),
            Expr::Call(func, args, kws) => self.on_call(e, func, args, kws),
            Expr::BinOp(l, op, r) => self.on_binop(l, *op, r),
            Expr::UnaryOp(op, x) => {
                let a = self.run_e(x)?;
                unaryop(*op, &a)
            }
            Expr::BoolOp(op, values) => self.on_boolop(*op, values),
            Expr::Compare(left, rest) => self.on_compare(left, rest),
            Expr::IfExp(test, body, orelse) => {
                let t = self.run_e(test)?;
                if t.truthy() {
                    self.run_e(body)
                } else {
                    self.run_e(orelse)
                }
            }
            Expr::Lambda(def) => self.make_procedure(def, true),
            Expr::ListComp(elt, gens) => self.on_comp(Comp::List(elt), gens),
            Expr::SetComp(elt, gens) => self.on_setcomp(elt, gens),
            Expr::DictComp(k, v, gens) => self.on_comp(Comp::Dict(k, v), gens),
            Expr::JoinedStr(parts) => self.on_joinedstr(parts),
            Expr::Starred(_) => Err(self.not_in_handlers("starred")),
            Expr::Unsupported(kind) => Err(self.not_in_handlers(kind)),
        }
    }

    fn on_list(&mut self, elts: &[Expr]) -> PyRes<Py> {
        let mut out = Vec::with_capacity(elts.len());
        for x in elts {
            out.push(self.run_e(x)?);
        }
        Ok(Py::list(out))
    }

    fn on_set(&mut self, elts: &[Expr]) -> PyRes<Py> {
        let mut items = Vec::with_capacity(elts.len());
        for x in elts {
            items.push(self.run_e(x)?);
        }
        let mut s = PySet::new();
        for x in items {
            set_add(&mut s, hkey(&x)?, x);
        }
        Ok(Py::Set(rf(s)))
    }

    fn on_dict(&mut self, items: &[(Option<Expr>, Expr)]) -> PyRes<Py> {
        // PARITY: `{self.run(k): self.run(v) for k, v in zip(keys, values)}`:
        // the `None` key of a `**mapping` entry runs as `run(None)`, i.e. None.
        let mut d = PyDict::new();
        for (k, v) in items {
            let key = self.run_opt(k.as_ref())?;
            let val = self.run_e(v)?;
            dict_set(&mut d, hkey(&key)?, key, val);
        }
        Ok(Py::Dict(rf(d)))
    }

    fn on_subscript(&mut self, value: &Expr, slice: &Expr) -> PyRes<Py> {
        let v = self.run_e(value)?;
        let k = self.run_e(slice)?;
        getitem(&v, &k)
    }

    fn on_slice(&mut self, lo: Option<&Expr>, hi: Option<&Expr>, st: Option<&Expr>) -> PyRes<Py> {
        let a = self.run_opt(lo)?;
        let b = self.run_opt(hi)?;
        let c = self.run_opt(st)?;
        Ok(Py::Slice(Rc::new((a, b, c))))
    }

    fn on_binop(&mut self, l: &Expr, op: BinOp, r: &Expr) -> PyRes<Py> {
        let a = self.run_e(l)?;
        let b = self.run_e(r)?;
        binop(op, &a, &b)
    }

    fn on_boolop(&mut self, op: BoolOp, values: &[Expr]) -> PyRes<Py> {
        let mut val = self.run_e(&values[0])?;
        let is_and = op == BoolOp::And;
        if (is_and && val.truthy()) || (!is_and && !val.truthy()) {
            for nv in &values[1..] {
                let rhs = self.run_e(nv)?;
                // op2func(And/Or)(val, rhs): `val and rhs` / `val or rhs`.
                val = if is_and {
                    if val.truthy() { rhs } else { val }
                } else if val.truthy() {
                    val
                } else {
                    rhs
                };
                if (is_and && !val.truthy()) || (!is_and && val.truthy()) {
                    break;
                }
            }
        }
        Ok(val)
    }

    fn on_compare(&mut self, left: &Expr, rest: &[(CmpOp, Expr)]) -> PyRes<Py> {
        let mut lval = self.run_e(left)?;
        let multi = rest.len() > 1;
        let mut results: Vec<bool> = Vec::new();
        let mut ret = false;
        let mut lnode = left;
        for (op, rnode) in rest {
            let rval = self.run_e(rnode)?;
            ret = match op {
                // PARITY: numbers are values here, not objects: two reads of the same
                // float are taken to be one object unless one side is a literal.
                CmpOp::Is | CmpOp::IsNot
                    if matches!((&lval, &rval), (Py::Float(a), Py::Float(b)) if a.to_bits() == b.to_bits())
                        && !matches!(lnode, Expr::Constant(_))
                        && !matches!(rnode, Expr::Constant(_)) =>
                {
                    *op == CmpOp::Is
                }
                _ => compare(*op, &lval, &rval)?,
            };
            lnode = rnode;
            if multi {
                results.push(ret);
                if !results.iter().all(|b| *b) {
                    return Ok(Py::Bool(false));
                }
                lval = rval;
            }
        }
        if multi {
            ret = results.iter().all(|b| *b);
        }
        Ok(Py::Bool(ret))
    }

    fn on_setcomp(&mut self, elt: &Expr, gens: &[Comprehension]) -> PyRes<Py> {
        let r = self.on_comp(Comp::List(elt), gens)?;
        let mut s = PySet::new();
        if let Py::List(l) = r {
            for x in l.borrow().iter() {
                set_add(&mut s, hkey(x)?, x.clone());
            }
        }
        Ok(Py::Set(rf(s)))
    }

    /// `run`'s `KeyError` branch: a node class with no handler.
    fn not_in_handlers(&mut self, kind: &str) -> Box<ExcObj> {
        let key = Rc::new(ExcObj {
            class: "KeyError",
            args: vec![Py::str(kind.to_ascii_lowercase())],
        });
        self.raise_exception(None, Some("NotImplementedError"), "", Some(&key))
    }

    fn on_constant(&mut self, c: &Const) -> PyRes<Py> {
        Ok(match c {
            Const::None => Py::None,
            Const::Bool(b) => Py::Bool(*b),
            Const::Int(i) => Py::Int(*i),
            Const::BigInt(n) => Py::BigInt(Rc::new(n.clone())),
            Const::Float(f) => Py::Float(*f),
            Const::Str(s) => Py::Str(s.clone()),
            Const::Bytes(b) => Py::Bytes(b.clone()),
            Const::Ellipsis => Py::Ellipsis,
            Const::Complex(f) => Py::Complex(0.0, *f),
        })
    }

    fn on_name(&mut self, id: &str, pos: Pos) -> PyRes<Py> {
        match self.symbol(id) {
            Some(v) => Ok(v),
            None => {
                let msg = format!("name '{id}' is not defined");
                Err(self.raise_exception(Some(pos), Some("NameError"), &msg, None))
            }
        }
    }

    fn on_joinedstr(&mut self, parts: &[FPart]) -> PyRes<Py> {
        let mut out = String::new();
        for p in parts {
            out.push_str(&self.fpart(p)?);
        }
        Ok(Py::str(out))
    }

    fn fpart(&mut self, p: &FPart) -> PyRes<String> {
        match p {
            FPart::Lit(s) => Ok(s.clone()),
            FPart::Field {
                value,
                conversion,
                spec,
            } => {
                let mut val = self.run_e(value)?;
                match conversion {
                    Some('s') => val = Py::str(to_str_checked(&val)?),
                    Some('r') => val = Py::str(repr_checked(&val)?),
                    Some('a') => {
                        check_str_digits(&val)?;
                        val = Py::str(ascii_repr(&val));
                    }
                    _ => {}
                }
                let spec_text = match spec {
                    Some(parts) => {
                        let mut s = String::new();
                        for p in parts {
                            s.push_str(&self.fpart(p)?);
                        }
                        Some(s)
                    }
                    None => None,
                };
                // safe_format('{__fstring__[:spec]}', ...): the spec is parsed again as
                // a format string, so braces in it are replacement fields.
                if let Some(s) = &spec_text
                    && (s.contains('{') || s.contains('}'))
                {
                    return Err(exc("ValueError", "Max string recursion exceeded"));
                }
                super::fmt::format_value(&val, spec_text.as_deref().unwrap_or(""))
            }
        }
    }

    fn on_attribute(&mut self, value: &Expr, attr: &str) -> PyRes<Py> {
        let sym = self.run_e(value)?;
        self.safe_getattr(&sym, attr, None)
    }

    /// `astutils.safe_getattr`.
    fn safe_getattr(&mut self, obj: &Py, attr: &str, pos: Option<Pos>) -> PyRes<Py> {
        let mut bad = unsafe_attr(attr);
        if !bad {
            bad = matches!(obj, Py::Str(_)) && matches!(attr, "format" | "format_map")
                || (matches!(obj, Py::Type("str")) && matches!(attr, "format" | "format_map"));
        }
        if bad {
            let msg = format!("no safe attribute '{attr}' for {}", repr(obj));
            return Err(self.raise_exception(pos, Some("AttributeError"), &msg, None));
        }
        // PARITY: `getattr(obj, attr) is mod` runs without a default, so a missing
        // attribute raises AttributeError (only a DynObj answers None).
        get_attr(obj, attr)
    }

    // --- calls ------------------------------------------------------------------

    fn is_callable(v: &Py) -> bool {
        matches!(
            v,
            Py::Builtin(_) | Py::Type(_) | Py::Method(_) | Py::Proc(_)
        )
    }

    fn on_call(&mut self, node: &Expr, func: &Expr, args: &[Expr], kws: &[Keyword]) -> PyRes<Py> {
        let _ = node;
        let f = self.run_e(func)?;
        if !Self::is_callable(&f) {
            let msg = format!("'{}' is not callable!!", to_str(&f));
            return Err(self.raise_exception(None, Some("TypeError"), &msg, None));
        }
        let mut argv = Vec::with_capacity(args.len());
        for a in args {
            argv.push(self.run_e(a)?);
        }
        let mut keywords: Kwargs = Vec::new();
        for key in kws {
            match &key.arg {
                None => {
                    // keywords.update(self.run(key.value))
                    let m = self.run_e(&key.value)?;
                    let d = super::funcs::call_type_dict(vec![m], vec![]).map_err(|e| {
                        if e.class == "TypeError" {
                            exc("TypeError", "'mapping' object is not iterable")
                        } else {
                            e
                        }
                    })?;
                    if let Py::Dict(d) = d {
                        for (_, (k, v)) in d.borrow().iter() {
                            let Py::Str(k) = k else {
                                return type_err("keywords must be strings");
                            };
                            set_kw(&mut keywords, k.to_string(), v.clone());
                        }
                    }
                }
                Some(name) => {
                    if keywords.iter().any(|(k, _)| k == name) {
                        let msg = format!("keyword argument repeated: {name}");
                        return Err(self.raise_exception(None, Some("SyntaxError"), &msg, None));
                    }
                    let v = self.run_e(&key.value)?;
                    keywords.push((name.clone(), v));
                }
            }
        }
        let is_proc = matches!(f, Py::Proc(_));
        if is_proc {
            self.calldepth += 1;
        }
        let out = self.call_value(&f, argv.clone(), keywords.clone());
        if is_proc {
            self.calldepth -= 1;
        }
        match out {
            Ok(v) => Ok(v),
            Err(ex) => {
                if self.aborted {
                    return Err(ex);
                }
                let func_name = match &f {
                    Py::Builtin("type") => "_type".to_string(),
                    Py::Builtin("open") => "_open".to_string(),
                    Py::Builtin("uuid5") => "_uuid5".to_string(),
                    Py::Builtin("print") => "_printer".to_string(),
                    Py::Builtin(n) | Py::Type(n) => (*n).to_string(),
                    Py::Method(m) => m.1.to_string(),
                    Py::Proc(p) => p.name.clone(),
                    other => to_str(other),
                };
                let kwrepr = {
                    let items: Vec<String> = keywords
                        .iter()
                        .map(|(k, v)| format!("{}: {}", crate::pyfmt::py_repr_str(k), repr(v)))
                        .collect();
                    format!("{{{}}}", items.join(", "))
                };
                let msg = format!(
                    "Error running function '{func_name}' with args '{}' and kwargs {kwrepr}: {}",
                    repr(&Py::list(argv)),
                    ex.message()
                );
                let rc = Rc::new(*ex);
                Err(self.raise_exception(None, None, &msg, Some(&rc)))
            }
        }
    }

    /// Call any callable value (`func(*args, **kwargs)`).
    fn call_inner(&mut self, f: &Py, args: Vec<Py>, kwargs: Kwargs) -> PyRes<Py> {
        match f {
            Py::Builtin("slot") => {
                let name = args.first().cloned().unwrap_or(Py::None);
                if args.len() != 1 {
                    return type_err(format!(
                        "<lambda>() takes 1 positional argument but {} were given",
                        args.len()
                    ));
                }
                let Py::Str(n) = &name else {
                    return Ok(Py::None);
                };
                Ok(self
                    .slot_values
                    .as_ref()
                    .and_then(|m| m.get(&**n))
                    .map_or(Py::None, plain_from_value))
            }
            Py::Builtin(n) => call_builtin(self, n, args, kwargs),
            Py::Type(n) => call_type(self, n, args, kwargs),
            Py::Method(m) => {
                let (recv, name) = (&m.0, m.1);
                call_method(self, recv, name, args, kwargs)
            }
            Py::Proc(p) => self.call_procedure(p, args, kwargs),
            other => type_err(format!("'{}' object is not callable", other.type_name())),
        }
    }

    /// `Procedure.__call__`.
    fn call_procedure(
        &mut self,
        p: &Rc<Procedure>,
        args: Vec<Py>,
        mut kwargs: Kwargs,
    ) -> PyRes<Py> {
        if self.frames + 1 > MAX_FRAMES {
            return Err(exc("RecursionError", "maximum recursion depth exceeded"));
        }
        let mut args = args;
        let mut symlocals: Vec<(String, Py)> = Vec::new();
        let mut nargs = args.len();
        let nargs_expected = p.argnames.len();
        if nargs < nargs_expected && !kwargs.is_empty() {
            for name in &p.argnames[nargs..] {
                if let Some(i) = kwargs.iter().position(|(k, _)| k == name) {
                    args.push(kwargs.remove(i).1);
                }
            }
            nargs = args.len();
        }
        if nargs < nargs_expected {
            let msg = format!(
                "{}() takes at least {nargs_expected} arguments, got {nargs}",
                p.name
            );
            return Err(self.raise_exception(None, Some("TypeError"), &msg, None));
        }
        if !p.argnames.is_empty() {
            for targ in &p.argnames {
                if kwargs.iter().any(|(k, _)| k == targ) {
                    let msg = format!(
                        "multiple values for keyword argument '{targ}' in Procedure {}",
                        p.name
                    );
                    return Err(self.raise_exception(None, Some("TypeError"), &msg, None));
                }
            }
        }
        let vararg = p.def.params.vararg.as_ref();
        if nargs > nargs_expected && vararg.is_none() {
            if nargs - nargs_expected > p.kwargs.len() {
                let msg = format!(
                    "too many arguments for {}() expected at most {}, got {nargs}",
                    p.name,
                    p.kwargs.len() + nargs_expected
                );
                return Err(self.raise_exception(None, Some("TypeError"), &msg, None));
            }
            for (i, xarg) in args[nargs_expected..].iter().enumerate() {
                let kw_name = &p.kwargs[i].0;
                if !kwargs.iter().any(|(k, _)| k == kw_name) {
                    kwargs.push((kw_name.clone(), xarg.clone()));
                }
            }
        }
        for name in &p.argnames {
            symlocals.push((name.clone(), args.remove(0)));
        }
        if let Some(va) = vararg {
            symlocals.push((va.clone(), Py::tuple(args.clone())));
        }
        for (key, val) in &p.kwargs {
            let v = match kwargs.iter().position(|(k, _)| k == key) {
                Some(i) => kwargs.remove(i).1,
                None => val.clone(),
            };
            symlocals.push((key.clone(), v));
        }
        if let Some(kw) = &p.def.params.kwarg {
            let mut d = PyDict::new();
            for (k, v) in kwargs.drain(..) {
                dict_set(&mut d, HKey::Str(k.as_str().into()), Py::str(k.as_str()), v);
            }
            symlocals.push((kw.clone(), Py::Dict(rf(d))));
        } else if !kwargs.is_empty() {
            let names: Vec<&str> = kwargs.iter().map(|(k, _)| k.as_str()).collect();
            let msg = format!(
                "extra keyword arguments for Procedure {}: {}",
                p.name,
                names.join(",")
            );
            let first = self.raise_exception(None, Some("TypeError"), &msg, None);
            // except (ValueError, LookupError, TypeError, NameError, AttributeError)
            let rc = Rc::new(*first);
            let msg2 = format!("incorrect arguments for Procedure {}", p.name);
            return Err(self.raise_exception(None, None, &msg2, Some(&rc)));
        }
        let save_sym = (self.sym.clone(), self.removed.clone());
        for (k, v) in symlocals {
            self.set_symbol(&k, v);
        }
        self.retval = None;
        if self.calldepth == 0 && !self.error.is_empty() {
            self.error = Vec::new();
        }
        self.calldepth += 1;
        self.frames += 1;
        let mut retval = Py::None;
        let def = p.def.clone();
        let body_exprs: Vec<Node<'_>> = if p.is_lambda {
            def.lambda_body.iter().map(Node::E).collect()
        } else {
            def.body.iter().map(Node::S).collect()
        };
        let mut failure: Option<Box<ExcObj>> = None;
        for node in body_exprs {
            match self.run(Some(node), true) {
                Err(e) => {
                    failure = Some(e);
                    break;
                }
                Ok(out) => {
                    if p.is_lambda {
                        retval = out;
                        break;
                    }
                    if !self.error.is_empty() {
                        break;
                    }
                    if let Some(rv) = self.retval.take() {
                        retval = if is_returned_none(&rv) { Py::None } else { rv };
                        break;
                    }
                }
            }
        }
        self.frames -= 1;
        self.sym = save_sym.0;
        self.removed = save_sym.1;
        self.calldepth -= 1;
        self.interrupt = None;
        match failure {
            Some(e) => Err(e),
            None => Ok(retval),
        }
    }

    fn make_procedure(&mut self, def: &Arc<FuncDef>, is_lambda: bool) -> PyRes<Py> {
        let params = &def.params;
        let offset = params.args.len() - params.defaults.len();
        let mut kwargs = Vec::new();
        for (idef, dnode) in params.defaults.iter().enumerate() {
            let defval = self.run_e(dnode)?;
            let keyval = params.args[idef + offset].clone();
            kwargs.push((keyval, defval));
        }
        let argnames: Vec<String> = params.args[..offset].to_vec();
        Ok(Py::Proc(Rc::new(Procedure {
            name: def.name.clone(),
            def: def.clone(),
            argnames,
            kwargs,
            is_lambda,
        })))
    }

    // --- comprehensions ---------------------------------------------------------

    fn names_of_target(t: &Expr, out: &mut Vec<String>) {
        match t {
            Expr::Name(id, _) => out.push(id.clone()),
            Expr::Tuple(elts) => {
                for e in elts {
                    Self::names_of_target(e, out);
                }
            }
            _ => {}
        }
    }

    fn comp_check_names(&mut self, target: &Expr) -> PyRes<Vec<String>> {
        let mut names = Vec::new();
        Self::names_of_target(target, &mut names);
        for n in &names {
            if !valid_symbol_name(n) {
                let msg = format!("invalid symbol name (reserved word?) {n}");
                return Err(self.raise_exception(None, Some("NameError"), &msg, None));
            }
        }
        Ok(names)
    }

    fn on_comp(&mut self, kind: Comp<'_>, gens: &[Comprehension]) -> PyRes<Py> {
        // _comp_save_syms
        let mut saved: Vec<(String, Py)> = Vec::new();
        let mut delete: Vec<String> = Vec::new();
        for g in gens {
            for name in self.comp_check_names(&g.target)? {
                match self.symbol(&name) {
                    Some(v) if self.has_symbol(&name) => saved.push((name, deepcopy(&v))),
                    _ => delete.push(name),
                }
            }
        }
        let mut out_list: Vec<Py> = Vec::new();
        let mut out_dict = PyDict::new();
        self.do_generator(gens, &kind, &mut out_list, &mut out_dict)?;
        for (name, val) in saved {
            self.set_symbol(&name, val);
        }
        for name in delete {
            self.sym.remove(&name);
        }
        Ok(match kind {
            Comp::List(_) => Py::list(out_list),
            Comp::Dict(..) => Py::Dict(rf(out_dict)),
        })
    }

    fn do_generator(
        &mut self,
        gens: &[Comprehension],
        kind: &Comp<'_>,
        out_list: &mut Vec<Py>,
        out_dict: &mut PyDict,
    ) -> PyRes<()> {
        let g = &gens[0];
        self.comp_check_names(&g.target)?;
        if self.frames + 1 > MAX_FRAMES {
            return Err(exc("RecursionError", "maximum recursion depth exceeded"));
        }
        self.frames += 1;
        let r = self.do_generator_inner(gens, kind, out_list, out_dict);
        self.frames -= 1;
        r
    }

    fn do_generator_inner(
        &mut self,
        gens: &[Comprehension],
        kind: &Comp<'_>,
        out_list: &mut Vec<Py>,
        out_dict: &mut PyDict,
    ) -> PyRes<()> {
        let g = &gens[0];
        let iterable = self.run_e(&g.iter)?;
        let mut cursor = iterate(&iterable)?;
        while let Some(val) = cursor.next() {
            self.unpack_to_target(&g.target, val)?;
            let mut add = true;
            for cond in &g.ifs {
                add = add && self.run_e(cond)?.truthy();
                if !add {
                    break;
                }
            }
            if add {
                if gens.len() > 1 {
                    self.do_generator(&gens[1..], kind, out_list, out_dict)?;
                } else {
                    match kind {
                        Comp::List(elt) => {
                            let v = self.run_e(elt)?;
                            out_list.push(v);
                        }
                        Comp::Dict(k, v) => {
                            let key = self.run_e(k)?;
                            let val = self.run_e(v)?;
                            dict_set(out_dict, hkey(&key)?, key, val);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// `_unpack_to_target`: only names and (nested) tuples of names assign.
    fn unpack_to_target(&mut self, target: &Expr, value: Py) -> PyRes<()> {
        match target {
            Expr::Name(id, _) => {
                self.set_symbol(id, value);
                Ok(())
            }
            Expr::Tuple(elts) => {
                let items = to_vec(&value)?;
                for (t, v) in elts.iter().zip(items) {
                    self.unpack_to_target(t, v)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    // --- assignment ---------------------------------------------------------------

    /// `node_assign`.
    fn node_assign(&mut self, target: &Expr, val: Py) -> PyRes<()> {
        match target {
            Expr::Name(id, pos) => {
                if !valid_symbol_name(id) {
                    let msg = format!("invalid symbol name (reserved word?) {id}");
                    return Err(self.raise_exception(Some(*pos), Some("NameError"), &msg, None));
                }
                self.set_symbol(id, val);
                Ok(())
            }
            Expr::Attribute(value, attr) => {
                let obj = self.run_e(value)?;
                if unsafe_attr(attr) {
                    let msg = format!("cannot assign to unsafe attribute {attr}");
                    return Err(self.raise_exception(None, Some("AttributeError"), &msg, None));
                }
                set_attr(&obj, attr, val)
            }
            Expr::Subscript(value, slice) => {
                let obj = self.run_e(value)?;
                let key = self.run_e(slice)?;
                setitem(&obj, &key, val)
            }
            Expr::Tuple(elts) | Expr::List(elts) => {
                let n = match &val {
                    Py::Str(s) => s.chars().count(),
                    other => py_len(other)? as usize,
                };
                if n == elts.len() {
                    let items = to_vec(&val)?;
                    for (t, v) in elts.iter().zip(items) {
                        self.node_assign(t, v)?;
                    }
                    Ok(())
                } else {
                    value_err("too many values to unpack")
                }
            }
            // Starred and anything else: silently ignored.
            _ => Ok(()),
        }
    }

    // --- statements ---------------------------------------------------------------

    fn handle_stmt(&mut self, s: &Stmt) -> PyRes<Py> {
        match s {
            Stmt::Expr(e) => self.run_e(e),
            Stmt::Assign(targets, value) => {
                let val = self.run_e(value)?;
                for t in targets {
                    self.node_assign(t, val.clone())?;
                }
                Ok(Py::None)
            }
            Stmt::AugAssign(target, op, value) => {
                // on_assign(Assign([target], BinOp(target, op, value)))
                let left = self.aug_target_value(target)?;
                let right = self.run_e(value)?;
                let val = binop(*op, &left, &right)?;
                self.node_assign(target, val)?;
                Ok(Py::None)
            }
            Stmt::Return(value) => {
                if self.calldepth == 0 {
                    return Err(exc("SyntaxError", "cannot return at top level"));
                }
                let v = self.run_opt(value.as_ref())?;
                self.retval = Some(if v.is_none() { returned_none() } else { v });
                self.interrupt = Some(Interrupt::Return);
                Ok(Py::None)
            }
            Stmt::Delete(targets) => self.on_delete(targets),
            Stmt::Pass => Ok(Py::None),
            Stmt::Break => {
                self.interrupt = Some(Interrupt::Break);
                Ok(Py::Ast("Break"))
            }
            Stmt::Continue => {
                self.interrupt = Some(Interrupt::Continue);
                Ok(Py::Ast("Continue"))
            }
            Stmt::If(test, body, orelse) => {
                let t = self.run_e(test)?;
                let block = if t.truthy() { body } else { orelse };
                for tnode in block {
                    self.run_s(tnode)?;
                }
                Ok(Py::None)
            }
            Stmt::While(test, body, orelse) => self.on_while(test, body, orelse),
            Stmt::For(target, iter, body, orelse) => self.on_for(target, iter, body, orelse),
            Stmt::With(items, body) => self.on_with(items, body),
            Stmt::Raise(exc_node, cause) => self.on_raise(exc_node.as_ref(), cause.as_ref()),
            Stmt::Try(body, handlers, orelse, finalbody) => {
                self.on_try(body, handlers, orelse, finalbody)
            }
            Stmt::Assert(test, msg) => self.on_assert(test, msg.as_ref()),
            Stmt::FunctionDef(def) => self.on_functiondef(def),
            Stmt::Unsupported(kind) => match *kind {
                "Import" | "ImportFrom" => {
                    let msg = format!("{kind} not supported");
                    Err(self.raise_exception(None, Some("NotImplementedError"), &msg, None))
                }
                other => Err(self.not_in_handlers(other)),
            },
        }
    }

    /// The `left` operand `on_augassign` builds: the target itself, in Store
    /// context.
    fn aug_target_value(&mut self, target: &Expr) -> PyRes<Py> {
        match target {
            Expr::Attribute(..) => {
                let msg = "attribute for storage: shouldn't be here!";
                Err(self.raise_exception(None, Some("RuntimeError"), msg, None))
            }
            other => self.run_e(other),
        }
    }

    fn on_while(&mut self, test: &Expr, body: &[Stmt], orelse: &[Stmt]) -> PyRes<Py> {
        let mut broke = false;
        while self.run_e(test)?.truthy() {
            self.interrupt = None;
            for t in body {
                self.run_s(t)?;
                if self.interrupt.is_some() {
                    break;
                }
            }
            if matches!(self.interrupt, Some(Interrupt::Break | Interrupt::Return)) {
                broke = true;
                break;
            }
        }
        if !broke {
            for t in orelse {
                self.run_s(t)?;
            }
        }
        self.interrupt = None;
        Ok(Py::None)
    }

    fn on_for(&mut self, target: &Expr, iter: &Expr, body: &[Stmt], orelse: &[Stmt]) -> PyRes<Py> {
        let iterable = self.run_e(iter)?;
        let mut cursor = iterate(&iterable)?;
        let mut broke = false;
        while let Some(val) = cursor.next() {
            self.tick()?;
            self.node_assign(target, val)?;
            self.interrupt = None;
            for t in body {
                self.run_s(t)?;
                if self.interrupt.is_some() {
                    break;
                }
            }
            if matches!(self.interrupt, Some(Interrupt::Break | Interrupt::Return)) {
                broke = true;
                break;
            }
        }
        if !broke {
            for t in orelse {
                self.run_s(t)?;
            }
        }
        self.interrupt = None;
        Ok(Py::None)
    }

    fn on_with(&mut self, items: &[WithItem], _body: &[Stmt]) -> PyRes<Py> {
        if let Some(item) = items.iter().next() {
            let ctx = self.run_e(&item.context_expr)?;
            // No value `asteval` can hold has `__enter__`.
            let msg = format!(
                "'<class '{}'>' object does not support the context manager protocol",
                ctx.type_name()
            );
            return type_err(msg);
        }
        Ok(Py::None)
    }

    fn on_raise(&mut self, excnode: Option<&Expr>, cause: Option<&Expr>) -> PyRes<Py> {
        let out = self.run_opt(excnode)?;
        // msg = ' '.join(out.args)
        let msg = match &out {
            Py::Exc(e) => {
                let strs: Option<Vec<String>> = e
                    .args
                    .iter()
                    .map(|a| {
                        if let Py::Str(s) = a {
                            Some(s.to_string())
                        } else {
                            None
                        }
                    })
                    .collect();
                match strs {
                    Some(v) => v.join(" "),
                    None => " ".to_string(),
                }
            }
            Py::None => {
                return Err(exc(
                    "AttributeError",
                    "'NoneType' object has no attribute 'args'",
                ));
            }
            Py::Type(_) => " ".to_string(),
            other => {
                return Err(exc(
                    "AttributeError",
                    format!("'{}' object has no attribute 'args'", other.type_name()),
                ));
            }
        };
        let msg2 = self.run_opt(cause)?;
        let mut msg = msg;
        if !(msg2.is_none() || matches!(&msg2, Py::Str(s) if &**s == "None")) {
            match &msg2 {
                Py::Str(s) => msg = format!("{msg}: {s}"),
                other => {
                    return type_err(format!(
                        "unsupported format string passed to {}.__format__",
                        other.type_name()
                    ));
                }
            }
        }
        match &out {
            Py::Exc(e) => Err(self.raise_exception(None, Some(e.class), &msg, None)),
            Py::Type(name) => {
                let m = format!("asteval will not raise {name}");
                Err(self.raise_exception(None, Some("RuntimeError"), &m, None))
            }
            _ => unreachable!("handled above"),
        }
    }

    fn on_assert(&mut self, test: &Expr, msg: Option<&Expr>) -> PyRes<Py> {
        if !self.run_e(test)?.truthy() {
            let m = match msg {
                None => String::new(),
                Some(Expr::Constant(c)) => match c {
                    Const::Str(s) => s.to_string(),
                    Const::Int(i) => i.to_string(),
                    Const::Float(f) => crate::pyfmt::py_float_repr(*f),
                    Const::Bool(true) => "True".into(),
                    Const::Bool(false) => "False".into(),
                    Const::None => "None".into(),
                    _ => String::new(),
                },
                Some(Expr::Attribute(..) | Expr::Subscript(..) | Expr::Starred(_)) => {
                    "<ast node object>".to_string()
                }
                Some(other) => {
                    let kind = match other {
                        Expr::Name(..) => "Name",
                        Expr::Call(..) => "Call",
                        Expr::BinOp(..) => "BinOp",
                        Expr::JoinedStr(_) => "JoinedStr",
                        _ => "expr",
                    };
                    return Err(exc(
                        "AttributeError",
                        format!("'{kind}' object has no attribute 'value'"),
                    ));
                }
            };
            return Err(self.raise_exception(None, Some("AssertionError"), &m, None));
        }
        Ok(Py::Bool(true))
    }

    fn on_try(
        &mut self,
        body: &[Stmt],
        handlers: &[Handler],
        orelse: &[Stmt],
        finalbody: &[Stmt],
    ) -> PyRes<Py> {
        let mut no_errors = true;
        for tnode in body {
            self.run(Some(Node::S(tnode)), false)?;
            no_errors = no_errors && self.error.is_empty();
            if !self.error.is_empty() {
                let last = self.error.last().cloned().expect("non-empty");
                for hnd in handlers {
                    // htype = __builtins__.get(hnd.type.id, None)
                    let htype: Option<Py> = match &hnd.type_ {
                        None => None,
                        Some(Expr::Name(id, _)) => builtin_for_except(id),
                        Some(_) => {
                            return Err(exc(
                                "AttributeError",
                                "'Tuple' object has no attribute 'id'",
                            ));
                        }
                    };
                    let matched = match &htype {
                        None => true,
                        Some(h) => match &last.exc_info {
                            // e_type() is the exception class instantiated.
                            Some(info) => {
                                let inst = Py::Exc(Rc::new(ExcObj {
                                    class: info.class,
                                    args: vec![],
                                }));
                                super::funcs::isinstance(&inst, h)?
                            }
                            None => return type_err("'NoneType' object is not callable"),
                        },
                    };
                    if matched {
                        self.error = Vec::new();
                        if let Some(name) = &hnd.name {
                            let v = last
                                .exc_info
                                .as_ref()
                                .map_or(Py::None, |i| Py::Exc(i.clone()));
                            self.set_symbol(name, v);
                        }
                        for tline in &hnd.body {
                            self.run_s(tline)?;
                        }
                        break;
                    }
                }
                break;
            }
        }
        if no_errors {
            for tnode in orelse {
                self.run_s(tnode)?;
            }
        }
        for tnode in finalbody {
            self.run_s(tnode)?;
        }
        Ok(Py::None)
    }

    fn on_functiondef(&mut self, def: &Arc<FuncDef>) -> PyRes<Py> {
        if def.has_decorators {
            return Err(exc("Warning", "decorated procedures not supported!"));
        }
        if !valid_symbol_name(&def.name) {
            let msg = format!("invalid function name (reserved word?) {}", def.name);
            return Err(self.raise_exception(None, Some("NameError"), &msg, None));
        }
        let proc = self.make_procedure(def, false)?;
        self.set_symbol(&def.name, proc);
        Ok(Py::None)
    }

    /// `on_delete`.
    fn on_delete(&mut self, targets: &[Expr]) -> PyRes<Py> {
        for t in targets {
            let mut children: Vec<String> = Vec::new();
            let mut node = t;
            while let Expr::Attribute(v, attr) = node {
                children.push(attr.clone());
                node = v;
            }
            match node {
                Expr::Name(id, _) => {
                    children.push(id.clone());
                    children.reverse();
                    let key = children.join(".");
                    if self.has_symbol(&key) {
                        if self.sym.remove(&key).is_none() {
                            self.removed.insert(key);
                        }
                    } else {
                        return Err(Box::new(ExcObj {
                            class: "KeyError",
                            args: vec![Py::str(key)],
                        }));
                    }
                }
                Expr::Subscript(value, slice) => {
                    let nslice = self.run_e(slice)?;
                    let mut children: Vec<String> = Vec::new();
                    let mut node: &Expr = value;
                    while let Expr::Attribute(v, attr) = node {
                        children.push(attr.clone());
                        node = v;
                    }
                    if let Expr::Name(id, _) = node {
                        children.push(id.clone());
                        children.reverse();
                        let sname = children.join(".");
                        let val = self.eval_dotted(&children)?;
                        delitem(&val, &nslice)?;
                        if children.len() == 1 {
                            self.set_symbol(&sname, val);
                        } else {
                            let mut child = self.symbol(&children[0]).ok_or_else(|| {
                                Box::new(ExcObj {
                                    class: "KeyError",
                                    args: vec![Py::str(children[0].as_str())],
                                })
                            })?;
                            for cname in &children[1..children.len() - 1] {
                                child = getitem(&child, &Py::str(cname.as_str()))?;
                            }
                            set_attr(&child, &children[children.len() - 1], val)?;
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(Py::None)
    }

    /// `self.run('a.b.c')`: a nested `eval` of a dotted name, which also clears
    /// the error list.
    fn eval_dotted(&mut self, children: &[String]) -> PyRes<Py> {
        self.error.clear();
        self.error_msg = None;
        let mut v = match self.symbol(&children[0]) {
            Some(v) => v,
            None => {
                let msg = format!("name '{}' is not defined", children[0]);
                return Err(self.raise_exception(None, Some("NameError"), &msg, None));
            }
        };
        for attr in &children[1..] {
            v = self.safe_getattr(&v, attr, None)?;
        }
        Ok(v)
    }
}

enum Comp<'a> {
    List(&'a Expr),
    Dict(&'a Expr, &'a Expr),
}

fn set_kw(kws: &mut Kwargs, k: String, v: Py) {
    if let Some(slot) = kws.iter_mut().find(|(n, _)| *n == k) {
        slot.1 = v;
    } else {
        kws.push((k, v));
    }
}

/// `__builtins__.get(name)` for an `except` clause: only Python's real
/// builtins resolve (classes, functions); anything else (including `asteval`'s
/// math functions) is `None`, which the handler treats as a catch-all.
fn builtin_for_except(name: &str) -> Option<Py> {
    const CLASSES: [&str; 26] = [
        "bool",
        "bytearray",
        "bytes",
        "classmethod",
        "complex",
        "dict",
        "enumerate",
        "filter",
        "float",
        "frozenset",
        "int",
        "list",
        "map",
        "memoryview",
        "object",
        "property",
        "range",
        "reversed",
        "set",
        "slice",
        "staticmethod",
        "str",
        "super",
        "tuple",
        "type",
        "zip",
    ];
    const FUNCS: [&str; 42] = [
        "abs",
        "aiter",
        "all",
        "anext",
        "any",
        "ascii",
        "bin",
        "breakpoint",
        "callable",
        "chr",
        "compile",
        "delattr",
        "dir",
        "divmod",
        "eval",
        "exec",
        "format",
        "getattr",
        "globals",
        "hasattr",
        "hash",
        "help",
        "hex",
        "id",
        "input",
        "isinstance",
        "issubclass",
        "iter",
        "len",
        "locals",
        "max",
        "min",
        "next",
        "oct",
        "open",
        "ord",
        "pow",
        "print",
        "repr",
        "round",
        "setattr",
        "sorted",
    ];
    if let Some(c) = exc_class(name) {
        return Some(Py::Type(c));
    }
    if let Some(c) = CLASSES.iter().find(|c| **c == name) {
        return Some(Py::Type(c));
    }
    if let Some(f) = FUNCS.iter().find(|f| **f == name) {
        return Some(Py::Builtin(f));
    }
    if name == "sum" {
        return Some(Py::Builtin("sum"));
    }
    if name == "vars" {
        return Some(Py::Builtin("vars"));
    }
    None
}

impl Caller for Interp<'_> {
    fn call_value(&mut self, f: &Py, args: Vec<Py>, kwargs: Kwargs) -> PyRes<Py> {
        self.call_inner(f, args, kwargs)
    }
}

fn ascii_repr(v: &Py) -> String {
    let r = repr(v);
    let mut out = String::new();
    for c in r.chars() {
        if c.is_ascii() {
            out.push(c);
        } else if (c as u32) < 0x100 {
            out.push_str(&format!("\\x{:02x}", c as u32));
        } else if (c as u32) < 0x10000 {
            out.push_str(&format!("\\u{:04x}", c as u32));
        } else {
            out.push_str(&format!("\\U{:08x}", c as u32));
        }
    }
    out
}

#[allow(dead_code)]
fn _unused(_: HashSet<String>) {}
