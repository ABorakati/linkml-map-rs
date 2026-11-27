# Upstream sync review — 2026-11-26

Point-in-time snapshot. This is not living documentation — it records what was
found and decided on this date and will not be kept up to date after this pin
bump. For current pin state, see `CLAUDE.md` → "Upstream reference".

## Summary

This review takes the pin from **`v0.6.0-rc1`** (`1323cc8`) to the **`v0.6.0`**
tag (**`0617b59`**, 2026-11-03, also upstream `main` at the time). It starts where
[`upstream-sync-2026-10-27.md`](upstream-sync-2026-10-27.md) stopped and does not
revisit that review's findings. The range `1323cc8..0617b59` holds **7 commits**.
Tags in the range: `v0.6.0-rc2` and `v0.6.0` both point at `0617b59`.

Outcome of the 7 commits:

- **Ported (4):** `09667a6`, `5123a84`, `0617b59`, `e201702`.
- **N/A (3):** `60db354` (an agent skill document), `fbd9f29` (ruff bump) and
  `e3b81ae` (`setup-uv` bump). None touches behaviour or a `transformer_model.yaml`
  field.

No public Rust API signature changed, so the three-surface rule needed no PyO3, stub
or shim work. The rust-linkml-core dependency is unchanged (`v0.5.0`, `7bd81d5`).

The differential corpus, the examples sweep, the `yaml_e2e` and `input_e2e`
expectations were **re-recorded against `linkml-map` 0.6.0 on Linux** (WSL, upstream's
own `uv.lock` versions: DuckDB 1.5.6, pint 0.25.3, python-slugify 9.1.3). Every
expectation that moved is explained by one of the four ported commits (table below);
nothing is unexplained.

## Commit-by-commit

| Commit | Date | Summary | Status |
|---|---|---|---|
| `09667a6` | 2026-11-02 | Omit nested objects whose slots all come out None | **Ported.** `derive_nested_objects` skips a nested `Value::Map` whose entries are all null (the minted id is ignored), and an empty map; see detail. |
| `e3b81ae` | 2026-11-03 | Bump astral-sh/setup-uv from 10.0.1 to 10.3.3 in the actions group | **N/A.** CI. |
| `fbd9f29` | 2026-11-03 | Bump ruff from 0.17.1 to 0.17.2 in the minor-and-patch group (#392) | **N/A.** Lockfile only. |
| `5123a84` | 2026-11-03 | Resolve an enum source value against the target enum when no enum derivation exists | **Ported.** An enum source value with no enum derivation passes through unchanged. |
| `60db354` | 2026-11-03 | Add an agent skill for writing linkml-map specs | **N/A.** Documentation for agents. |
| `e201702` | 2026-11-03 | Separate YAML output documents instead of ending each with `---` (closes #400) | **Ported** (`linkml-map-io`). `---` goes between documents, none trailing. |
| `0617b59` | 2026-11-03 | `_perform_join_resolution` tests `class_slots` membership (#403, closes #402) | **Ported.** A joined `populated_from` value is now coerced to the target. Tags `v0.6.0-rc2` and `v0.6.0`. |

## Per-commit detail

### `09667a6` — omit nested objects whose slots all come out None

Upstream `_derive_nested_objects` now drops a nested object when
`all(v is None for v in obj.values())`. The port had kept hollow `{slot: null}`
objects, following the earlier a19eb095 / #217 note.

- `linkml-map-core` `ObjectTransformer::derive_nested_objects`: after each nested
  `map_object_internal`, a `Value::Map` is skipped when every entry is null. The key
  returned by `underived_identifier(cls_deriv)` (a minted id) is ignored, as
  upstream's `_derive_nested_objects` does. An **empty** map is skipped too, as
  Python's `all()` over an empty dict is `True` (`// PARITY:` note citing 09667a6).
  Hidden slots are already gone at this point. Cardinality is unchanged: a
  single-valued slot gets `None`, a multivalued one `[]`.
  `content_id::underived_identifier` became `pub(super)`.
- Tests: new `crates/linkml-map-schemaview/tests/empty_nested_objects.rs` (7 tests,
  ported from `test_empty_nested_objects.py`, plus probes against 0.6.0 for an
  object with no slots or only hidden ones, and for `[]`, `''` and `False` counting as
  values); `engine::tests::same_table_hollow_nested_object_is_omitted`,
  `merged_row_tests::nested_within_nested_keeps_ancestor_tables_and_ambiguity` and
  `absent_slot_bindings::nested_class_derivations_use_their_own_source_class` updated.
- **Suggested upstream issue** (see below): `all()` over an empty dict silently
  drops a nested derivation that has no visible slots.

### `5123a84` — enum source value without an enum derivation passes through

Upstream `map_object`'s enum branch no longer raises `Could not find what to derive
from a source <Enum>` when the spec has no enum derivation for it: the value is
returned unchanged. The port always called `transform_enum`.

- `linkml-map-core` `engine/mod.rs`: new `enum_derivations_from` (mirrors
  `Transformer._enum_derivations_from`), `get_enum_derivation` (mirrors
  `_get_enum_derivation`; still raises unless exactly one derivation matches) and
  `map_enum_value` (the `map_object` enum branch; `// PARITY:` note). Callers switched:
  `map_object_with_type_and_derivation` and the `RangeKind::Enum` arm of
  `map_value_by_range`. The `any_of`-enum path still calls `transform_enum` directly,
  as upstream.
- Tests in `crates/linkml-map-schemaview/tests/enum_resolution.rs` (15 tests): M becomes
  `OMOP:8507` without a derivation; a derivation from another enum does not count;
  one from the source enum still applies; several raise.
  `upstream_errors::enum_ranged_slot_needs_exactly_one_enum_derivation` updated.
- Not changed (predates the commit): upstream maps a **dict** held in a multivalued
  enum source slot per value (`{k: map_object(v)}`); the port has no dict arm there
  and wraps the dict as a one-element list. Not an upstream bug; worth a port issue
  if fuzzing ever finds it.

### `e201702` — YAML stream separators (#400)

Upstream `YAMLStreamWriter.write_chunk` (per-object branch) emits `---\n` *before*
every document except the writer's first (`_first_chunk`), so there is no trailing
`---` and no empty last document. Confirmed against 0.6.0: `map-data` of
`tests/examples/tabular` ends `label: bob smith\n`, also with `--chunk-size 1`.

- `linkml-map-io` `writers.rs`: `YamlStreamWriter` gained `first_chunk: bool`
  (`new()`, `Default`); `write_chunk` pushes the separator before a document when it is
  not the first (`// PARITY:` citing e201702 and #400). The state is per writer
  instance, so chunk and block boundaries cannot add or move a separator
  (`MultiStreamWriter` keeps one writer per target for every chunk). Single-object
  output (`dump_output`) never emitted `---`; the JSONL bytes fast path is unaffected.
- The `key_name` branch is unchanged upstream and not ported (the CLI never sets it).
  The loader still drops trailing null documents, so YAML written before e201702 still
  reads back.
- Tests: new `writers::tests::yaml_stream_writer_separates_documents_without_an_empty_one`;
  existing expectations that encoded the trailing `---` updated (`writers.rs`,
  `io/src/lib.rs`, `decimal_output.rs`, `integration_map_data.rs`).

### `0617b59` — `class_slots` membership in `_perform_join_resolution` (#403, closes #402)

Upstream replaced `field_path in class_induced_slots(joined_class)` (a string
compared with `SlotDefinition` objects, always false) with `class_slots`, so a value
read through a join is now range-, cardinality-, datatype- and enum-coerced like any
other `populated_from`. This closes the oldest entry of
[`suggested-upstream-issues.md`](suggested-upstream-issues.md) (marked resolved there).

- `linkml-map-core` `engine/populated_from.rs::resolve_populated_from`: the join-path
  branch used to return `(v, None)` under a `PARITY:` note; it now returns
  `induced_slot_cow(source_schema, field_path, joined_class)`, where `joined_class` is
  a non-empty `class_named`, otherwise the join name.
  `SchemaViewProvider::induced_slot` only resolves slots of the class itself
  (inherited ones included), so this equals
  `joined_class in all_classes() and field_path in class_slots(joined_class)`. The
  `PARITY:` note now cites 0617b59.
- Tests: new `crates/linkml-map-schemaview/tests/cross_table_lookup.rs`
  (`populated_from_cross_table_is_typed_to_target`);
  `dotted_populated_from.rs::join_path_is_not_coerced` replaced by
  `join_path_is_coerced_to_the_target` and
  `join_path_to_a_column_the_joined_class_lacks_is_not_coerced`.

## Differential evidence at 0.6.0

All of it re-recorded on Linux against upstream 0.6.0 (see "Environment" for why not
Windows). New tool: `tests/differential/rerecord_corpus.py` (re-runs upstream over the
corpus and rewrites `expected/`; `--check` is the cheap form the weekly drift workflow
runs).

### Corpus (`tests/differential/corpus`, 632 cases, 2,975 case-format cells)

**254 expectations in 199 cases changed**, every one explained:

| Commit | Cases | Cells | Cases and what changed |
|---|---|---|---|
| `e201702` | 185 | 185 | yaml cells only: the trailing `---` line is gone |
| `09667a6` | 8 | 39 | `4871a0a6f918`, `b70da1eb0c01` (`addrs: [{}]` becomes `[]`); `4f31490d1fe3`, `b89c6773f7a7`, `cd611361b18d` (`inner: {}` dropped); `612ca7500011` (`left`/`right: {}` dropped); `e903812e2e10` (`location: {}` dropped, json/jsonl/tsv/csv; the yaml cell had no `{}`); `fec76f6c8a54` (`org: {}` dropped) |
| `5123a84` | 2 | 10 | `7d8cef5c3417`, `8ee12775ef35`: `Could not find what to derive from a source Status/Kind` error becomes output (`notes.md` re-graded `upstream_error` to `match`) |
| `0617b59` | 4 | 20 | `12722e8ffb8a`, `cd7f86e25307`, `f81e7c481497` (`out: b` becomes `y` through the enum derivation); `844bfedd42f1` (`outf: 5` becomes `5.0`, float target) |

In 11 of the engine cases (7 of `09667a6`, all 4 of `0617b59`) the yaml cell also lost
its trailing `---`; those cells are counted under the engine commit above.

Handled outside the 254:

- **Not re-recorded, by design.** `9bcfde5d19ed` and `fa1c0134ade9` (yaml): upstream
  dumps its own in-memory objects (`!!python/object`, absolute `source_file` paths, uuids,
  hashes), different on every run; both are marked divergences (`proxy-semantics`).
  `7ad591acce02`: only Python 3.14's wording of the error message differs
  (`too many values to unpack (expected 1, got 2)`); the stored 3.12/3.13 text is
  kept. The script skips the first two and compares messages with paths normalised.
- **Markers.** `refresh_corpus.py` reports *fixed 0, still divergent 9, regressions 0*:
  no `expected_divergence.md` can be deleted at 0.6.0. (Run without `--dry-run` it
  rewrites the hand-written text of eight markers, so the re-pin used `--dry-run` and
  the README now says so.)
- The Windows-only disagreements the owners saw (12 cases: DuckDB timestamp `+00`,
  1900/1970 dates, row order, `charmap` decoding, `Order` keyword, `Org.tsv` case
  collisions) **all vanish on Linux with upstream's locked versions**; none needed a
  re-record.

`cargo test -p linkml-map-cli --test differential_corpus` (WSL): 1 passed, 1 ignored.

### Examples sweep (`tests/differential/EXAMPLES_REPORT.md`)

`known_sweep_divergences.txt` regenerates **byte-identical** (9 cells, all upstream's
`TypeError: Object of type date is not JSON serializable` on `personinfo_basic`);
the gate reports 0 cells outside the allow-list. The captured set grew from 224 to 230
invocations (new 0.6.0 tests) and now reads `both_error: 155, match: 995` (was
`both_error: 155, match: 935, rust_error: 30`). Running the same 0.6.0 sweep with the
binary built from the commit before the ports differs from the post-port run in exactly
these cells (the pre-port captured set has 13 `output_diff` and 10 `rust_error` cells):

| Commit | Cells |
|---|---|
| `e201702` | `tabular/person_to_agent` yaml: 6 `map-data` cells (CSV, TSV, directory x restricted/unrestricted) and 3 `-o FILE` cells |
| `09667a6` | captured `test_empty_nested_objects.py::test_single_object_path` (x2) and `test_minted_id_does_not_keep_an_empty_object` |
| `5123a84` | captured `test_target_enum_resolution.py::test_unit[An...]` (enum without an enum derivation) |
| not isolated | captured `test_slot_derivation_features.py::test_integration`: error with the pre-port binary, matches now; only the ported set differs |

The six `rust_error` rows of the committed rc1 report (`test_range_override`,
`test_expr_strict_mode`, `test_flatten_inlined`, `test_slot_derivation_features`) were
already `match` with the pre-port binary at 0.6.0: stale report rows, not an effect
of these commits.

### `tests/yaml_e2e` and `tests/input_e2e`

`REGEN=1 tests/input_e2e/run.sh` (92 compared, 4 skipped, as before) and the two
`yaml_e2e` commands, run against upstream 0.6.0 on Linux, produce files **identical to
the 14 the runtime-engineer edited by hand on Windows**: exactly one trailing `---` line
removed from `data.tsv.yaml` and from the 13 stream-YAML files of `input_e2e`.
`tests/derive_schema` and `tests/invert` expectations were regenerated against 0.6.0
too (see "Verification").

## Pre-existing items noticed (not caused by upstream drift)

- The committed `EXAMPLES_REPORT.md` carried six stale `rust_error` rows (above).
- `refresh_corpus.py` without `--dry-run` overwrites hand-written
  `expected_divergence.md` text (it rebuilds it from `triage.py`). Fixed in the
  README procedure, not in the tool.
- `.github/workflows/ci.yml` still checks upstream out at `ref: v0.6.0-rc1` in the
  `differential` and `expr-differential` jobs; the stored expectations are now 0.6.0, so
  both refs must move to `v0.6.0` (left untouched: another owner's jobs).
  The expression-level fixtures (`tests/differential/expr`, `crates/*/tests/**` doc
  comments) still say "v0.6.0-rc1"; their values were not re-recorded and none of the
  four commits touches the evaluator, but they are not re-verified at 0.6.0 by this review.

## Environment: why the re-recording is on Linux

On Windows upstream itself disagrees with 12 stored corpus expectations: the
case-insensitive filesystem makes the `org` join open `Org.tsv` (the corpus has
`Org.tsv`/`org.tsv` pairs), non-ASCII text is mangled by the console codepage and
`core.autocrlf=true` writes CRLF fixtures. Use Linux for any re-pin, with upstream's
own `uv.lock` versions: DuckDB 1.5.6 versus 2.0.1 changes timestamp text
(`2020-01-01 08:00:00` versus `...+00`) and a 1900-versus-1970 date; pint, slugify and
dateutil also differ between a fresh `pip install` and the lock.

## Suggested upstream issues (not filed)

- **`_derive_nested_objects` (09667a6)**: `all(v is None for v in obj.values())` is true
  for an empty dict, so a nested class derivation with no slot derivations, or with only
  `hide: true` ones, is silently dropped instead of emitted as `{}` (and nothing says
  so). Reproduced; kept with a `PARITY:` note.
- **`class_induced_slots` entry resolved.** The `_perform_join_resolution` bug in
  `suggested-upstream-issues.md` (a string tested against `SlotDefinition` objects) is
  fixed by #403 / #402 in 0.6.0 and is marked resolved there.

## Weekly drift check

New workflow `.github/workflows/upstream-drift.yml` (Mondays 04:43 UTC and
`workflow_dispatch`). Matrix `pinned` / `latest` / `main`; the pinned tag is read from
`tests/differential/UPSTREAM_PIN`. Each leg builds the release CLI, runs the examples
sweep with its allow-list gate and `rerecord_corpus.py --check` (upstream re-run over
the whole corpus against the stored expectations), and writes a job summary that lists
the divergent cells and the corpus cases that moved. `pinned` must pass; a failing
`latest` or `main` leg is drift. Bump `UPSTREAM_PIN` with the pin.

## Verification

On Linux (WSL, rustc 1.100, Python 3.14 venv with upstream's locked versions):

- `cargo test --workspace --exclude linkml-map-py --no-fail-fast`: **1,108 passed,
  0 failed, 6 ignored** across 57 test binaries. (On Windows the same tree shows
  failures that come only from the checkout: CRLF fixtures, backslash paths and the
  case-insensitive filesystem.)
- Compliance **55/55** and golden **14/14**: the generated `tests/CONFORMANCE_REPORT.md`
  and `tests/COMPLIANCE_REPORT.md` are unchanged.
- `tests/derive_schema` (88 cases) and `tests/invert` (560 cases) regenerated against
  0.6.0 with no change to any `expected/` file.
- The PyO3 pytest suite was not run (no `linkml-map-py` build here); no binding-consumed
  signature changed.

## Next review

Start from **`0617b59`** (`v0.6.0`). The first thing to read is the output of the
`latest` and `main` legs of the weekly drift workflow.
