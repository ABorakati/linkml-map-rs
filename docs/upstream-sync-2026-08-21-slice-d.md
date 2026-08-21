# Upstream sync review — 2026-08-21 (Slice D: Tests, Packaging, CI, Dependencies)

Point-in-time snapshot. This review covers the long tail of 14 commits from upstream
`linkml/linkml-map` following tag `v0.5.3` (commit `ea0d4ee8c`).

## Summary

This slice reviews 14 upstream commits categorized into two groups:
1. **Group 1 (Test commits — 4 commits):** Two commits reveal unverified semantics or
   divergences against the Rust implementation:
   - `9e073beba48f`: Exercises per-row join fallback and engine parity on unmatched keys.
   - `552747114da5`: Replaces mock unit-conversion tests with live scaffold assertions,
     revealing that Rust's `perform_unit_conversion` silently passes through non-numeric
     strings when `none_if_non_numeric` is false instead of raising an error.
   - `e3236180a7eb`: Deletes the NMDC batch while retaining two edge cases (two-level inlined
     dotpath null propagation and same-target class derivations). Both are supported in Rust
     engine code but unasserted in the 55-case compliance suite.
   - `a7ef5dffbe`: Cleans up dead example files and renames a test module.
2. **Group 2 (Packaging, CI, Dependencies — 10 commits):** All 10 commits are **N/A (CI/packaging)**.
   Upstream widened supported Python versions to Python 3.14 (`302ed8eff84d`); widening our
   `python-compat` CI matrix is recommended.

## Commit-by-commit

| Commit | Date | Summary | Status |
|---|---|---|---|
| `9e073beba48f` | 2026-08-05 | Exercise the per-row join fallback with an actual join | **Reveals unverified semantics (join fallback parity and error collection).** |
| `552747114da5` | 2026-08-12 | Replace mock unit-conversion tests; relocate misfiled engine tests (#323) | **Reveals unverified semantics (unit conversion error on non-numeric input).** |
| `e3236180a7eb` | 2026-08-06 | Delete the NMDC test batch, keeping its two unique scenarios | **N/A (test-only).** Retained scenarios are supported in Rust engine code. |
| `a7ef5dffbe` | 2026-08-05 | Delete assertion-free and unreferenced test files | **N/A (test-only).** Test cleanup and file rename. |
| `bcb2b701e2` | 2026-08-03 | Declare jsonschema, drop deepdiff from runtime deps, delete tox.ini | **N/A (CI/packaging).** Runtime dependency cleanup and tox removal. |
| `302ed8eff8` | 2026-08-04 | Support Python 3.14: drop requires-python upper cap | **N/A (CI/packaging).** Python 3.14 matrix expansion in upstream. |
| `5e9df524f7` | 2026-08-04 | Hold ucumvert at 0.2.x while Python 3.10 is still supported | **N/A (CI/packaging).** Dependabot pin for Python 3.10 compatibility. |
| `29df3c9bc2` | 2026-08-04 | Add Dependabot and a scheduled Python support-window check | **N/A (CI/packaging).** CI automation scripts and workflows. |
| `f27372b431` | 2026-08-04 | Lint with the ruff from uv.lock instead of a separate pre-commit pin | **N/A (CI/packaging).** Pre-commit hook configuration. |
| `02ecbc0572` | 2026-08-05 | Retire the cookiecutter scaffolding in the Makefile and agent docs | **N/A (CI/packaging).** Scaffolding cleanup in Makefile and docs. |
| `47d02d706a` | 2026-08-04 | Bump the actions group with 4 updates | **N/A (CI/packaging).** GitHub Actions version bumps. |
| `d63ca9cc0b` | 2026-08-04 | Bump the minor-and-patch group across 1 directory with 4 updates (#313) | **N/A (CI/packaging).** Dependabot bumps for curies, duckdb, pre-commit, ruff. |
| `0c90a81166` | 2026-08-17 | Bump the minor-and-patch group with 2 updates (#329) | **N/A (CI/packaging).** Dependabot bumps for curies and ruff. |
| `eb28a58cfb` | 2026-08-21 | Bump the minor-and-patch group with 2 updates | **N/A (CI/packaging).** Dependabot bumps for pre-commit and ruff. |

---

## 1. Commit `9e073beba48f106a8fb6ae44af76c016f1a079a1` — "Exercise the per-row join fallback with an actual join" (2026-08-05)

### Upstream behaviour

Upstream commit `9e073beba48f` added three integration tests in `tests/test_transformer/test_join_engine.py`:
1. `test_per_row_fallback_resolves_a_real_join`:
   - Configures a primary table as YAML (`samples.yaml`) and a secondary table as TSV (`sites.tsv`).
   - Because DuckDB cannot directly scan YAML, `can_use_join_engine` returns `False`.
   - The engine falls back to `LookupIndex`.
   - Rows with unmatched foreign keys (`site_code: "NO_SUCH_SITE"`) evaluate `{sites.site_name}` to `None`.
   - Output matches `["Boston Medical", "Seattle Clinic", None]`.
2. `test_per_row_fallback_matches_the_engine`:
   - Runs identical rows with TSV primary (set-based DuckDB engine) vs YAML primary (per-row `LookupIndex`).
   - Asserts `engine_out == fallback_out`.
3. `test_engine_on_error_collects_and_continues`:
   - An expression error (`name: {expr: "1 / 0"}`) on joined records collects errors in `on_error` with `row_index` (0, 1, 2) and `class_derivation_name: "FlatSample"`.

### Check against previously-unverified join semantics

The 2026-07-25 review (`docs/upstream-sync-2026-07-25.md`, "Still deferred") listed two unverified semantic questions for the DuckDB join path (commit `510b0a50ca`):
1. **To-many join deduplication:** Upstream uses `QUALIFY row_number() OVER (PARTITION BY key) = 1` to ensure at most one row per key.
2. **Missing join key column in secondary file:** Upstream degrades to an all-miss null STRUCT instead of raising.

**Findings from `9e073beba48f`:**
- `9e073beba48f` tests **unmatched key values** (foreign key values not present in the secondary table). It proves that unmatched keys resolve to null in both engine and fallback paths.
- `9e073beba48f` does **not** test duplicate keys in the secondary table (to-many deduplication).
- `9e073beba48f` does **not** test a secondary file missing the join key column entirely.
- Neither of the two deferred questions from the 2026-07-25 review is settled by this upstream commit.

### Rust evidence

1. **Rust DuckDB join compiler (`crates/linkml-map-duckdb/src/compiler.rs:180-201`):**
   ```rust
   s.push_str(&format!(
       "\nLEFT JOIN (SELECT * FROM {table} QUALIFY row_number() OVER (PARTITION BY {lkp_key}) = 1) AS {alias} ON {source}.{src_key} = {alias}.{lkp_key}"
   ));
   ```
   Rust already generates `QUALIFY row_number() OVER (PARTITION BY lkp_key) = 1`.
2. **Rust per-row `LookupIndex` (`crates/linkml-map-core/src/engine/lookup_index.rs:79-85`):**
   ```rust
   pub fn lookup_row(&self, table: &str, key_val: &str) -> Option<&IndexMap<String, Value>> {
       self.tables.get(table)?.1.get(key_val)?.first()
   }
   ```
   Rust's `lookup_row` selects the first matching row (`LIMIT 1` semantics).
3. **Rust join binding resolution (`crates/linkml-map-core/src/engine/mod.rs:983-1002`):**
   An unmatched key evaluates `li.lookup_row(...)` to `None`, inserting `bindings.insert(alias, Value::Null)`. Expressions like `{sites.site_name}` resolve to `Value::Null`.

### Verdict

**Reveals unverified semantics.**
Rust's per-row fallback and DuckDB join compiler already match the standard join semantics tested in `9e073beba48f`. The two edge cases (duplicate lookup rows and missing join key header) remain unexercised upstream.

---

## 2. Commit `552747114da55cb42c181cda8f19af33c3bc14dc` — "Replace mock unit-conversion tests; relocate misfiled engine tests (#323)" (2026-08-12)

### Upstream behaviour

Upstream deleted mock tests in `test_object_transformer.py` and added live scaffold-driven tests in `tests/test_transformer/test_unit_conversion_config.py`.
The tests assert the following behaviors for `UnitConversionConfiguration`:
1. **Bare magnitude conversion:** Converts scalar number to target unit (`height_cm: 120` $\rightarrow$ `height_m: 1.2`).
2. **Structured value target:** Emits `{target_magnitude_slot: 2.5, target_unit_slot: "m"}` when target slots are configured.
3. **Numeric string input:** String `"120"` parses as float `120.0` and converts to `1.2`.
4. **`none_if_non_numeric: true`:** Non-numeric string `"A"` returns `None` (`null`).
5. **`none_if_non_numeric: false` (default):** Non-numeric string `"A"` **raises `ValueError`** and fails the transformation.
6. **Schema unit conflict:** When spec `source_unit` conflicts with schema `unit.ucum_code`, transformation **raises `ValueError`**.

### Rust evidence and divergence

In `crates/linkml-map-core/src/engine/mod.rs:834-844` (`perform_unit_conversion`):
```rust
// Coerce magnitude to f64.
let magnitude = match magnitude_val.try_numeric() {
    Some(m) => m,
    None => {
        if uc.none_if_non_numeric.unwrap_or(false) {
            return Ok(Value::Null);
        }
        // No numeric magnitude and not told to null it: leave unchanged.
        return Ok(curr_v.clone());
    }
};
```

**Divergence:**
- When `magnitude_val` is non-numeric (e.g. `Value::Str("A")`) and `none_if_non_numeric` is `false` (or `None`), Rust returns `Ok(curr_v.clone())` (leaves the string unconverted).
- Upstream Python `_perform_unit_conversion` in `reference/upstream-python/transformer/object_transformer.py:1060-1066` executes:
  ```python
  try:
      magnitude = float(magnitude)
  except (TypeError, ValueError):
      if uc.none_if_non_numeric:
          return None
      raise
  ```
- In Python, `float("A")` raises `ValueError`, failing the slot transformation.
- Rust should return `Err(Error::SlotTransform { ... })` instead of `Ok(curr_v.clone())` when `none_if_non_numeric` is not enabled.

### Verdict

**Reveals unverified semantics (and a port divergence).**
Rust must update `perform_unit_conversion` so that non-numeric inputs raise `Error::SlotTransform` when `none_if_non_numeric` is false or unset.

---

## 3. Commit `e3236180a7ebf49bfa7bba0155da067336e55b3a` — "Delete the NMDC test batch, keeping its two unique scenarios" (2026-08-06)

### Upstream behaviour

Upstream deleted four NMDC-specific test files (~1270 lines) and preserved two unique scenarios:
1. **Two-level inlined dotpaths (`test_flatten_inlined.py`):**
   - Expressions referencing two levels of inlined nested objects: `env_broad_scale.term.id`.
   - Null propagation: when the outer object is null, middle is missing, or middle is null, the expression resolves to `None`.
2. **Multiple derivations for the same target class (`test_multiple_derivations_same_target.py`):**
   - Top-level `class_derivations` list contains three derivations all targeting `FlatEvent`.
   - Each derivation specifies a different `populated_from` source class (`Acquisition`, `QualityCheck`, `Assembly`) with distinct discriminators.

### Conformance suite check

- `tests/COMPLIANCE_REPORT.md`: The 55 compliance cases cover `compliance/expr/4:s1 + s2.s3.s4` (dotpaths on present nested objects), but do not explicitly test null propagation through absent intermediate inlined objects.
- Neither scenario is in `tests/COMPLIANCE_REPORT.md` or `tests/golden/`.

### Rust engine support

1. **Inlined dotpath null propagation (`crates/linkml-map-core/src/engine/mod.rs:2025-2088`):**
   `resolve_inline_path` iterates through segments and breaks with `Ok(Value::Null)` as soon as an intermediate map segment resolves to null.
2. **Multiple derivations with identical target name (`crates/linkml-map-core/src/engine/mod.rs:1725-1758`):**
   `get_class_derivation(source_type)` filters by `populated_from == source_type`. It unambiguously selects the correct derivation when derivations share the same target class name.

### Verdict

**N/A (test-only).**
The underlying semantics are already implemented in Rust.

---

## 4. Commit `302ed8eff84d560984b4119614f4f4e123417d68` — "Support Python 3.14: drop requires-python upper cap" (2026-08-04)

### Upstream behaviour

- Changed `pyproject.toml` `requires-python` from `">=3.10,<3.14"` to `">=3.10"`.
- Added `'3.14'` to `.github/workflows/main.yaml` test matrix (`['3.10', '3.11', '3.12', '3.13', '3.14']`).
- Updated doctest in `src/linkml_map/functions/unit_conversion.py` to allow flexible registry unit representation.

### Rust wheel and CI configuration

- `crates/linkml-map-py/pyproject.toml` declares `requires-python = ">=3.8"`.
- `Cargo.toml` pins `pyo3 = { version = "0.23", features = ["extension-module"] }`.
- `.github/workflows/ci.yml` `python-compat` matrix currently tests `['3.11', '3.12']`.

### Verdict

**N/A (CI/packaging).**

---

## Recommendations

1. **Fix non-numeric error handling in `perform_unit_conversion`:**
   In `crates/linkml-map-core/src/engine/mod.rs:837-844`, replace the `Ok(curr_v.clone())` fallback with an error:
   ```rust
   None => {
       if uc.none_if_non_numeric.unwrap_or(false) {
           return Ok(Value::Null);
       }
       return Err(Error::SlotTransform {
           class: source_type.to_string(),
           slot: slot_deriv.name.clone(),
           cause: format!(
               "cannot convert non-numeric value '{magnitude_val:?}' for slot '{src_key}'"
           ),
       });
   }
   ```
2. **Add unit conversion test cases:**
   Add unit tests in `crates/linkml-map-core/` covering:
   - Non-numeric magnitude with `none_if_non_numeric: false` raising an error.
   - Declared `source_unit` in spec contradicting schema slot unit raising an error.
3. **Add regression tests for preserved NMDC scenarios:**
   Add unit tests in `crates/linkml-map-core/` verifying:
   - Two-level inlined dotpaths with null propagation across missing/null intermediate links.
   - Multiple class derivations targeting the same output class in a list.
4. **Widen `python-compat` CI matrix:**
   Widen the matrix in `.github/workflows/ci.yml` (`python-compat` job) to include Python 3.13 and Python 3.14 as CI runners become available.
