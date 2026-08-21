# Upstream sync review — 2026-08-21

Point-in-time snapshot. This is not living documentation — it records what was
found and decided on this date and will not be kept up to date after this pin
bump. For current pin state, see `CLAUDE.md` → "Upstream reference".

## Summary

Upstream `linkml/linkml-map` `main` is **20 commits** ahead of this repo's pin,
the **`v0.5.3`** tag (commit `ea0d4ee8ec`, 2026-07-16). Upstream has cut **no new
release tag** — `v0.5.3` is still the newest tag, so these 20 commits are
unreleased `main`.

Three items are actionable, in priority order:

1. **A pre-existing parity bug in this port, exposed (not caused) by upstream
   `552747114da5`.** Non-numeric unit-conversion magnitudes with
   `none_if_non_numeric` unset/false **raise** in Python and **silently pass the
   value through** in Rust. This diverges from the **current pin**, not from new
   upstream work — it was masked because the upstream tests that covered this
   path were mocks until 2026-08-12. This is the highest-value finding of the
   review.
2. **`7cffbf0a59` — enum `expr` evaluation.** Two real behavioural gaps: a scalar
   enum source is not bound as `src`, and strict-mode evaluation errors are
   swallowed instead of raised.
3. **`c69db3e0fe` — CLI logging flags.** `-q/--quiet` and `-v/--verbose` do not
   exist in the Rust CLI at all, so a drop-in caller passing them fails.

Everything else is N/A (CI, packaging, dependency bumps, test-only churn, or
Python-implementation-specific), **already matching**, or a deliberate
divergence recorded below.

This review was produced by four parallel investigation agents; their raw
per-slice findings are retained in
`docs/upstream-sync-2026-08-21-slice-{a,b,c,d}.md`. Every claim reproduced in
this document was re-verified directly against the vendored pin copy in
`reference/upstream-python/` and the Rust sources.

## Commit-by-commit

| Commit | Date | Summary | Status |
|---|---|---|---|
| `7cffbf0a59` | 2026-07-31 | Route enum expr evaluation through the gated eval path | **Needs porting.** Scalar `src` binding missing; strict-mode errors swallowed. See below. |
| `bcb2b701e2` | 2026-08-03 | Declare jsonschema, drop deepdiff from runtime deps, delete tox.ini | **N/A.** Python runtime deps + tox removal. |
| `abeeb1b1aa` | 2026-08-03 | Delete unreferenced modules and symbols | **Already matching.** Every deleted symbol (`r2rml_compiler`, `sparql_compiler`, `sssom_compiler`, `importer/`, `transformer_model_new`, `QuantitySyntaxError`, `copy_attributes`) was never ported. Nothing in `crates/linkml-map-py/` exposes them, so the three-surface contract is unaffected. |
| `302ed8eff8` | 2026-08-04 | Support Python 3.14: drop requires-python upper cap | **N/A (packaging).** See recommendation on our `python-compat` matrix. |
| `5e9df524f7` | 2026-08-04 | Hold ucumvert at 0.2.x while Python 3.10 is still supported | **N/A.** Upstream pins its own unit-conversion backend; Rust implements units natively in `crates/linkml-map-core/src/engine/units.rs`. |
| `29df3c9bc2` | 2026-08-04 | Add Dependabot and a scheduled Python support-window check | **N/A.** CI automation. |
| `0db653b539` | 2026-08-04 | Name the real cause when an expression attributes a comprehension variable | **Deliberate divergence.** Works around a `simpleeval` scoping limit Rust does not have; upstream now *refuses* a construct Rust *supports*. See below. |
| `5f92b32129` | 2026-08-04 | Collapse map-data onto a single writer API | **Already matching.** `crates/linkml-map-io/src/writers.rs` already routes every format and both sinks (file, stdout) through one async writer. Output bytes unchanged upstream. |
| `f27372b431` | 2026-08-04 | Lint with the ruff from uv.lock instead of a separate pre-commit pin | **N/A.** Python lint tooling. |
| `47d02d706a` | 2026-08-04 | Bump the actions group with 4 updates | **N/A.** Dependabot. |
| `d63ca9cc0b` | 2026-08-04 | Bump the minor-and-patch group across 1 directory with 4 updates (#313) | **N/A.** Dependabot (curies, duckdb, pre-commit, ruff). |
| `a7ef5dffbe` | 2026-08-05 | Delete assertion-free and unreferenced test files | **N/A (test-only).** |
| `02ecbc0572` | 2026-08-05 | Retire the cookiecutter scaffolding in the Makefile and agent docs | **N/A.** Repo scaffolding. |
| `c69db3e0fe` | 2026-08-05 | Make `-q/--quiet` a boolean flag | **Needs porting.** The Rust CLI has no global logging flags at all. See below. |
| `9e073beba48f` | 2026-08-05 | Exercise the per-row join fallback with an actual join | **N/A (test-only), but settles part of a deferred question.** See below. |
| `e3236180a7eb` | 2026-08-06 | Delete the NMDC test batch, keeping its two unique scenarios | **N/A (test-only).** Both retained scenarios (two-level inlined dotpath null propagation; multiple class derivations for one target class) are implemented in `crates/linkml-map-core/src/engine/mod.rs`, but neither is asserted in our 55-case suite. |
| `552747114da5` | 2026-08-12 | Replace mock unit-conversion tests; relocate misfiled engine tests (#323) | **Exposes a pre-existing parity bug.** Highest-priority item in this review. See below. |
| `0c90a81166` | 2026-08-17 | Bump the minor-and-patch group with 2 updates (#329) | **N/A.** Dependabot (curies, ruff). |
| `429379ce71` | 2026-08-21 | Cache ReferenceValidator object in `Transformer._shape_normalize` | **N/A to Rust.** Pure Python perf fix for a mechanism Rust does not have. See below. |
| `eb28a58cfb` | 2026-08-21 | Bump the minor-and-patch group with 2 updates | **N/A.** Dependabot (pre-commit, ruff). |

---

## 1. `552747114da5` — unit conversion: non-numeric magnitude (pre-existing divergence)

**This is a divergence from the current pin, not new upstream drift.** Upstream
replaced mocked unit-conversion tests with real ones, which is what made the gap
visible; the Python behaviour itself is unchanged and is present in our own
vendored pin copy.

**Pinned upstream** — `reference/upstream-python/transformer/object_transformer.py:1060-1065`:

```python
try:
    magnitude = float(magnitude)
except (TypeError, ValueError):
    if uc.none_if_non_numeric:
        return None
    raise
```

So with `none_if_non_numeric` false or unset, a non-numeric magnitude **raises**
(`ValueError` from `float()`, or `TypeError`).

**Rust** — `crates/linkml-map-core/src/engine/mod.rs:835-844`:

```rust
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

Rust returns the original value unchanged. Observable divergence: for a
`unit_conversion` slot fed `"not a number"` with `none_if_non_numeric` unset,
Python aborts the row with an error and Rust emits the untouched source string.
`none_if_non_numeric: true` behaves identically in both (yields null), which is
why the golden corpus never caught it.

**Action — needs porting.** In `perform_unit_conversion`
(`crates/linkml-map-core/src/engine/mod.rs`), replace the
`return Ok(curr_v.clone())` fall-through with an error return, mirroring the
bare `raise`. Use the existing `Error::SlotTransform` shape already used a few
lines above for the missing-unit and missing-magnitude cases so the surrounding
error surface stays consistent; carry `class`, `slot`, and a cause naming the
non-numeric magnitude.

**Verification required after the change:** re-run the conformance suite and the
golden corpus (`cargo test --workspace`, plus the `conformance_suite` test which
regenerates `tests/COMPLIANCE_REPORT.md`). PASS/FAIL/SKIP counts must not
regress. Add a case covering non-numeric magnitude with `none_if_non_numeric`
unset — this is a new observable contract and is currently untested on both
sides of the port.

---

## 2. `7cffbf0a59` — enum `expr` evaluation

**Upstream before:** `transform_enum` called
`eval_expr(enum_deriv.expr, **source_obj, NULL=None)` and, on *any* exception,
unconditionally built an `asteval.Interpreter` — bypassing the
`unrestricted_eval` opt-in entirely.

**Upstream after:** it builds an explicit mapping (`{**source_obj, "NULL": None}`
for a `Mapping`, else `{"src": source_obj, "NULL": None}`), calls
`eval_expr_with_mapping(..., strict=self.strict, warned_unbound=self._warned_unbound_names)`,
catches only `(InvalidExpression, TypeError, ValueError)`, and re-raises unless
`unrestricted_eval` is set.

**Rust** — `crates/linkml-map-core/src/engine/mod.rs:1676-1699`. Two gaps, and
one structural non-issue:

- **Structural non-issue: there is no gate to route through.** Rust has no
  `unrestricted_eval` flag; the restricted and unrestricted paths are merged into
  one evaluator (`crates/linkml-map-core/src/expr/eval.rs` for single
  expressions, `crates/linkml-map-core/src/expr/stmt.rs` for `target = …`
  statement blocks). Rust is therefore permanently "unrestricted" from Python's
  point of view. Porting the gate itself is a much larger design question and is
  **not** proposed here.
- **Gap 1 — scalar source not bound as `src`.** Rust only contributes bindings
  when `_source_obj` is a `Value::Map` (line 1678). A plain enum-range slot
  passes the scalar permissible value, so upstream's new
  `test_scalar_enum_expr_can_reference_source_as_src_in_restricted_mode`
  (`expr: "'X:' + src"`) has **no** `src` binding in Rust and cannot evaluate.
  Note the slot path already does bind `src` (line 1015) — only the enum path
  does not.
- **Gap 2 — errors swallowed.** Line 1695 is
  `if let Ok(v) = evaled && !v.is_null()`, which discards the error and falls
  through to permissible-value derivations, then `mirror_source`, then
  `Value::Null`. Upstream now raises. In particular a strict-mode typo
  (`eval_expr_with_mapping_strict` returning `Err`) is silently ignored here,
  while the slot path at line 1032 correctly maps the same error into
  `Error::ExprEval`.

**Action — needs porting**, in `fn transform_enum`:
1. When `_source_obj` is not a `Value::Map`, insert `src` bound to the scalar.
2. Propagate the evaluation error instead of discarding it, mirroring the slot
   path's `Error::ExprEval { class, slot, cause }`. Preserve the existing
   "null result falls through to PV derivations" behaviour — upstream still only
   returns `v` when it is non-`None`; it is the *error* case that must change,
   not the null case.

Neither gap is covered by the 55-case compliance suite or the 14 golden
fixtures: no fixture uses `enum_derivations[…].expr`.

---

## 3. `c69db3e0fe` — CLI logging flags

Upstream's `-q/--quiet` was declared without `is_flag=True`, so click consumed
the following token as its value: `-q derive-schema` swallowed the subcommand and
printed group help. The fix makes it a boolean flag and documents the resolved
levels: default `WARNING`, `-q` → `ERROR`, `-v` → `INFO`, `-vv` → `DEBUG`.

**Rust:** the Rust CLI has **no** global logging options — neither `quiet` nor
`verbose` appears anywhere in `crates/linkml-map-cli/`. So the upstream bug is
not reproducible here, but the flags upstream offers are absent, which is a
drop-in gap in its own right: a caller that passes `-q` or `-v` gets a clap
parse error rather than a quieter or louder run.

**Action — needs porting**, scoped as a small addition to
`crates/linkml-map-cli/src/main.rs`: add global `-q/--quiet` (bool) and
`-v/--verbose` (repeatable count) to the top-level `Cli` struct and map them to
the log level with upstream's exact ladder. This is CLI-only; it does not touch
`linkml-map-core` or the PyO3 surface, so the three-surface sync gate in
`harness-ownership.toml` is not tripped.

Note the pre-existing, still-open naming divergence recorded on 2026-07-25:
upstream's subcommand is `validate-spec`, ours is `validate`. Unchanged by this
review.

---

## 4. `0db653b539` — comprehension-bound attribute access (deliberate divergence)

Upstream's `simpleeval` binds comprehension targets in a scope that
`_eval_attribute`'s root lookup deliberately bypasses, so `[c.code for c in codes]`
reported a nonexistent typo. Upstream's fix does **not** make the construct work
— it renames the diagnosis, telling the author that attribute access on a
comprehension-bound name "is not supported" and to declare a `class_derivation`
instead.

Rust has no such limitation: `Ast::ListComp` evaluates the body against a child
scope with the loop variable inserted
(`crates/linkml-map-core/src/expr/eval.rs:365-374`), so the unbound-root guard at
`eval.rs:314-323` never fires for a loop variable and
`[c.code for c in codes]` simply evaluates. Rust keeps the identical
`"…no such source class, join, or slot. Typo or stale reference?"` message for a
genuinely unknown root (`eval.rs:319-322`), matching upstream's non-comprehension
branch.

**Consequence:** Rust **accepts a spec upstream rejects.** This is a divergence
in the permissive direction. Recording it rather than acting on it: the
alternative is to add an artificial restriction that deletes working, tested Rust
functionality in order to reproduce a Python implementation artifact. Flagging
for a deliberate decision — under a strict drop-in reading, a spec that
transforms in Rust and errors in Python is a real difference, and upstream's
message asserts the construct is unsupported *by design* (it bypasses range,
cardinality, and datatype coercion), not merely unimplemented.

**Suggested upstream issue (not filed, per doctrine):** upstream's stated reason
for refusing the construct is semantic (expression-built records skip coercion),
but the enforcement is incidental — it only triggers because `simpleeval` cannot
resolve the root. A `class_derivation`-shaped alternative is prescribed in the
error text without a supported path for the simple projection case.

---

## 5. `429379ce71` — `ReferenceValidator` caching

Upstream wraps the `ReferenceValidator` construction in
`@lru_cache(maxsize=1)` because building one materialises the derived
`transformer_model` schema, which is expensive. The upstream docstring states the
validator holds no per-call state, and the diff confirms it: the only change is
where the object comes from. No observable behaviour changes — no state can now
leak across calls that did not before.

Rust has no counterpart. There is no `ReferenceValidator`, no
`normalize_transform_spec`, and no runtime metamodel reflection: the spec is
deserialised straight into typed structs by serde
(`crates/linkml-map-core/src/datamodel/mod.rs`), and expression ASTs are compiled
once per spec (`CompiledExprs::build`). The expense the cache removes does not
exist here.

**N/A to Rust.** No action.

---

## 6. `9e073beba48f` — join fallback, and the two deferred questions

Upstream added three tests: a per-row `LookupIndex` fallback resolving a real
join when the primary table is YAML (DuckDB cannot scan it); an equality check
that the set-based engine and the per-row fallback produce identical output; and
an `on_error` test collecting per-row expression errors with `row_index` and
`class_derivation_name`.

Against the two questions the 2026-07-25 review left **unverified** for the
deferred DuckDB set-based join engine (`510b0a50ca`):

1. **To-many join deduped to one row per key** — still **not** exercised
   upstream. The new tests use unique lookup keys. Rust's `QUALIFY row_number()
   OVER (PARTITION BY …) = 1` dedup (added in `b147c95`) remains unconfirmed
   head-to-head.
2. **Join key column missing from the joined file** — still **not** exercised
   upstream. The new tests cover an unmatched *key value*
   (`site_code: "NO_SUCH_SITE"` → `None`), which is a different case from a
   missing *column*.

What the commit **does** settle: unmatched key values resolve to null on both the
engine and fallback paths, and Rust matches that (`lookup_row` yielding
`Value::Null`).

**Both original questions stay open**, and are now known to be unexercised
upstream as well — so a future port of the join engine cannot lean on upstream
tests to pin them down and will need its own head-to-head fixtures.

---

## Still deferred (unchanged)

- **`510b0a50ca` (DuckDB set-based join engine).** Large rewrite, not ported.
  The two semantic questions above remain unverified on both sides.
- **SCAN/MIGRATE normalization refactor** (`5a42c2af67` structural half,
  `9674b92d65` remaining half). Structural JSON-Schema validation and the
  deprecation scan stay out of scope per the 2026-07-07 decision.

## Open questions

- **Subcommand naming** (carried from 2026-07-25): **partly stale — corrected
  here.** `crates/linkml-map-cli/src/main.rs` already declares
  `#[command(visible_alias = "validate-spec")]`, so upstream's name *is*
  accepted: `linkml-tr-rs -q validate-spec …` runs (verified 2026-08-21). Only
  the primary/displayed name differs (`validate`). The drop-in gap for
  invocation is therefore already closed; what remains is a cosmetic decision
  about which name is canonical in `--help`.
- **Comprehension attribute access** (new, see §4): keep Rust's working support,
  or restrict to match upstream's refusal.

## Recommendations, in priority order

Items 1-3 were **implemented on 2026-08-21**, in the same session as this
review. Their status is recorded inline below.

1. **Fix the unit-conversion non-numeric divergence (§1).** ✅ **Done.**
   `perform_unit_conversion` in `crates/linkml-map-core/src/engine/mod.rs` now
   returns `Error::SlotTransform` instead of passing the value through,
   matching upstream's bare `raise`. The `none_if_non_numeric: true` branch is
   unchanged. Tests added: `unit_conversion_non_numeric_raises_unset`,
   `unit_conversion_non_numeric_raises_false`, with the existing
   `unit_conversion_non_numeric_none` kept as the regression guard.
2. **Port the two enum-`expr` gaps (§2).** ✅ **Done.** `transform_enum` now
   binds `src` when the source is not a `Value::Map` (and still spreads keys,
   with no `src`, when it is — mirroring upstream's two mapping branches), and
   propagates evaluation errors as `Error::ExprEval` instead of discarding
   them. A successful null result still falls through to the
   permissible-value derivations. Tests added:
   `test_enum_expr_scalar_src_binding`, `test_enum_expr_map_source_binds_keys`,
   `test_enum_expr_error_propagates`, `test_enum_expr_null_falls_through_to_pv`.
3. **Add the global `-q/--quiet` and `-v/--verbose` CLI flags (§3).** ✅
   **Done.** Added as clap `global` options on `Cli` in
   `crates/linkml-map-cli/src/main.rs` with upstream's exact ladder (none →
   `Warn`, `-q` → `Error`, `-v` → `Info`, `-vv` → `Debug`; quiet wins, matching
   upstream applying the quiet override after the verbose ladder). A minimal
   `log::Log` implementation writes to **stderr** so log output cannot
   contaminate the `-o -` stdout data path; no new third-party dependency
   (`log` was already a workspace dependency).
4. **Decide the two open questions above** and record the decisions. Still
   outstanding.
5. **Consider widening the `python-compat` CI matrix.** Upstream dropped its
   `requires-python` upper cap and now tests Python 3.14; our matrix is 3.11 and
   3.12. This is a recommendation, not a parity requirement — the wheel's own
   floor is declared separately in `crates/linkml-map-py/pyproject.toml`.
   Still outstanding.
6. **Consider adding coverage for the two retained NMDC scenarios**
   (`e3236180a7eb`): two-level inlined dotpath null propagation, and multiple
   class derivations targeting one class. Rust implements both; neither is
   asserted in our 55 cases. Still outstanding.

## Verification of items 1-3

- `cargo test --workspace --exclude linkml-map-py` — all green, 0 failures
  (`linkml-map-core` 245 passed).
- `tests/COMPLIANCE_REPORT.md` regenerated **byte-identically** by the
  `conformance_suite` test, so the 55-case compliance count and the 14 golden
  fixtures are unaffected.
- `cargo xtask harness-gate` — exit 0. Both changed functions are private, so
  the three-surface rule is not engaged; no PyO3 or `.pyi` change was needed.
- CLI smoke-tested against a real fixture: `-q` and `-vv` before a subcommand
  execute the subcommand rather than printing group help.

Do **not** re-pin `CLAUDE.md` → "Upstream reference": upstream has cut no tag
past `v0.5.3`, so there is no new pin to move to. Items 1-3 are now ported
against unreleased `main`; the remaining deferrals are unchanged.

## Next review

Start the next upstream review from `main` at `eb28a58cfb` (2026-08-21), or from
the next release tag once upstream cuts one past `v0.5.3`.
