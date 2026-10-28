//! `MergedRow`: the parent and joined rows a nested `class_derivations` reads.
//!
//! Port of `MergedRow`, `_merge_rows`, `_resolve_joined_row`'s pre-joined fast path
//! and `_raise_ambiguous_column` (`object_transformer.py`). When a slot's nested
//! `class_derivations` is populated from another table than its parent, the
//! parent's `joins:` names the join, and upstream maps the nested object against a
//! merge of the two rows:
//!
//! * the parent row's columns come first, the joined row's follow;
//! * a column both rows have, other than the join key (the parent-side
//!   `source_key`), is *ambiguous*: upstream stores a sentinel under that name, and
//!   reading it (`populated_from`, an implicit same-name copy, an `expr:` name)
//!   raises. `Table.column` reads the original row of a table instead;
//! * the original rows are kept by table name (`rows_by_table`), the ancestors'
//!   included, so dot-notation and a deeper nested derivation resolve at any depth.
//!
//! The merge is built only for a nested object that joins another table, and is
//! the one copy of the two rows: the original rows are held by reference. An
//! ambiguous column is *left out* of [`MergedRow::map`] and named in
//! [`MergedRow::ambiguous`] instead of holding a sentinel [`Value`], so no code that
//! reads the plain map can mistake it for data; the places upstream checks for the
//! sentinel check the list.

use std::sync::Arc;

use indexmap::IndexMap;

use super::{ObjectTransformer, join_source_key};
use crate::{
    datamodel::ClassDerivation,
    error::{Error, Result},
    value::Value,
};

/// The merge of a parent row and the row of a joined table (upstream `MergedRow`).
pub(super) struct MergedRow<'a> {
    /// The merged columns: the parent's, then the joined row's new ones. The
    /// ambiguous columns are not in it.
    pub(super) map: IndexMap<String, Value>,
    /// The columns both rows have (upstream's `_AMBIGUOUS` entries), carried down
    /// through deeper merges. `None` when there are none.
    pub(super) ambiguous: Option<Arc<[String]>>,
    /// The original row of each table merged so far, parent first
    /// (upstream's `rows_by_table`).
    /// A table with no row (a miss of the set-based engine's `LEFT JOIN`) is `None`.
    rows_by_table: Vec<(&'a str, Option<&'a IndexMap<String, Value>>)>,
}

impl<'a> MergedRow<'a> {
    /// `_merge_rows(parent_row, joined_row, join_key, parent_source, nested_source)`.
    ///
    /// `parent` is `parent_merged`'s own map when the parent row is itself a merge;
    /// its ambiguous columns stay ambiguous and its tables stay available.
    pub(super) fn merge(
        parent: &'a IndexMap<String, Value>,
        parent_merged: Option<&MergedRow<'a>>,
        joined_row: &'a IndexMap<String, Value>,
        join_key: &str,
        parent_source: &'a str,
        nested_source: &'a str,
    ) -> Self {
        let inherited = parent_merged.and_then(|m| m.ambiguous.as_deref());
        let mut ambiguous: Vec<String> = Vec::new();
        let mut map = IndexMap::with_capacity(parent.len() + joined_row.len());
        for (k, v) in parent {
            if k != join_key && joined_row.contains_key(k) {
                ambiguous.push(k.clone());
            } else {
                map.insert(k.clone(), v.clone());
            }
        }
        for (k, v) in joined_row {
            // `if k not in merged`: a name the parent has (ambiguous or not) stays.
            if !parent.contains_key(k) && !inherited.is_some_and(|names| names.contains(k)) {
                map.insert(k.clone(), v.clone());
            }
        }
        if let Some(names) = inherited {
            for name in names {
                if !ambiguous.contains(name) {
                    ambiguous.push(name.clone());
                }
            }
        }
        if !ambiguous.is_empty() {
            log::debug!(
                "Ambiguous columns {ambiguous:?} shared between {parent_source} and {nested_source} \
                 -- dot notation required"
            );
        }

        // `{**inherited, nested_source: joined_row}`: a table seen before keeps its
        // place and takes the new row.
        // `{parent_source: parent_row}` is the start only for a parent that is no merge;
        // a merge carries its own tables.
        let mut rows_by_table: Vec<(&'a str, Option<&'a IndexMap<String, Value>>)> =
            match parent_merged {
                Some(m) => m.rows_by_table.clone(),
                None => vec![(parent_source, Some(parent))],
            };
        match rows_by_table.iter_mut().find(|(t, _)| *t == nested_source) {
            Some(entry) => entry.1 = Some(joined_row),
            None => rows_by_table.push((nested_source, Some(joined_row))),
        }
        Self {
            map,
            ambiguous: (!ambiguous.is_empty()).then(|| ambiguous.into()),
            rows_by_table,
        }
    }

    /// Whether `name` is an ambiguous column (upstream `source_obj[name] is _AMBIGUOUS`).
    pub(super) fn is_ambiguous(&self, name: &str) -> bool {
        self.ambiguous
            .as_deref()
            .is_some_and(|names| names.iter().any(|n| n == name))
    }

    /// `rows_by_table[table]`: `None` when the table is not one of the merged row's
    /// (`table in rows_by_table` is false), `Some(None)` when it is but had no row.
    pub(super) fn table(&self, table: &str) -> Option<Option<&'a IndexMap<String, Value>>> {
        self.rows_by_table
            .iter()
            .find_map(|(t, row)| (*t == table).then_some(*row))
    }

    /// The tables whose original rows are available, with those rows.
    pub(super) fn tables(&self) -> &[(&'a str, Option<&'a IndexMap<String, Value>>)] {
        &self.rows_by_table
    }

    /// The `MergedRow(primary_row, rows_by_table=...)` of the set-based join engine
    /// (`join_engine.transform_block_via_join`): the primary row and, for every join of
    /// the block and of its nested derivations, the row its `source_key` finds, or none.
    ///
    /// The row is read through the engine's `source_map`; this value only carries the
    /// tables, so its [`map`](Self::map) is empty and nothing is ambiguous.
    pub(super) fn prejoin(
        engine: &'a ObjectTransformer<'_>,
        primary: &'a str,
        row: &'a IndexMap<String, Value>,
        class_deriv: &'a ClassDerivation,
    ) -> Self {
        let mut rows_by_table: Vec<(&'a str, Option<&'a IndexMap<String, Value>>)> =
            vec![(primary, Some(row))];
        let mut joins: Vec<(&'a str, &'a crate::datamodel::AliasedClass)> = Vec::new();
        collect_joins(class_deriv, &mut joins);
        for (name, ac) in joins {
            let joined = join_source_key(ac)
                .and_then(|key| row.get(key))
                .and_then(|key| match key {
                    Value::Str(s) => Some(s.clone()),
                    Value::Int(i) => Some(i.to_string()),
                    Value::BigInt(n) => Some(n.to_string()),
                    _ => None,
                })
                .and_then(|key| {
                    let table = ac.class_named.as_deref().unwrap_or(name);
                    engine.lookup_index.as_deref()?.lookup_row(table, &key)
                });
            // `rows_by_table[table] = ...` for each join, after the primary's own entry.
            match rows_by_table.iter_mut().find(|(t, _)| *t == name) {
                Some(entry) => entry.1 = joined,
                None => rows_by_table.push((name, joined)),
            }
        }
        Self {
            map: IndexMap::new(),
            ambiguous: None,
            rows_by_table,
        }
    }
}

/// `join_engine._collect_joins`: this derivation's joins and its nested derivations',
/// keyed by join name; a name already seen keeps its first spec.
fn collect_joins<'a>(
    class_deriv: &'a ClassDerivation,
    acc: &mut Vec<(&'a str, &'a crate::datamodel::AliasedClass)>,
) {
    for (name, join) in class_deriv.joins.iter().flatten() {
        if !acc.iter().any(|(seen, _)| seen == name) {
            acc.push((name, join));
        }
    }
    for (_, slot_deriv) in class_deriv.slot_derivations.iter().flatten() {
        for nested in slot_deriv.class_derivations.iter().flatten() {
            collect_joins(nested, acc);
        }
    }
}

/// `_raise_ambiguous_column`: a read of a column both the parent and the nested
/// source have.
pub(super) fn ambiguous_column_error(column: &str) -> Error {
    Error::Python(format!(
        "TransformationError: Column {column:?} is ambiguous \u{2014} it exists in both the parent \
         and nested source tables. Use dot notation (e.g., 'TableName.{column}') to disambiguate."
    ))
}

impl ObjectTransformer<'_> {
    /// `_resolve_joined_row(table_name, source_obj, class_deriv)`: the row of the joined
    /// table `table_name` (a key of `class_deriv.joins`) for the row `source_map`.
    ///
    /// A merged row that holds the table's row hands it back as it is (the pre-joined
    /// fast path); otherwise the row's `source_key` is looked up in the table's index.
    ///
    /// PARITY: `join_keys(spec)` raises on a spec without keys; no key value on the row
    /// is a miss (`None`); but a key value with no `lookup_index` at all (single-object
    /// input) raises `ValueError`, and with an index that has no such table (a join
    /// table with no file) DuckDB raises a `CatalogException`. Only a registered table
    /// can miss quietly.
    pub(super) fn resolve_joined_row<'r>(
        &'r self,
        table_name: &str,
        source_map: &IndexMap<String, Value>,
        merged: Option<&MergedRow<'r>>,
        class_deriv: &ClassDerivation,
    ) -> Result<Option<&'r IndexMap<String, Value>>> {
        // `table_name in source_obj.rows_by_table`: a table the merge holds, row or not.
        if let Some(row) = merged.and_then(|m| m.table(table_name)) {
            return Ok(row);
        }
        let Some(ac) = class_deriv
            .joins
            .as_ref()
            .and_then(|joins| joins.get(table_name))
        else {
            return Err(Error::Python(format!("KeyError: {table_name:?}")));
        };
        let Some(source_key) = join_source_key(ac) else {
            return Err(Error::Python(format!(
                "ValueError: Join {table_name:?} must specify 'join_on' or both 'source_key' and 'lookup_key'"
            )));
        };
        let key_str = match source_map.get(source_key) {
            Some(Value::Str(s)) => s.clone(),
            Some(Value::Int(i)) => i.to_string(),
            Some(Value::BigInt(n)) => n.to_string(),
            // Null / absent / non-scalar key: no join possible, a miss.
            _ => return Ok(None),
        };
        let Some(li) = self.lookup_index.as_deref() else {
            return Err(Error::Python(format!(
                "ValueError: Join configured for {table_name:?} but lookup_index has not been initialized"
            )));
        };
        let table = ac.class_named.as_deref().unwrap_or(table_name);
        if !li.is_registered(table) {
            return Err(Error::Python(format!(
                "duckdb.CatalogException: Catalog Error: Table with name {table} does not exist!"
            )));
        }
        Ok(li.lookup_row(table, &key_str))
    }
}
