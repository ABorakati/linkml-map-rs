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
//! and every last bit survive; a `complex` is `{"$c": ["<re bits>", "<im bits>"]}`; a map is a JSON object (keys never start with
//! `$`). The Python side additionally emits `{"$tuple": [...]}`, `{"$set":
//! [...]}`, `{"$dict": [[k, v], ...]}` and `{"$other": "<type>"}` for results
//! the Rust [`Value`] model has no counterpart for; the Rust side emits
//! `{"$dec": "<text>"}` for a decimal.
//!
//! The expression modes are the entry points the engine uses for a slot
//! `expr:`: `restricted` and `unrestricted` are `eval_parsed_gated` with
//! `EvalMode { unrestricted: false / true }` (upstream `ObjectTransformer._eval_expr`).

use std::cell::RefCell;
use std::io::{BufRead, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};

use indexmap::IndexMap;
use linkml_map_core::decimal::Decimal;
use linkml_map_core::expr::{
    Bindings, EvalMode, ExprError, eval_expr_with_mapping, eval_parsed_gated, parse_expr,
};
use linkml_map_core::pyfmt;
use linkml_map_core::pynum::BigInt;
use linkml_map_core::pynum::complex::Complex64;
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
                return match i.as_str().and_then(|s| s.parse::<BigInt>().ok()) {
                    Some(v) => Ok(Value::from_bigint(v)),
                    None => Err(format!("unrepresentable integer {i}")),
                };
            }
            if let Some(parts) = m.get("$c").and_then(J::as_array) {
                let bits = |j: Option<&J>| {
                    j.and_then(J::as_str)
                        .and_then(|h| u64::from_str_radix(h, 16).ok())
                        .map(f64::from_bits)
                        .ok_or_else(|| "bad $c".to_owned())
                };
                let (re, im) = (bits(parts.first())?, bits(parts.get(1))?);
                return Ok(Value::from_complex(Complex64::new(re, im)));
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
        Value::BigInt(n) => json!({"$i": n.to_string()}),
        Value::Float(f) => json!({"$f": format!("{:016x}", f.to_bits())}),
        Value::Complex(z) => json!({"$c": [
            format!("{:016x}", z.re.to_bits()),
            format!("{:016x}", z.im.to_bits()),
        ]}),
        Value::Decimal(d) => json!({"$dec": d.to_sci_string()}),
        Value::Temporal(t) => json!({"$temporal": t.to_string()}),
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
    // The engine's slot-expression path: bindings are `NULL` plus the source
    // slots (no `src`; the unrestricted gate builds it), evaluated through the
    // gate with upstream's `strict` / `unrestricted_eval` settings.
    let eval_mode = EvalMode {
        strict,
        unrestricted: mode == "unrestricted",
    };
    guarded(|| {
        let outcome =
            parse_expr(expr).and_then(|parsed| eval_parsed_gated(&parsed, &vars, eval_mode));
        match outcome {
            Ok(v) => Outcome::Ok(v),
            // A result Python can produce but `Value` cannot hold (a bound
            // method, bytes): not comparable, not a divergence.
            Err(e) if e.is_unrepresentable() => fail("unrepresentable", e.to_string()),
            Err(e) => fail(err_kind(&e), e.to_string()),
        }
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
        return eval_expr_with_mapping(&expr, &vars)
            .map_err(|e| (err_kind(&e).to_owned(), e.to_string()));
    }
    Ok(match name {
        "py_eq" => Value::Bool(args[0].py_eq(&args[1])),
        "truthy" => Value::Bool(args[0].is_truthy()),
        "try_numeric" => args[0].try_numeric().map_or(Value::Null, Value::Float),
        "parse_float" => {
            parse_python_float(s(args, 0).map_err(bad)?).map_or(Value::Null, Value::Float)
        }
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
                    // Python's `normalize()` raises `decimal.Overflow` past Emax.
                    let n = d
                        .normalize()
                        .map_err(|e| ("Overflow".to_owned(), format!("{e:?}")))?;
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
                let stuck = current.lock().ok().and_then(|c| {
                    c.as_ref()
                        .filter(|(_, t)| t.elapsed() > limit)
                        .map(|(id, _)| id.clone())
                });
                if let Some(id) = stuck {
                    if let Ok(mut out) = output.lock() {
                        let _ = writeln!(
                            out,
                            "{}",
                            json!({"id": id, "err": "hang", "message": "exceeded watchdog limit"})
                        );
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
        let mut out = output
            .lock()
            .map_err(|_| std::io::Error::other("poisoned"))?;
        writeln!(out, "{resp}")?;
        out.flush()?;
    }
    Ok(())
}

// ─── Replay against stored upstream results ──────────────────────────────────

/// Strict, Python-flavoured equality for comparing a Rust result with a stored
/// upstream one: `1`, `1.0` and `True` are different, floats compare by bit
/// pattern (all NaNs equal), a tuple/set folds into a list (sets compare
/// order-insensitively because the port keeps them as insertion-ordered lists).
fn strict_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::BigInt(x), Value::BigInt(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => {
            (x.is_nan() && y.is_nan()) || x.to_bits() == y.to_bits()
        }
        (Value::Complex(x), Value::Complex(y)) => {
            let same = |p: f64, q: f64| (p.is_nan() && q.is_nan()) || p.to_bits() == q.to_bits();
            same(x.re, y.re) && same(x.im, y.im)
        }
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::List(x), Value::List(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| strict_eq(p, q))
        }
        (Value::Map(x), Value::Map(y)) => {
            x.len() == y.len()
                && x.iter()
                    .zip(y)
                    .all(|((kx, vx), (ky, vy))| kx == ky && strict_eq(vx, vy))
        }
        _ => false,
    }
}

/// Decode a stored upstream result (`$tuple`/`$set`/`$dict` folded into the
/// closest `Value`). `None` when it has no Rust counterpart (`$other`, big
/// integers, non-string dict keys).
fn upstream_value(j: &J) -> Option<(Value, bool)> {
    // The bool says "contains a set", making list comparison order-insensitive.
    match j {
        J::Object(m) if m.contains_key("$tuple") => {
            let items = m["$tuple"].as_array()?;
            let mut any_set = false;
            let v = items
                .iter()
                .map(|x| {
                    upstream_value(x).map(|(v, s)| {
                        any_set |= s;
                        v
                    })
                })
                .collect::<Option<Vec<_>>>()?;
            Some((Value::List(v), any_set))
        }
        J::Object(m) if m.contains_key("$set") => {
            let items = m["$set"].as_array()?;
            let v = items
                .iter()
                .map(|x| upstream_value(x).map(|(v, _)| v))
                .collect::<Option<Vec<_>>>()?;
            Some((Value::List(v), true))
        }
        J::Object(m) if m.contains_key("$dict") => {
            let mut out = IndexMap::new();
            for pair in m["$dict"].as_array()? {
                let pair = pair.as_array()?;
                let key = match decode(&pair[0]).ok()? {
                    Value::Str(s) => s,
                    _ => return None,
                };
                out.insert(key, upstream_value(&pair[1])?.0);
            }
            Some((Value::Map(out), false))
        }
        J::Object(m) if m.contains_key("$other") => None,
        J::Array(a) => {
            let mut any_set = false;
            let v = a
                .iter()
                .map(|x| {
                    upstream_value(x).map(|(v, s)| {
                        any_set |= s;
                        v
                    })
                })
                .collect::<Option<Vec<_>>>()?;
            Some((Value::List(v), any_set))
        }
        J::Object(m)
            if !m.contains_key("$f") && !m.contains_key("$i") && !m.contains_key("$c") =>
        {
            let mut out = IndexMap::new();
            let mut any_set = false;
            for (k, v) in m {
                let (v, s) = upstream_value(v)?;
                any_set |= s;
                out.insert(k.clone(), v);
            }
            Some((Value::Map(out), any_set))
        }
        other => decode(other).ok().map(|v| (v, false)),
    }
}

fn sorted_debug(v: &Value) -> Value {
    match v {
        Value::List(items) => {
            let mut items: Vec<Value> = items.iter().map(sorted_debug).collect();
            items.sort_by_key(|x| format!("{x:?}"));
            Value::List(items)
        }
        other => other.clone(),
    }
}

/// Check one corpus entry (expression or leaf) against its stored upstream
/// result. `Ok(())` means Rust agrees: both raise, or both return strictly
/// equal values.
///
/// # Errors
/// A one-line description of the disagreement.
pub fn check_entry(entry: &J) -> Result<(), String> {
    let up = entry.get("upstream").ok_or("entry has no `upstream`")?;
    let resp = handle_line(&entry.to_string());
    let up_err = up.get("err").is_some();
    let rs_err = resp.get("err").is_some();
    if resp.get("err").and_then(J::as_str) == Some("unrepresentable") {
        return Ok(());
    }
    match (up_err, rs_err) {
        (true, true) => Ok(()),
        (true, false) => Err(format!(
            "upstream raised {} but Rust returned {}",
            up["err"], resp["ok"]
        )),
        (false, true) => Err(format!(
            "upstream returned {} but Rust raised {}: {}",
            up["ok"], resp["err"], resp["message"]
        )),
        (false, false) => {
            let Some((exp, sets)) = upstream_value(&up["ok"]) else {
                return Ok(()); // no Rust counterpart to compare with
            };
            let got = decode(&resp["ok"]).map_err(|e| e.to_string())?;
            let same = if sets {
                strict_eq(&sorted_debug(&exp), &sorted_debug(&got))
            } else {
                strict_eq(&exp, &got)
            };
            if same {
                Ok(())
            } else {
                Err(format!("upstream {} != Rust {}", up["ok"], resp["ok"]))
            }
        }
    }
}
