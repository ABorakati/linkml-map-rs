# Upstream sync review — 2026-08-21 (Slice A: Expression / Evaluation Semantics)

Point-in-time snapshot for the expression and evaluation semantics slice of the pin bump beyond `v0.5.3` (commit `ea0d4ee8c`). This review covers two upstream commits in `linkml/linkml-map`: `7cffbf0a59` and `0db653b539`.

## Summary

Two upstream commits touching expression evaluation were reviewed:
1. **`7cffbf0a59`**: Routes enum expression evaluation through the gated eval path (`eval_expr_with_mapping`), introduces the `src` binding for scalar enum sources, and respects `unrestricted_eval` and `strict` flags. **Needs porting** in Rust to support `src` bindings on scalar enum derivations and propagate strict-mode evaluation errors.
2. **`0db653b539`**: Refines error messages when attribute access is performed on comprehension loop variables in Python's `simpleeval` engine. **N/A to Rust** because Rust's native recursive AST evaluator already correctly scopes comprehension variables and natively supports attribute access on comprehension elements (`x.attr`).

Neither change is covered by the current 55-case compliance suite or 14 golden fixtures.

## Commit-by-commit

| Commit | Date | Summary | Status |
|---|---|---|---|
| `7cffbf0a59` | 2026-07-31 | Route enum expr evaluation through the gated eval path | **Needs porting.** Rust's `transform_enum` does not bind `src` for scalar source objects and silently suppresses evaluation errors in strict mode. |
| `0db653b539` | 2026-08-04 | Name the real cause when an expression attributes a comprehension variable | **N/A to Rust.** Python `simpleeval` scoping artifact; Rust's AST evaluator already natively supports comprehension variable attribute access. |

---

## Commit `7cffbf0a59` (2026-07-31)

### Upstream behaviour
Upstream title: *"Route enum expr evaluation through the gated eval path"*

**Context ("gated eval path"):** Upstream evaluates transformation expressions primarily through `simpleeval` (restricted/safe evaluation). The "gated eval path" refers to `_unrestricted_eval` (using `asteval.Interpreter`), which serves as an opt-in escape hatch for expressions that cannot be evaluated under `simpleeval` (such as statements or assignment to `target`). This fallback is strictly gated by the `--unrestricted-eval` CLI flag (`self.unrestricted_eval`).

**Before:** In `src/linkml_map/transformer/object_transformer.py` (`transform_enum`), enum-level `expr` evaluation called `eval_expr(enum_deriv.expr, **source_obj, NULL=None)`. If this failed with any exception (or if `source_obj` was a scalar, causing `**source_obj` to raise a `TypeError`), the `except Exception:` block caught the error and unconditionally instantiated `asteval.Interpreter(usersyms={"src": source_obj, "target": None, "uuid5": _uuid5})`. This completely bypassed the `self.unrestricted_eval` security gate. Furthermore, `eval_expr` did not respect `self.strict` or provide `src` for scalar enum mappings in restricted mode.

**After:** `transform_enum` constructs an explicit mapping:
```python
mapping = (
    {**source_obj, "NULL": None}
    if isinstance(source_obj, Mapping)
    else {"src": source_obj, "NULL": None}
)
```
It calls `eval_expr_with_mapping(enum_deriv.expr, mapping, strict=self.strict, warned_unbound=self._warned_unbound_names)`.
On `(InvalidExpression, TypeError, ValueError)`, it checks `if not self.unrestricted_eval: raise` before invoking `self._unrestricted_eval(enum_deriv.expr, source_obj)`.

**Observable differences:**
1. Multi-statement or unrestricted expressions in enum derivations now fail with `TransformationError` if `--unrestricted-eval` is not passed.
2. Scalar enum mappings expose the scalar value as `src` during restricted evaluation (e.g. `'X:' + src`), succeeding without needing `--unrestricted-eval`.
3. In strict mode (`strict=True`), unknown variable references in enum expressions raise an error immediately rather than falling back to `asteval` or falling through.

### Rust evidence
- **`crates/linkml-map-core/src/engine/mod.rs:1675-1699`** (`transform_enum`):
  ```rust
  if let Some(expr) = &ed.expr {
      let mut bindings: Bindings = IndexMap::new();
      if let Value::Map(m) = _source_obj {
          for (k, v) in m {
              bindings.insert(k.clone(), v.clone());
          }
      }
      bindings.insert("NULL".to_string(), Value::Null);
      let evaled = match (
          self.compiled.and_then(|c| c.enum_exprs.get(ed_key)),
          self.strict,
      ) {
          (Some(parsed), false) => eval_parsed(parsed, &bindings),
          (Some(parsed), true) => eval_parsed_strict(parsed, &bindings),
          (None, false) => eval_expr_with_mapping(expr, &bindings),
          (None, true) => eval_expr_with_mapping_strict(expr, &bindings),
      };
      if let Ok(v) = evaled
          && !v.is_null() {
              return Ok(v);
          }
  }
  ```
- **Discrepancy 1 (scalar `src` binding):** When `_source_obj` is not a `Value::Map` (e.g. `Value::Str`), `bindings` does not bind `"src"`. In lax mode, `src` evaluates to `Value::Null` (causing expressions like `'X:' + src` to produce `Null` and fail to return a value). In strict mode, `eval_expr_with_mapping_strict` returns `Err(ExprError::Eval("name 'src' is not defined"))`.
- **Discrepancy 2 (strict-mode error suppression):** `if let Ok(v) = evaled` silently drops any `Err` from `evaled` and falls through to permissible value derivations. In strict mode, an invalid expression or typo in an enum expression should surface as an error (mirroring `eval_expr_for_slot` in `crates/linkml-map-core/src/engine/mod.rs:1032`).

### Verdict
**Needs porting.**

### Implementation instruction
In `crates/linkml-map-core/src/engine/mod.rs` inside `fn transform_enum`:
1. Populate `bindings` with `"src"` when `_source_obj` is not a `Value::Map`:
   ```rust
   let mut bindings: Bindings = IndexMap::new();
   if let Value::Map(m) = _source_obj {
       for (k, v) in m {
           bindings.insert(k.clone(), v.clone());
       }
   } else {
       bindings.insert("src".to_string(), _source_obj.clone());
   }
   bindings.insert("NULL".to_string(), Value::Null);
   ```
2. Handle evaluation errors in strict mode:
   ```rust
   let evaled = match (
       self.compiled.and_then(|c| c.enum_exprs.get(ed_key)),
       self.strict,
   ) {
       (Some(parsed), false) => eval_parsed(parsed, &bindings),
       (Some(parsed), true) => eval_parsed_strict(parsed, &bindings),
       (None, false) => eval_expr_with_mapping(expr, &bindings),
       (None, true) => eval_expr_with_mapping_strict(expr, &bindings),
   };
   match evaled {
       Ok(v) if !v.is_null() => return Ok(v),
       Ok(_) => {}
       Err(e) if self.strict => {
           return Err(Error::ExprEval {
               class: enum_name.to_string(),
               slot: "expr".to_string(),
               cause: e.to_string(),
           });
       }
       Err(_) => {}
   }
   ```
3. Add unit tests covering:
   - Scalar enum derivation expression referencing `src` (e.g. `'X:' + src`).
   - Strict-mode unknown identifier in enum `expr:` raising `Error::ExprEval`.

### Conformance / golden suite check
- `tests/COMPLIANCE_REPORT.md` (`cases_map_enum` in `crates/linkml-map-conformance/src/compliance.rs:763-839`) only tests `permissible_value_derivations`, not `enum_derivations[...].expr`.
- No fixture in `tests/golden/` includes an enum derivation with `expr:`.
- The suite passes 55/55 and cannot detect this drift. Dedicated unit tests are required.

---

## Commit `0db653b539` (2026-08-04)

### Upstream behaviour
Upstream title: *"Name the real cause when an expression attributes a comprehension variable"*

**Before:** In `src/linkml_map/utils/eval_utils.py`, `RestrictedEvaluator` (inheriting from `simpleeval.SimpleEval`) resolves attribute roots (`node.value.attr`) by invoking `super()._eval_name(node.value)`. In `simpleeval`, comprehension loop targets (`c` in `[c.code for c in codes]`) are managed in a local scope via closure dispatch on `ast.Name` and are not stored in `self.names`. Consequently, `super()._eval_name` always raised `NameNotDefined` for any comprehension variable.
`_eval_attribute` caught `NameNotDefined` and raised:
```
Expression references unknown 'c' in 'c.code': no such source class, join, or slot. Typo or stale reference?
```
This falsely diagnosed valid comprehension variable references as typos or stale schema references.

**After:** `RestrictedEvaluator` tracks bound comprehension loop variable names in `self._comprehension_targets` within `_eval_comprehension`. When `NameNotDefined` is caught in `_eval_attribute`, it checks whether the variable name is in `self._comprehension_targets`. If so, it raises:
```
Attribute access on comprehension-bound '<name>' in '<ref>' is not supported. Building records in an expression bypasses the range, cardinality, and datatype coercion applied to derived objects; declare a class_derivation keyed on the source range class and reduce the slot to populated_from instead.
```
If the identifier is not a comprehension target (e.g. `[bogus.code for c in codes]`), it continues to emit the original `"Typo or stale reference?"` message.

### Rust evidence
- **`crates/linkml-map-core/src/expr/eval.rs:342-377`** (`Ast::ListComp`):
  Rust evaluates list comprehensions natively by cloning the current bindings environment into a child `scope` and inserting the loop variable:
  ```rust
  let mut scope = vars.clone();
  let mut out = Vec::new();
  for item in items {
      scope.insert(var.clone(), item);
      if let Some(c) = cond
          && !eval_ast(c, &scope)?.is_truthy()
      {
          continue;
      }
      out.push(eval_ast(elt, &scope)?);
  }
  Ok(Value::List(out))
  ```
- **`crates/linkml-map-core/src/expr/eval.rs:303-326`** (`Ast::Attribute`):
  ```rust
  if let Ast::Name(root) = value.as_ref()
      && !INJECTED_EVAL_NAMES.contains(&root.as_str())
      && !is_function_name(root)
      && !vars.contains_key(root)
  {
      return Err(ExprError::Eval(format!(
          "Expression references unknown '{root}' in '{root}.{attr}': \
           no such source class, join, or slot. Typo or stale reference?"
      )));
  }
  let obj = eval_ast(value, vars)?;
  distributed_getattr(&obj, attr)
  ```
  When `eval_ast(elt, &scope)` executes for an element expression like `c.code`, `vars` is `&scope`, which contains `"c"`. The check `!vars.contains_key(root)` is false, `c` evaluates to `item`, and `distributed_getattr` extracts the attribute.
- **`crates/linkml-map-core/src/expr/mod.rs:497-516`** (`comprehension_attribute_is_per_element`):
  Rust explicitly tests and supports per-element attribute access on comprehension variables:
  `"[x.important_event_date for x in events if str(x.event_name) == \"PASSED_DRIVING_TEST\"]"` correctly evaluates to `["2014-03-31"]`.
- **Typo handling:** When a genuine unbound root is referenced inside a comprehension (e.g. `[bogus.code for c in codes]`), `vars.contains_key("bogus")` returns false and Rust already returns:
  `Expression references unknown 'bogus' in 'bogus.code': no such source class, join, or slot. Typo or stale reference?`

### Verdict
**N/A to Rust.**

Upstream commit `0db653b539` addresses an error reporting shortcoming caused by `simpleeval`'s internal scoping limitation. Rust's native recursive AST evaluator does not use `simpleeval` and already cleanly supports comprehension loop variable scoping and element attribute access. Artificial restriction in Rust would break existing supported functionality.

### Conformance / golden suite check
- Neither `tests/COMPLIANCE_REPORT.md` nor `tests/golden/` tests comprehension attribute access errors.
- Conformance suite passes (55/55) regardless.

---

## Unverified items
- Upstream Python behavior for non-dict mapping types in `isinstance(source_obj, Mapping)` was not verified against custom collections (Rust `Value` only has `Map`, `List`, `Str`, `Int`, `Float`, `Bool`, `Null`).
