# Upstream sync review — 2026-10-27

Point-in-time snapshot. This is not living documentation — it records what was
found and decided on this date and will not be kept up to date after this pin
bump. For current pin state, see `CLAUDE.md` → "Upstream reference".

## Summary

This review takes the pin from **`v0.5.3`** (`ea0d4ee8ec`) to the **`v0.6.0-rc1`**
tag (**`1323cc8`**, 2026-10-23). It starts where
[`upstream-sync-2026-08-21.md`](upstream-sync-2026-08-21.md) stopped
(`eb28a58cfb`) and does not revisit that review's findings. The range
`eb28a58cfb..1323cc8` holds **35 commits**. Tags in the range: `v0.5.4` =
`f18e3f5`, `v0.6.0-rc1` = `1323cc8`.

Outcome of the 35 commits:

- **Ported (16):** `04dc4c8`, `72c0c28`, `76db207`, `0469da7`, `11b425b`,
  `51b03eb`, `209f794`, `7665ee3`, `b332fc7`, `cbf2f97`, `e373d93`, `173121b`,
  `b53a81f`, `f299f3a`, `cd395ff`, `1323cc8`.
- **Already matching (2):** `d2297e5` (regression tests added), `e82db08`.
- **N/A (17):** pure moves, docs, CI, lockfile bumps, Python-only mechanisms and
  features the port does not have (`Session`, `MultiFileTransformer`).

The rust-linkml-core dependency also moved, `df9d18ec` to `v0.5.0` (`7bd81d5`).

The port work surfaced several **pre-existing bugs in this repository**
(unrelated to upstream drift); they are fixed and listed under "Pre-existing bugs
found and fixed". Rust sources carry `// PARITY:` notes for each deliberate
divergence.

Rust baseline for this review: `6d08010` (v0.1.21). Triage reports behind this
document were written per slice against that baseline.

## Commit-by-commit

Our shas are those on branch `claude/sleepy-ramanujan-nj2b1i` (merge history included).

| Commit | Date | Summary | Status |
|---|---|---|---|
| `64bcbd1` | 2026-08-25 | Generate the compliance doc reproducibly, and drift-check it | **N/A.** Docs, CI and test tooling; no new combos (57 upstream minus 2 skipped = our 55). |
| `8f12e19` | 2026-08-25 | Consolidate the join tests | **N/A.** Test-only; the Rust suite already holds every relocated case. |
| `246e0d7` | 2026-08-25 | Extract the spec normalizer out of the Transformer base class | **N/A.** Pure move; Rust has no dict-normalisation phase. |
| `04dc4c8` | 2026-08-25 | Add Parquet and DuckDB output formats | **Ported** (`f88ab03`), behind the `columnar` cargo feature. |
| `a62221f` | 2026-08-25 | Extract the pivot and unit-conversion blocks out of object_transformer | **N/A.** Pure move, verified by diff. See "Still deferred" for three divergences it exposed. |
| `f18e3f5` | 2026-08-25 | Document the columnar output formats; fix the mkdocs nav (tag `v0.5.4`) | **N/A.** Docs only. |
| `c265cea` | 2026-10-08 | Bump astral-sh/setup-uv from 9.0.0 to 10.0.1 | **N/A.** CI. |
| `0aebe86` | 2026-10-08 | Bump the minor-and-patch group across 1 directory with 6 updates | **N/A.** Lockfile only. |
| `d2297e5` | 2026-10-09 | Don't synthesize populated_from for constant value: slot derivations | **Already matching.** Rust has no induction step; regression tests added (`56f6a83`). |
| `e82db08` | 2026-10-09 | Drop Session from linkml_map.__all__ (#351) | **Already matching.** Shim `__all__` is `["ObjectTransformer"]`, no `Session`. |
| `4a81f6c` | 2026-10-09 | Fix multi_file_transformer overwriting root_directory with output_directory (#352) | **N/A.** `MultiFileTransformer` is not ported. |
| `72c0c28` | 2026-10-09 | Coerce range: decimal to exact Decimals and keep them exact in every output format | **Ported** (`4d150a0`; PyO3 `decimal.Decimal` in `88e3b10`). |
| `76db207` | 2026-10-13 | Synthesize content-hash ids for identifier slots the spec doesn't derive | **Ported** (`26a8e40`, `53adec6`); pinned ids reproduced. |
| `5e54918` | 2026-10-21 | Bump linkml and linkml-runtime to 1.12.1 | **N/A.** Lockfile bump; no `transformer_model.yaml` field change. |
| `27ca24d` | 2026-10-21 | Bump python-slugify from 8.0.4 to 9.1.3 | **N/A.** Lockfile bump, no default output change; prompted the slugify fix (`9b49e83`). |
| `a76e4a8` | 2026-10-21 | Bump the minor-and-patch group across 1 directory with 2 updates | **N/A.** Lockfile only. |
| `0469da7` | 2026-10-21 | Coerce slot values to the target schema's range when the spec declares none | **Ported** (`87f2832`). |
| `e70715e` | 2026-10-21 | Store the minted id key as a plain str so YAML output loads | **N/A.** Python `str`-subclass leak; Rust has no equivalent. |
| `11b425b` | 2026-10-21 | Coerce expr: and value: results to the target slot like populated_from | **Ported** (`bcc57b7`). |
| `51b03eb` | 2026-10-21 | Drop null slots in YAML stream output like the JSON writers do | **Ported** (`686b190`, writers rewrite). |
| `209f794` | 2026-10-21 | Resolve enum-ranged slot values to the target enum's permissible value keys | **Ported** (`e2c9089`). |
| `7665ee3` | 2026-10-21 | Check value and value_mappings literals against the target enum in validate-spec | **Ported** (`5fe391e`). |
| `b332fc7` | 2026-10-22 | Warn in validate-spec when a spec range overrides a conflicting target range | **Ported** (`e8ddad9`). |
| `173121b` | 2026-10-23 | Construct transformers from paths, and export load_specification | **Ported** (`193c005`; PyO3, shim and `.pyi` surfaces). |
| `cbf2f97` | 2026-10-23 | Validate dotted references through joined tables instead of reporting them missing | **Ported** (`95bf3c9`). |
| `b53a81f` | 2026-10-23 | Map a single input file as --source-type, and fail when no class derivation has data | **Ported** (`af83464` primary `--source-type` flag, `973cf9c` no-data error text). |
| `249d007` | 2026-10-23 | Restructure the docs, rewrite the tutorials without Session, and deprecate Session | **N/A.** `Session` is not ported. |
| `e373d93` | 2026-10-23 | Check join keys in a derivation's own joins: block (#382) | **Ported** (`f3cd69c`). |
| `6277044` | 2026-10-23 | Run deptry in CI (#386) | **N/A.** CI. |
| `0e6cc27` | 2026-10-23 | Remove the deprecated function-based stream writers | **N/A.** Nothing ported or exposed. |
| `7a1dc2d` | 2026-10-23 | Deprecate Session with the standard @deprecated decorator | **N/A.** No `Session` in Rust or the shim. |
| `f299f3a` | 2026-10-23 | Quiet the warnings a correct quick-start run prints, and fix statement-block evaluation | **Ported** (`b142afb` statement blocks; `a1bb20e` `unknown-range` check). Log-noise changes already matched. |
| `9608e9a` | 2026-10-23 | Generate schema docs into per-kind subfolders and drop the stale pages | **N/A.** Generated docs, Makefile, CI. |
| `cd395ff` | 2026-10-23 | Apply is_a inheritance when streaming tabular input | **Ported** (`973cf9c`). Slot inheritance already matched; inherited `joins` ported. |
| `1323cc8` | 2026-10-23 | Emit a class derivation built only from constants once, reading no input (tag `v0.6.0-rc1`) | **Ported** (`973cf9c`); see the multi-block divergence below. |

## rust-linkml-core: `df9d18ec` → `v0.5.0` (`7bd81d5`)

Bumped in `c67599f`. No source change was needed in
`crates/linkml-map-schemaview`; every `schemaview`/`linkml_meta` item we use is
source-compatible and test results were identical before and after.

- **Deterministic CURIE collision resolution (`4094da4`).** `compress_uri` now
  always picks the lexicographically smallest prefix (`dc:title`, `schema:name`).
  Previously the choice depended on a randomly seeded `HashMap`, so output could
  vary from run to run.
- **Regenerated metamodel (`e641216`)** keeps `abstract` and `typeof`, which were
  silently dropped before. A non-boolean `abstract` now fails to load.
- Pin by commit SHA, not by the lightweight `v0.5.0` tag.
- Main (`ab693fe`) adds nothing we use. **Never enable `linkml_meta/pyo3` or
  `stubgen`:** upstream's optional pyo3 would add a second `pyo3-ffi`
  (`links = "python"`) that conflicts with our pyo3 0.23. The lockfile still
  holds a single pyo3.

## Pre-existing bugs found and fixed

Not caused by upstream drift; found while porting and fixed here.

- **Output key order was alphabetical** (`serde_json` lacked `preserve_order`);
  the conformance runner sorts keys, which masked it. Fixed in `8db9251`: golden
  personinfo `Person-001` is now byte-identical to upstream's expected YAML.
- **`--target-schema` defaulted to the source schema.** Upstream uses `None`,
  which disables every target-driven behaviour. Fixed in `246d944` (optional end
  to end).
- **Writers:** JSON/JSONL never stripped nulls (upstream does so since `88c6598`,
  before our `v0.5.3` pin), stream YAML was not a sorted multi-document stream,
  and the CSV header came from the first record only. Fixed in the writers
  rewrite (`686b190`).
- **`is_multi_statement` misclassified** trailing-newline and bracketed
  multi-line expressions. Fixed in `b142afb` (mirrors `_is_statement_block`).
- **`slugify()` differed from upstream `_slugify`** (separator, `None` on empty,
  leading digit, HTML entities). Rewritten in `9b49e83`. python-slugify 9.1.3
  keeps the legacy output, so the upstream bump itself changes nothing.
- **PyO3:** `SchemaView`/`SchemaDefinition` arguments failed under
  linkml-runtime 1.12.1 (`json_dumper` module versus instance). Fixed in
  `193c005`.
- **PyO3 value bridge** went via JSON; it now converts directly (`88e3b10`;
  about 1.5x on 50k rows, 4.6x on conversion-heavy specs).
- **Expression built-in table** (`54fdfcd`). Upstream's `eval_utils` function
  table was unchanged across this range, so the earlier reviews had missed the
  gap. Added: `floor`, `ceil`, `substr`, `strip`/`lstrip`/`rstrip`, `replace`,
  `startswith`/`endswith`/`contains`, `split`/`join`, `to_snake`/`to_camel`/
  `to_pascal`, `sum`, `sorted`, `any`/`all`, `reversed`, `first`/`last`,
  `is_str`/`is_int`/`is_float`/`is_bool`/`is_list`, `coalesce`, `list`, `tuple`.
  Brought to Python semantics: `upper`/`lower`/`title`/`capitalize` (CPython
  titlecase rules; non-strings raise), `int`/`float` (base, underscores, Unicode
  digits), `round` (banker's rounding, exact-decimal `ndigits`, `int` stays
  `int`), `max`/`min` (plain Python comparison), `is_numeric`, `uuid5`, `case`,
  and upstream's "Unknown function … Did you mean" message. About 960 expected
  values were generated by running upstream's table under CPython 3.13.

## Deliberate divergences

Each is marked `PARITY:` in the code.

- **Multi-block streaming (closed in a follow-up, `739394b`).** The pipeline now
  streams every class derivation in spec order, as upstream `transform_spec`
  does, with tables matched like upstream `DataLoader` (directory stems; a single
  file is the table named by `--source-type`, else its stem). Remaining nuance:
  `--source-type` on a directory overrides every block's source type, as
  upstream, but Rust's lenient source-slot handling then succeeds where upstream
  fails with "No such slot".
- **Unrestricted mode is a native interpreter, not asteval.** The gate is ported
  (`expr/gate.rs`): restricted by default, `--unrestricted-eval` / `with_unrestricted_eval`
  / `unrestricted_eval=` to opt in. Unrestricted blocks cover assignments, `if` and
  expression statements only (no `for`, `while`, `def`, `lambda`, `try`); those fail to
  parse at plan-build time, where asteval would run them.
- **Errors are classified by variant, not by Python exception class.**
  `ExprError::Eval` stands for upstream's `InvalidExpression`/`TypeError`/`ValueError`
  (the unrestricted fallback catches these); `ExprError::Uncaught` covers `IndexError`,
  `KeyError`, `AttributeError` and private-attribute `NameError`. Errors inside the
  built-in function table are all `Eval`, so the few that upstream raises as
  something else (an `AttributeError` from `uuid5` with a non-string name, say) become
  null under `unrestricted_eval` instead of raising. Syntax errors in `{...}`
  (`{1 + 2}`, `{x, y}`) surface at parse time in Rust but at evaluation time upstream,
  so they are not swallowed by the unrestricted fallback.
- **SQL-null leniency in statement blocks** (division by zero, `None` arithmetic)
  where asteval stops.
- **`deunicode` stands in for `text-unidecode`** in `slugify`; rare characters may
  differ.
- **Decimals:** ASCII digits only when parsing; Parquet/DuckDB decimals are staged
  as strings (VARCHAR), not cast to `DECIMAL(p,s)`.
- **YAML anchors for a repeated `Decimal` object.** All YAML output now goes
  through a port of PyYAML's emitter (`linkml_map_core::pyyaml`), including
  quoting, 80-column folding and multi-line scalars. The one thing it cannot
  reproduce is object identity: PyYAML writes the *same* `Decimal` object twice
  as `&id001` / `*id001` (`SafeRepresenter.ignore_aliases` exempts `str`, `int`,
  `float` and `bool` but not `Decimal`), whereas `Value` always writes the value
  out in full.
- **`--chunk-size 0` is an error** (upstream writes nothing); a failed columnar
  conversion removes its staging file.
- **`validate`:** the schemaview backend does not load `linkml:types` and builtins
  are matched by name, so unknown-range is slightly lenient.
- **An undeclared target slot is skipped**, not raised as Python's `induced_slot`
  would.
- **`load_specification`** returns a dict-backed `TransformationSpecification`
  stand-in (no pydantic): unset or misspelt fields read `None`.
  `PathNotFoundError` subclasses both `FileNotFoundError` and `ValueError`.
- **`--strict` only**, no `--no-strict`.
- **Two ported constant-block tests adjusted.** Upstream drives `transform_spec`,
  which has no identifier pre-check; the Rust pipeline mirrors the CLI's
  `_require_identifiers`, so the tests set `mint_ids` or drop the target schema.

## Suggested upstream issues (not filed)

- `read_json_auto`'s default `sample_size` (20,480) silently drops columns that
  first appear late in Parquet/DuckDB output; ISO timestamp typing differs between
  duckdb 1.5.2 and 2.0.0.
- `-O` is silently ignored for single-object input; `chunked(it, 0)` drops all
  output.
- `Decimal.normalize()` in content-id canonicalisation rounds to 28 digits, so ids
  for values differing beyond digit 28 merge.
- `pivot_operation` derivations are flagged by `missing_identifiers` though the
  runtime skips them.
- `_with_inherited_fields` extends list fields on every call (not idempotent),
  copies `is_a` from a mixin's ancestor and copies every unset scalar.
- `get_available_identifiers` omits `.jsonl`; the "no input" error prints
  `found []` with no hint.
- `_check_range_exists` relies on `all_types()` including the `linkml:types`
  builtins, so a schema without that import warns on `range: integer`.
- `load_specification` normalises the caller's dict in place (the constructor
  deep-copies); `Transformer`'s third positional field is the private
  `_derived_specification`, so `ObjectTransformer(spec, sv, tv)` mis-binds.
- An undeclared target slot raises `ValueError` mid-row with a target schema, but
  `validate-spec` does not catch it.
- `data_loaders._NUMERIC_TYPE_NAMES` coerces decimal source columns to float (not
  exact for TSV input).
- `_unrestricted_eval` errors go to asteval's stderr writer and are swallowed
  (a typo such as `src.evnts` silently gives `None`).

## Still deferred

- **DuckDB set-based join engine** (`510b0a50ca`) and **SCAN/MIGRATE
  normalisation**: unchanged from earlier reviews.
- **Expression language gaps (closed):** the conditional expression
  `x if c else y`, dict displays, `dict()`/`set()` (with keyword arguments),
  dict/set comprehensions, generator expressions and `for k, v in ...` targets
  now parse and evaluate, checked against upstream v0.6.0-rc1 itself (restricted
  simpleeval and unrestricted asteval). Upstream does not pre-process `{x}`:
  Python parses it as a set display and `LinkMLEvaluator._eval_set` gives the
  one-name/attribute form its reference meaning, which the parser mirrors. Set
  displays other than `{x}` and set comprehensions are rejected by upstream's
  restricted evaluator; `expr/gate.rs` enforces this in restricted mode. A set is a
  de-duplicated list in insertion order and dict keys are stringified
  (`expr/compound.rs`).
- **`Value` has no tuple, set, bytes or big-integer variant**, so for example
  `is_list((1, 2))` is true, and integer results beyond `i64` raise.
- **Joined/dotted `populated_from` values** are not coerced to the target range.
- **`type: Agent`.** linkml 1.12's `transformer_model` drops the abstract `Agent`
  from the `creator`/`author`/`reviewer` union; Rust still accepts and injects
  `type: Agent`. Low priority.
- **`python-compat` CI matrix** is capped at Python 3.13 by PyO3 0.23 (upstream
  supports 3.14).
- **Three Rust gaps exposed by the `a62221f` triage, not addressed here:** a
  slot-level `pivot_operation` is ignored by the engine; the UNMELT fallback
  passes the row through where upstream returns the `id_slots` subset; and a
  missing source unit is not an error where upstream raises `ValueError`.

## Verification

- `cargo test --workspace --exclude linkml-map-py`: **341** tests at baseline,
  **615** after the merges (0 failures).
- PyO3 pytest: **103 passed**.
- Compliance **55/55** and golden **14/14**, unchanged.
- The columnar (Parquet/DuckDB) tests pass with `--features columnar`.

## Next review

Start from **`1323cc8`** (`v0.6.0-rc1`). If the **`v0.6.0` final tag** exists by
then, review `1323cc8..v0.6.0` first, since the final release may differ from the
release candidate.
