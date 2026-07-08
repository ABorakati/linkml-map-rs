# Upstream sync review — 2026-07-07

Point-in-time snapshot. This is not living documentation — it records what was
found and decided on this date and will not be kept up to date after this pin
bump. For current pin state, see `CLAUDE.md` → "Upstream reference".

## Summary

Upstream `linkml/linkml-map` released tag **`v0.5.3-rc2`** at commit
`510b0a50cae9a76d054f80b6dc814292d4c05a80` (2026-07-07) — **10 commits** ahead
of this repo's previously-documented pin, `19b1985889ec0fc247145e9aa03eb74a4d7b3588`
(2026-06-12).

**Surprise finding:** 3 of those 10 commits were *already ported* to Rust before
this review, but none of the pin-tracking docs (`CLAUDE.md`,
`tests/compliance/PROVENANCE.md`, `reference/upstream-python/README.md`) were
updated to say so. The port had quietly moved ahead of its own bookkeeping.

This review moves the documented pin to **`66cbcc31b5169f8f95a70f164161a4cee1c6a2bb`**
(2026-07-06, the 9th of the 10 commits) — not the full RC tip `510b0a50ca` — because
the tip commit (a DuckDB join-engine rewrite) is explicitly deferred, not ported.
Claiming the pin covers it would overstate what's actually mirrored.

## Commit-by-commit

| Commit | Date | Summary | Status |
|---|---|---|---|
| `5a42c2af67` | 2026-06-15 | Refactor deprecation scanning to pre-normalize (new SCAN/MIGRATE two-phase spec-normalization split in `transformer.py`); allow list-form `populated_from` on `PermissibleValueDerivation` | **Partially ported (2026-07-08).** This commit bundles three distinct pieces upstream; each has its own status now: (1) list-form `populated_from` on `PermissibleValueDerivation` — **already ported**, predates this review (see README "Feature coverage": "list-form `populated_from` mapping several source PVs to one target"). (2) `validator.py`'s *semantic* cross-reference validation (does a referenced class/slot/enum/join-key/`is_a`/`mixins` actually resolve against the source/target schema) — **ported**, `crates/linkml-map-core/src/validate/` (`mod.rs` + `expr_refs.rs` + `tests.rs`, 29 tests), citing this commit. (3) `validator.py`'s *structural* JSON-Schema validation (`_build_json_schema`/`_validate_structural`, depends on `linkml.generators.JsonSchemaGenerator`) and the deprecation-scan (`check_deprecated_fields`, scans for `sources`/`derived_from`/`object_derivations` usage) — **deliberately out of scope, not deferred**: structural JSON-Schema validation is redundant here (serde already rejects malformed specs at deserialize time), and the deprecation scan exists only to warn about fields (`sources`, `derived_from`, `object_derivations`) that this port already removed outright rather than keeping as deprecated-but-supported (see README "v0.6.0 parity") — porting the scan would mean resurrecting dead fields solely to warn about them, which reverses that shipped decision rather than mirroring it. The new `validate` module is an opt-in library function (`validate_spec_semantics`), not wired into the CLI/PyO3 yet and not called from the transform path — `map_object`/`map_container` output is unchanged. |
| `a19eb09505` | 2026-06-15 | Suppress nested object on sparse join miss (don't emit a hollow nested object when a join finds no matching row) | **Already ported** — Rust commit `b556f54` ("port upstream inline dot-path traversal (#247) and sparse-join-miss key retention (#217)") |
| `b5fca1962f` | 2026-06-15 | Traverse inlined nested data in `populated_from` dot-paths | **Already ported** — same Rust commit `b556f54`, citing upstream issue #247 |
| `d9be8e9304` | 2026-06-17 | Add `missing_values` field to `SlotDerivation` for sentinel-to-null mapping (e.g. `-9`, `999`, `"NA"` → null) | **Already ported** — Rust commit `114012a` ("feat: missing_values, sparse-join omission, copy-directive tests"); see `SlotDerivation.missing_values` in `crates/linkml-map-core/src/datamodel/mod.rs` (cites "v0.6.0 (#269)") and `apply_missing_values` in `crates/linkml-map-core/src/engine/mod.rs` |
| `9674b92d65` | 2026-06-25 | Restore back-compat: coerce a bare-string `source_schema`/`target_schema` to the structured `SchemaReference` object form | **Deferred** — minor; bundle with future SCAN/MIGRATE work, same normalization-layer territory |
| `fc0080f287` | 2026-06-30 | Configure DuckDB to respect container cgroup memory/CPU limits | **N/A** — Python-process infra config only; no Rust analog needed (`crates/linkml-map-duckdb`'s DuckDB usage doesn't share this Python-specific resource-detection problem) |
| `4f44ec35ec` | 2026-07-01 | Synthesize implicit joins from expression references (not just `populated_from`) | **Being ported now** (parallel agent, `crates/linkml-map-core`) |
| `8ef782da52` | 2026-07-02 | Cache parsed AST and reuse the evaluator across expression evaluations (perf) | **Deferred** — pure Python perf optimization (simpleeval-based evaluator reuse); Rust's hand-written evaluator (`crates/linkml-map-core/src/expr/`) has a different cost profile and architecture, so this doesn't directly translate — would need its own investigation into whether Rust's evaluator has an analogous per-row re-parse cost at all |
| `66cbcc31b5` | 2026-07-06 | Fix dotted `populated_from` crash (range-induction pass previously required `cd.joins` to already be populated) and synthesize its join for flat top-level slot derivations | **Being ported now** (parallel agent) |
| `510b0a50ca` | 2026-07-07 | Set-based DuckDB join engine, replacing the per-row point-lookup join path with a single star `LEFT JOIN` query (new `join_engine.py`, ~295 lines); default with per-row fallback | **Deferred** — large rewrite; a Python-side *performance* rewrite (the per-row lookup path stays as fallback), but with semantic details worth a future dedicated review: (1) a to-many join is deduped to one row per key via `QUALIFY row_number() OVER (PARTITION BY key) = 1`, matching the old `LIMIT 1` per-row semantics — confirm Rust's `LookupIndex`/`linkml-map-duckdb` path does the same; (2) a join whose key column is missing from the joined file (e.g. headerless/0-byte file) degrades to an all-miss null STRUCT rather than raising — confirm Rust degrades the same way instead of erroring |

## Already ported, now documented

Three commits (`a19eb09505`, `b5fca1962f`, `d9be8e9304`) landed in Rust via
commits `b556f54` and `114012a` before this review, but the pin-tracking docs
still cited the older `19b1985889` commit. This review's doc fixes bring the
documented pin in line with what's actually mirrored — no source changes were
needed for these three, only bookkeeping.

## CI risk note (heads-up, not fixed here)

`.github/workflows/ci.yml`'s `test` job installs **unpinned** `linkml`/`linkml-map`
from PyPI (`uv pip install pytest linkml-runtime pyyaml deepdiff linkml linkml-map --system`)
to run the vendored `tests/compliance/test_compliance_suite.py`, which imports
upstream's own `linkml_map.compiler`/`.datamodel`/`.inference` directly. That job
doesn't test the Rust port itself, but it does mean it's exercised against
whatever's latest on PyPI at CI-run time — once v0.5.3 stabilizes and publishes,
this job could start failing for reasons unrelated to this repo (upstream API
drift vs. the vendored test file's expectations). Flagging for whoever
maintains CI; not touched as part of this review.

## Next review

Start the next upstream review from commit **`66cbcc31b5169f8f95a70f164161a4cee1c6a2bb`**
(the new pin) — the largest remaining deferred item is `510b0a50ca` (the DuckDB
set-based join engine; a 2026-07-08 re-check found its dedup/degrade semantics
already match Rust's pre-existing `LookupIndex` behavior, so what's left there
is narrower than originally scoped — mainly confirming `linkml-map-duckdb`'s
own `SqlCompiler` join SQL, not the core engine). The SCAN/MIGRATE
normalization refactor (`5a42c2af67`/`9674b92d65`) is now only partially open:
the semantic-validation half of `5a42c2af67` is ported (see above); still
deferred are `9674b92d65` (bare-string `source_schema`/`target_schema`
back-compat coercion) and wiring the new `validate` module into a CLI/PyO3
entry point.
