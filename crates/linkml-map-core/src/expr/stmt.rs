//! Statement blocks: the `asteval` ("unrestricted") side of `expr:`.
//!
//! Native port of the linkml-map "unrestricted" expression path
//! (`ObjectTransformer._unrestricted_eval`), reached only with
//! `unrestricted_eval` on; see [`super::gate`] for the split. The interpreter
//! itself is [`super::asteval`]: a port of `asteval` 1.0.x, so a block runs
//! exactly the Python upstream runs, with the same symbol table, the same error
//! policy and the same quirks.
//!
//! ```python
//! aeval = Interpreter(usersyms={"src": ctxt_obj, "target": None, "uuid5": _uuid5})
//! aeval(slot_derivation.expr)
//! return aeval.symtable["target"]
//! ```
//!
//! The whole source object is bound as `src`; `target` is pre-seeded to None;
//! after running the block, the value of the `target` symbol is the result. A
//! text that does not parse (`SyntaxError`, `IndentationError`) is swallowed by
//! `asteval`: the block does nothing and the result is None.
//!
//! The restricted evaluator (`simpleeval`) runs only the first statement of a
//! block; that statement is kept here ([`Program::eval_first_statement`]) as an
//! expression parsed by the restricted evaluator's own parser.

use std::sync::Arc;

use super::asteval::{self, Parsed};
use super::error::{ExprError, ExprResult};
use super::eval::{Bindings, eval_ast_public};
use super::parser::{Ast, parse};
use crate::value::Value;

/// What the restricted evaluator does with a block: it parses the whole text
/// but evaluates only `body[0]`.
#[derive(Debug, Clone, PartialEq)]
enum First {
    /// `Expr`, `Assign` and `AugAssign` evaluate their value (`simpleeval`
    /// ignores the target).
    Value(Ast),
    /// Any other statement: `FeatureNotAvailable`.
    Unavailable(String),
    /// Nothing to evaluate.
    Empty,
    /// The text does not parse: `ast.parse` raises `SyntaxError`.
    Syntax(ExprError),
}

#[derive(Debug, PartialEq)]
struct Inner {
    /// The text as given (asteval parses it unstripped).
    text: String,
    /// `ast.parse(text)`, or why it failed (asteval then does nothing).
    compiled: Result<Arc<Parsed>, String>,
    /// `src` attributes the block reads, when it only ever reads `src.<name>`.
    src_attrs: Option<Vec<String>>,
    first: First,
    /// The expressions of the block, parsed by the restricted parser.
    exprs: Vec<Ast>,
    assigned: Vec<String>,
    /// The error reported when the text does not parse.
    syntax_error: Option<ExprError>,
}

/// A parsed statement block.
#[derive(Debug, Clone, PartialEq)]
pub struct Program(Arc<Inner>);

impl Program {
    /// Parse `text` as a statement block. `single_err` is the error the text
    /// gives as one expression; the block's own parse error is reported
    /// instead when the text looks like statements.
    fn new(text: &str, single_err: ExprError) -> Self {
        let stripped = text.trim_matches(super::pystr::is_py_space);
        let compiled = asteval::compile(text).map(Arc::new);
        // simpleeval parses `expr.strip()`.
        let stripped_parse = if stripped == text {
            compiled.clone()
        } else {
            asteval::compile(stripped).map(Arc::new)
        };
        let syntax_error = match &stripped_parse {
            Ok(_) => None,
            Err(msg) => {
                let program_err = ExprError::Parse(msg.clone());
                Some(if looks_like_statements(text) {
                    program_err
                } else {
                    single_err
                })
            }
        };
        let first = match &stripped_parse {
            Err(_) => First::Syntax(syntax_error.clone().expect("set above")),
            Ok(p) => match p.first {
                None => First::Empty,
                Some((kind @ ("Expr" | "Assign" | "AugAssign"), span)) => {
                    let value = span.and_then(|(a, b)| p.source.get(a..b));
                    match value.map(parse) {
                        Some(Ok(ast)) => First::Value(ast),
                        Some(Err(e)) => First::Syntax(e),
                        None => First::Unavailable(kind.to_string()),
                    }
                }
                Some(("Import", _)) => First::Syntax(ExprError::Eval(
                    "Sorry, 'import' is not allowed.".into(),
                )),
                Some((kind, _)) => First::Unavailable(kind.to_string()),
            },
        };
        let (src_attrs, exprs, assigned) = match &compiled {
            Ok(p) => (
                asteval::src_attributes(&p.module),
                p.expr_spans
                    .iter()
                    .filter_map(|&(a, b)| p.source.get(a..b))
                    .filter_map(|t| parse(t).ok())
                    .collect(),
                assigned_names(&p.module),
            ),
            Err(_) => (None, Vec::new(), Vec::new()),
        };
        Program(Arc::new(Inner {
            text: text.to_string(),
            compiled,
            src_attrs,
            first,
            exprs,
            assigned,
            syntax_error,
        }))
    }

    /// The error the restricted evaluator reports for a text that does not
    /// parse (`None` when it does).
    pub(crate) fn syntax_error(&self) -> Option<&ExprError> {
        self.0.syntax_error.as_ref()
    }

    /// Every expression carried by the program (assignment right-hand sides,
    /// conditions, expression statements, iterables, ...), parsed by the
    /// restricted parser; one that it cannot parse is left out. Used by
    /// expression-location scanning (`crate::normalize::expression_locations`)
    /// to find cross-table references inside multi-statement `expr:` blocks,
    /// mirroring upstream's `ast.walk` over an exec-mode parse.
    pub fn exprs(&self) -> Vec<&Ast> {
        self.0.exprs.iter().collect()
    }

    /// Every name bound by a statement in this program (assignment, `for` and
    /// `with` targets, augmented assignment): the `Store`-context names that
    /// expression-reference scanning (`crate::validate::expr_refs`) must
    /// exclude, mirroring how upstream's `extract_expr_slot_references` drops
    /// them. Assignment targets are not `Ast` nodes in the restricted parser,
    /// so they cannot be recovered by walking [`exprs`](Self::exprs).
    pub fn assigned_names(&self) -> Vec<String> {
        self.0.assigned.clone()
    }

    /// Evaluate the program against `vars` and return the `target` symbol.
    ///
    /// `vars["src"]` is the source object, or, when absent, the whole of `vars`
    /// (less the engine's own `NULL` / `__slot_values` entries). Equivalent to
    /// [`Program::eval_vars`] with `raise_undefined` set.
    ///
    /// # Errors
    /// Only the cases described on [`Program::eval_vars`].
    pub fn eval(&self, vars: &Bindings) -> ExprResult<Value> {
        match vars.get("src") {
            Some(src) => self.eval_src(src, vars.get("__slot_values"), true),
            None => self.eval_vars(vars, true),
        }
    }

    /// Run the program the way upstream's `ObjectTransformer._unrestricted_eval`
    /// does and return the `target` symbol.
    ///
    /// The block runs in a fresh namespace holding only `src` (the source
    /// object: every binding of `vars` but `NULL` and `__slot_values`),
    /// `target` (pre-seeded to None), `uuid5`, the `slot()` lookup (when
    /// `vars` carries `__slot_values`) and what `asteval` itself provides:
    /// Python's built-in functions, types and exceptions, and the `math`
    /// functions. Never the bare slot names, join aliases, `NULL` or the LinkML
    /// built-ins of single expressions.
    ///
    /// PARITY: `asteval` stops at the first error and swallows it. With
    /// `raise_undefined` (`_unrestricted_eval(.., raise_undefined=True)`, which
    /// `_eval_expr` sets for statement blocks) a `NameError` at the head of the
    /// error list (an undefined name, typically a bare slot or a `{slot}`
    /// reference) is re-raised as `ValueError("Unrestricted expression uses an
    /// undefined name: ...")`. Every other error, and a `NameError` without
    /// `raise_undefined` (the enum-expression fallback), leaves `target` as it
    /// was: None if never assigned. See `_unrestricted_eval` in
    /// `transformer/object_transformer.py`.
    ///
    /// # Errors
    /// The undefined-name case above ([`ExprError::Eval`]); a block that
    /// deletes `target` (`aeval.symtable["target"]` is a `KeyError`) or exceeds
    /// the step limit ([`ExprError::Uncaught`]).
    pub fn eval_vars(&self, vars: &Bindings, raise_undefined: bool) -> ExprResult<Value> {
        let slot_values = match vars.get("__slot_values") {
            Some(Value::Map(m)) => Some(m),
            _ => None,
        };
        let src = asteval::build_src(vars.iter(), self.0.src_attrs.as_deref());
        self.run(src, slot_values, raise_undefined)
    }

    /// As [`Program::eval_vars`] with the source object given as a value (the
    /// enum-expression fallback, where `src` is whatever the source is).
    pub fn eval_src(
        &self,
        src: &Value,
        slot_values: Option<&Value>,
        raise_undefined: bool,
    ) -> ExprResult<Value> {
        let slots = match slot_values {
            Some(Value::Map(m)) => Some(m),
            _ => None,
        };
        // The enum path hands `src` over as the plain source object (a dict).
        self.run(asteval::src_from_value(src), slots, raise_undefined)
    }

    fn run(
        &self,
        src: asteval::PySrc,
        slot_values: Option<&indexmap::IndexMap<String, Value>>,
        raise_undefined: bool,
    ) -> ExprResult<Value> {
        let Ok(parsed) = &self.0.compiled else {
            // asteval swallows the SyntaxError: `target` stays None.
            return Ok(Value::Null);
        };
        match asteval::execute(parsed, &self.0.text, src, slot_values, raise_undefined) {
            Ok(v) => Ok(v),
            Err(asteval::ExecError::Undefined { name, message }) => {
                let _ = name;
                Err(ExprError::Eval(message))
            }
            Err(asteval::ExecError::Uncaught(m)) => Err(ExprError::Uncaught(m)),
        }
    }

    /// Evaluate the program the way upstream's restricted evaluator does.
    ///
    /// PARITY: `simpleeval.SimpleEval.parse` keeps only the first statement
    /// (`ast.parse(expr.strip()).body[0]`, with a `MultipleExpressions`
    /// warning for the rest). An assignment evaluates its right-hand side and
    /// returns that value, ignoring the target (`_eval_assign`, with an
    /// `AssignmentAttempted` warning), so `x = {age} * 2\ntarget = x + 1`
    /// yields `age * 2`; an augmented assignment likewise (`_eval_aug_assign`).
    /// A statement the evaluator has no handler for, such as `if`, is
    /// `FeatureNotAvailable`, an `InvalidExpression`; so is `import`. A text
    /// that does not parse raises its `SyntaxError`.
    pub(super) fn eval_first_statement(&self, vars: &Bindings) -> ExprResult<Value> {
        match &self.0.first {
            First::Value(ast) => eval_ast_public(ast, vars),
            First::Unavailable(kind) => Err(ExprError::Eval(format!(
                "Sorry, {kind} is not available in this evaluator"
            ))),
            First::Empty => Err(ExprError::Eval(
                "Sorry, cannot evaluate empty string".into(),
            )),
            First::Syntax(e) => Err(e.clone()),
        }
    }
}

fn assigned_names(module: &asteval::ast::Module) -> Vec<String> {
    use asteval::ast::{Expr, Stmt};
    fn target(e: &Expr, out: &mut Vec<String>) {
        match e {
            Expr::Name(n, _) => out.push(n.clone()),
            Expr::Tuple(v) | Expr::List(v) => v.iter().for_each(|x| target(x, out)),
            Expr::Starred(inner) => target(inner, out),
            _ => {}
        }
    }
    fn stmts(body: &[Stmt], out: &mut Vec<String>) {
        for s in body {
            match s {
                Stmt::Assign(ts, _) => ts.iter().for_each(|t| target(t, out)),
                Stmt::AugAssign(t, _, _) => target(t, out),
                Stmt::For(t, _, b, o) => {
                    target(t, out);
                    stmts(b, out);
                    stmts(o, out);
                }
                Stmt::With(items, b) => {
                    for it in items {
                        if let Some(v) = &it.optional_vars {
                            target(v, out);
                        }
                    }
                    stmts(b, out);
                }
                Stmt::If(_, b, o) | Stmt::While(_, b, o) => {
                    stmts(b, out);
                    stmts(o, out);
                }
                Stmt::Try(b, hs, o, f) => {
                    stmts(b, out);
                    for h in hs {
                        stmts(&h.body, out);
                    }
                    stmts(o, out);
                    stmts(f, out);
                }
                Stmt::FunctionDef(def) => stmts(&def.body, out),
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    stmts(&module.body, &mut out);
    out
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
    let restricted = parse_with_restricted_parser(expr);
    // CPython decides what is an expression; the restricted parser then builds
    // the tree (it may know less than Python, never more).
    if !asteval::parses_as_expression(expr.trim_matches(super::pystr::is_py_space)) {
        return Err(restricted
            .err()
            .unwrap_or_else(|| ExprError::Parse("invalid syntax".into())));
    }
    restricted
}

fn parse_with_restricted_parser(expr: &str) -> ExprResult<Ast> {
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
/// A block never fails to parse here: `asteval` swallows a `SyntaxError`
/// (`unrestricted_eval` then yields None) and the restricted evaluator raises
/// it when the block is evaluated, as upstream does, so a text that is not
/// Python is a [`Program`] that remembers its error.
pub(crate) fn parse_single_or_program(expr: &str) -> ExprResult<SingleOrProgram> {
    match parse_single_expression(expr) {
        Ok(ast) => Ok(SingleOrProgram::Single(ast)),
        Err(single_err) => Ok(SingleOrProgram::Program(Program::new(expr, single_err))),
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
/// # Errors
/// A syntax error.
pub fn parse_program(src: &str) -> ExprResult<Program> {
    let program = Program::new(src, ExprError::Parse("invalid syntax".into()));
    match program.syntax_error() {
        Some(e) => Err(e.clone()),
        None => Ok(program),
    }
}

/// A run of physical lines (zero-based, inclusive) that one logical line spans.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Span {
    first: usize,
    last: usize,
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
    fn syntax_errors_become_programs_that_remember_the_error() {
        // asteval swallows the SyntaxError; the restricted evaluator raises it.
        let Ok(SingleOrProgram::Program(p)) = parse_single_or_program("x = \ntarget = 1") else {
            panic!("expected a program");
        };
        assert!(matches!(p.syntax_error(), Some(ExprError::Parse(_))));
        assert_eq!(p.eval(&Bindings::new()).unwrap(), Value::Null);
        assert!(matches!(
            p.eval_first_statement(&Bindings::new()),
            Err(ExprError::Parse(_))
        ));
        // Not statements: the error is the expression parser's, as before.
        let Ok(SingleOrProgram::Program(p)) = parse_single_or_program("{1 + }") else {
            panic!("expected a program");
        };
        assert_eq!(p.syntax_error(), parse_single_expression("{1 + }").err().as_ref());
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
