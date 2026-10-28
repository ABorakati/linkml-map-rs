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
  upstream, and a `populated_from` (or same-name copy) that is not a slot of that
  type now fails with "No such slot", as upstream does (closed: error parity).
- **Unrestricted mode is a port of asteval 1.0.x** (`expr/asteval/`, wrapped by
  `expr/stmt.rs`; the gate is `expr/gate.rs`): restricted by default,
  `--unrestricted-eval` / `with_unrestricted_eval` / `unrestricted_eval=` to opt in. It
  is a literal port of `asteval.Interpreter`: CPython 3.13's `ast.parse` rules
  (indentation, tab/space check, f-strings), every statement form asteval handles
  (`for`/`while` with `else`, `break`/`continue`, augmented and chained assignment,
  unpacking, `def`/`lambda`/`return`, `try`/`except`/`else`/`finally`, `del`, `assert`,
  `raise`), the exact symbol table (Python built-ins, `math`, `type`/`open`, no LinkML
  built-ins), its error list and interrupt flags (including the odd outcomes, such as a
  typed `except` after a direct `raise`), and its limits (`MAX_STR_LEN`,
  `MAX_EXPONENT`, `MAX_SHIFT`, recursion). A text that does not parse is swallowed
  (`target` stays None) exactly as asteval swallows `SyntaxError`/`IndentationError`.
  A source map is a `DynObj` for attribute access (a missing attribute is None) and
  also iterates, subscripts and tests membership like the dict it may be upstream (the
  engine's `Value` cannot tell the two apart). Remaining differences: integers beyond `i64` (an `OverflowError`), sets
  and tuples leave the block as lists, dict keys as strings, objects that have no
  `Value` form (functions, iterators, bytes, complex numbers) are an
  "unrepresentable value" error, `id()`/`hash()` of
  strings, CPython's hash-order iteration of sets, `open()` always raising
  `FileNotFoundError`. A runaway loop stops at a
  step limit where upstream hangs.
- **Errors are classified by variant, not by Python exception class.**
  `ExprError::Eval` stands for upstream's `InvalidExpression`/`TypeError`/`ValueError`
  (the unrestricted fallback catches these); `ExprError::Uncaught` covers `IndexError`,
  `KeyError`, `AttributeError` and private-attribute `NameError`. Errors inside the
  built-in function table are all `Eval`, so the few that upstream raises as
  something else (an `AttributeError` from `uuid5` with a non-string name, say) become
  null under `unrestricted_eval` instead of raising.
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
- **An undeclared target slot** now raises, as Python's `induced_slot` does
  (closed: error parity). Not modelled: `induced_slot` also finds a *schema-level* slot the
  class does not list, and resolves a name through `slot_name_mappings` (`related_to` for
  `related to`); here only the class's induced slots count, so those cases raise where
  upstream does not.
- **`load_specification`** returns a dict-backed `TransformationSpecification`
  stand-in (no pydantic): unset or misspelt fields read `None`.
  `PathNotFoundError` subclasses both `FileNotFoundError` and `ValueError`.
- **Two ported constant-block tests adjusted.** Upstream drives `transform_spec`,
  which has no identifier pre-check; the Rust pipeline mirrors the CLI's
  `_require_identifiers`, so the tests set `mint_ids` or drop the target schema.
- **Integers are `i64`, never wrapped.** Python has big integers; an `int` result outside
  `i64` (`i + 1` at the maximum, `2**63`, `1 << 100`, `-(-2**63)`, `int(1e30)`) is an
  `Eval` error ("big integers are not supported"), so `2**63 - 1` fails where Python
  gives `9223372036854775807`. An integer literal outside `i64` is still read as a float.
  The simpleeval limits (`MAX_POWER`, `MAX_SHIFT`, `MAX_STRING_LENGTH`) are mirrored.
- **Expression depth.** More than 200 open brackets is a lexer error (CPython's
  `MAXLEVEL`), more than 99 nested statement blocks an indentation error, and an
  expression tree deeper than 496 nodes an uncaught `RecursionError`
  (`expr::parser::MAX_AST_DEPTH`; measured against v0.6.0-rc1, the figure depends on the
  caller's stack depth). Upstream raises at evaluation time, so a conditional expression
  whose deep branch is never taken evaluates there but is refused here.
- **Complex results** (`(-8) ** 0.5`) are an `Eval` error: `Value` has no complex type.

## Suggested upstream issues (not filed)

The complete, grouped list (including everything found by the later fuzzing rounds) is in
[`suggested-upstream-issues.md`](suggested-upstream-issues.md). The items below are the ones found during the sync itself.

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

- A constant `value: [a, b]` (and `overrides`, `target_definition`, a copy
  directive's `include`/`exclude`/`add`) is silently truncated to its first
  element by `ReferenceValidator`, which treats an `Any`-ranged slot as
  single-valued; `[[...]]` raises `NotImplementedError: List of Lists`.
- `perform_unit_conversion` ignores `unit_conversion` on a slot with no
  `populated_from` (`source_obj.get(None)`) instead of using the slot name, and
  writes a `None` key when `target_magnitude_slot` is set without
  `target_unit_slot`.

## Still deferred

- **DuckDB set-based join engine** (`510b0a50ca`): not ported as a query engine, but
  its observable *row order* is (`crates/linkml-map-pipeline/src/join_order.rs`: DuckDB's
  `LEFT JOIN` probe, 64-row output caches and executor stack, checked against DuckDB 1.5).
  Not modelled: a joined table whose file lacks the `lookup_key` column, files over a few
  tens of megabytes, and DuckDB swapping the build side for a one-row primary table.
  **SCAN/MIGRATE normalisation**: unchanged from earlier reviews.
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
- **Expression syntax (closed).** The lexer and parser follow CPython 3.13's
  tokenizer and grammar, so an expression that parses upstream parses here even
  where the construct is never evaluated: slices (`l[1:]`, `s[::-2]`), hex, octal,
  binary, underscore and imaginary literals and `1.5.real`, triple-quoted, raw,
  `u''`, `b''` and adjacent string literals with every escape (`\N{BULLET}` through
  `unicode_names2`), f-strings (PEP 701: nested quotes, specs, `!r`, `{x=}`), `is` /
  `is not`, `@`, `lambda`, `:=`, `...` and `*`/`**` unpacking. What simpleeval cannot
  evaluate (`@`, a lambda, `:=`, a `Starred` argument, a call on anything but a name
  or an attribute) is refused at evaluation, as `FeatureNotAvailable`/
  `OperatorNotDefined`, so the unrestricted fallback retries it. Method calls and
  attributes on scalars reach the real Python methods, as upstream's
  `_eval_attribute` has no allow-list (`s.upper()`, `s.split(' ')`, `n.bit_length()`,
  `x.real`, all of `str`'s methods); inside a statement block asteval's own rules
  apply (`str.format` is refused, a list has its own methods, `**` in a dict display
  is a `None` key, nothing unpacks with `*`). Remaining differences (`PARITY:` in
  `expr/syntax.rs`, `expr/pymethods.rs`, `expr/pyformat.rs`): results with no `Value`
  (`bytes`, complex numbers, `...`, functions and bound methods such as `s.upper`
  or a bare `upper`) raise an "unrepresentable" error where upstream succeeds; a
  list method called in a statement block does not mutate the binding; a tuple of
  affixes for `startswith` is recognised by how it is written (`(..)` or `tuple(..)`)
  because `Value` has no tuple; a lone-surrogate escape (`'\ud800'`) and an integer
  literal beyond `i64` cannot be held; `str.casefold` folds the common special
  cases, not all of `CaseFolding.txt`; `'{0.__class__}'.format(..)` is refused.
- **`Value` has no tuple, set, bytes or big-integer variant**, so for example
  `is_list((1, 2))` is true, and integer results beyond `i64` raise.
- ~~**Joined/dotted `populated_from` values** are not coerced to the target range.~~ Closed (port/absentslot):
  `Class.field`, inlined and FK paths now return their source slot and are coerced like any other
  `populated_from`; a joined value is never coerced, as upstream (`_perform_join_resolution`'s
  `field_path in class_induced_slots(..)` compares a string with slot objects: see `PARITY:` in
  `engine/populated_from.rs`).
- **`type: Agent`.** linkml 1.12's `transformer_model` drops the abstract `Agent`
  from the `creator`/`author`/`reviewer` union; Rust still accepts and injects
  `type: Agent`. Low priority.
- **`python-compat` CI matrix** is capped at Python 3.13 by PyO3 0.23 (upstream
  supports 3.14).
- **The three Rust gaps exposed by the `a62221f` triage are closed** (engine
  parity pass, port/fixengine): `engine/pivot.rs` is a faithful port of
  `transformer/pivot.py` (slot-level `pivot_operation` melts; the UNMELT
  fallback returns the `id_slots` subset; `slot_name_template` follows
  `str.format`), and a missing source unit is a `ValueError`. The same pass
  fixed the other engine gaps in the differential corpus: `str(True)` as the
  `value_mappings` key; `offset` applied before the range coercion with
  Python's arithmetic (a `TypeError` on a non-numeric operand); `_coerce_cardinality`
  in upstream's order (a scalar bound for a multivalued target is split on the
  delimiter; `delimiter.join` raises on non-strings; a reversed JSON/YAML
  `stringification` parses); a scalar in a multivalued source slot wrapped in
  `_map_value_by_range` (which needs the induced slot to take the schema's
  `default_range`, as `SchemaView.induced_slot` does); enum-derivation `expr:`
  seeing the whole row for an `any_of` enum slot; slot-level `class_derivations`
  as a *list* (repeated names kept); a scalar for any list field of the spec
  wrapped; and the `Any`-list collapse extended to `SlotDerivation.value`.
  Still open: a list of lists in an `Any` field raises `NotImplementedError` at
  upstream's spec load (here it collapses to its first list), and an unset
  `target_unit_slot` writes a `None` key upstream (omitted here).

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

## Error parity (round 2c)

Where upstream raises, the port now raises (non-zero exit, equivalent message); each is marked
`PARITY:` in the code and replayed from `tests/differential/corpus`.

- **`validate-spec`** is upstream's command: positional `SPEC_FILES...` (files or directories),
  `<path>: ok [(with warnings)]` on stdout and the findings on stderr, `--merge`, `--entity`,
  `--emit-spec`, `--list-entities`, `--no-warnings`, `--strict`, exit status 1 on an error (or a
  warning under `--strict`). The deprecated-field scan (`spec_scan`) is ported;
  the structural layer is serde's deserialisation, so its messages differ from jsonschema's
  (and stop at the first unknown field).
  `--emit-spec` key order follows the Rust datamodel, not pydantic.
- **Spec loading is as permissive and as strict as the pydantic model**: `extra="forbid"` at
  every level (`deny_unknown_fields`), list forms and compact keys everywhere, `copy_directives`
  keyed by element name, deprecated `sources` / `object_derivations` / `derived_from` accepted
  and migrated. `SchemaReference` has upstream's fields (`name`, `version`, `schema_uri`,
  `source_file`); `unit_conversion.molecular_weight` / `valence` remain Rust extensions that
  upstream rejects.
- **Engine raises**: a slot that is not on the source class, a derived slot the target class lacks
  (`_is_coerce_to_*` / `_target_slot_datatype`), a nested derivation without a target schema,
  an enum-ranged source slot without exactly one enum derivation, a join with a key value and no
  lookup index or table, a YAML join table (`LookupIndex does not yet support 'yaml' files`).
- **Laziness**: `CompiledExprs::build` no longer fails the plan on an expression the parser rejects
  (the row path reports it on first use). Join synthesis still parses every `expr:` while
  building the plan, because upstream does too (`ast.parse` raises `SyntaxError` there, even with
  no rows); a Python-valid expression the Rust parser lacks therefore still fails early.

Remaining, not modelled:
- An explicit `permissible_value_derivations: null` (the legacy `attic/transform-s1-to-s2` spec)
  makes upstream's `derive-schema` and `transform_enum` raise `AttributeError`; the datamodel cannot
  tell an explicit null from an absent field, so Rust derives the schema
  (`tests/derive_schema/cases.json`: `known_divergence`).
- ~~A populated_from dotted path that is neither a join alias, the source class, an inlined path nor
  an FK path raises upstream~~ Closed (port/absentslot): it raises here too, and `Class.field` and FK
  paths (`org_id.name`, through the `ObjectIndex`) are resolved.
- A nested derivation with a different `populated_from` and no join spec raises upstream
  (`no implicit join could be synthesized`); Rust derives it from the parent row.
- A target schema without `imports: linkml:types` has no `integer` etc. upstream, so nothing is
  coerced; the schemaview backend treats builtin type names as types regardless.
- Rows of a joined TSV/CSV table: upstream's `LookupIndex` parses *every* digit-bearing string cell
  with `int()` then `float()` (`_parse_numeric`); Rust coerces only columns whose schema range is
  numeric.

Suggested upstream issues (not filed):
- `schema_mapper._derive_enum` and `transform_enum` crash on an explicit `permissible_value_derivations: null`
  (pydantic accepts the null; the `default_factory=dict` default only covers an absent field).
- `LookupIndex.lookup_row` applies `_parse_numeric` to every cell, so a joined text column such as `"007"` becomes `7`.
- A join over a table with no file fails with DuckDB's `CatalogException` rather than a message naming the join.
