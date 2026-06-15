//! DuckDB-backed transformer for linkml-map.
//!
//! # Features
//!
//! | Feature   | What it adds                                    |
//! |-----------|--------------------------------------------------|
//! _(none)_    | [`SqlCompiler`] — pure-Rust SQL generation, no heavy deps |
//! | `engine`  | [`DuckDBTransformer`] — requires `duckdb` crate  |
//! | `bundled` | Statically links DuckDB (use with `engine`)      |
//!
//! The `engine` feature is **off by default** to keep `cargo test` fast
//! (DuckDB's bundled C++ build takes several minutes on first compile).
//! Enable it explicitly:
//!
//! ```toml
//! linkml-map-duckdb = { features = ["engine", "bundled"] }
//! ```

pub mod compiler;

#[cfg(feature = "engine")]
pub mod transformer;

pub use compiler::{CompiledSql, SqlCompiler};

#[cfg(feature = "engine")]
pub use transformer::DuckDBTransformer;

// ── error ─────────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("DuckDB error: {0}")]
    DuckDb(String),

    #[error("no class derivation found for target type '{0}'")]
    NoDerivation(String),

    #[error("serialisation error: {0}")]
    Serialisation(String),
}

pub type Result<T> = std::result::Result<T, Error>;
