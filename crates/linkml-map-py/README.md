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

- Every schema/spec argument takes a `str` **or** a `pathlib.Path` (any
  `os.PathLike`), as upstream does since `173121b`. A missing file raises
  `linkml_map_rs.PathNotFoundError`, which is both a `FileNotFoundError` (what
  upstream raises) and a `ValueError` (what this binding raised before it took
  `Path`s).
- `strict=True` (constructors and the free functions) mirrors upstream
  `ObjectTransformer.strict`: an expression reference that does not resolve
  raises instead of yielding `null`.
- Schema + spec are parsed **once** at construction and reused for every call
  (previously they were reloaded per `transform` — fixed).
- `Transformer.from_yaml(source_schema_yaml, spec_yaml, target_schema_yaml=None,
  source_class=None)` builds from in-memory YAML strings instead of paths.
- `t.map_object(obj, source_type=None)` is an alias of `transform`.
- **Decimals are exact.** A slot derivation with `range: decimal` (upstream
  `72c0c28`) returns a real `decimal.Decimal` that keeps the source digits
  (`"52.30"` -> `Decimal("52.30")`, `str()` is `"52.30"`), and a
  `decimal.Decimal` in a source object stays exact through the engine. A
  non-finite `Decimal` (`NaN`, `Infinity`) is carried as its `str()`. Other
  values are converted directly rather than through JSON, so `None`, `bool`,
  `int` (of any size: an integer beyond 64 bits stays an exact `int`), `float`,
  `complex`, `str`, `list`, `tuple`, `set` and `dict` (in insertion order) keep their Python
  types; any other scalar (`datetime.date`, `pathlib.Path`, ...) becomes its
  `str()`.
- `t.register_join_table(name, rows, key_column)` registers a joined table's
  rows (list of dicts) in-memory so a spec's `joins:` block (explicit, or
  synthesized from an implicit `{Table.col}` reference) resolves against real
  data instead of `null`. Mirrors upstream `Transformer.lookup_index`
  (`utils/lookup_index.py`), except rows are supplied directly rather than
  loaded from a CSV/TSV/JSON file. Call it before `.transform()` /
  `.map_object()` / `.transform_many()`; calling again with the same `name`
  replaces that table, different names accumulate.

### Loading a specification: `load_specification`

```python
from linkml_map_rs import load_specification

spec = load_specification("transform.yaml")          # str or Path: a FILE path
spec = load_specification({"class_derivations": {"Agent": {"populated_from": "Person"}}})
spec["class_derivations"][0]["name"]                 # 'Agent'
```

Native counterpart of upstream `linkml_map.utils.loaders.load_specification`:
a dict is normalised like a file's contents (dict-keyed `class_derivations`
become a named list, and so on) and validated against the specification
datamodel (as upstream's pydantic model: an unknown field is an error, derivation
sections may be lists, deprecated `sources` / `object_derivations` still load).
**Divergence:** upstream returns a pydantic
`TransformationSpecification` and normalises a dict argument in place; the
native function returns the normalised plain **dict** (a new one) and leaves its
argument untouched. The `linkml_map` shim below mirrors upstream more closely.

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
from linkml_map import ObjectTransformer, load_specification

tr = ObjectTransformer("transform.yaml", "source.yaml",   # str / Path / dict / loaded spec;
                       target_schemaview="target.yaml")   # SchemaView / SchemaDefinition / path
out = tr.map_object({"id": "P:1", "height": {"value": 172.0, "unit": "cm"}})

spec = load_specification("transform.yaml")    # also: linkml_map.utils.loaders
spec.class_derivations[0].name
```

Constructor, as upstream since `173121b`: `ObjectTransformer(specification=None,
source_schemaview=None, *, target_schemaview=None, strict=False,
unrestricted_eval=False)`. The first two positional arguments are
`(specification, source_schemaview)` as upstream's dataclass fields (earlier
shim releases took them the other way round); `target_schemaview` is
keyword-only because upstream's third positional field is the private
`_derived_specification`. The constructor **loads**: afterwards `.specification`
is a loaded specification and the schema views are `linkml_runtime.SchemaView`s,
so `tr.source_schemaview.schema.name` works. A dict specification is deep-copied
(the caller's dict is never changed); a missing file raises `FileNotFoundError`
at construction.

Covered: `ObjectTransformer(...)`, `.map_object()`,
`.create_transformer_specification()`, `.load_transformer_specification()`,
`.load_source_schema()`, `.index()` (no-op — the Rust engine builds any FK index
internally), `.strict`, `.unrestricted_eval`, `load_specification()`.

`unrestricted_eval` is real, as upstream, and defaults to `False`: the safe
evaluator has no `src`, runs only the first statement of an `expr:` block and
refuses attribute access on comprehension variables. Pass
`unrestricted_eval=True` for a spec that uses statement blocks (it also exists
on `Transformer`, `transform_object` and `transform_objects` as a keyword).
This is a behaviour change for callers of the Rust-backed classes, which used to
always run unrestricted.

Divergences from upstream, all deliberate:

- `load_specification` (and `.specification`) return a small **dict-backed
  `TransformationSpecification`** (`linkml_map/_specification.py`), not the
  pydantic model: the shim must not ship upstream's datamodel. It offers
  attribute access (`spec.class_derivations[0].name`), `model_dump()` (a plain
  dict that `ObjectTransformer` accepts), `==`, and an unset field reads
  `None`. It applies no defaults or Python-side validation (the Rust engine
  validates when `load_specification` runs), a misspelt field reads `None`
  rather than raising, and a list attribute is a fresh list of views over the
  same dicts. Like upstream, `load_specification(dict)` normalises the dict in
  place.
- `spec_messages` is always `[]`: the deprecation scan that fills it upstream
  is not ported.
- A multi-line `str` that is not an existing file is read as YAML text (kept
  from earlier releases; upstream raises `FileNotFoundError`). A `Path` is
  always a file path.
- `source_schema_patches` are applied inside the engine, not to the
  `source_schemaview` object.

**Install it instead of upstream `linkml-map`** (it shadows the import name).
Needs `linkml-runtime` (for `SchemaView`) and `pyyaml`. `linkml_map.__all__` is
upstream's: `["ObjectTransformer", "load_specification"]`.

## Identifiers (`mint_ids`)

With a `target_schema`, a class derivation whose target class has an
`identifier: true` slot must derive that slot or set `mint_ids: true` (on the
specification, in `class_defaults`, or on the class derivation); otherwise the
transform raises `ValueError` naming the class derivation. Minted ids are UUID v5
content hashes of the emitted record. See the top-level README, section
*Identifiers*.

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
- The shim covers `ObjectTransformer.map_object` and `load_specification`; other
  upstream classes/methods (`DuckDBTransformer`, `derived_specification`, ...)
  are not shimmed.
