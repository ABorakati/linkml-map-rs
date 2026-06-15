//! DuckDB-backed transformer: loads `Value` rows into an in-memory DuckDB
//! database, runs compiled SQL, and returns results as `Vec<Value>`.
//!
//! # Data flow
//!
//! ```text
//! Vec<Value>  ──serialize──►  JSON temp file
//!                                  │
//!                    read_json_auto('path')
//!                                  │
//!                         DuckDB in-memory table
//!                                  │
//!                    INSERT INTO target SELECT ...
//!                                  │
//!                         read back as Vec<Value>
//! ```
//!
//! Temp files are created via the `tempfile` crate and are automatically
//! deleted when the [`TempPath`] guard drops at end of scope.

use std::io::Write as IoWrite;

use duckdb::Connection;
use indexmap::IndexMap;
use linkml_map_core::{datamodel::TransformationSpecification, value::Value};
use tempfile::NamedTempFile;

use crate::{Error, Result, SqlCompiler};

// ── transformer ───────────────────────────────────────────────────────────────

/// Transforms tabular data using DuckDB SQL execution.
///
/// Each call to [`map_rows`] / [`map_rows_with_joins`] opens a **fresh**
/// in-memory DuckDB connection, so tables from previous calls never interfere.
pub struct DuckDBTransformer<'s> {
    spec: &'s TransformationSpecification,
    compiler: SqlCompiler,
}

impl<'s> DuckDBTransformer<'s> {
    /// Create a new transformer for the given spec.
    pub fn new(spec: &'s TransformationSpecification) -> Result<Self> {
        Ok(Self {
            spec,
            compiler: SqlCompiler::new(),
        })
    }

    /// Transform `source_rows` (all of `source_type`) into rows of `target_type`.
    pub fn map_rows(
        &self,
        source_rows: &[Value],
        source_type: &str,
        target_type: &str,
    ) -> Result<Vec<Value>> {
        self.map_rows_with_joins(source_rows, source_type, target_type, &[])
    }

    /// Transform with additional join tables pre-loaded.
    ///
    /// `join_tables` is `&[(table_name, rows)]` — each entry is loaded into
    /// DuckDB before the main transformation SQL runs.
    pub fn map_rows_with_joins(
        &self,
        source_rows: &[Value],
        source_type: &str,
        target_type: &str,
        join_tables: &[(&str, &[Value])],
    ) -> Result<Vec<Value>> {
        if source_rows.is_empty() {
            return Ok(vec![]);
        }

        let conn = Connection::open_in_memory().map_err(|e| Error::DuckDb(e.to_string()))?;

        // Load source table.
        load_json_table(&conn, source_type, source_rows)?;

        // Load join tables.
        for (name, rows) in join_tables {
            if !rows.is_empty() {
                load_json_table(&conn, name, rows)?;
            }
        }

        // Compile and find the target statement.
        let compiled = self.compiler.compile(self.spec);
        let insert_sql = compiled
            .statements
            .iter()
            .find(|(name, _)| name == target_type)
            .map(|(_, sql)| sql.clone())
            .ok_or_else(|| Error::NoDerivation(target_type.to_string()))?;

        // Create empty target table from the SELECT shape (LIMIT 0).
        let select_part = insert_sql
            .splitn(2, '\n')
            .nth(1)
            .unwrap_or(insert_sql.as_str());

        conn.execute_batch(&format!(
            "CREATE TABLE {target_type} AS {select_part} LIMIT 0;"
        ))
        .map_err(|e| Error::DuckDb(format!("creating '{target_type}': {e}")))?;

        // Run the INSERT.
        conn.execute_batch(&format!("{insert_sql};"))
            .map_err(|e| Error::DuckDb(format!("transform SQL: {e}")))?;

        read_table(&conn, target_type)
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Serialise `rows` to a JSON temp file and load into a DuckDB table via
/// `read_json_auto`.
///
/// The temp file is written, flushed, closed (handle dropped), and then passed
/// to DuckDB by path — avoiding Windows file-locking conflicts.
fn load_json_table(conn: &Connection, table: &str, rows: &[Value]) -> Result<()> {
    // Write JSON array to a named temp file.
    let json_array = values_to_json_array(rows)?;
    let mut tmp = NamedTempFile::new().map_err(|e| Error::DuckDb(format!("tempfile: {e}")))?;
    tmp.write_all(json_array.as_bytes())
        .map_err(|e| Error::DuckDb(format!("write tempfile: {e}")))?;
    tmp.flush()
        .map_err(|e| Error::DuckDb(format!("flush tempfile: {e}")))?;

    // Keep the file alive via TempPath until DuckDB finishes reading it.
    // On Windows: close the write handle before DuckDB opens it for read.
    let path = tmp.into_temp_path();
    let path_str = path
        .to_str()
        .ok_or_else(|| Error::DuckDb("non-UTF8 temp path".into()))?
        .replace('\\', "/");

    conn.execute_batch(&format!(
        "CREATE TABLE {table} AS SELECT * FROM read_json_auto('{path_str}');"
    ))
    .map_err(|e| Error::DuckDb(format!("loading '{table}': {e}")))?;

    // path drops here → temp file deleted.
    Ok(())
}

/// Serialise a slice of `Value` rows to a JSON array string.
fn values_to_json_array(rows: &[Value]) -> Result<String> {
    let json_rows: Vec<serde_json::Value> = rows.iter().map(value_to_json).collect();
    serde_json::to_string(&json_rows).map_err(|e| Error::Serialisation(e.to_string()))
}

/// Convert a linkml-map [`Value`] to [`serde_json::Value`] for JSON serialisation.
fn value_to_json(v: &Value) -> serde_json::Value {
    match v {
        Value::Null => serde_json::Value::Null,
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Int(i) => serde_json::Value::Number((*i).into()),
        Value::Float(f) => serde_json::Number::from_f64(*f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Value::Str(s) => serde_json::Value::String(s.clone()),
        Value::List(items) => serde_json::Value::Array(items.iter().map(value_to_json).collect()),
        Value::Map(m) => {
            let obj: serde_json::Map<String, serde_json::Value> = m
                .iter()
                .map(|(k, v)| (k.clone(), value_to_json(v)))
                .collect();
            serde_json::Value::Object(obj)
        }
    }
}

/// Read all rows from a DuckDB table as `Vec<Value::Map>`.
fn read_table(conn: &Connection, table: &str) -> Result<Vec<Value>> {
    let mut stmt = conn
        .prepare(&format!("SELECT * FROM {table}"))
        .map_err(|e| Error::DuckDb(format!("prepare SELECT '{table}': {e}")))?;

    let col_count = stmt.column_count();
    let col_names: Vec<String> = (0..col_count)
        .map(|i| {
            stmt.column_name(i)
                .map_or("?".to_string(), |s| s.to_string())
        })
        .collect();

    let mut rows: Vec<Value> = Vec::new();
    let mut query = stmt
        .query([])
        .map_err(|e| Error::DuckDb(format!("query '{table}': {e}")))?;

    while let Some(row) = query.next().map_err(|e| Error::DuckDb(e.to_string()))? {
        let mut map: IndexMap<String, Value> = IndexMap::new();
        for (i, col) in col_names.iter().enumerate() {
            // Use a type-triaging approach: try most common types in order.
            // This avoids needing to enumerate every DuckDB internal Value variant.
            let v = duck_row_get(row, i);
            map.insert(col.clone(), v);
        }
        rows.push(Value::Map(map));
    }
    Ok(rows)
}

/// Extract column `i` from a DuckDB row, converting to linkml-map [`Value`].
///
/// Tries typed accessors in order: bool → i64 → f64 → String → Null.
/// This avoids depending on the full `duckdb::types::Value` variant set
/// (which changes across duckdb-rs minor versions).
fn duck_row_get(row: &duckdb::Row<'_>, i: usize) -> Value {
    // Null check first.
    if let Ok(v) = row.get::<_, duckdb::types::Value>(i) {
        return duck_value_to_value(v);
    }
    Value::Null
}

/// Convert `duckdb::types::Value` to linkml-map [`Value`].
///
/// Handles the most common DuckDB types.  Unknown / exotic types fall back to
/// a debug-string representation so no information is silently lost.
fn duck_value_to_value(v: duckdb::types::Value) -> Value {
    use duckdb::types::Value as DV;
    match v {
        DV::Null => Value::Null,
        DV::Boolean(b) => Value::Bool(b),
        DV::TinyInt(i) => Value::Int(i as i64),
        DV::SmallInt(i) => Value::Int(i as i64),
        DV::Int(i) => Value::Int(i as i64),
        DV::BigInt(i) => Value::Int(i),
        DV::HugeInt(i) => Value::Int(i as i64),
        DV::UTinyInt(i) => Value::Int(i as i64),
        DV::USmallInt(i) => Value::Int(i as i64),
        DV::UInt(i) => Value::Int(i as i64),
        DV::UBigInt(i) => Value::Int(i as i64),
        DV::Float(f) => Value::Float(f as f64),
        DV::Double(f) => Value::Float(f),
        DV::Text(s) => Value::Str(s),
        DV::Blob(b) => Value::Str(String::from_utf8_lossy(&b).into_owned()),
        DV::Timestamp(_, micros) => Value::Int(micros),
        DV::Date32(d) => Value::Int(d as i64),
        DV::Time64(_, micros) => Value::Int(micros),
        DV::Interval {
            months,
            days,
            nanos,
        } => Value::Str(format!("{months}mo {days}d {nanos}ns")),
        DV::List(items) | DV::Array(items) => {
            Value::List(items.into_iter().map(duck_value_to_value).collect())
        }
        DV::Enum(s) => Value::Str(s),
        DV::Struct(fields) => {
            // duckdb::types::OrderedMap<String, Value>
            let m: IndexMap<String, Value> = fields
                .iter()
                .map(|(k, v)| (k.clone(), duck_value_to_value(v.clone())))
                .collect();
            Value::Map(m)
        }
        DV::Map(entries) => {
            // duckdb::types::OrderedMap<Value, Value>
            let m: IndexMap<String, Value> = entries
                .iter()
                .map(|(k, v)| {
                    let key = match duck_value_to_value(k.clone()) {
                        Value::Str(s) => s,
                        other => format!("{other:?}"),
                    };
                    (key, duck_value_to_value(v.clone()))
                })
                .collect();
            Value::Map(m)
        }
        // Union(Box<Value>) in duckdb 1.x — no tag string.
        DV::Union(boxed) => duck_value_to_value(*boxed),
        // Decimal: rust_decimal::Decimal — convert to f64 via Display.
        DV::Decimal(d) => {
            let s = d.to_string();
            s.parse::<f64>().map(Value::Float).unwrap_or(Value::Str(s))
        }
        // Any future/unknown variants: debug string so nothing is silently lost.
        #[allow(unreachable_patterns)]
        other => Value::Str(format!("{other:?}")),
    }
}
