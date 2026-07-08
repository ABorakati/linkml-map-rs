//! Single source of truth for where expressions live in a transformation spec,
//! and for extracting the cross-table references they contain.
//!
//! Native port of Python `linkml_map.utils.expression_locations`. Cross-table
//! references (`{Table.col}`) can appear in *any* expression — not just
//! `SlotDerivation.expr`, but also enum / permissible-value `expr` and the
//! `expression*` mapping tables inherited from `ElementDerivation`. The join
//! normalizer must scan every one of these, so this module enumerates them in
//! one place: [`ExpressionBearing::expressions`] yields every expression string
//! a derivation carries, and [`extract_table_references`] /
//! [`extract_braced_reference_roots`] pull the references out of one expression.

use std::collections::BTreeSet;

use indexmap::IndexMap;

use crate::datamodel::{
    ClassDerivation, EnumDerivation, KeyVal, PermissibleValueDerivation, SlotDerivation,
};
use crate::expr::{expression_asts, Ast, ExprResult};

// ── expression enumeration ──────────────────────────────────────────────────

/// A `*Derivation` that carries expression strings the join normalizer must
/// scan. Mirrors the field set of Python `expression_locations.iter_expressions`
/// (`SELF_EXPR_FIELDS` = `expr`; `MAPPING_EXPR_FIELDS` = `expression_mappings`
/// values, `expression_to_value_mappings` keys, `expression_to_expression_mappings`
/// both sides). Each Rust derivation type carries the subset of these fields it
/// actually has.
pub trait ExpressionBearing {
    /// Every non-empty expression string carried directly by this derivation.
    /// Does not recurse into nested derivations — callers walk the tree.
    fn expressions(&self) -> Vec<String>;
}

/// Push `expr` if present and non-empty (a `SELF_EXPR_FIELDS` field).
fn push_self_expr(out: &mut Vec<String>, expr: &Option<String>) {
    if let Some(e) = expr
        && !e.is_empty()
    {
        out.push(e.clone());
    }
}

/// Push each non-empty mapping *key* (the `"keys"`/`"both"` case): the map key
/// is itself the expression.
fn push_mapping_keys(out: &mut Vec<String>, mapping: &Option<IndexMap<String, KeyVal>>) {
    if let Some(m) = mapping {
        for key in m.keys() {
            if !key.is_empty() {
                out.push(key.clone());
            }
        }
    }
}

/// Push each non-empty mapping *value* (the `"values"`/`"both"` case): the
/// entry value is the expression. Non-string values (e.g. literal
/// `value_mappings` targets) never reach here — only the `expression*` tables
/// are passed.
fn push_mapping_values(out: &mut Vec<String>, mapping: &Option<IndexMap<String, KeyVal>>) {
    if let Some(m) = mapping {
        for entry in m.values() {
            if let Some(serde_json::Value::String(s)) = &entry.value
                && !s.is_empty()
            {
                out.push(s.clone());
            }
        }
    }
}

impl ExpressionBearing for SlotDerivation {
    fn expressions(&self) -> Vec<String> {
        let mut out = Vec::new();
        push_self_expr(&mut out, &self.expr);
        push_mapping_values(&mut out, &self.expression_mappings);
        push_mapping_keys(&mut out, &self.expression_to_value_mappings);
        push_mapping_keys(&mut out, &self.expression_to_expression_mappings);
        push_mapping_values(&mut out, &self.expression_to_expression_mappings);
        out
    }
}

impl ExpressionBearing for EnumDerivation {
    fn expressions(&self) -> Vec<String> {
        let mut out = Vec::new();
        push_self_expr(&mut out, &self.expr);
        push_mapping_keys(&mut out, &self.expression_to_value_mappings);
        push_mapping_keys(&mut out, &self.expression_to_expression_mappings);
        push_mapping_values(&mut out, &self.expression_to_expression_mappings);
        out
    }
}

impl ExpressionBearing for PermissibleValueDerivation {
    fn expressions(&self) -> Vec<String> {
        let mut out = Vec::new();
        push_self_expr(&mut out, &self.expr);
        push_mapping_keys(&mut out, &self.expression_to_value_mappings);
        push_mapping_keys(&mut out, &self.expression_to_expression_mappings);
        push_mapping_values(&mut out, &self.expression_to_expression_mappings);
        out
    }
}

impl ExpressionBearing for ClassDerivation {
    fn expressions(&self) -> Vec<String> {
        // A ClassDerivation has no `expr` / `expression_mappings` of its own,
        // only the `expression_to_*` tables inherited from ElementDerivation.
        let mut out = Vec::new();
        push_mapping_keys(&mut out, &self.expression_to_value_mappings);
        push_mapping_keys(&mut out, &self.expression_to_expression_mappings);
        push_mapping_values(&mut out, &self.expression_to_expression_mappings);
        out
    }
}

// ── reference extraction ────────────────────────────────────────────────────

/// Visit every node in `node`, invoking `visit` on each (pre-order).
fn walk_ast<F: FnMut(&Ast)>(node: &Ast, visit: &mut F) {
    visit(node);
    match node {
        Ast::Brace(inner) => walk_ast(inner, visit),
        Ast::Attribute { value, .. } => walk_ast(value, visit),
        Ast::List(items) | Ast::Tuple(items) => {
            for item in items {
                walk_ast(item, visit);
            }
        }
        Ast::Subscript { value, index } => {
            walk_ast(value, visit);
            walk_ast(index, visit);
        }
        Ast::ListComp {
            elt, iter, cond, ..
        } => {
            walk_ast(elt, visit);
            walk_ast(iter, visit);
            if let Some(c) = cond {
                walk_ast(c, visit);
            }
        }
        Ast::Call { args, .. } => {
            for a in args {
                walk_ast(a, visit);
            }
        }
        Ast::Unary { operand, .. } => walk_ast(operand, visit),
        Ast::Binary { left, right, .. } => {
            walk_ast(left, visit);
            walk_ast(right, visit);
        }
        Ast::Compare {
            left, comparators, ..
        } => {
            walk_ast(left, visit);
            for c in comparators {
                walk_ast(c, visit);
            }
        }
        Ast::BoolOp { values, .. } => {
            for v in values {
                walk_ast(v, visit);
            }
        }
        // Leaf nodes.
        Ast::Int(_)
        | Ast::Float(_)
        | Ast::Str(_)
        | Ast::Bool(_)
        | Ast::None
        | Ast::Name(_) => {}
    }
}

/// Return the tables referenced as `{Table.col}` in `expression`.
///
/// The expression is parsed and every attribute access `Table.col` whose root
/// is a bare name matching a known table is collected; bare column names
/// (`{col}`) and attribute access on non-tables are ignored. Mirrors Python
/// `expression_locations.extract_table_references` (which walks an exec-mode
/// parse and matches any `ast.Attribute` on a `ast.Name` root in `table_names`).
///
/// # Errors
/// Propagates a parse error for a malformed expression (surfaced here rather
/// than masked, mirroring upstream's `SyntaxError`).
pub fn extract_table_references(
    expression: &str,
    table_names: &BTreeSet<String>,
) -> ExprResult<BTreeSet<String>> {
    let mut refs = BTreeSet::new();
    for ast in expression_asts(expression)? {
        walk_ast(&ast, &mut |node| {
            if let Ast::Attribute { value, .. } = node
                && let Ast::Name(root) = value.as_ref()
                && table_names.contains(root)
            {
                refs.insert(root.clone());
            }
        });
    }
    Ok(refs)
}

/// Return the bare-name roots of braced `{Name.attr}` references in `expression`.
///
/// Only braced references (`{x.y}` — the LinkML reference syntax) are inspected,
/// so lambda params, comprehension targets, and raw attribute access are not
/// treated as references. Each braced `{x.y}` contributes its root `x`; a bare
/// `{col}` contributes nothing. Mirrors Python
/// `expression_locations.extract_braced_reference_roots` (which inspects
/// `ast.Set` displays).
///
/// # Errors
/// Propagates a parse error for a malformed expression.
pub fn extract_braced_reference_roots(expression: &str) -> ExprResult<BTreeSet<String>> {
    let mut roots = BTreeSet::new();
    for ast in expression_asts(expression)? {
        walk_ast(&ast, &mut |node| {
            if let Ast::Brace(inner) = node
                && let Ast::Attribute { value, .. } = inner.as_ref()
                && let Ast::Name(root) = value.as_ref()
            {
                roots.insert(root.clone());
            }
        });
    }
    Ok(roots)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tables() -> BTreeSet<String> {
        ["Reading", "Measurement", "pht003099"]
            .into_iter()
            .map(String::from)
            .collect()
    }

    fn kv(key: &str, value: &str) -> KeyVal {
        KeyVal {
            key: key.to_string(),
            value: Some(serde_json::Value::String(value.to_string())),
        }
    }

    fn map1(key: &str, value: KeyVal) -> IndexMap<String, KeyVal> {
        let mut m = IndexMap::new();
        m.insert(key.to_string(), value);
        m
    }

    // ── iter_expressions equivalent (ExpressionBearing::expressions) ─────────

    #[test]
    fn self_expr_on_slot() {
        let sd = SlotDerivation {
            name: "x".into(),
            expr: Some("{A.col}".into()),
            ..Default::default()
        };
        assert_eq!(sd.expressions(), vec!["{A.col}".to_string()]);
    }

    #[test]
    fn self_expr_on_enum_and_pv() {
        let ed = EnumDerivation {
            name: "e".into(),
            expr: Some("{B.col}".into()),
            ..Default::default()
        };
        assert_eq!(ed.expressions(), vec!["{B.col}".to_string()]);
        let pv = PermissibleValueDerivation {
            name: "pv".into(),
            expr: Some("{C.col}".into()),
            ..Default::default()
        };
        assert_eq!(pv.expressions(), vec!["{C.col}".to_string()]);
    }

    #[test]
    fn expression_mappings_values_are_exprs() {
        let sd = SlotDerivation {
            name: "x".into(),
            expression_mappings: Some(map1("k", kv("k", "{A.col}"))),
            ..Default::default()
        };
        assert_eq!(sd.expressions(), vec!["{A.col}".to_string()]);
    }

    #[test]
    fn expression_to_value_mappings_keys_are_exprs() {
        let sd = SlotDerivation {
            name: "x".into(),
            expression_to_value_mappings: Some(map1(
                "{A.col} == 1",
                kv("{A.col} == 1", "literal"),
            )),
            ..Default::default()
        };
        assert_eq!(sd.expressions(), vec!["{A.col} == 1".to_string()]);
    }

    #[test]
    fn expression_to_expression_mappings_both_sides() {
        let sd = SlotDerivation {
            name: "x".into(),
            expression_to_expression_mappings: Some(map1("{A.k}", kv("{A.k}", "{B.v}"))),
            ..Default::default()
        };
        let got: BTreeSet<String> = sd.expressions().into_iter().collect();
        assert_eq!(
            got,
            ["{A.k}", "{B.v}"].into_iter().map(String::from).collect()
        );
    }

    #[test]
    fn value_mappings_are_not_expressions() {
        // Literal value_mappings must not be treated as expressions.
        let sd = SlotDerivation {
            name: "x".into(),
            value_mappings: Some(map1("F", kv("F", "Female"))),
            ..Default::default()
        };
        assert!(sd.expressions().is_empty());
    }

    #[test]
    fn empty_derivation_yields_nothing() {
        let sd = SlotDerivation {
            name: "x".into(),
            ..Default::default()
        };
        assert!(sd.expressions().is_empty());
    }

    // ── extract_table_references ─────────────────────────────────────────────

    #[test]
    fn extract_dotted_table_reference() {
        assert_eq!(
            extract_table_references("{Reading.score}", &tables()).unwrap(),
            ["Reading"].into_iter().map(String::from).collect()
        );
    }

    #[test]
    fn extract_ignores_bare_column() {
        assert!(extract_table_references("{score}", &tables())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn extract_ignores_attribute_on_non_table() {
        assert!(extract_table_references("{notatable.x}", &tables())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn extract_multiple_tables_in_one_expression() {
        let expr = "{Reading.id} + \"_\" + {Measurement.id}";
        assert_eq!(
            extract_table_references(expr, &tables()).unwrap(),
            ["Reading", "Measurement"]
                .into_iter()
                .map(String::from)
                .collect()
        );
    }

    #[test]
    fn extract_from_realistic_case_expression() {
        let expr = "case(({phv00254011} == 1, {pht003099.phv00177946} * 365), (True, 0))";
        assert_eq!(
            extract_table_references(expr, &tables()).unwrap(),
            ["pht003099"].into_iter().map(String::from).collect()
        );
    }

    #[test]
    fn extract_malformed_expression_raises() {
        assert!(extract_table_references("{Reading.", &tables()).is_err());
    }

    // ── extract_braced_reference_roots ───────────────────────────────────────

    #[test]
    fn braced_roots_pick_up_attribute_roots_only() {
        assert_eq!(
            extract_braced_reference_roots("{Reading.score}").unwrap(),
            ["Reading"].into_iter().map(String::from).collect()
        );
        // A bare braced reference has no attribute root.
        assert!(extract_braced_reference_roots("{score}")
            .unwrap()
            .is_empty());
    }
}
