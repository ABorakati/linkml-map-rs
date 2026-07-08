//! Expression reference extraction for semantic spec validation.
//!
//! Native port of the two AST helpers in Python
//! `linkml_map.validator` (commit `5a42c2af67`):
//! `extract_expr_slot_references` and `extract_expr_attribute_references`.
//! Both parse a LinkML transformation expression and collect the identifiers
//! it references, so the validator can check them against the source/target
//! schemas.
//!
//! Upstream walks a CPython `ast` tree; this port walks *this crate's* own
//! expression AST ([`crate::expr::Ast`]) via [`crate::expr::expression_asts`],
//! reproducing the same observable result. Two structural differences between
//! the parsers are handled explicitly (each noted with a `// PARITY:` comment):
//!
//! - A function call stores its callee as `Ast::Call { func: String, .. }`,
//!   not as an `Ast::Name` child, so upstream's "the call target is a
//!   `ast.Name`" collection is reproduced by inserting `func` into the name
//!   set (then filtering by the safe-names set exactly as upstream filters
//!   `ast.Name('case')` via `FUNCTIONS`).
//! - Assignment targets live on `Stmt::Assign.name`, not as `Store`-context
//!   `Ast::Name` nodes, so the "locally bound names" are recovered from
//!   [`crate::expr::stmt::Program::assigned_names`] rather than from the walk.

use std::collections::{BTreeMap, BTreeSet};

use crate::expr::stmt::{is_multi_statement, parse_program};
use crate::expr::{expression_asts, Ast, FUNCTION_NAMES, INJECTED_EVAL_NAMES};

/// Names that must never be treated as slot references in an expression.
///
/// Mirrors Python `validator._EXPR_SAFE_NAMES`: the keyword-values
/// `True`/`False`/`None`, the LinkML expression builtins `NULL`/`target`/`src`,
/// and every registered evaluator function name. In this crate the function
/// names come from [`FUNCTION_NAMES`] (the keys of Python `FUNCTIONS`) and the
/// injected names (`src`, `target`, `uuid5`) from [`INJECTED_EVAL_NAMES`];
/// `True`/`False`/`None` never reach here as bare names (this crate's parser
/// lexes them as literals, not `Ast::Name`) but are included for parity.
fn is_safe_name(name: &str) -> bool {
    FUNCTION_NAMES.contains(&name)
        || INJECTED_EVAL_NAMES.contains(&name)
        || matches!(name, "True" | "False" | "None" | "NULL")
}

/// Visit every node in `node` in pre-order, invoking `visit` on each.
///
/// Structurally identical to the walk in
/// `crate::normalize::expression_locations`; kept local so the `validate`
/// module stays self-contained.
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
        Ast::Int(_)
        | Ast::Float(_)
        | Ast::Str(_)
        | Ast::Bool(_)
        | Ast::None
        | Ast::Name(_) => {}
    }
}

/// Parse `expr` into the ASTs to walk plus the names bound by assignments.
///
/// Returns `None` when the expression cannot be parsed, matching upstream's
/// "return `set()` / `{}` on `SyntaxError`" behaviour. Assignment-target names
/// are only meaningful for multi-statement (`exec`-mode) programs.
fn parse_for_scan(expr: &str) -> Option<(Vec<Ast>, BTreeSet<String>)> {
    let asts = expression_asts(expr).ok()?;
    let mut bound = BTreeSet::new();
    // PARITY: upstream collects `Store`-context ast.Name nodes into `bound`;
    // this crate keeps assignment targets on Stmt::Assign, so recover them from
    // the parsed program (multi-statement exprs only).
    if is_multi_statement(expr)
        && let Ok(program) = parse_program(expr)
    {
        bound.extend(program.assigned_names());
    }
    Some((asts, bound))
}

/// Extract candidate slot-name references from a LinkML expression.
///
/// Collects bare name references (covering both `x` and the `{x}` reference
/// syntax), `src.attr` accesses, and call targets, then removes the known-safe
/// names ([`is_safe_name`]) and locally bound names (assignment targets and
/// comprehension variables). Mirrors Python
/// `validator.extract_expr_slot_references`; an unparsable expression yields an
/// empty set.
pub fn extract_expr_slot_references(expr: &str) -> BTreeSet<String> {
    let Some((asts, mut bound)) = parse_for_scan(expr) else {
        return BTreeSet::new();
    };
    let mut names = BTreeSet::new();
    for ast in &asts {
        walk_ast(ast, &mut |node| match node {
            Ast::Name(id) => {
                names.insert(id.clone());
            }
            // `src.slot` contributes the accessed slot name (upstream's
            // `ast.Attribute` on `ast.Name('src')` branch).
            Ast::Attribute { value, attr } => {
                if let Ast::Name(root) = value.as_ref()
                    && root == "src"
                {
                    names.insert(attr.clone());
                }
            }
            // PARITY: the call target is an `ast.Name` upstream; here it is the
            // `func` string, so add it to the name set (built-ins are then
            // dropped by the safe-names filter, just as upstream drops them via
            // FUNCTIONS.keys()).
            Ast::Call { func, .. } => {
                names.insert(func.clone());
            }
            // A comprehension variable is a locally bound name.
            Ast::ListComp { var, .. } => {
                bound.insert(var.clone());
            }
            _ => {}
        });
    }
    names
        .into_iter()
        .filter(|n| !is_safe_name(n) && !bound.contains(n))
        .collect()
}

/// Extract `base.attr` accesses from a LinkML expression, grouped by base name.
///
/// Used to validate cross-table references like `{joined_table.field}` against
/// the joined class's slots. The `src.attr` form is excluded (it is handled as
/// a bare source-slot reference by [`extract_expr_slot_references`]), as are
/// safe names. Mirrors Python `validator.extract_expr_attribute_references`; an
/// unparsable expression yields an empty map.
pub fn extract_expr_attribute_references(expr: &str) -> BTreeMap<String, BTreeSet<String>> {
    let mut result: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let Some((asts, _)) = parse_for_scan(expr) else {
        return result;
    };
    for ast in &asts {
        walk_ast(ast, &mut |node| {
            if let Ast::Attribute { value, attr } = node
                && let Ast::Name(base) = value.as_ref()
                && base != "src"
                && !is_safe_name(base)
            {
                result.entry(base.clone()).or_default().insert(attr.clone());
            }
        });
    }
    result
}
