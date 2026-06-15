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
