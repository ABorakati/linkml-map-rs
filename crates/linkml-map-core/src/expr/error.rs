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

    /// A name that has no binding was read. Displays exactly like
    /// [`ExprError::Eval`]; it is a separate variant so the statement
    /// interpreter can tell an undefined name (Python `NameError`, which
    /// upstream re-raises) from every other evaluation error (which asteval
    /// swallows). See [`crate::expr::stmt::Program::eval`].
    #[error("eval error: {message}")]
    Undefined {
        /// The unbound identifier.
        name: String,
        /// The message shown when the error is surfaced as-is.
        message: String,
    },
}

/// Convenience alias.
pub type ExprResult<T> = std::result::Result<T, ExprError>;
