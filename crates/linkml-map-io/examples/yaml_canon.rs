//! Scratch differential harness: load each file given and print a canonical form.
use linkml_map_core::pyfmt::py_float_repr;
use linkml_map_core::pyyaml::LoadError;
use linkml_map_core::value::Value;

fn s(out: &mut String, t: &str) {
    out.push('"');
    for c in t.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            ' '..='~' => out.push(c),
            _ => {
                let mut u = [0u16; 2];
                for x in c.encode_utf16(&mut u) {
                    out.push_str(&format!("\\u{x:04x}"));
                }
            }
        }
    }
    out.push('"');
}
fn canon(out: &mut String, v: &Value) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Int(i) => out.push_str(&i.to_string()),
        Value::Float(f) => out.push_str(&py_float_repr(*f)),
        Value::Decimal(d) => out.push_str(&d.to_sci_string()),
        Value::Temporal(t) => {
            out.push('D');
            s(out, &t.to_string())
        }
        Value::Str(x) => s(out, x),
        Value::List(l) => {
            out.push('[');
            for (i, x) in l.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                canon(out, x)
            }
            out.push(']')
        }
        Value::Map(m) => {
            out.push('{');
            for (i, (k, x)) in m.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                s(out, k);
                out.push(':');
                canon(out, x)
            }
            out.push('}')
        }
    }
}
fn main() {
    for p in std::env::args().skip(1) {
        let text = match std::fs::read_to_string(&p) {
            Ok(t) => t,
            Err(_) => {
                println!("ERR_READ");
                continue;
            }
        };
        match linkml_map_io::yaml_load::safe_load_all(&text) {
            Ok(docs) => {
                let mut out = String::new();
                for (i, d) in docs.iter().enumerate() {
                    if i > 0 {
                        out.push_str(" || ");
                    }
                    canon(&mut out, d);
                }
                if docs.is_empty() {
                    out.push_str("null");
                }
                println!("{out}");
            }
            Err(LoadError::Yaml(_)) => println!("ERR_YAML"),
            Err(LoadError::Value(_)) => println!("ERR_VALUE"),
        }
    }
}
