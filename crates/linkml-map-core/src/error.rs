use thiserror::Error;

/// Core transformation errors.
#[derive(Debug, Error)]
pub enum Error {
    #[error("stub error")]
    Stub,
    // TODO: Add domain-specific error variants.
}

pub type Result<T> = std::result::Result<T, Error>;
