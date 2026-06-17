//! Compile a [`TransformationSpecification`] to SQL statements executable by DuckDB.
//!
//! The output is one `INSERT INTO <target> SELECT ... FROM <source> [JOIN ...]`
//! statement per class derivation, mirroring Python's `SQLCompiler`.
//!
//! # Supported derivation forms
//!
//! | Spec field                       | SQL output                         |
//! |----------------------------------|------------------------------------|
//! | `value: "lit"`                   | `'lit' AS slot`                    |
//! | `populated_from: "field"`        | `field AS slot`                    |
//! | `populated_from: "alias.field"`  | `alias.field AS slot`              |
//! | `expr: "{a} + {b}"`              | `a + b AS slot` (brace-stripped)   |
//! | `aggregation_operation: Sum`     | `SUM(field) AS slot`               |
//! | `joins:` entries                 | `LEFT JOIN table AS alias ON ...`  |
//! | (implicit) same-name             | `field AS field`                   |
//!
//! ## Limitations
//! - `unit_conversion`, `object_derivations`, `pivot_operation`, `offset`, and
//!   `value_mappings` are not compiled; those slots emit `NULL AS slot` with a
//!   log warning.
//! - `expr:` is compiled via best-effort brace-stripping.  Python-style ternary
//!   (`x if cond else y`) and `in`/`not in` are not translated; they fall back
//!   to `NULL`.

use indexmap::IndexMap;
use linkml_map_core::datamodel::{
    AggregationType, ClassDerivation, SlotDerivation, TransformationSpecification,
};

// ── public types ──────────────────────────────────────────────────────────────

/// A compiled SQL program: one statement per class derivation.
#[derive(Debug, Default, Clone)]
pub struct CompiledSql {
    /// Ordered list of `(target_table_name, sql_statement)` pairs.
    pub statements: Vec<(String, String)>,
}

impl CompiledSql {
    /// All SQL statements concatenated with `;\n`.
    pub fn to_sql_string(&self) -> String {
        self.statements
            .iter()
            .map(|(_, s)| s.as_str())
            .collect::<Vec<_>>()
            .join(";\n")
    }
}

// ── compiler ──────────────────────────────────────────────────────────────────

/// Compiles a [`TransformationSpecification`] to [`CompiledSql`].
#[derive(Debug, Default, Clone)]
pub struct SqlCompiler;

impl SqlCompiler {
    pub fn new() -> Self {
        Self
    }

    /// Compile all class derivations in `spec`.
    pub fn compile(&self, spec: &TransformationSpecification) -> CompiledSql {
        let mut out = CompiledSql::default();
        let derivations = match spec.class_derivations.as_ref() {
            Some(d) => d,
            None => return out,
        };
        for cd in derivations {
            if let Some(stmt) = self.compile_class(cd) {
                out.statements.push((cd.name.clone(), stmt));
            }
        }
        out
    }

    fn compile_class(&self, cd: &ClassDerivation) -> Option<String> {
        let target = &cd.name;
        let source = cd.populated_from.as_deref().unwrap_or(target.as_str());

        let empty = IndexMap::new();
        let slots = cd.slot_derivations.as_ref().unwrap_or(&empty);
        if slots.is_empty() {
            return None;
        }

        // Build SELECT columns.
        let mut select_cols: Vec<String> = Vec::new();
        // Track which slots are aggregated (needed for GROUP BY).
        let mut aggregated_slots: Vec<String> = Vec::new();

        for (_, sd) in slots {
            if sd.hide.unwrap_or(false) {
                continue;
            }
            let (col_expr, is_agg) = self.compile_slot(sd);
            select_cols.push(format!("{col_expr} AS {}", sd.name));
            if is_agg {
                aggregated_slots.push(sd.name.clone());
            }
        }

        if select_cols.is_empty() {
            return None;
        }

        // Build FROM + JOIN clause.
        let from_clause = self.compile_from(source, cd);

        // GROUP BY: all non-aggregated slots when at least one agg present.
        let group_by = if !aggregated_slots.is_empty() {
            let non_agg: Vec<String> = slots
                .values()
                .filter(|sd| {
                    !sd.hide.unwrap_or(false)
                        && sd.aggregation_operation.is_none()
                        && sd.value.is_none()
                })
                .map(|sd| {
                    // Use the source expression (without alias) for GROUP BY.
                    let (expr, _) = self.compile_slot(sd);
                    expr
                })
                .collect();
            if non_agg.is_empty() {
                String::new()
            } else {
                format!("\nGROUP BY {}", non_agg.join(", "))
            }
        } else {
            String::new()
        };

        let cols = select_cols.join(",\n  ");
        let stmt = format!("INSERT INTO {target}\nSELECT\n  {cols}\n{from_clause}{group_by}");
        Some(stmt)
    }

    /// Returns `(sql_expression, is_aggregated)`.
    fn compile_slot(&self, sd: &SlotDerivation) -> (String, bool) {
        let name = sd.name.as_str();

        // Constant value.
        if let Some(v) = &sd.value {
            let lit = json_to_sql_literal(v);
            return (lit, false);
        }

        // Unsupported forms → NULL.
        if sd.unit_conversion.is_some()
            || sd.class_derivations.is_some()
            || sd.pivot_operation.is_some()
            || sd.offset.is_some()
            || sd.value_mappings.is_some()
        {
            return (format!("NULL /*unsupported: {name}*/"), false);
        }

        // Aggregation: aggregate `populated_from` (or same-name) column.
        if let Some(agg) = &sd.aggregation_operation {
            let src = sd.populated_from.as_deref().unwrap_or(name);
            let sql_agg = agg_to_sql(agg.operator.clone(), src);
            return (sql_agg, true);
        }

        // Expression: strip `{` `}` braces for SQL column references.
        if let Some(expr) = &sd.expr {
            if let Some(sql) = expr_to_sql(expr) {
                return (sql, false);
            }
            return (format!("NULL /*expr unsupported: {name}*/"), false);
        }

        // Direct field reference.
        let src = sd.populated_from.as_deref().unwrap_or(name);
        (src.to_string(), false)
    }

    fn compile_from(&self, source: &str, cd: &ClassDerivation) -> String {
        let mut s = format!("FROM {source}");
        if let Some(joins) = &cd.joins {
            for (alias, ac) in joins {
                let table = ac.class_named.as_deref().unwrap_or(alias.as_str());
                // ON condition: source.source_key = alias.lookup_key
                let src_key = ac
                    .source_key
                    .as_deref()
                    .or(ac.join_on.as_deref())
                    .unwrap_or(alias.as_str());
                let lkp_key = ac
                    .lookup_key
                    .as_deref()
                    .or(ac.join_on.as_deref())
                    .unwrap_or(alias.as_str());
                s.push_str(&format!(
                    "\nLEFT JOIN {table} AS {alias} ON {source}.{src_key} = {alias}.{lkp_key}"
                ));
            }
        }
        s
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Map `AggregationType` to a SQL aggregate function call.
fn agg_to_sql(agg: AggregationType, col: &str) -> String {
    match agg {
        AggregationType::Sum => format!("SUM({col})"),
        AggregationType::Count => format!("COUNT({col})"),
        AggregationType::Min => format!("MIN({col})"),
        AggregationType::Max => format!("MAX({col})"),
        AggregationType::Average => format!("AVG({col})"),
        AggregationType::StdDev => format!("STDDEV({col})"),
        AggregationType::Variance => format!("VARIANCE({col})"),
        AggregationType::Median => {
            format!("PERCENTILE_CONT(0.5) WITHIN GROUP (ORDER BY {col})")
        }
        AggregationType::Mode => {
            format!("MODE() WITHIN GROUP (ORDER BY {col})")
        }
        AggregationType::Set => format!("ARRAY_AGG(DISTINCT {col})"),
        AggregationType::List | AggregationType::Array => format!("ARRAY_AGG({col})"),
        AggregationType::Custom => format!("NULL /*custom agg on {col}*/"),
    }
}

/// Convert a serde_json `Value` to a SQL literal string.
fn json_to_sql_literal(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Null => "NULL".to_string(),
        serde_json::Value::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => {
            // Escape single quotes by doubling them (standard SQL).
            format!("'{}'", s.replace('\'', "''"))
        }
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
            format!("'{}'", v.to_string().replace('\'', "''"))
        }
    }
}

/// Best-effort translation of a linkml-map expr string to SQL.
///
/// - Strips `{var}` braces: `{a} + {b}` → `a + b`
/// - Substitutes Python operators: `==` → `=`, `not ` → `NOT `, etc.
/// - Returns `None` when the expression contains Python-specific syntax
///   (ternary `if/else`) that cannot be trivially translated.
fn expr_to_sql(expr: &str) -> Option<String> {
    // Bail on Python ternary — would need a CASE WHEN rewrite.
    if expr.contains(" if ") && expr.contains(" else ") {
        return None;
    }
    // Bail on `in` / `not in` list syntax (Python-specific).
    if expr.contains(" in [") || expr.contains(" not in [") {
        return None;
    }

    let mut s = expr.to_string();
    // Strip {var} braces — replace {identifier} with identifier.
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '{' {
            let mut ident = String::new();
            for ic in chars.by_ref() {
                if ic == '}' {
                    break;
                }
                ident.push(ic);
            }
            out.push_str(&ident);
        } else {
            out.push(c);
        }
    }
    s = out;

    // Python → SQL operator substitution.
    s = s.replace("==", "=");
    s = s.replace("!=", "<>");
    s = s.replace(" and ", " AND ");
    s = s.replace(" or ", " OR ");
    s = s.replace(" not ", " NOT ");

    Some(s.trim().to_string())
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use linkml_map_core::datamodel::{
        AggregationOperation, AliasedClass, ClassDerivation, SlotDerivation,
        TransformationSpecification,
    };

    fn simple_spec(cd: ClassDerivation) -> TransformationSpecification {
        TransformationSpecification {
            class_derivations: Some(vec![cd]),
            ..Default::default()
        }
    }

    fn sd(name: &str, pf: &str) -> SlotDerivation {
        SlotDerivation {
            name: name.to_string(),
            populated_from: Some(pf.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn test_simple_populated_from() {
        let mut slots = IndexMap::new();
        slots.insert("id".to_string(), sd("id", "patient_id"));
        slots.insert("name".to_string(), sd("name", "full_name"));
        let cd = ClassDerivation {
            name: "Patient".to_string(),
            populated_from: Some("RawPatient".to_string()),
            slot_derivations: Some(slots),
            ..Default::default()
        };
        let compiled = SqlCompiler::new().compile(&simple_spec(cd));
        assert_eq!(compiled.statements.len(), 1);
        let sql = &compiled.statements[0].1;
        assert!(sql.contains("INSERT INTO Patient"));
        assert!(sql.contains("patient_id AS id"));
        assert!(sql.contains("full_name AS name"));
        assert!(sql.contains("FROM RawPatient"));
    }

    #[test]
    fn test_expr_brace_strip() {
        assert_eq!(expr_to_sql("{age} * 2"), Some("age * 2".to_string()));
        assert_eq!(expr_to_sql("{a} + {b}"), Some("a + b".to_string()));
        assert_eq!(expr_to_sql("{x} == {y}"), Some("x = y".to_string()));
        // Ternary not supported.
        assert_eq!(expr_to_sql("{x} if {c} else {y}"), None);
    }

    #[test]
    fn test_aggregation_group_by() {
        let mut slots = IndexMap::new();
        slots.insert("pid".to_string(), sd("pid", "patient_id"));
        let agg_sd = SlotDerivation {
            name: "total_cost".to_string(),
            populated_from: Some("cost".to_string()),
            aggregation_operation: Some(AggregationOperation {
                operator: AggregationType::Sum,
                null_handling: None,
                invalid_value_handling: None,
            }),
            ..Default::default()
        };
        slots.insert("total_cost".to_string(), agg_sd);
        let cd = ClassDerivation {
            name: "Summary".to_string(),
            populated_from: Some("Visits".to_string()),
            slot_derivations: Some(slots),
            ..Default::default()
        };
        let compiled = SqlCompiler::new().compile(&simple_spec(cd));
        let sql = &compiled.statements[0].1;
        assert!(sql.contains("SUM(cost) AS total_cost"));
        assert!(sql.contains("GROUP BY"));
        assert!(sql.contains("patient_id")); // non-agg col in GROUP BY
    }

    #[test]
    fn test_join_left_join() {
        let mut slots = IndexMap::new();
        slots.insert("pid".to_string(), sd("pid", "patient_id"));
        slots.insert("age".to_string(), sd("age", "demo.age"));
        let mut joins = IndexMap::new();
        joins.insert(
            "demo".to_string(),
            AliasedClass {
                alias: "demo".to_string(),
                class_named: Some("Demographics".to_string()),
                source_key: Some("patient_id".to_string()),
                lookup_key: Some("patient_id".to_string()),
                join_on: None,
            },
        );
        let cd = ClassDerivation {
            name: "Patient".to_string(),
            populated_from: Some("RawPatient".to_string()),
            slot_derivations: Some(slots),
            joins: Some(joins),
            ..Default::default()
        };
        let compiled = SqlCompiler::new().compile(&simple_spec(cd));
        let sql = &compiled.statements[0].1;
        assert!(sql
            .contains("LEFT JOIN Demographics AS demo ON RawPatient.patient_id = demo.patient_id"));
    }
}
