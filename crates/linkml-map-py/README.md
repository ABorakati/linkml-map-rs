# linkml-map-py — Python bindings

PyO3 bindings to the Rust linkml-map transform engine, plus a `linkml_map`
compatibility shim so existing Python code can run unchanged.

## Two ways to use it

### 1. Native Rust API (`linkml_map_rs`) — fastest

```python
from linkml_map_rs import Transformer

t = Transformer(
    source_schema="source.yaml",
    spec="transform.yaml",
    target_schema="target.yaml",   # optional
    source_class="Person",         # optional
)
out  = t.transform({"id": "P:1", "height": {"value": 172.0, "unit": "cm"}})
outs = t.transform_many([obj1, obj2])     # schema/spec parsed once
```

- Schema + spec are parsed **once** at construction and reused for every call
  (previously they were reloaded per `transform` — fixed).
- `Transformer.from_yaml(source_schema_yaml, spec_yaml, target_schema_yaml=None,
  source_class=None)` builds from in-memory YAML strings instead of paths.
- `t.map_object(obj, source_type=None)` is an alias of `transform`.
- `t.register_join_table(name, rows, key_column)` registers a joined table's
  rows (list of dicts) in-memory so a spec's `joins:` block (explicit, or
  synthesized from an implicit `{Table.col}` reference) resolves against real
  data instead of `null`. Mirrors upstream `Transformer.lookup_index`
  (`utils/lookup_index.py`), except rows are supplied directly rather than
  loaded from a CSV/TSV/JSON file. Call it before `.transform()` /
  `.map_object()` / `.transform_many()`; calling again with the same `name`
  replaces that table, different names accumulate.

### Pre-flight validation: `validate_spec`

Cross-reference a spec against its source/target schema(s) **without running
any transform** — catches an unresolved class/slot/enum name, an unresolvable
expression reference, a misconfigured join, an unresolved `is_a`/`mixins`, or
a required target slot with no derivation, before any of that surfaces as a
silent `null` or a runtime error.

```python
from linkml_map_rs import validate_spec

messages = validate_spec(
    "transform.yaml",
    source_schema="source.yaml",     # optional
    target_schema="target.yaml",     # optional
    strict=False,                    # optional; True escalates expr warnings to errors
)
for msg in messages:
    print(msg.severity, msg.path, msg.message)   # or just: print(msg)
```

- Each finding is a `ValidationMessage` with `.severity` (`"error"` /
  `"warning"` / `"info"`), `.path`, `.message`, and `.category` (currently
  always `None`).
- Returns `[]` if neither `source_schema` nor `target_schema` is given —
  there's nothing to check the spec against.
- Validates the spec **exactly as parsed** — it does not run implicit-join
  synthesis first (the same step `Transformer.__init__` runs before
  transforming), since the point of this check is to catch problems *before*
  that stage.

### 2. `linkml_map`-compatible shim — drop-in for existing code

The `python/linkml_map/` package shadows upstream `linkml-map` for the common
path, so existing imports work unchanged:

```python
from linkml_runtime import SchemaView
from linkml_map.transformer.object_transformer import ObjectTransformer

tr = ObjectTransformer(source_schemaview=SchemaView("source.yaml"),
                       specification=spec)   # dict, object, path, or YAML
out = tr.map_object({"id": "P:1", "height": {"value": 172.0, "unit": "cm"}})
```

The shim dumps the SchemaView / spec to YAML and calls the Rust engine. Covered:
`ObjectTransformer(source_schemaview=, specification=, target_schemaview=)`,
`.map_object()`, `.create_transformer_specification()`, `.index()` (no-op — the
Rust engine builds any FK index internally).

**Install it instead of upstream `linkml-map`** (it shadows the import name).
Needs `linkml-runtime` (for `SchemaView`) and `pyyaml`.

## Build

```bash
pip install maturin
cd crates/linkml-map-py
maturin develop --release          # builds linkml_map_rs + installs the shim
```

`pyproject.toml` includes `python/` so the `linkml_map` shim ships alongside the
compiled `linkml_map_rs` extension.

## Not a full drop-in

Parity is the data-transformation core (the compliance feature set), not the
whole library:

- **No** SchemaMapper, inverter, Python/SQL compilers, or JSON-schema validation
  exposed to Python.
- **Unit conversion** uses a finite (medically-extended) table — not pint's full
  universe. Within-table units (incl. molar↔mass via `molecular_weight` and
  equivalents↔molar via `valence`) match; unknown units error loudly.
- The shim covers `ObjectTransformer.map_object`; other upstream classes/methods
  are not shimmed.
