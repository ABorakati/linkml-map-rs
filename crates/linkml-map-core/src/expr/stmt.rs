//! Multi-statement (`asteval`-style) interpreter for `expr:` blocks.
//!
//! Native port of the linkml-map "unrestricted" expression path in
//! `ObjectTransformer._derive_from_expr`:
//!
//! ```python
//! aeval = Interpreter(usersyms={"src": ctxt_obj, "target": None, "uuid5": _uuid5})
//! aeval(slot_derivation.expr)
//! return aeval.symtable["target"]
//! ```
//!
//! The whole source object is bound as `src`; `target` is pre-seeded to None;
//! after running the block, the value of the `target` symbol is the result. If
//! `target` is never assigned (e.g. an `if` guard did not fire), the result is
//! [`Value::Null`] — matching Python returning the pre-seeded `None`.
//!
//! Supported constructs (exactly the set the personinfo_basic specs use):
//!   * assignment: `name = expr`
//!   * `if cond:` with a single-line trailing statement OR an indented block
//!   * expression statements (evaluated, value discarded)
//!   * list comprehension / subscript / `len` / `str` / attribute access /
//!     string compare — all handled by the shared expression evaluator.
//!
//! This layer is additive: single-expression `expr:` strings never reach it
//! (see [`super::eval::parse_expr`]'s dispatch), so the cached single-AST fast
//! path is untouched.

use super::error::{ExprError, ExprResult};
use super::eval::{eval_ast_public, Bindings};
use super::parser::{parse, Ast};
use crate::value::Value;

/// A parsed statement.
#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    /// `name = <expr>`
    Assign { name: String, value: Ast },
    /// `if <cond>:` followed by a body of statements.
    If { cond: Ast, body: Vec<Stmt> },
    /// A bare expression statement (evaluated for effect; value discarded).
    Expr(Ast),
}

/// A parsed multi-statement program.
#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    stmts: Vec<Stmt>,
}

impl Program {
    /// Evaluate the program against `vars` and return the `target` symbol.
    ///
    /// `vars` must already carry the `src` binding (the whole source object)
    /// and any slot-name bindings; this mirrors the asteval `usersyms`. After
    /// execution, the value of `target` is returned, or [`Value::Null`] if it
    /// was never assigned.
    pub fn eval(&self, vars: &Bindings) -> ExprResult<Value> {
        let mut env = vars.clone();
        // Pre-seed `target` to None, exactly like the asteval usersyms.
        env.insert("target".to_string(), Value::Null);
        exec_block(&self.stmts, &mut env)?;
        Ok(env.get("target").cloned().unwrap_or(Value::Null))
    }
}

fn exec_block(stmts: &[Stmt], env: &mut Bindings) -> ExprResult<()> {
    for stmt in stmts {
        exec_stmt(stmt, env)?;
    }
    Ok(())
}

fn exec_stmt(stmt: &Stmt, env: &mut Bindings) -> ExprResult<()> {
    match stmt {
        Stmt::Assign { name, value } => {
            let v = eval_ast_public(value, env)?;
            env.insert(name.clone(), v);
            Ok(())
        }
        Stmt::If { cond, body } => {
            let c = eval_ast_public(cond, env)?;
            if c.is_truthy() {
                exec_block(body, env)?;
            }
            Ok(())
        }
        Stmt::Expr(ast) => {
            // Evaluate for side effects (there are none in this language), but
            // surface evaluation errors so they are not silently swallowed.
            let _ = eval_ast_public(ast, env)?;
            Ok(())
        }
    }
}

// === parser ===============================================================

/// Heuristic: does this `expr:` string need the multi-statement interpreter?
///
/// True when it contains a statement separator (newline / `;`), a bare `if`
/// statement, or a top-level assignment (`=` that is not part of `==`, `!=`,
/// `<=`, `>=`). Single-expression strings return false and keep the fast path.
pub fn is_multi_statement(src: &str) -> bool {
    if src.contains('\n') || src.contains(';') {
        return true;
    }
    let trimmed = src.trim_start();
    if trimmed.starts_with("if ") || trimmed.starts_with("if(") {
        return true;
    }
    has_top_level_assignment(src)
}

/// Detect a top-level `=` assignment, ignoring `==`, `!=`, `<=`, `>=` and any
/// `=` inside string literals or brackets/parens.
fn has_top_level_assignment(src: &str) -> bool {
    let chars: Vec<char> = src.chars().collect();
    let n = chars.len();
    let mut i = 0;
    let mut depth = 0i32;
    let mut in_str: Option<char> = None;
    while i < n {
        let c = chars[i];
        if let Some(q) = in_str {
            if c == '\\' {
                i += 2;
                continue;
            }
            if c == q {
                in_str = None;
            }
            i += 1;
            continue;
        }
        match c {
            '\'' | '"' => in_str = Some(c),
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            '=' if depth == 0 => {
                let prev = if i > 0 { Some(chars[i - 1]) } else { None };
                let next = chars.get(i + 1).copied();
                let part_of_cmp = matches!(prev, Some('=' | '!' | '<' | '>'))
                    || next == Some('=');
                if !part_of_cmp {
                    return true;
                }
            }
            _ => {}
        }
        i += 1;
    }
    false
}

/// Parse a multi-statement program from `src`.
///
/// Indentation-aware (Python-style): an `if cond:` line owns the following
/// lines that are indented deeper than it. A trailing single statement on the
/// same line as the colon is also supported (`if x: target = y`).
pub fn parse_program(src: &str) -> ExprResult<Program> {
    let lines = logical_lines(src);
    let mut pos = 0usize;
    let stmts = parse_stmts(&lines, &mut pos, None)?;
    Ok(Program { stmts })
}

/// A physical line with its indentation depth (in columns; tabs = 1 each) and
/// trimmed content. Blank lines are dropped.
struct Line {
    indent: usize,
    text: String,
}

fn logical_lines(src: &str) -> Vec<Line> {
    let mut out = Vec::new();
    for raw_line in src.split('\n') {
        // Also split on `;` for inline multiple statements.
        // First compute indent from the physical line.
        let stripped_newline = raw_line.trim_end_matches('\r');
        let indent = stripped_newline
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .count();
        let content = stripped_newline.trim();
        if content.is_empty() {
            continue;
        }
        // Inline `;` separation: subsequent segments inherit the same indent
        // (siblings at the same level). None of the conformance specs use `;`,
        // but Python allows it.
        for seg in split_semicolons(content) {
            let seg = seg.trim();
            if seg.is_empty() {
                continue;
            }
            out.push(Line {
                indent,
                text: seg.to_string(),
            });
        }
    }
    out
}

/// Split a line on top-level `;` (not inside strings/brackets).
fn split_semicolons(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    let mut in_str: Option<char> = None;
    let mut i = 0;
    while i < n {
        let c = chars[i];
        if let Some(q) = in_str {
            cur.push(c);
            if c == '\\' && i + 1 < n {
                cur.push(chars[i + 1]);
                i += 2;
                continue;
            }
            if c == q {
                in_str = None;
            }
            i += 1;
            continue;
        }
        match c {
            '\'' | '"' => {
                in_str = Some(c);
                cur.push(c);
            }
            '(' | '[' | '{' => {
                depth += 1;
                cur.push(c);
            }
            ')' | ']' | '}' => {
                depth -= 1;
                cur.push(c);
            }
            ';' if depth == 0 => {
                out.push(std::mem::take(&mut cur));
            }
            _ => cur.push(c),
        }
        i += 1;
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Parse statements at the current indentation level. `min_indent` is `None` at
/// top level (everything), or `Some(d)` to consume only lines indented `>= d`
/// that belong to the current block; parsing stops when a line dedents below
/// the block's base indent.
fn parse_stmts(
    lines: &[Line],
    pos: &mut usize,
    block_indent: Option<usize>,
) -> ExprResult<Vec<Stmt>> {
    let mut stmts = Vec::new();
    let base = block_indent.unwrap_or(0);
    while *pos < lines.len() {
        // A nested block ends as soon as a line dedents below its base indent.
        if block_indent.is_some() && lines[*pos].indent < base {
            break;
        }
        let stmt = parse_one_stmt(lines, pos)?;
        stmts.push(stmt);
    }
    Ok(stmts)
}

fn parse_one_stmt(lines: &[Line], pos: &mut usize) -> ExprResult<Stmt> {
    let line_indent = lines[*pos].indent;
    let text = lines[*pos].text.clone();
    *pos += 1;

    // `if <cond>:` — optionally with a trailing statement on the same line.
    if let Some(rest) = strip_if_header(&text) {
        let (cond_src, trailer) = split_if_colon(rest)?;
        let cond = parse(cond_src.trim())?;
        let trailer = trailer.trim();
        if !trailer.is_empty() {
            // Single-line body: `if cond: stmt`
            let body_stmt = parse_simple_stmt(trailer)?;
            return Ok(Stmt::If {
                cond,
                body: vec![body_stmt],
            });
        }
        // Indented block body: gather deeper-indented following lines.
        if *pos < lines.len() && lines[*pos].indent > line_indent {
            let body_indent = lines[*pos].indent;
            let body = parse_stmts(lines, pos, Some(body_indent))?;
            return Ok(Stmt::If { cond, body });
        }
        // `if` with empty body — degrade to a no-op guard.
        return Ok(Stmt::If { cond, body: vec![] });
    }

    parse_simple_stmt(&text)
}

/// Parse a non-compound statement: assignment or expression.
fn parse_simple_stmt(text: &str) -> ExprResult<Stmt> {
    if let Some((lhs, rhs)) = split_assignment(text) {
        let name = lhs.trim();
        if !is_identifier(name) {
            return Err(ExprError::Parse(format!(
                "invalid assignment target: {name:?}"
            )));
        }
        let value = parse(rhs.trim())?;
        return Ok(Stmt::Assign {
            name: name.to_string(),
            value,
        });
    }
    let ast = parse(text.trim())?;
    Ok(Stmt::Expr(ast))
}

/// If `text` begins an `if` statement, return the part after `if`.
fn strip_if_header(text: &str) -> Option<&str> {
    let t = text.trim_start();
    if let Some(rest) = t.strip_prefix("if ") {
        return Some(rest);
    }
    if t.starts_with("if(") {
        // Keep the `(` so the condition parses; caller splits on the colon.
        return Some(&t[2..]);
    }
    None
}

/// Split `cond:` (or `cond: trailing`) at the top-level colon. Returns
/// `(cond_src, trailer)`.
fn split_if_colon(rest: &str) -> ExprResult<(&str, &str)> {
    let chars: Vec<char> = rest.chars().collect();
    let n = chars.len();
    let mut depth = 0i32;
    let mut in_str: Option<char> = None;
    let mut i = 0;
    let mut byte_idx = 0usize;
    // Track byte index alongside char index for slicing.
    let mut char_byte_positions = Vec::with_capacity(n + 1);
    for (b, _) in rest.char_indices() {
        char_byte_positions.push(b);
    }
    char_byte_positions.push(rest.len());
    while i < n {
        let c = chars[i];
        byte_idx = char_byte_positions[i];
        if let Some(q) = in_str {
            if c == '\\' {
                i += 2;
                continue;
            }
            if c == q {
                in_str = None;
            }
            i += 1;
            continue;
        }
        match c {
            '\'' | '"' => in_str = Some(c),
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ':' if depth == 0 => {
                let cond = &rest[..byte_idx];
                let trailer = &rest[char_byte_positions[i + 1]..];
                return Ok((cond, trailer));
            }
            _ => {}
        }
        i += 1;
    }
    let _ = byte_idx;
    Err(ExprError::Parse("expected ':' after if condition".into()))
}

/// Split `name = expr` at the top-level single `=`, skipping `==`/`!=`/`<=`/`>=`
/// and any `=` inside strings or brackets. Returns `(lhs, rhs)` if found.
fn split_assignment(text: &str) -> Option<(&str, &str)> {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut char_byte_positions = Vec::with_capacity(n + 1);
    for (b, _) in text.char_indices() {
        char_byte_positions.push(b);
    }
    char_byte_positions.push(text.len());
    let mut depth = 0i32;
    let mut in_str: Option<char> = None;
    let mut i = 0;
    while i < n {
        let c = chars[i];
        if let Some(q) = in_str {
            if c == '\\' {
                i += 2;
                continue;
            }
            if c == q {
                in_str = None;
            }
            i += 1;
            continue;
        }
        match c {
            '\'' | '"' => in_str = Some(c),
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            '=' if depth == 0 => {
                let prev = if i > 0 { Some(chars[i - 1]) } else { None };
                let next = chars.get(i + 1).copied();
                let part_of_cmp =
                    matches!(prev, Some('=' | '!' | '<' | '>')) || next == Some('=');
                if !part_of_cmp {
                    let lhs = &text[..char_byte_positions[i]];
                    let rhs = &text[char_byte_positions[i + 1]..];
                    return Some((lhs, rhs));
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_alphanumeric() || c == '_')
}
