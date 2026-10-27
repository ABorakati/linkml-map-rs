//! Expression-level differential-fuzzing oracle.
//!
//! This is the Rust half of `tests/differential/expr/` (the Python half drives
//! upstream `linkml_map.utils.eval_utils`/asteval). One JSON object per line goes
//! in, one comes out, in the same order:
//!
//! ```text
//! {"id": 1, "expr": "{a} + 1", "bindings": {"a": 2}, "mode": "restricted"}
//!   -> {"id": 1, "ok": 3}
//! {"id": 2, "expr": "1/0", "bindings": {}, "mode": "restricted"}
//!   -> {"id": 2, "err": "eval", "message": "..."}
//! {"id": 3, "leaf": "py_eq", "args": [1, {"$f": "3ff0000000000000"}]}
//!   -> {"id": 3, "ok": true}
//! ```
//!
//! # Value encoding (shared with the Python driver)
//!
//! `null`, booleans, strings and arrays are native JSON; an `int` is a JSON
//! integer (`{"$i": "<digits>"}` outside the `i64` range); a `float` is
//! `{"$f": "<16 hex digits of the IEEE-754 bits>"}` so NaN, infinities, `-0.0`
//! and every last bit survive; a map is a JSON object (keys never start with
//! `$`). The Python side additionally emits `{"$tuple": [...]}`, `{"$set":
//! [...]}`, `{"$dict": [[k, v], ...]}` and `{"$other": "<type>"}` for results
//! the Rust [`Value`] model has no counterpart for; the Rust side emits
//! `{"$dec": "<text>"}` for a decimal.
//!
//! The expression modes are the entry points the engine uses for a slot
//! `expr:`: `restricted` is `eval_expr_with_mapping`, `unrestricted` is the
//! same call with the engine's `src` binding (the whole source object) added.

use std::cell::RefCell;
use std::io::{BufRead, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};

use indexmap::IndexMap;
use linkml_map_core::decimal::Decimal;
use linkml_map_core::expr::{
    Bindings, ExprError, eval_expr_with_mapping, eval_expr_with_mapping_strict,
};
use linkml_map_core::pyfmt;
use linkml_map_core::value::{Value, parse_python_float};
use serde_json::{Value as J, json};

/// How a case failed (or that it could not be run at all).
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Evaluation produced a value.
    Ok(Value),
    /// Evaluation raised; `kind` is `lex`/`parse`/`eval`/`undefined`/`panic`/...
    Err {
        /// Error class.
        kind: String,
        /// Human-readable message.
        message: String,
    },
}

// ─── Value <-> JSON ──────────────────────────────────────────────────────────

/// Decode the shared JSON encoding into a [`Value`].
///
/// # Errors
/// A message naming the problem for an encoding the Rust `Value` cannot hold
/// (an integer outside `i64`, or an unknown `$` tag).
pub fn decode(j: &J) -> Result<Value, String> {
    Ok(match j {
        J::Null => Value::Null,
        J::Bool(b) => Value::Bool(*b),
        J::Number(n) => match n.as_i64() {
            Some(i) => Value::Int(i),
            None => return Err(format!("unrepresentable integer {n}")),
        },
        J::String(s) => Value::Str(s.clone()),
        J::Array(a) => Value::List(a.iter().map(decode).collect::<Result<_, _>>()?),
        J::Object(m) => {
            if let Some(h) = m.get("$f") {
                let bits = u64::from_str_radix(h.as_str().unwrap_or(""), 16)
                    .map_err(|e| format!("bad $f: {e}"))?;
                return Ok(Value::Float(f64::from_bits(bits)));
            }
            if let Some(i) = m.get("$i") {
                return match i.as_str().and_then(|s| s.parse::<i64>().ok()) {
                    Some(v) => Ok(Value::Int(v)),
                    None => Err(format!("unrepresentable integer {i}")),
                };
            }
            if m.keys().any(|k| k.starts_with('$')) {
                return Err("unknown $ tag".to_owned());
            }
            Value::Map(
                m.iter()
                    .map(|(k, v)| Ok((k.clone(), decode(v)?)))
                    .collect::<Result<IndexMap<_, _>, String>>()?,
            )
        }
    })
}

/// Encode a [`Value`] in the shared JSON encoding.
pub fn encode(v: &Value) -> J {
    match v {
        Value::Null => J::Null,
        Value::Bool(b) => J::Bool(*b),
        Value::Int(i) => J::from(*i),
        Value::Float(f) => json!({"$f": format!("{:016x}", f.to_bits())}),
        Value::Decimal(d) => json!({"$dec": d.to_sci_string()}),
        Value::Str(s) => J::String(s.clone()),
        Value::List(l) => J::Array(l.iter().map(encode).collect()),
        Value::Map(m) => J::Object(m.iter().map(|(k, v)| (k.clone(), encode(v))).collect()),
    }
}

// ─── Expression cases ────────────────────────────────────────────────────────

fn err_kind(e: &ExprError) -> &'static str {
    match e {
        ExprError::Lex(_) => "lex",
        ExprError::Parse(_) => "parse",
        ExprError::Eval(_) => "eval",
        ExprError::Undefined { .. } => "undefined",
        #[allow(unreachable_patterns)]
        _ => "other",
    }
}

/// Evaluate one expression case the way the engine evaluates a slot `expr:`.
pub fn eval_case(expr: &str, bindings: &J, mode: &str, strict: bool) -> Outcome {
    let given = match decode(bindings) {
        Ok(Value::Map(m)) => m,
        Ok(_) => return fail("input", "bindings must be an object".into()),
        Err(e) => return fail("unrepresentable", e),
    };
    let mut vars: Bindings = IndexMap::new();
    vars.insert("NULL".to_owned(), Value::Null);
    for (k, v) in &given {
        vars.insert(k.clone(), v.clone());
    }
    if mode == "unrestricted" {
        // The engine binds the whole source object as `src` (asteval's
        // `usersyms={"src": ctxt_obj}`).
        vars.insert("src".to_owned(), Value::Map(given));
    }
    let run = || {
        if strict {
            eval_expr_with_mapping_strict(expr, &vars)
        } else {
            eval_expr_with_mapping(expr, &vars)
        }
    };
    guarded(|| match run() {
        Ok(v) => Outcome::Ok(v),
        Err(e) => fail(err_kind(&e), e.to_string()),
    })
}

fn fail(kind: &str, message: String) -> Outcome {
    Outcome::Err {
        kind: kind.to_owned(),
        message,
    }
}

thread_local! {
    static PANIC_MSG: RefCell<String> = const { RefCell::new(String::new()) };
}

/// Run `f`, turning a panic into an `Outcome::Err { kind: "panic" }`.
fn guarded(f: impl FnOnce() -> Outcome) -> Outcome {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(o) => o,
        Err(_) => fail("panic", PANIC_MSG.with(|m| m.borrow().clone())),
    }
}

// ─── Leaf cases ──────────────────────────────────────────────────────────────

fn s(args: &[Value], i: usize) -> Result<&str, String> {
    match args.get(i) {
        Some(Value::Str(x)) => Ok(x),
        other => Err(format!("arg {i}: expected str, got {other:?}")),
    }
}

fn io_value(v: &Value) -> Value {
    v.clone()
}

/// Run a leaf-function case against the Rust implementation.
///
/// Leaf names starting `fn:` run the named expression builtin through the
/// evaluator as `name(a0, a1, ...)`; everything else calls a public helper
/// directly.
pub fn leaf_case(name: &str, args_json: &[J]) -> Outcome {
    let mut args = Vec::with_capacity(args_json.len());
    for a in args_json {
        match decode(a) {
            Ok(v) => args.push(v),
            Err(e) => return fail("unrepresentable", e),
        }
    }
    guarded(|| match leaf_inner(name, &args) {
        Ok(v) => Outcome::Ok(v),
        Err((kind, msg)) => fail(&kind, msg),
    })
}

type LeafErr = (String, String);

fn bad(e: String) -> LeafErr {
    ("input".to_owned(), e)
}

fn leaf_inner(name: &str, args: &[Value]) -> Result<Value, LeafErr> {
    use linkml_map_io::pycompat;
    if let Some(func) = name.strip_prefix("fn:") {
        let mut vars: Bindings = IndexMap::new();
        let mut names = Vec::new();
        for (i, a) in args.iter().enumerate() {
            vars.insert(format!("a{i}"), a.clone());
            names.push(format!("a{i}"));
        }
        let expr = format!("{func}({})", names.join(", "));
        return eval_expr_with_mapping(&expr, &vars).map_err(|e| (err_kind(&e).to_owned(), e.to_string()));
    }
    Ok(match name {
        "py_eq" => Value::Bool(args[0].py_eq(&args[1])),
        "truthy" => Value::Bool(args[0].is_truthy()),
        "try_numeric" => args[0]
            .try_numeric()
            .map_or(Value::Null, Value::Float),
        "parse_float" => parse_python_float(s(args, 0).map_err(bad)?).map_or(Value::Null, Value::Float),
        "float_repr" => match &args[0] {
            Value::Float(f) => Value::Str(pyfmt::py_float_repr(*f)),
            o => return Err(bad(format!("expected float, got {o:?}"))),
        },
        "float_repr_io" => match &args[0] {
            Value::Float(f) => Value::Str(pycompat::py_float_repr(*f)),
            o => return Err(bad(format!("expected float, got {o:?}"))),
        },
        "repr_str" => Value::Str(pyfmt::py_repr_str(s(args, 0).map_err(bad)?)),
        "py_repr" => Value::Str(pycompat::py_repr(&io_value(&args[0]))),
        "py_str" => Value::Str(pycompat::py_str(&io_value(&args[0]))),
        "json_dumps" => {
            let indent = match args.get(1) {
                Some(Value::Int(n)) if *n >= 0 => Some(*n as usize),
                _ => None,
            };
            Value::Str(pycompat::dumps_json(&args[0], indent, false))
        }
        "json_dumps_skipnull" => {
            let indent = match args.get(1) {
                Some(Value::Int(n)) if *n >= 0 => Some(*n as usize),
                _ => None,
            };
            Value::Str(pycompat::dumps_json(&args[0], indent, true))
        }
        "decimal" => {
            let text = s(args, 0).map_err(bad)?;
            match Decimal::parse_python(text) {
                Ok(d) => {
                    let n = d.normalize();
                    let mut m = IndexMap::new();
                    m.insert("sci".into(), Value::Str(d.to_sci_string()));
                    m.insert("fixed".into(), Value::Str(d.to_fixed_string()));
                    m.insert("norm_sci".into(), Value::Str(n.to_sci_string()));
                    m.insert("zero".into(), Value::Bool(d.is_zero()));
                    m.insert("neg".into(), Value::Bool(d.is_negative()));
                    m.insert("f64".into(), Value::Float(d.to_f64()));
                    Value::Map(m)
                }
                Err(e) => return Err((format!("{e:?}").to_lowercase(), format!("{e:?}"))),
            }
        }
        other => return Err(("input".to_owned(), format!("unknown leaf {other}"))),
    })
}

// ─── JSONL driver ────────────────────────────────────────────────────────────

/// Handle one request line, returning the response object.
pub fn handle_line(line: &str) -> J {
    let req: J = match serde_json::from_str(line) {
        Ok(r) => r,
        Err(e) => return json!({"id": null, "err": "input", "message": e.to_string()}),
    };
    let id = req.get("id").cloned().unwrap_or(J::Null);
    let outcome = if let Some(leaf) = req.get("leaf").and_then(J::as_str) {
        let args: Vec<J> = req
            .get("args")
            .and_then(J::as_array)
            .cloned()
            .unwrap_or_default();
        leaf_case(leaf, &args)
    } else {
        let expr = req.get("expr").and_then(J::as_str).unwrap_or("");
        let mode = req.get("mode").and_then(J::as_str).unwrap_or("restricted");
        let strict = req.get("strict").and_then(J::as_bool).unwrap_or(false);
        let empty = J::Object(Default::default());
        let bindings = req.get("bindings").unwrap_or(&empty);
        eval_case(expr, bindings, mode, strict)
    };
    match outcome {
        Outcome::Ok(v) => json!({"id": id, "ok": encode(&v)}),
        Outcome::Err { kind, message } => json!({"id": id, "err": kind, "message": message}),
    }
}

/// Install a panic hook that records the message instead of printing it.
pub fn install_quiet_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let msg = match info.payload().downcast_ref::<&str>() {
            Some(s) => (*s).to_owned(),
            None => info
                .payload()
                .downcast_ref::<String>()
                .cloned()
                .unwrap_or_default(),
        };
        let loc = info
            .location()
            .map(|l| format!(" at {}:{}", l.file(), l.line()))
            .unwrap_or_default();
        PANIC_MSG.with(|m| *m.borrow_mut() = format!("{msg}{loc}"));
    }));
}

/// Serve JSONL requests from `input` to `output`, flushing after every line so
/// a crash leaves a precise record of how far the batch got.
///
/// A watchdog thread answers a case that runs longer than `limit` with
/// `{"err": "hang"}` and exits the process (the driver reruns the remainder),
/// so one runaway expression cannot stall a campaign.
///
/// # Errors
/// I/O errors on either stream.
pub fn serve<W: Write + Send + 'static>(
    input: impl BufRead,
    output: W,
    limit: std::time::Duration,
) -> std::io::Result<()> {
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    let output = Arc::new(Mutex::new(output));
    // (id of the running case, start time); None while idle.
    let current: Arc<Mutex<Option<(J, Instant)>>> = Arc::new(Mutex::new(None));
    {
        let output = Arc::clone(&output);
        let current = Arc::clone(&current);
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(std::time::Duration::from_millis(50));
                let stuck = current
                    .lock()
                    .ok()
                    .and_then(|c| c.as_ref().filter(|(_, t)| t.elapsed() > limit).map(|(id, _)| id.clone()));
                if let Some(id) = stuck {
                    if let Ok(mut out) = output.lock() {
                        let _ = writeln!(out, "{}", json!({"id": id, "err": "hang", "message": "exceeded watchdog limit"}));
                        let _ = out.flush();
                    }
                    std::process::exit(0);
                }
            }
        });
    }
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let id = serde_json::from_str::<J>(&line)
            .ok()
            .and_then(|r| r.get("id").cloned())
            .unwrap_or(J::Null);
        if let Ok(mut c) = current.lock() {
            *c = Some((id, Instant::now()));
        }
        let resp = handle_line(&line);
        if let Ok(mut c) = current.lock() {
            *c = None;
        }
        let mut out = output.lock().map_err(|_| std::io::Error::other("poisoned"))?;
        writeln!(out, "{resp}")?;
        out.flush()?;
    }
    Ok(())
}
