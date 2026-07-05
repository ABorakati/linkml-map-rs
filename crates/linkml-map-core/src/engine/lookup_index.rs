//! In-memory cross-table lookup index for join resolution.
//!
//! Mirrors Python's `LookupIndex` but works entirely in-memory with
//! [`Value`] rows instead of DuckDB-backed file loading.  Each registered
//! table is indexed on a single key column for fast O(1) lookup.
//!
//! # Usage
//!
//! Build a [`LookupIndex`], register secondary tables, attach it to
//! [`ObjectTransformer`] via [`ObjectTransformer::with_lookup_index`], then
//! call `map_object`.  The engine resolves `populated_from: "table.field"`
//! and `expr: "{table.field}"` references through the index.
//!
//! Multi-row aggregation (`aggregation_operation` + join) uses
//! [`LookupIndex::lookup_rows`] to collect all matching rows before
//! reducing with the aggregation operator.

use std::collections::HashMap;
use std::sync::Arc;

use indexmap::IndexMap;

use crate::value::Value;

// ── key coercion ──────────────────────────────────────────────────────────────

/// Coerce a [`Value`] to a string suitable for use as a HashMap key.
fn value_to_key(v: &Value) -> String {
    match v {
        Value::Str(s) => s.clone(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        Value::List(_) | Value::Map(_) => format!("{v:?}"),
    }
}

// ── LookupIndex ───────────────────────────────────────────────────────────────

/// An in-memory cross-table lookup index for join resolution.
///
/// Populated from `Value` objects (lists of row maps).  Supports both
/// single-row (`LIMIT 1`) and multi-row access so that aggregation over
/// joined tables can collect all matching rows.
///
/// `LookupIndex` is `Send + Sync` — it contains only `String` and
/// [`Value`], so a built index wrapped in [`Arc`] can be shared across
/// rayon / tokio worker threads with zero per-row cloning.
#[derive(Debug, Default, Clone)]
pub struct LookupIndex {
    /// table_name → (key_column, key_value_string → rows)
    tables: HashMap<String, (String, HashMap<String, Vec<IndexMap<String, Value>>>)>,
}

impl LookupIndex {
    /// Create an empty index.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a table from a slice of `Value::Map` rows, indexed on `key_column`.
    ///
    /// Rows that are not `Value::Map` or that lack `key_column` are silently
    /// skipped.  Calling again with the same name replaces the existing table.
    pub fn register_table(&mut self, name: &str, rows: &[Value], key_column: &str) {
        let mut index: HashMap<String, Vec<IndexMap<String, Value>>> = HashMap::new();
        for row in rows {
            if let Value::Map(m) = row
                && let Some(key_val) = m.get(key_column) {
                    let key_str = value_to_key(key_val);
                    index.entry(key_str).or_default().push(m.clone());
                }
        }
        self.tables
            .insert(name.to_string(), (key_column.to_string(), index));
    }

    /// Return the **first** row where the registered key column equals `key_val`.
    ///
    /// Mirrors Python `LookupIndex.lookup_row()` (`LIMIT 1` semantics).
    /// Returns `None` when the table is unregistered or no row matches.
    pub fn lookup_row(&self, table: &str, key_val: &str) -> Option<&IndexMap<String, Value>> {
        self.tables.get(table)?.1.get(key_val)?.first()
    }

    /// Return **all** rows where the registered key column equals `key_val`.
    ///
    /// Used for multi-row aggregation (e.g. COUNT / SUM over joined rows).
    /// Returns an empty slice when the table is unregistered or no rows match.
    pub fn lookup_rows(&self, table: &str, key_val: &str) -> &[IndexMap<String, Value>] {
        self.tables
            .get(table)
            .and_then(|(_, idx)| idx.get(key_val))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// True when `table` has been registered.
    pub fn is_registered(&self, table: &str) -> bool {
        self.tables.contains_key(table)
    }

    /// Drop a registered table, releasing its memory.
    pub fn drop_table(&mut self, table: &str) {
        self.tables.remove(table);
    }
}

// Compile-time Send + Sync proof (same pattern as ObjectIndex).
const _: () = {
    fn _assert_send_sync<T: Send + Sync>() {}
    fn _check() {
        _assert_send_sync::<LookupIndex>();
    }
};

// ── LookupIndexRef ────────────────────────────────────────────────────────────

/// A shareable reference to a [`LookupIndex`].
///
/// Wrapping in `Arc` lets a single built index be shared across parallel
/// workers without cloning.
pub type LookupIndexRef = Arc<LookupIndex>;
