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
//! Since upstream f299f3a the block sees *only* that namespace: slots are read
//! as `src.<slot>`, and a bare slot name or `{slot}` reference is a
//! `NameError` that is re-raised (see [`Program::eval`]). Any other error stops
//! the block, as asteval does, and the result is whatever `target` holds then.
//!
//! Supported constructs (exactly the set the personinfo_basic specs use):
//!   * assignment: `name = expr`
//!   * `if cond:` with a single-line trailing statement OR an indented block
//!   * expression statements (evaluated, value discarded)
//!   * list comprehension / subscript / `len` / `str` / attribute access /
//!     string compare — all handled by the shared expression evaluator.
//!
//! This layer is additive: single-expression `expr:` strings never reach it
//! (see [`parse_single_or_program`]'s dispatch), so the cached single-AST fast
//! path is untouched.

use super::error::{ExprError, ExprResult};
use super::eval::{Bindings, eval_ast_public, with_object_attr_mode};
use super::parser::{Ast, parse};
use crate::value::Value;

/// A run of physical lines (zero-based, inclusive) that one statement spans.
///
/// Only used to render the source line asteval quotes in its `NameError`
/// detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    first: usize,
    last: usize,
}

/// An evaluation failure together with the statement that raised it.
struct Failure {
    err: ExprError,
    span: Span,
}

/// A parsed statement.
#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    /// `name = <expr>`
    Assign {
        name: String,
        value: Ast,
        span: Span,
    },
    /// `if <cond>:` followed by a body of statements. `span` is the header.
    If {
        cond: Ast,
        body: Vec<Stmt>,
        span: Span,
    },
    /// A bare expression statement (evaluated for effect; value discarded).
    Expr(Ast, Span),
}

/// A parsed multi-statement program.
#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    stmts: Vec<Stmt>,
    /// The physical source lines, kept to quote the failing line in an
    /// undefined-name error like asteval does.
    lines: Vec<String>,
}

impl Program {
    /// Collect every expression [`Ast`] carried by the program — assignment
    /// right-hand sides, `if` conditions, and bare expression statements —
    /// recursing into nested `if` bodies. Used by expression-location scanning
    /// (`crate::normalize::expression_locations`) to find cross-table
    /// references inside multi-statement `expr:` blocks, mirroring upstream's
    /// `ast.walk` over an exec-mode parse.
    pub fn exprs(&self) -> Vec<&Ast> {
        let mut out = Vec::new();
        collect_stmt_exprs(&self.stmts, &mut out);
        out
    }

    /// Collect every name bound by an assignment in this program — the
    /// left-hand side of each `name = <expr>` statement, recursing into `if`
    /// bodies. These are the "locally bound" names that expression-reference
    /// scanning (`crate::validate::expr_refs`) must exclude, mirroring how
    /// upstream's `extract_expr_slot_references` drops `ast.Name` nodes in a
    /// `Store` context. Assignment targets are not `Ast` nodes in this parser
    /// (they live on [`Stmt::Assign`]), so they cannot be recovered by walking
    /// [`exprs`](Self::exprs) alone.
    pub fn assigned_names(&self) -> Vec<String> {
        let mut out = Vec::new();
        collect_assigned_names(&self.stmts, &mut out);
        out
    }

    /// Evaluate the program against `vars` and return the `target` symbol.
    ///
    /// Mirrors upstream `ObjectTransformer._unrestricted_eval(..,
    /// raise_undefined=True)`: the block runs in a fresh namespace holding only
    /// `src` (the whole source object, taken from `vars["src"]`), `target`
    /// (pre-seeded to None) and the `slot()` lookup table, never the bare slot
    /// names, join aliases or `NULL` that single expressions see.
    ///
    /// PARITY: asteval stops at the first error. A `NameError` (an undefined
    /// name, typically a bare slot or a `{slot}` reference) is re-raised as
    /// `ValueError("Unrestricted expression uses an undefined name: ...")`;
    /// every other error (a missing value iterated, an index out of range, ...)
    /// is swallowed and the result is whatever `target` held at that point,
    /// None if it was never assigned. See `_unrestricted_eval` in
    /// `transformer/object_transformer.py`.
    ///
    /// # Errors
    /// Only the undefined-name case above. It is an [`ExprError::Eval`].
    pub fn eval(&self, vars: &Bindings) -> ExprResult<Value> {
        let mut env = Bindings::new();
        // Pre-seed `target` to None, exactly like the asteval usersyms.
        env.insert("target".to_string(), Value::Null);
        // Callers that bind no `src` (the enum-level `expr`, whose bindings are
        // the source map itself) get the whole namespace as `src`, which is
        // what upstream passes there (`_unrestricted_eval(expr, source_obj)`).
        let src = vars.get("src").cloned().unwrap_or_else(|| {
            Value::Map(
                vars.iter()
                    .filter(|(k, _)| !matches!(k.as_str(), "NULL" | "__slot_values"))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
            )
        });
        env.insert("src".to_string(), src);
        if let Some(slots) = vars.get("__slot_values") {
            env.insert("__slot_values".to_string(), slots.clone());
        }
        let outcome = with_object_attr_mode(|| exec_block(&self.stmts, &mut env));
        match outcome {
            Ok(()) => {}
            Err(Failure {
                err: ExprError::Undefined { name, .. },
                span,
            }) => {
                return Err(ExprError::Eval(format!(
                    "Unrestricted expression uses an undefined name: {}. \
                     Statements read slots as src.<slot>.",
                    self.undefined_name_detail(&name, span)
                )));
            }
            // asteval records the error and stops; `target` keeps its value.
            Err(_) => {}
        }
        Ok(env.get("target").cloned().unwrap_or(Value::Null))
    }

    /// The text asteval's `ExceptionHolder.get_error()` gives for a `NameError`:
    /// the source line, a caret line when the name is not in column 0, then
    /// `NameError: name '<name>' is not defined`.
    ///
    /// The column is found by scanning the failing statement's lines for the
    /// first use of `name` (this crate's AST carries no positions).
    fn undefined_name_detail(&self, name: &str, span: Span) -> String {
        let located = (span.first..=span.last).find_map(|idx| {
            let line = self.lines.get(idx)?;
            Some((idx, find_name_column(line, name)?))
        });
        let (idx, col) = located.unwrap_or((span.first, 0));
        let mut out = String::new();
        if let Some(line) = self.lines.get(idx) {
            out.push_str(line);
            out.push('\n');
        }
        if col > 0 {
            out.push_str(&" ".repeat(col));
            out.push_str("^^^^\n");
        }
        out.push_str(&format!("NameError: name '{name}' is not defined"));
        out
    }
}

/// Byte offset of the first use of `name` as a variable (not an attribute, not
/// inside a string or comment) in `line`: Python's `col_offset` for that
/// `ast.Name` node.
fn find_name_column(line: &str, name: &str) -> Option<usize> {
    let mut in_str: Option<char> = None;
    let mut escaped = false;
    let mut prev_significant: Option<char> = None;
    let mut chars = line.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if let Some(q) = in_str {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q {
                in_str = None;
            }
            continue;
        }
        match c {
            '#' => return None,
            '\'' | '"' => {
                in_str = Some(c);
                prev_significant = Some(c);
            }
            c if c.is_alphabetic() || c == '_' => {
                let mut end = i + c.len_utf8();
                while let Some(&(j, d)) = chars.peek() {
                    if d.is_alphanumeric() || d == '_' {
                        end = j + d.len_utf8();
                        chars.next();
                    } else {
                        break;
                    }
                }
                if &line[i..end] == name && prev_significant != Some('.') {
                    return Some(i);
                }
                prev_significant = Some('a');
            }
            c if c.is_whitespace() => {}
            c if c.is_ascii_digit() => {
                // Swallow a numeric literal so `1e5` does not read as a name.
                while let Some(&(_, d)) = chars.peek() {
                    if d.is_alphanumeric() || d == '_' || d == '.' {
                        chars.next();
                    } else {
                        break;
                    }
                }
                prev_significant = Some('0');
            }
            other => prev_significant = Some(other),
        }
    }
    None
}

fn collect_assigned_names(stmts: &[Stmt], out: &mut Vec<String>) {
    for stmt in stmts {
        match stmt {
            Stmt::Assign { name, .. } => out.push(name.clone()),
            Stmt::If { body, .. } => collect_assigned_names(body, out),
            Stmt::Expr(..) => {}
        }
    }
}

fn collect_stmt_exprs<'a>(stmts: &'a [Stmt], out: &mut Vec<&'a Ast>) {
    for stmt in stmts {
        match stmt {
            Stmt::Assign { value, .. } => out.push(value),
            Stmt::If { cond, body, .. } => {
                out.push(cond);
                collect_stmt_exprs(body, out);
            }
            Stmt::Expr(ast, _) => out.push(ast),
        }
    }
}

fn exec_block(stmts: &[Stmt], env: &mut Bindings) -> Result<(), Failure> {
    for stmt in stmts {
        exec_stmt(stmt, env)?;
    }
    Ok(())
}

fn exec_stmt(stmt: &Stmt, env: &mut Bindings) -> Result<(), Failure> {
    match stmt {
        Stmt::Assign { name, value, span } => {
            let v = eval_ast_public(value, env).map_err(|err| Failure { err, span: *span })?;
            env.insert(name.clone(), v);
            Ok(())
        }
        Stmt::If { cond, body, span } => {
            let c = eval_ast_public(cond, env).map_err(|err| Failure { err, span: *span })?;
            if c.is_truthy() {
                exec_block(body, env)?;
            }
            Ok(())
        }
        Stmt::Expr(ast, span) => {
            // Evaluate for side effects (there are none in this language); a
            // failure still stops the block, as it does in asteval.
            eval_ast_public(ast, env).map_err(|err| Failure { err, span: *span })?;
            Ok(())
        }
    }
}

// === parser ===============================================================

/// The result of [`parse_single_or_program`].
pub(crate) enum SingleOrProgram {
    /// The text is one expression (the cached fast path).
    Single(Ast),
    /// The text is Python statements.
    Program(Program),
}

/// Whether this `expr:` string is Python statements rather than one expression.
///
/// Mirrors upstream `_is_statement_block`
/// (`ast.parse(expr.strip(), mode="eval")` raising `SyntaxError`): anything that
/// parses as a single expression is one, whatever its layout, so a trailing
/// newline (every YAML `|` block ends with one) or a bracketed multi-line
/// expression stays an expression, and a statement block is anything else.
pub fn is_statement_block(expr: &str) -> bool {
    parse_single_expression(expr).is_err()
}

/// `ast.parse(expr.strip(), mode="eval")`: the text as one expression.
///
/// Python's eval mode accepts one logical line, where a newline inside
/// brackets (or after a trailing backslash) does not end the line and blank or
/// comment-only lines are ignored.
fn parse_single_expression(expr: &str) -> ExprResult<Ast> {
    let lines = raw_lines(expr.trim());
    match lines.as_slice() {
        [line] => parse(&line.text),
        [] => parse(""),
        _ => Err(ExprError::Parse(
            "an expression must be a single logical line".into(),
        )),
    }
}

/// Parse `expr` as one expression when it is one, else as a statement block.
///
/// # Errors
/// A parse error. PARITY: asteval would record a `SyntaxError` and the row
/// would yield None; failing at plan-build time instead is deliberate. The
/// error reported is the single expression's unless the text looks like
/// statements (several logical lines, an assignment, a `;` or an `if`).
pub(crate) fn parse_single_or_program(expr: &str) -> ExprResult<SingleOrProgram> {
    let single_err = match parse_single_expression(expr) {
        Ok(ast) => return Ok(SingleOrProgram::Single(ast)),
        Err(e) => e,
    };
    match parse_program(expr) {
        Ok(program) => Ok(SingleOrProgram::Program(program)),
        Err(program_err) => {
            if looks_like_statements(expr) {
                Err(program_err)
            } else {
                Err(single_err)
            }
        }
    }
}

/// Does the text carry statement syntax? Only used to pick which parse error
/// to report for text that is neither one expression nor a valid program.
fn looks_like_statements(src: &str) -> bool {
    let lines = raw_lines(src);
    if lines.len() > 1 {
        return true;
    }
    let Some(line) = lines.first() else {
        return false;
    };
    strip_if_header(&line.text).is_some()
        || split_assignment(&line.text).is_some()
        || split_semicolons(&line.text).len() > 1
}

/// Parse a multi-statement program from `src`.
///
/// Indentation-aware (Python-style): an `if cond:` line owns the following
/// lines that are indented deeper than it. A trailing single statement on the
/// same line as the colon is also supported (`if x: target = y`). A statement
/// may continue over several physical lines inside brackets or after a
/// trailing backslash, and `#` comments are ignored.
pub fn parse_program(src: &str) -> ExprResult<Program> {
    let lines = logical_lines(src);
    let mut pos = 0usize;
    let stmts = parse_stmts(&lines, &mut pos, None)?;
    Ok(Program {
        stmts,
        lines: src.split('\n').map(str::to_string).collect(),
    })
}

/// A logical line with its indentation depth (in columns; tabs = 1 each) and
/// trimmed content. Blank lines are dropped.
struct Line {
    indent: usize,
    text: String,
    span: Span,
}

/// A logical line before `;` splitting: physical lines joined across brackets
/// and backslash continuations, comments removed.
struct RawLine {
    indent: usize,
    text: String,
    span: Span,
}

/// Python's logical lines: blank and comment-only lines are dropped, and a
/// newline inside brackets or after a trailing `\` does not end the line.
fn raw_lines(src: &str) -> Vec<RawLine> {
    let mut out = Vec::new();
    let mut pending: Option<RawLine> = None;
    let mut depth = 0i32;
    for (idx, physical) in src.split('\n').enumerate() {
        let physical = physical.trim_end_matches('\r');
        let (code, continued) = scan_code(physical, &mut depth);
        let content = code.trim();
        match pending.as_mut() {
            Some(line) => {
                if !content.is_empty() {
                    line.text.push(' ');
                    line.text.push_str(content);
                }
                line.span.last = idx;
            }
            None => {
                if content.is_empty() {
                    continue;
                }
                let indent = physical
                    .chars()
                    .take_while(|c| *c == ' ' || *c == '\t')
                    .count();
                pending = Some(RawLine {
                    indent,
                    text: content.to_string(),
                    span: Span {
                        first: idx,
                        last: idx,
                    },
                });
            }
        }
        if depth <= 0 && !continued {
            depth = 0;
            out.extend(pending.take());
        }
    }
    // An unclosed bracket or trailing backslash: keep what there is so the
    // statement parser reports it.
    out.extend(pending);
    out
}

/// The code part of one physical line (a `#` comment removed), updating the
/// bracket `depth`; the flag is true when the line ends with a continuation
/// backslash, which is removed.
fn scan_code(line: &str, depth: &mut i32) -> (String, bool) {
    let mut out = String::with_capacity(line.len());
    let mut in_str: Option<char> = None;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if let Some(q) = in_str {
            out.push(c);
            if c == '\\' {
                if let Some(next) = chars.next() {
                    out.push(next);
                }
            } else if c == q {
                in_str = None;
            }
            continue;
        }
        match c {
            '#' => break,
            '\'' | '"' => {
                in_str = Some(c);
                out.push(c);
            }
            '(' | '[' | '{' => {
                *depth += 1;
                out.push(c);
            }
            ')' | ']' | '}' => {
                *depth -= 1;
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    let trimmed = out.trim_end();
    if in_str.is_none() && trimmed.ends_with('\\') {
        out.truncate(trimmed.len() - 1);
        return (out, true);
    }
    (out, false)
}

fn logical_lines(src: &str) -> Vec<Line> {
    let mut out = Vec::new();
    for raw in raw_lines(src) {
        // Inline `;` separation: subsequent segments inherit the same indent
        // (siblings at the same level). None of the conformance specs use `;`,
        // but Python allows it.
        for seg in split_semicolons(&raw.text) {
            let seg = seg.trim();
            if seg.is_empty() {
                continue;
            }
            out.push(Line {
                indent: raw.indent,
                text: seg.to_string(),
                span: raw.span,
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
    let span = lines[*pos].span;
    *pos += 1;

    // `if <cond>:` — optionally with a trailing statement on the same line.
    if let Some(rest) = strip_if_header(&text) {
        let (cond_src, trailer) = split_if_colon(rest)?;
        let cond = parse(cond_src.trim())?;
        let trailer = trailer.trim();
        if !trailer.is_empty() {
            // Single-line body: `if cond: stmt`
            let body_stmt = parse_simple_stmt(trailer, span)?;
            return Ok(Stmt::If {
                cond,
                body: vec![body_stmt],
                span,
            });
        }
        // Indented block body: gather deeper-indented following lines.
        if *pos < lines.len() && lines[*pos].indent > line_indent {
            let body_indent = lines[*pos].indent;
            let body = parse_stmts(lines, pos, Some(body_indent))?;
            return Ok(Stmt::If { cond, body, span });
        }
        // `if` with empty body — degrade to a no-op guard.
        return Ok(Stmt::If {
            cond,
            body: vec![],
            span,
        });
    }

    parse_simple_stmt(&text, span)
}

/// Parse a non-compound statement: assignment or expression.
fn parse_simple_stmt(text: &str, span: Span) -> ExprResult<Stmt> {
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
            span,
        });
    }
    let ast = parse(text.trim())?;
    Ok(Stmt::Expr(ast, span))
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
                let part_of_cmp = matches!(prev, Some('=' | '!' | '<' | '>')) || next == Some('=');
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

#[cfg(test)]
mod tests {
    use super::*;

    // PARITY: upstream `_is_statement_block` doctests, then the cases around
    // `ast.parse(expr.strip(), mode="eval")` that a text heuristic gets wrong.
    #[test]
    fn is_statement_block_mirrors_ast_parse_eval() {
        let expressions = [
            "{age} * 12",
            "{age} * 12\n",
            "  {age} * 12  \n\n",
            "case(\n    ({age} > 5, \"big\"),\n    (True, \"small\"),\n)\n",
            "[\n  e.year\n  for e in src.events\n]",
            "{a} + \\\n{b}",
            "{a} + {b}  # trailing comment",
            "{a} + {b}\n# comment-only line after",
            "None",
            "len(src.events) == 0",
        ];
        for expr in expressions {
            assert!(!is_statement_block(expr), "{expr:?} is one expression");
        }
        let blocks = [
            "if {age}:\n    target = 1",
            "target = 1",
            "x = 1\ntarget = x",
            "a; b",
            "{a} +\n{b}",
            "{a}\n{b}",
            "",
            "   \n",
            "# only a comment",
        ];
        for expr in blocks {
            assert!(is_statement_block(expr), "{expr:?} is a statement block");
        }
    }

    #[test]
    fn single_or_program_picks_the_parse() {
        assert!(matches!(
            parse_single_or_program("{age} * 2\n"),
            Ok(SingleOrProgram::Single(_))
        ));
        assert!(matches!(
            parse_single_or_program("target = 1"),
            Ok(SingleOrProgram::Program(_))
        ));
    }

    #[test]
    fn malformed_single_line_reports_the_expression_error() {
        // Not statements: the error is the expression parser's, as before.
        let err = parse_single_or_program("{1 + 2}").err().expect("error");
        assert_eq!(Some(err), parse_single_expression("{1 + 2}").err());
        // Statements with a bad part: the program parser's error.
        let err = parse_single_or_program("x = \ntarget = 1")
            .err()
            .expect("error");
        assert!(matches!(err, ExprError::Parse(_)), "{err:?}");
    }

    #[test]
    fn find_name_column_matches_python_col_offset() {
        assert_eq!(find_name_column("target = age * 2", "age"), Some(9));
        assert_eq!(find_name_column("    x = {age} * 2", "age"), Some(9));
        // Not an attribute, string, or comment.
        assert_eq!(find_name_column("x = src.age", "age"), None);
        assert_eq!(find_name_column("x = 'age'", "age"), None);
        assert_eq!(find_name_column("x = 1  # age", "age"), None);
        assert_eq!(find_name_column("x = ages + age", "age"), Some(11));
    }

    #[test]
    fn program_eval_does_not_leak_slot_names() {
        let mut vars = Bindings::new();
        vars.insert("age".into(), Value::Int(10));
        let program = parse_program("target = age").unwrap();
        let err = program.eval(&vars).unwrap_err();
        assert!(err.to_string().contains("undefined name"), "got {err}");
    }

    #[test]
    fn program_without_src_binding_gets_the_namespace_as_src() {
        // The enum-level `expr` binds the source map itself, no `src`.
        let mut vars = Bindings::new();
        vars.insert("age".into(), Value::Int(10));
        vars.insert("NULL".into(), Value::Null);
        let program = parse_program("target = src.age").unwrap();
        assert_eq!(program.eval(&vars).unwrap(), Value::Int(10));
    }
}
