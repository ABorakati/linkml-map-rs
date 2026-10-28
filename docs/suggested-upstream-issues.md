# Suggested upstream issues (not filed)

Upstream bugs and quirks found while porting `linkml/linkml-map` `v0.6.0-rc1` to Rust and fuzzing the two
implementations against each other (October 2026). None has been filed. The Rust port reproduces each behaviour
(marked `// PARITY:` in the code) unless a note says otherwise, so fixing one upstream means changing the port
when we re-pin.

The pin-drift review [`upstream-sync-2026-10-27.md`](upstream-sync-2026-10-27.md) has the porting context.

## Security-relevant

- **Format-string escape in restricted mode.** `LinkMLEvaluator._eval_attribute` bypasses simpleeval's
  `DISALLOW_METHODS` and attribute allow-list, so `'{0.__class__.__mro__}'.format(x)` reaches private attributes.
  (The Rust port refuses this.)
- **Unrestricted mode exposes Python builtins** such as `open()` to spec expressions (`open(1)` closes the
  process's stdout).

## Expression evaluation

- `ZeroDivisionError` escapes the SQL-style null wrappers and aborts the row, even under `unrestricted_eval`,
  unlike every other arithmetic error.
- The `(TypeError, ValueError)` retry in `_null_propagating_arithmetic` re-raises on the second attempt for floats
  and bitwise operators; `1 in "1"` raises from inside the `except` handler while `"a" in 5` gives `None`.
- `_unrestricted_eval` swallows every error, so a typo such as `src.evnts` silently gives `None`; after a
  restricted failure a single (non-statement) expression always yields `None`, because the fallback returns
  `symtable["target"]`, which is never assigned.
- Restricted mode evaluates only the first statement of a block and returns it as the result.
- The restricted evaluator refuses attribute access on a comprehension variable (`[x.a for x in xs]`) while asteval
  allows it; a slot with the same name as the loop variable shadows the variable in `_eval_attribute`.
- `_distributed_getattr` on a plain dict never looks up the key (`{"a": 1}.a` raises `AttributeError`), while a
  `DynObj` returns the value, so behaviour depends on the slot's range; a `DynObj` is always truthy, even when empty.
- `_distributing` distributes over the first argument only (`contains(x, ['a'])` with a list `x`); `case()`
  unpacks a two-character string as a pair.
- `to_camel("")` raises `IndexError` and `last({...})` raises `KeyError` (not covered by `_null_safe`).
- simpleeval's `_eval_formattedvalue` ignores `!r`/`!s`/`!a` and the implicit `!r` of `{x=}`; `_eval_joinedstr`
  never accumulates `length`, so `MAX_STRING_LENGTH` is checked per part.
- `safe_power` rejects `9223372036854775807 ** 0`; `2**1000000` evaluates but cannot be printed past 4,300 digits;
  `round(x, n)` with a data-supplied huge `n` hangs.
- A bare function name such as `upper` evaluates to the function object.
- The validator's safe names omit `list`/`tuple`/`dict`/`set`, although the evaluator provides them.

## asteval (unrestricted statement blocks)

- `safe_pow`/`safe_lshift` compare their right operand with `exp > MAX_EXPONENT` for any `numbers.Number`, so a
  `complex` exponent (`2 ** 1j`) or shift count is a `TypeError` ('>' not supported between 'complex' and 'int').
- `safe_getattr` calls `getattr(obj, attr)` without a default, so any missing attribute raises, contradicting the
  `getattr(obj, attr, None)` that follows.
- `raise_exception` leaves `_interrupt` set after a handled error, silently skipping later statements in a loop
  body.
- `ExceptionHolder.exc_info` is empty for errors raised directly, so a typed `except` fails after `raise`.
- A function call replaces the symtable with a pre-call snapshot, so assignments to globals inside a function are
  lost.
- `while` with `continue` never re-tests the condition and loops forever.
- `{**m}` builds `{None: m}`; starred expressions are unsupported.

## Spec loading and validation

- `ReferenceValidator` treats an `Any`-ranged slot as single-valued, so a constant `value: [a, b]` (and
  `overrides`, `target_definition`, a copy directive's `include`/`exclude`/`add`) is silently truncated to its first
  element; `[[...]]` raises `NotImplementedError: List of Lists`.
- `value_mappings` with an empty-string value crashes spec loading (`TypeError: 'str' object does not support item
  assignment`).
- `load_specification` normalises the caller's dict in place (the constructor deep-copies); `Transformer`'s third
  positional field is the private `_derived_specification`, so `ObjectTransformer(spec, sv, tv)` silently
  mis-binds.
- `_check_range_exists` relies on `all_types()` including the `linkml:types` builtins, so a schema without that
  import warns on `range: integer`.
- An undeclared target slot raises `ValueError` mid-row once a target schema is given, but `validate-spec` does not
  catch it.
- `_with_inherited_fields` extends list fields on every call (not idempotent), copies `is_a` from a mixin's
  ancestor, and copies every unset scalar.
- `pivot_operation` derivations are flagged by `missing_identifiers` though the runtime skips them.
- `_is_coerce_to_multivalued` lists `MultiValuedDict` twice (probably a typo for `MultiValuedList`).

## Transformation engine

- `perform_unit_conversion` ignores `unit_conversion` on a slot with no `populated_from` (`source_obj.get(None)`)
  and writes a `None` key when `target_magnitude_slot` is set without `target_unit_slot`; `convert_units` with an
  offset unit (for example `Cel` to `K`) raises "Ambiguous operation with offset unit".
- `Decimal.normalize()` in content-id canonicalisation rounds to 28 digits, so ids for values differing beyond
  digit 28 merge; it also raises `decimal.Overflow` above exponent 999999 and silently rounds below -1000026.
- `--source-type X` with directory input forces `X` on every class derivation, so a second block over another table
  fails with "No such slot".

## Join engine and lookup index

- The two "Conflicting join specs" raise sites (`join_engine._collect_joins` and `engine._collect_all_joins`) word
  the message differently, only the join-engine one gives the "use distinct join names" advice, and
  `_collect_joins` prints an unresolved key as `None` where the per-row path raises a different `ValueError`.
- A table named after a SQL reserved word (for example `Order`) breaks the per-row `LookupIndex`, whose
  `CREATE TABLE` does not quote the name.
- `_perform_join_resolution` tests `field_path in class_induced_slots(joined_class)`, comparing a string with
  `SlotDefinition` objects, so it is always false and joined values are never range-, cardinality-, datatype- or
  enum-coerced.
- `_resolve_joined_row` queries `lookup_index.lookup_row(table_name=<join alias>)`, so a join's `class_named` has no
  effect on which table is read: `transform_spec` registers `if join_name in data_loader` and the join engine reads
  `data_loader` under the join name too, so `joins: {org: {class_named: Org, ...}}` reads `org.tsv` and, with no
  such file, fails with DuckDB's `CatalogException: Table with name org does not exist!` at the first row that has a
  key (`class_named` only picks the schema class for the slot lookup that never matches, above). Either read the
  table under `class_named` or document that the join name must be the file stem. A join named like a single input
  file's table reads that file on the engine path but raises `ValueError: Cannot search for files when loader is in
  single-file mode.` on the per-row path.
- `DynObj.__getattr__` returns `None` for every missing attribute, hiding typos such as `Person.x` or `src.x`.
- The set-based join engine's row order depends on DuckDB internals (a `LEFT JOIN` probe returns matches first,
  caches small outputs, and changes order with several joins and with file size). An `ORDER BY` on the primary row
  number would make it deterministic and equal to the per-row fallback.
- The join engine hands the transformer a different primary row than the per-row path: every column goes through
  `_parse_numeric` and JSON is cast to `VARCHAR`, so `"007"` becomes `7` only when a block has joins.
  `LookupIndex.lookup_row` likewise coerces text cells to numbers.
- A joined table whose name is a SQL keyword (for example `Order`) fails, because the table name is interpolated
  into SQL; a join over a table with no file fails with DuckDB's `CatalogException` instead of naming the join.
- `read_json_auto`'s default `sample_size` (20,480 rows) silently drops columns that first appear later in
  Parquet/DuckDB output; ISO timestamp typing differs between duckdb 1.5.2 and 2.0.0.

## Input and output

- `map-data -f json`/`jsonl` crashes on YAML-loaded dates (`TypeError: Object of type date is not JSON
  serializable`), for example on upstream's own `personinfo_basic` example.
- `_DecimalDumper` does not exempt `Decimal` from YAML aliasing, so one value copied into two slots is written as
  `&id001` / `*id001`.
- `_DelimitedFileLoader` keeps `None` for missing trailing cells while dropping empty ones, and keeps a BOM in the
  first header; `data_loaders._NUMERIC_TYPE_NAMES` coerces decimal source columns to float (not exact for TSV).
- Pure-Python `yaml.safe_load` rejects tab-indented JSON, so `cli.py` falls back to `json.loads`, and `1e5`
  parses as a string whenever YAML accepts the document.
- `DataLoader._find_file` matches lowercase extensions only while `get_available_identifiers` lowercases them, so
  `X.TSV` is listed as available but never found; `.jsonl` is omitted, and the "no input" error prints `found []`
  with no hint.
- A slot expression that evaluates to a `complex` (`(-8) ** 0.5`, `1j`) reaches the writers: `map-data -f json`/`jsonl`
  abort part-way with `TypeError: Object of type complex is not JSON serializable`, while `-f yaml` writes
  `!!python/complex '1.7e-16+2.83j'` (the full `yaml.Dumper`, which also emits `!!python/tuple`/`!!set`, and a file
  `safe_load` then refuses) and the delimited writers write `str(z)`. An integer above 4300 digits likewise aborts
  every writer with CPython's `ValueError: Exceeds the limit (4300 digits)`.
- `-O` is silently ignored for single-object input; `chunked(it, 0)` (`--chunk-size 0`) drops all output.

## derive-schema (`SchemaMapper`)

- `_copy_dict` aliases source objects into the target, so merge and rewire mutate the source schema.
- `_derive_enum` shallow-copies and mutates the source enum, builds a `ClassDefinition` when the enum is missing,
  and `mirror_source` is a no-op; an explicit `permissible_value_derivations: null` crashes it and `transform_enum`.
- `target_definition.attributes`/`slot_usage` is never applied to a copied class.
- `exclude` without `copy_all` raises `KeyError`; `_rewire_parent` raises `AttributeError` for a non-source parent.
- `derive-schema` ignores `source_schema_patches`.

## invert (`TransformationSpecificationInverter`)

Details in [`invert-parity.md`](invert-parity.md).

- The `if inverted_ed.expr:` guard tests the freshly built inverse instead of `ed.expr`, so it never fires and the
  strict "Cannot invert slot derivation" branch (`elif self.strict`) is dead.
- An unresolvable source slot raises `AttributeError: 'NoneType' object has no attribute 'range'` instead of
  `NonInvertibleSpecificationError`.
- Unit inversion uses only `ucum_code` then `symbol`, ignoring `iec61360code`, `abbreviation` and `descriptive_name`.
- `re.match(r"^\w+$", expr)` accepts a trailing newline (should be `fullmatch`).
- Two slots mapped to the same target silently overwrite each other; `value_mappings`, nested derivations and `hide`
  are dropped without warning.
- `_pre_flight_validate` can raise on a broken source schema although it is documented as informational.

## Not linkml-map (third-party)

- PyYAML cannot round-trip U+0085, and misreads a double-quoted scalar folded across lines with `\` at a line end.
- `text-unidecode`'s Artistic/GPL licence prevents vendoring its tables into an Apache-2.0 port of `slugify`.
