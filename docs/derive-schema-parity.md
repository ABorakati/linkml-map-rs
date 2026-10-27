# `derive-schema` parity with upstream (2026-10-27)

`linkml-tr-rs derive-schema` now writes byte-identical YAML to upstream
`linkml-map derive-schema` (v0.6.0-rc1) on every pair in `tests/derive_schema/cases.json`:
the vendored examples and golden fixtures, the 55 compliance combos, the legacy "attic"
specs, and hand-written feature cases (`tests/derive_schema/inputs/`). It was also fuzzed against
upstream in-process (about 2,000 random schema/spec pairs, success output and failure/success both
compared) with no remaining mismatches.

## How it works

`crates/linkml-map-core/src/inference/`:

| File | Upstream |
|------|----------|
| `schema_mapper.rs` | `linkml_map/inference/schema_mapper.py` (`SchemaMapper`, same method names) |
| `schema_view.rs` | the parts of `linkml_runtime.SchemaView` it calls (`imports_closure` incl. `from_schema` injection, `get_class`, `get_slot`, `get_enum`) |
| `metamodel.rs` + `metamodel.json` | `linkml_runtime` dataclasses: field order, `__post_init__` normalisation, `remove_empty_items` |

`metamodel.json` is generated from the installed `linkml_runtime` by `generate_metamodel.py`. The
derived schema is built from those dataclass-equivalents and dumped through the PyYAML emitter,
so key order, `name`/`text`/`tag` injection, `Bool(...)` coercion and dropped empties all match.
Containers are reference-counted because upstream relies on `copy()` being shallow.

Regenerate the corpus with `/home/user/upstream/upvenv/bin/python tests/derive_schema/generate.py
--discover` (`--only SUBSTRING` for a subset); the compliance inputs come from
`COMPLIANCE_EXPORT_DIR=tests/derive_schema/compliance cargo test -p linkml-map-conformance export_cases`.
The replay test is `crates/linkml-map-cli/tests/derive_schema_upstream_parity.rs`.

## Upstream behaviour reproduced on purpose (`PARITY:` in the code)

- `_copy_dict`: `exclude` on something never copied raises `KeyError`; a string `exclude` is a
  substring test.
- `copy(source_enum)` is shallow: permissible-value derivations write into the *source* enum's
  `permissible_values` (replacing PVs and dropping `meaning`); a missing source enum yields a
  `ClassDefinition`, so PV derivations or `mirror_source` then crash.
- `_derive_slot` with `target_definition` overwrites every field, set or not.
- `_derive_class`: for a class copied from a source class, `target_class.attributes = {}` /
  `.slot_usage = {}` become empty `JsonObj`s that `curr_v in ([], {})` does not match, so
  `target_definition` never fills them.
- `get_slot` is `lru_cache`d: a miss stays a miss even after a derivation merges the attribute
  into the shared source class.
- `_rewire_parent` raises when an `is_a`/mixin is not a source class; `dictionary_key` on a slot whose
  range is not a derived class raises `KeyError`.
- Spec loading: linkml's `ReferenceValidator` treats `range: Any` fields as single-valued, so
  `exclude: [a, b]` keeps only `a` (and `include`, `add`, `overrides`, `target_definition` likewise).
  Implemented in `normalise_spec_json` (`collapse_any_lists`).

## Remaining divergences

- `linkml:*` imports are empty stand-in schemas (upstream loads the bundled metamodel files;
  `linkml:types` has no classes, slots or enums, so this only matters for `linkml:meta` and friends).
  Remote and CURIE-mapped imports are not resolved. Relative file imports are.
- Schemas are parsed as YAML 1.2 (`serde_yaml_ng`); PyYAML's 1.1 scalars (`yes`/`no`, unquoted
  dates) can differ.
- Unknown fields in a spec are ignored; pydantic rejects them (`extra_forbidden`), so upstream
  fails on e.g. `tests/compliance/attic/mapping_norm.yaml`.
- Error text differs (Python tracebacks vs `Error: ...`); exit status agrees.
- `slot_derivation.value: [1, 2]` becomes `1` upstream (same `Any` collapse) and is not
  replicated for `value`, because it affects the engine; not checked here.
- The CLI takes `-s/--source-schema`, upstream a positional `SCHEMA` argument; `compile` and
  `invert` subcommands do not exist (see below).

## `invert`

Upstream has `linkml-map invert -T SPEC SCHEMA`. Rust has the library
(`TransformationSpecificationInverter`) but no subcommand. Wiring one is small; byte-identical
output is not, because upstream dumps a pydantic model (`model_dump()` field order and defaults)
and none of the shipped examples is invertible (each raises `NonInvertibleSpecificationError`),
so there is nothing to diff against. Not implemented.

## Suggested upstream issues (not filed)

- `ReferenceValidator` collapses a list in any `Any`-ranged field to its first element
  (`CopyDirective.include/exclude/add`, `overrides`, `target_definition`, `SlotDerivation.value`);
  empty lists become `null`.
- `SchemaMapper._copy_dict` aliases source elements into the target schema, so later merging
  and `_rewire_class` mutate the source `SchemaView`'s own schema.
- `_derive_enum` shallow-copies the enum (mutating the source's permissible values) and builds a
  `ClassDefinition` when the source enum is missing; `mirror_source` is a no-op because the dicts are shared.
- `_derive_class` never applies `target_definition.attributes`/`slot_usage` to a class derived from a
  source class (`JsonObj` vs `{}` comparison).
- `_copy_dict` with `exclude` but no `copy_all` raises `KeyError`.
- `_rewire_parent` crashes with `AttributeError` on a parent that is not a source class.
- `derive-schema` ignores `source_schema_patches`.
