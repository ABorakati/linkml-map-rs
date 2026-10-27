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

    /// Evaluation failed (unknown function, bad arity, bad operand, ...).
    ///
    /// These are the failures upstream raises as `InvalidExpression`,
    /// `TypeError` or `ValueError`, the three classes
    /// `ObjectTransformer._eval_expr` catches: re-raised in restricted mode,
    /// swallowed into the unrestricted (asteval) fallback otherwise. See
    /// [`ExprError::is_restricted_rejection`].
    #[error("eval error: {0}")]
    Eval(String),

    /// Evaluation failed with an exception upstream does **not** catch:
    /// `IndexError`, `KeyError`, `AttributeError` or a private-attribute
    /// `NameError`. Displays exactly like [`ExprError::Eval`] but propagates in
    /// both evaluation modes, past the unrestricted fallback.
    #[error("eval error: {0}")]
    Uncaught(String),

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

impl ExprError {
    /// An integer result outside `i64`, where Python would return a big
    /// integer. [`crate::value::Value`] has no big integers, so this is an
    /// evaluation error rather than a silently wrapped or saturated number.
    pub(crate) fn big_int() -> Self {
        Self::Eval("integer result out of range (big integers are not supported)".into())
    }

    /// `RecursionError: maximum recursion depth exceeded`. Upstream does not
    /// catch it, so it propagates in both evaluation modes.
    pub(crate) fn recursion() -> Self {
        Self::Uncaught("maximum recursion depth exceeded".into())
    }

    /// True for the failures upstream's `ObjectTransformer._eval_expr` and
    /// `transform_enum` catch: `except (InvalidExpression, TypeError,
    /// ValueError)`. In restricted mode they are re-raised; with
    /// `unrestricted_eval` they hand the expression to the asteval fallback.
    pub fn is_restricted_rejection(&self) -> bool {
        matches!(self, Self::Eval(_))
    }
}

/// Convenience alias.
pub type ExprResult<T> = std::result::Result<T, ExprError>;
