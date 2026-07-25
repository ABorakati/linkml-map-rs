# Upstream sync review — 2026-07-25

Point-in-time snapshot. This is not living documentation — it records what was
found and decided on this date and will not be kept up to date after this pin
bump. For current pin state, see `CLAUDE.md` → "Upstream reference".

## Summary

Upstream `linkml/linkml-map` released tag **`v0.5.3`** — **7 commits** ahead
of this repo's prior documented pin, `66cbcc31b5169f8f95a70f164161a4cee1c6a2bb`
(2026-07-06).

**Surprise finding:** two items the 2026-07-07 review left open were already
done in the source tree and only needed bookkeeping — the semantic-validation
CLI (commit `4616df5`, "feat: semantic spec validation + implicit-join
synthesis", exposed as `validate`, not upstream's `validate-spec` — see the open
question below) and the bare-string `source_schema`/`target_schema` coercion
(`deserialize_schema_reference_option` in `datamodel/mod.rs`, citing #215).

This review moves the documented pin to the **`v0.5.3`** tag (commit
`ea0d4ee8c`).

## Commit-by-commit

| Commit | Date | Summary | Status |
|---|---|---|---|
| `510b0a50ca` | 2026-07-07 | Set-based DuckDB join engine (default, with per-row fallback), replacing the per-row point-lookup join path with a single star `LEFT JOIN` query (new `join_engine.py`, ~295 lines). Also adds the shared `join_keys()` helper in `utils/join_utils.py`. | **Deferred.** Large rewrite. The `join_keys()` half is **N/A (DRY refactor)** — Rust already resolves `source_key or join_on` / `lookup_key or join_on` independently in the engine, the DuckDB compiler, the pipeline, and the validator, so there is no shared helper to extract. The join-engine half needs a dedicated review; see "Still deferred" for the two semantic questions that remain **unverified**. |
| `040fb81494` | 2026-07-08 | Publish to PyPI on release published, not created | **N/A.** CI infra only. |
| `46850de7e3` | 2026-07-09 | Reuse source SchemaView for TSV/CSV coercion instead of rebuilding per file | **N/A to Rust.** The Rust loaders (`linkml-map-io`) construct a single `SchemaViewProvider` at pipeline start and reuse it for both schema resolution and type coercion — no per-file rebuild cost exists. |
| `0d57bec4b5` | 2026-07-09 | Report `--continue-on-error` errors as they occur so a mid-stream crash can't drop them | **Already matching.** `crates/linkml-map-pipeline/src/lib.rs` L786 and L811 already `eprintln` per failed row. |
| `8966fe7eda` | 2026-07-15 | Evaluate arithmetic numerically when an operand is a string (`_coerce_like`, `_maybe_coerce_arithmetic`, `_null_propagating_arithmetic`) | **Ported in this sync.** `crates/linkml-map-core/src/expr/eval.rs`: added `coerce_like`, `maybe_coerce_arithmetic`, `warn_non_numeric`, rewrote `eval_binary` to mirror `_null_propagating_arithmetic`. The old `str * int` repetition path in `try_native_binary` was removed — that was the bug (#285). |
| `328dc30f52` | 2026-07-16 | Warn when a non-numeric-typed slot is used in arithmetic (`_warn_non_numeric`) | **Ported alongside the above.** `warn_non_numeric` added; `log::warn!` replaces Python's `logger.warning`. |
| `ea0d4ee8ec` | 2026-07-16 | Document that the `personinfo_basic` example needs `--unrestricted-eval` | **N/A.** Docs-only, no behavioural change. |

## Items carried forward from 2026-07-07, now resolved

- **`9674b92d65` (bare-string `source_schema`/`target_schema` back-compat coercion):**
  The prior review marked this "Deferred." It is **already ported** — see
  `deserialize_schema_reference_option` in `crates/linkml-map-core/src/datamodel/mod.rs`
  (L68-73, citing v0.6.0 #215). Corrected in the 2026-07-07 doc above.

- **Semantic-validation CLI subcommand:** The prior review deferred wiring the
  semantic validator into a CLI/PyO3 entry point. Both are already done: the
  CLI `validate` subcommand (`crates/linkml-map-cli/src/main.rs`) wraps
  `validate_spec_semantics` via `run_validate_config`, and the PyO3 surface is
  exercised by `crates/linkml-map-py/tests/test_validate_spec.py`.

- **`--continue-on-error` per-row reporting:** Already matches upstream (see
  commit table above).

- **`join_keys()` DRY refactor:** N/A — Rust resolves keys independently at
  each call site (see commit table above).

## Open question — subcommand naming

Upstream v0.5.3 names its CLI command **`validate-spec`**; this port names the
equivalent command **`validate`**. The underlying semantic checks match, but the
names do not. For a project whose aim is drop-in replacement, decide
deliberately whether to rename, alias, or document the divergence — this review
only records it.

## Still deferred (unchanged)

- **`510b0a50ca` (DuckDB set-based join engine).** Large rewrite, not ported.
  Two semantic details from the upstream implementation remain **unverified**
  against the Rust path and should gate any future port: (1) a to-many join is
  deduped to one row per key upstream via `QUALIFY row_number() OVER (PARTITION
  BY key) = 1` — Rust commit `b147c95` added a `QUALIFY row_number` dedup, but
  no head-to-head check has been run; (2) a join whose key column is missing
  from the joined file degrades upstream to an all-miss null STRUCT rather than
  raising — whether the Rust path degrades identically has **not** been
  confirmed.

- **SCAN/MIGRATE normalization refactor** (`5a42c2af67` structural half,
  `9674b92d65` remaining half). The structural JSON-Schema validation and
  deprecation scan are deliberately out of scope per the 2026-07-07 decision.

## Next review

Start the next upstream review from the **`v0.5.3`** tag (commit `ea0d4ee8c`).
