//! Error type for the restricted expression evaluator.

use thiserror::Error;

/// Errors raised while lexing, parsing, or evaluating an expression.
///
/// Note: an *unbound variable* is NOT an error — it resolves to
/// [`crate::value::Value::Null`] (SQL-style). These errors correspond to the
/// Python paths that raise `ValueError`/`TypeError`/`NameError`/
/// `InvalidExpression`.
#[derive(Debug, Error, PartialEq)]
pub enum ExprError {
    /// Lexing failed (unexpected character, unterminated string, ...).
    #[error("lex error: {0}")]
    Lex(String),

    /// Parsing failed (unexpected token, bad `{}` contents, ...).
    #[error("parse error: {0}")]
    Parse(String),

    /// Evaluation failed (unknown function, bad arity, private attr, ...).
    #[error("eval error: {0}")]
    Eval(String),
}

/// Convenience alias.
pub type ExprResult<T> = std::result::Result<T, ExprError>;
