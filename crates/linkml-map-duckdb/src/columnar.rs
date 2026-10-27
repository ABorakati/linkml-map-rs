//! JSON Lines to Parquet / DuckDB-file conversion.
//!
//! Mirrors the conversion half of upstream
//! `linkml_map/writers/output_streams.py::ColumnarStreamWriter`: the rows are
//! staged as JSON Lines and DuckDB's `read_json_auto` infers a column type per
//! field (`STRUCT` for nested objects, `STRUCT[]` for lists of them).  The
//! caller owns the staging file.

use std::path::Path;

use duckdb::Connection;

use crate::{Error, Result};

// Re-exported so downstream tests can read the artifacts back with the same
// DuckDB build.
pub use duckdb;

/// Write `staged` (JSON Lines) to `target` as a single Parquet file, replacing
/// any existing file.
///
/// Mirror of `ParquetStreamWriter`: an in-memory connection runs
/// `COPY (SELECT * FROM read_json_auto('<staged>')) TO '<target>' (FORMAT PARQUET)`.
///
/// PARITY: upstream relies on `read_json_auto`'s default `sample_size` (20 480
/// rows), so a key that first appears after that point silently loses its
/// column.  Kept as is (suggested upstream issue: `sample_size=-1`).
pub fn jsonl_to_parquet(staged: &Path, target: &Path) -> Result<()> {
    let connection = Connection::open_in_memory().map_err(db_err)?;
    let select = select_staged(staged);
    connection
        .execute_batch(&format!(
            "COPY ({select}) TO {} (FORMAT PARQUET)",
            sql_literal(target)
        ))
        .map_err(db_err)
}

/// Write `staged` (JSON Lines) into the DuckDB database file `target` as table
/// `table`, creating the file if absent.
///
/// Mirror of `DuckDBStreamWriter`: `CREATE OR REPLACE TABLE "<table>" AS SELECT *
/// FROM read_json_auto('<staged>')`.  Other tables in the file are preserved and a
/// same-named table is replaced.
pub fn jsonl_to_duckdb(staged: &Path, target: &Path, table: &str) -> Result<()> {
    let connection = Connection::open(target).map_err(db_err)?;
    let select = select_staged(staged);
    connection
        .execute_batch(&format!(
            "CREATE OR REPLACE TABLE {} AS {select}",
            quote_identifier(table)
        ))
        .map_err(db_err)?;
    // Close explicitly so the file is complete before the caller returns.
    connection.close().map_err(|(_, e)| db_err(e))
}

fn select_staged(staged: &Path) -> String {
    format!("SELECT * FROM read_json_auto({})", sql_literal(staged))
}

fn db_err(e: duckdb::Error) -> Error {
    Error::DuckDb(e.to_string())
}

/// Render a path as a SQL string literal (`'` doubled), as upstream `_sql_literal`.
pub fn sql_literal(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "''"))
}

/// Render a name as a quoted SQL identifier (`"` doubled), as upstream `_quote_identifier`.
pub fn quote_identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_doubles_embedded_quotes() {
        assert_eq!(
            sql_literal(Path::new("/tmp/it's.jsonl")),
            "'/tmp/it''s.jsonl'"
        );
        assert_eq!(quote_identifier("we\"ird"), "\"we\"\"ird\"");
    }

    #[test]
    fn converts_staged_rows_to_parquet_and_table() {
        let dir = tempfile::tempdir().unwrap();
        let staged = dir.path().join("it's.staging.jsonl");
        std::fs::write(
            &staged,
            "{\"id\": \"a\", \"q\": {\"unit\": \"cm\"}}\n{\"id\": \"b\", \"q\": {\"unit\": \"m\"}}\n",
        )
        .unwrap();

        let parquet = dir.path().join("out.parquet");
        jsonl_to_parquet(&staged, &parquet).unwrap();
        let con = Connection::open_in_memory().unwrap();
        let n: i64 = con
            .query_row(
                &format!(
                    "SELECT count(*) FROM read_parquet({})",
                    sql_literal(&parquet)
                ),
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 2);

        let db = dir.path().join("out.duckdb");
        jsonl_to_duckdb(&staged, &db, "we\"ird").unwrap();
        let con = Connection::open(&db).unwrap();
        let unit: String = con
            .query_row(
                "SELECT q.unit FROM \"we\"\"ird\" ORDER BY id LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(unit, "cm");
    }
}
