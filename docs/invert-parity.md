# `invert` parity with upstream (2026-10-28)

`linkml-tr-rs invert` writes byte-identical YAML to upstream `linkml-map invert` (v0.6.0-rc1), and fails
where upstream fails (exit status 1; the exception text for `NonInvertibleSpecificationError`,
`AttributeError` and `ValueError` is reproduced on stderr).

```
linkml-tr-rs invert [--strict | --no-strict] -T SPEC [-T SPEC ...] [-o FILE] SCHEMA
```

`SCHEMA` is positional as upstream; `-s/--source-schema` is accepted too (the other Rust subcommands
spell it that way). `-T` may be repeated and may name directories, as upstream (`load_and_merge_specs`).
`--strict` is the default.

## What was ported

* `crates/linkml-map-core/src/inference/inverter.rs`: `inverter.py`, method for method. The old
  `SchemaProvider`-based inverter was replaced because upstream reads the *induced* source slot through
  `linkml_runtime.SchemaView`, and the engine's `SchemaProvider` projects away what that depends on.
* `crates/linkml-map-core/src/inference/induced_slot.rs`: `SchemaView.induced_slot`, `get_slot`,
  `class_ancestors`, `slot_ancestors`, `class_slots`, `get_identifier_slot` / `get_key_slot` and the
  `underscore()` slot-name fallback, over the raw metamodel objects of `inference::SchemaView` (the same
  view `derive-schema` uses). `pattern` resolution, `alias`, `domain_of` and `owner` are not ported: nothing
  reads them here.
* `dump_inverted_spec`: `dump_output(spec, "yaml")` is `yaml_dumper.dumps(model)`, i.e.
  `yaml.dump(remove_empty_items(model.model_dump()), SafeDumper, sort_keys=False)`: pydantic field order,
  `None` and empty containers dropped, `False` and `""` kept.
* `crates/linkml-map-cli/src/invert.rs` and the `invert` subcommand, including the part of upstream's
  `_pre_flight_validate` that can raise. It is meant to be informational, but `validate_spec_semantics`
  calls `source_sv.class_induced_slots(cls)` for every source class the spec reads (populated_from, the
  identity fallback, nested derivations, `joins`, dotted references), so a source class whose slots do not
  resolve ends the run with a `ValueError` before anything is inverted. `preflight_source_classes` replays
  those calls in `validator.py`'s order.

## Upstream behaviours reproduced (`// PARITY:` in the code)

* A slot derivation with a `range` but no resolvable `populated_from` source slot (no `populated_from`, or the
  source class is not in the schema) dies with `AttributeError: 'NoneType' object has no attribute 'range'`.
* `unit_conversion` looks the source slot up unguarded: no `populated_from` or an unknown class is a `ValueError`.
* The source slot is looked up with `class is None` too: it then resolves a schema-level slot (or an attribute
  of exactly one class), so a class derivation without `populated_from` inverts fine.
* Only `ucum_code` then `symbol` of the source slot's unit become `target_unit` (`iec61360code`, `abbreviation`
  and `descriptive_name` are ignored, even when `iec61360code` is all there is).
* `re.match(r"^\w+$", expr)` accepts one trailing newline (`"name\n"` is a bare slot name, and becomes the
  slot name verbatim, newline included).
* `invert_enum_derivation` tests `inverted_ed.expr` (always `None`) rather than `ed.expr`, so an enum
  derivation with an `expr` is inverted from its permissible value derivations instead of refused.
* `invert_class_derivation`'s `elif self.strict: raise ... "Cannot invert slot derivation"` branch is
  unreachable.
* Nested `class_derivations`, `value_mappings`, `value`, joins and the other derivation fields are not
  carried over; two derivations of the same source slot collapse to the later one at the first one's position.
* `enum_derivations: null` and `permissible_value_derivations: null` (explicit) are `AttributeError`s
  (`NoneType` has no `values`); `class_derivations: null` / `slot_derivations: null` fail in upstream's
  spec normalisation.

## Corpus

`tests/invert/` (`generate.py` records upstream's answers; `cargo test -p linkml-map-cli --test
invert_upstream_parity` replays them):

* every spec/schema pair of `tests/derive_schema/cases.json` (shipped examples, golden fixtures, attic
  specs, the 55 compliance combos);
* `inputs/*.spec.yaml`: hand-written specs over `inputs/src.yaml` (imports, `slot_usage`, `attributes`,
  slot `is_a`, mixins, identifier / key slots, all unit spellings, enums) covering each branch above;
* `inputs/fuzz/`: Hypothesis-generated pairs (`fuzz.py --emit N --seed S`); `fuzz.py --run N --seed S` runs
  a live differential campaign against the release binary.

Every pair runs strict and `--no-strict`: 280 pairs, 560 cases, 179 of which end in an upstream exception
(`NonInvertibleSpecificationError`, `AttributeError`, `ValueError`, pydantic `ValidationError`).

A live campaign (`fuzz.py --run N --seed S`, 150 worlds at seed 777001 and 200 at 777002, both modes) found no
mismatch; so did the general differential run (`tests/differential/run.sh --fuzz ... --time-budget 600`).

## Divergences left

* Error *text* other than the three exception types above differs (Python tracebacks vs one line).
* The target schema's side of the pre-flight check is not reproduced: upstream loads it when the spec's
  `target_schema` names a path that exists relative to the working directory, and its `class_induced_slots`
  failures would end the run.
* `SchemaView` imports: only relative files and `linkml:` metamodel imports resolve (as for `derive-schema`).
* Schemas are read as YAML 1.2 (`derive-schema` shares this).
* `str.isalnum()` vs Rust `char::is_alphanumeric` for the `\w` test on exotic combining marks.
* The inverter is not exposed to Python: upstream's `linkml_map/__init__.py` exports only `ObjectTransformer` and
  `load_specification` (`linkml_map.inference.inverter` is a submodule, like `SchemaMapper`, which the shim
  does not offer either).

## Suggested upstream issues (not filed)

* `inverter.py`: `if inverted_ed.expr:` should test `ed.expr`; as written the TODO guard is dead.
* `invert_class_derivation`: the `elif self.strict` branch can never run.
* `invert_slot_derivation`: an unresolvable source slot gives an `AttributeError` (`source_slot.range`) rather
  than a `NonInvertibleSpecificationError`, and `unit_conversion` without `populated_from` a bare `ValueError`.
* Unit inversion ignores `iec61360code` (and `abbreviation`), although the forward conversion honours them.
* `re.match(r"^\w+$", ...)` should be `fullmatch`; a trailing newline slips through.
* Two source slots mapped from the same target slot silently overwrite each other in the inverse.
* `invert` never carries `value_mappings`, nested `class_derivations` or `hide` across; a mapping that uses them
  is inverted without warning to a different mapping.
