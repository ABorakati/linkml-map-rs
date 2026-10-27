# linkml-map-rs

[![DOI](https://zenodo.org/badge/1268538591.svg)](https://doi.org/10.5281/zenodo.20702210)
[![CI](https://github.com/ABorakati/linkml-map-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/ABorakati/linkml-map-rs/actions/workflows/ci.yml)
[![Rust](https://img.shields.io/badge/rust-1.75%2B-orange.svg)](https://www.rust-lang.org)
[![LinkML](https://img.shields.io/badge/linkml-1.3%2B-blue.svg)](https://linkml.io)
[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)

A fast, parallel **Rust port of [linkml-map](https://github.com/linkml/linkml-map)** — the
LinkML data-transformation engine. Maps source objects to a target data model
from a declarative `TransformationSpecification`, with a **drop-in Python API**,
a CLI, and a streaming concurrent pipeline.

- **Drop-in** for the Python `linkml-map` transform path — identical output,
  same spec/schema files, a compatible `import linkml_map` shim.
- **Verified correct** — passes the ported compliance suite (55/55) and the
  golden fixtures (14/14), and produces **byte-identical output** to the
  reference Python engine on a direct head-to-head diff.
- **Fast** — ~12× single-thread and ~40–48× across cores vs base Python on the
  same workload, via a native engine and a rayon/tokio pipeline.

> Status: covers the data-transformation engine (the feature set exercised by
> the upstream compliance suite). See [Feature coverage](#feature-coverage).

## Citation

If you use this software, please cite it using the following BibTeX entry:

```bibtex
@software{borakati2026linkmlmaprs,
  author = {Borakati, Aditya},
  title = {{linkml-map-rs: A Fast, Parallel Rust Port of linkml-map}},
  year = {2026},
  publisher = {Zenodo},
  doi = {10.5281/zenodo.20702210},
  url = {https://doi.org/10.5281/zenodo.20702210}
}
```

## Why

`linkml-map` is the LinkML mechanism for declaratively transforming data between
schemas (e.g. source EHR/registry → OMOP CDM). The reference engine is Python,
single-threaded (GIL-bound), and interpreter-speed per row. This port keeps the
**same spec language and output** while running natively and fanning rows across
all cores — useful when transforming large datasets.

## Installation

### Python Bindings (Recommended Drop-in API)

Install this **instead of** upstream `linkml-map` (existing imports will resolve to this optimized version).

#### Option A: Direct Git Installation (No cloning required)

Using **uv** (recommended):
```bash
uv pip install "git+https://github.com/ABorakati/linkml-map-rs.git#subdirectory=crates/linkml-map-py"
```

Using **pip**:
```bash
pip install "git+https://github.com/ABorakati/linkml-map-rs.git#subdirectory=crates/linkml-map-py"
```

#### Option B: From Source (Requires cloning)

First, clone the repository:
```bash
git clone https://github.com/ABorakati/linkml-map-rs.git
cd linkml-map-rs
```

Then install the package:

Using **uv**:
```bash
uv pip install ./crates/linkml-map-py
```

Using **pip**:
```bash
pip install ./crates/linkml-map-py
```

---

### CLI (Rust Binary)

Install the command-line interface (`linkml-tr-rs`) from source.

1. Clone the repository (if you haven't already):
   ```bash
   git clone https://github.com/ABorakati/linkml-map-rs.git
   cd linkml-map-rs
   ```
2. Build and install the binary using Cargo:
   ```bash
   cargo install --path crates/linkml-map-cli
   ```
   Add `--features columnar` for Parquet/DuckDB output (see
   [Quickstart](#cli); it compiles a bundled DuckDB, so the first build is slow).

---

### Rust Library

To use the core engine as a dependency in your Rust project, add it to your `Cargo.toml`:

```toml
[dependencies]
linkml-map-core = { git = "https://github.com/ABorakati/linkml-map-rs.git" }
```

## Quickstart

### CLI

Once the CLI is installed, you can run transformations directly:

```bash
linkml-tr-rs map-data \
  -s source.yaml -T transform.yaml \
  --source-type Person -o out.jsonl data.jsonl
```

Output follows upstream `linkml-map` byte for byte where it can: JSON, JSONL and
YAML drop `null` slots, stream YAML is a key-sorted multi-document stream, and
CSV/TSV flatten nested objects with `__`. `-f/--output-format` overrides the
format inferred from the output extension (YAML when there is neither), `-O FILE`
(repeatable) writes further copies in other formats, and `--chunk-size N` sets the
records written per chunk. `--source-class` is accepted as an alias of
`--source-type`.

Input is read as upstream reads it. YAML data and transformation specs are YAML 1.1
(PyYAML `safe_load`): an unquoted `yes`/`on` is a boolean, `017` the integer 15,
`1_000` 1000, `1:30` 90 and `2024-01-01` a date (written back unquoted in YAML), while
`1e3`, `0o7` and `1.0e20` are strings. A single `.json`/`.yaml` file goes through that
loader first and falls back to JSON. TSV/CSV rows follow linkml's `TsvLoader`/`CsvLoader`
(Python `csv` rules: universal newlines, spaces after a delimiter skipped, a byte-order
mark kept in the first header; empty cells dropped, short rows padded with null), and
columns whose source range is numeric are read with `int()`, then `float()`.

Parquet and DuckDB output (`-f parquet`, `-f duckdb`, or a `.parquet`/`.duckdb`
output extension) is built by DuckDB from the staged JSONL, so nested objects
become `STRUCT` columns and lists of them `STRUCT[]`. It needs an output file,
`--table-name NAME` sets the DuckDB table (default: the file stem), and it is
behind the off-by-default `columnar` cargo feature because it compiles DuckDB:
`cargo build --release -p linkml-map-cli --features columnar`.

### Python (Recommended Drop-in API)

Once installed, use it as you would standard `linkml-map`:

```python
from linkml_runtime import SchemaView
from linkml_map.transformer.object_transformer import ObjectTransformer

tr = ObjectTransformer(source_schemaview=SchemaView("source.yaml"),
                       specification=open("transform.yaml").read())
out = tr.map_object({"id": "P:1", "height": {"value": 172.0, "unit": "cm"}})
```

Source objects and specifications are converted directly between Python and
Rust values, with no JSON round-trip. A `range: decimal` slot comes back as an
exact `decimal.Decimal`. Any other non-JSON scalar (a `date`/`datetime`, say) is
converted with `str()`, so a `date`/`datetime` anywhere in the input, including a
`yaml.safe_load`-parsed spec dict passed to `create_transformer_specification`,
comes back as a plain `str`, never a native `datetime.date`/`datetime.datetime`.
Schema and specification arguments accept a `str` or `os.PathLike`;
`ObjectTransformer(..., strict=True)` turns on strict mode, and
`linkml_map.load_specification(path_or_dict)` loads and validates a specification.

<details>
<summary><b>Native Python API (Reference Only)</b></summary>

A low-level native API is exported directly by the compiled binary (`linkml_map_rs`):

```python
from linkml_map_rs import Transformer

t = Transformer(source_schema="source.yaml", spec="transform.yaml")
out = t.transform(obj)            # one object
outs = t.transform_many([a, b])    # batch; schema/spec parsed once
```

This is generally not needed as the recommended drop-in API has matching performance, but remains available for low-level direct Rust engine integration.

Cross-table joins (a spec's `joins:` block, explicit or synthesized from an
implicit `{Table.col}` reference) need the joined table's rows supplied
in-memory before transforming, mirroring upstream `Transformer.lookup_index`:

```python
t.register_join_table("demographics", [{"patient_id": "P:1", "age": 42}], "patient_id")
out = t.transform({"pid": "P:1"})   # populated_from "demo.age" / {demo.age} now resolves
```

`register_join_table(name, rows, key_column)` may be called multiple times to
register several tables; calling it again with the same `name` replaces that
table. Call it before `.transform()` / `.map_object()` / `.transform_many()`.

A pre-flight `validate_spec` checks a spec against its schema(s) — unresolved
class/slot/enum names, bad expression refs, misconfigured joins — **without**
running any transform:

```python
from linkml_map_rs import validate_spec

messages = validate_spec("transform.yaml", source_schema="source.yaml", target_schema="target.yaml")
for msg in messages:
    print(msg.severity, msg.path, msg.message)
```
</details>

### Rust

Usage in Rust:

```rust
use linkml_map_core::{engine::ObjectTransformer, value::Value};
let engine = ObjectTransformer::new(spec, Some(&source_provider), None);
let out = engine.map_container(&input, Some("Person"))?;
```

## Performance

Same workload (measurements fixture) through base Python and this engine,
16-core machine, 100k rows:

| Mode | Python | Rust 1-thread | Rust all-cores |
|---|---|---|---|
| Transform-only (CPU) | 1× | **11.9×** | **47.8×** |
| End-to-end (read→transform→write) | 1× | **9.4×** | **40.2×** |

Transform-only is CPU-bound and scales with cores; end-to-end parallelises the
whole JSONL path (bulk I/O + parallel parse/transform/serialise). Reproduce:

```bash
cd benchmarks/vs_python && ./run.sh 100000
```

## Correctness

| Check | Result |
|---|---|
| Ported compliance suite (`test_compliance_suite.py`) | **55 / 55** |
| Golden + example fixtures | **14 / 14** |
| Direct output diff vs real Python `linkml-map` | **0 differences** on all transformable fixtures |

The compliance suite mirrors the upstream parametrized feature tests (type
coercion, collections, expressions, joins, enums, inheritance, stringification,
unit conversion); expected values are Python's own. See
[`tests/COMPLIANCE_REPORT.md`](tests/COMPLIANCE_REPORT.md).

## Feature coverage

**Implemented:** type coercion, list↔dict collections, the expression language
(incl. safe string builtins — `lower`/`upper`/`title`/`capitalize`/`slugify` —
and an opt-in **strict mode** that errors on unbound names, and on enum
values matching no permissible value of the target enum, instead of yielding
null / warning), target-schema-driven type coercion (a `--target-schema` types
slots the spec gives no `range`; `range: decimal` is an exact decimal kept
exact in every output) and enum values resolved to the target enum's
permissible value keys (by key, `meaning`, `title` or alias), foreign-key joins, enum/permissible-value mapping (incl. list-form
`populated_from` mapping several source PVs to one target), inheritance
(is_a/mixins), stringification (delimiter + JSON/YAML), unit conversion
(scheme-aware, with a medically-extended table incl. molar↔mass via
`molecular_weight` and equivalents↔molar via `valence`), slot-level nested
`class_derivations`, `missing_values` (sentinel codes → null), **offset /
aggregation / pivot (melt/unmelt)**, target-schema derivation, content-hash
identifier minting (see [Identifiers](#identifiers)), Parquet/DuckDB output
(`columnar` feature), and the inverse (round-trip) transformer.

> **v0.6.0 parity:** tracks upstream linkml-map v0.6.0. The deprecated
> `sources`, `derived_from`, `object_derivations`, and top-level
> `slot_derivations` forms have been **removed** — use `populated_from` and
> slot-level `class_derivations`. `source_schema`/`target_schema` are structured
> `SchemaReference` objects (a bare string still parses as the schema name).

**Streaming tabular input** (`.tsv`/`.csv` or a directory of tables) applies
`is_a`/`mixins` inheritance to every field of a class derivation, including
inherited `joins`. A class derivation built only from constant `value:`s (no
`populated_from`, `joins`, `copy_directives`, `expr` or row-reading slot, and not
named after a source class) is emitted **once** and reads no input; a spec made
only of such blocks needs no data. The pipeline streams one primary table, so a
spec with several row blocks emits only the first (upstream streams each block).

**Not implemented** (out of scope): spec→Python/SQL compilation and JSON-schema
validation of output.

**Unit conversion** uses a finite table (SI + clinical/medical units), not
pint's full universe — unknown units error loudly rather than silently
diverging.

## Identifiers

When a **target schema** is supplied (`--target-schema`, or `target_schema=` in
Python) and it gives a class an identifier slot (`identifier: true`), every
class derivation of that class must either derive the slot or mint it:

- **Derive it** like any other slot (a natural key a referring spec can recompute).
- **Mint it** with `mint_ids: true`: the id becomes a UUID v5 hash of the record's
  own content, `uuid5(uuid5(NAMESPACE_URL, "<target schema id>/<TargetClass>"),
  canonical(record))`, and is written first. Minting is off by default and can be
  set on the specification, per target class (`class_defaults: {Class: {mint_ids:
  true}}`, or the compact `{Class: true}`) and per class derivation; the most
  specific setting wins. Abstract and mixin target classes are skipped, and an id
  derived by an `is_a`/`mixins` ancestor counts as derived.

A derivation that does neither is an error: `map-data` refuses to run before it
opens any output (with `--continue-on-error` it reports each problem once, writes
the records without ids and exits non-zero), the engine raises, and `validate`
reports it. Without a target schema nothing is checked or minted.

The canonical form sorts keys, drops nulls and empty lists/objects, sorts list
items unless the target slot is `list_elements_ordered`, and writes numbers in one
fixed-point form (floats rounded to 15 significant digits). The same record gets
the same id whatever the row order, chunking or parallelism, but these are
content hashes, not stable identifiers: any change to an emitted value changes
the id. Mirrors upstream linkml-map #342.

## Architecture

```
linkml-map-core         pure-Rust transform engine (sync, Send+Sync)
├── datamodel           TransformationSpecification + derivations
├── schema              SchemaProvider trait + projections
├── expr                LinkML expression lexer/parser/evaluator
├── engine              ObjectTransformer, ObjectIndex (FK), units table
└── inference           TransformationSpecificationInverter

linkml-map-schemaview   SchemaProvider over the LinkML metamodel (SchemaView)
linkml-map-io           async streaming readers/writers (JSONL/JSON/YAML/CSV)
linkml-map-pipeline     concurrent pipeline: Arc<CompiledPlan> fanned across
                        a rayon pool via tokio; streaming, bounded backpressure
linkml-map-cli          command-line interface
linkml-map-py           PyO3 bindings + the `linkml_map` compatibility shim
linkml-map-conformance  the ported compliance suite + golden-fixture runner
```

The engine is a **synchronous pure function** (`Send + Sync`, no globals);
concurrency lives in the pipeline, which parses the spec once into an
`Arc<CompiledPlan>` and transforms many rows in parallel.

## Dependencies

Runtime: `serde` / `serde_json` / `serde_yaml_ng` (schema loading) and libyaml via `unsafe-libyaml` (YAML data and spec input, with PyYAML's YAML 1.1 scalar rules ported in `linkml-map-core`; YAML output is the PyYAML-compatible emitter there too), `indexmap`, `anyhow` /
`thiserror`, `tokio` + `futures` (async I/O), `rayon` (data parallelism),
`walkdir`. Python bindings: `pyo3` (built with `maturin`); the shim needs
`linkml-runtime` + `pyyaml`. Schema introspection uses the LinkML metamodel and
SchemaView Rust crates, compiled from the unofficial [Kapernikov/rust-linkml-core](https://github.com/Kapernikov/rust-linkml-core) repository.

## Building

```bash
cargo build --release            # engine + CLI + pipeline
cargo test --workspace           # all tests
cargo build --release -p linkml-map-cli --features columnar   # + Parquet/DuckDB output (bundled DuckDB, slow first build)

# Python wheel + shim
pip install maturin
maturin build --release -m crates/linkml-map-py/Cargo.toml
```

## Citing

If you use this software, please cite it (see [Citation](#citation) near the top for a BibTeX entry, and [`CITATION.cff`](CITATION.cff) for CFF metadata) as well as the upstream LinkML / linkml-map project.

## Acknowledgements

A port of [linkml/linkml-map](https://github.com/linkml/linkml-map) by the
LinkML project. The transform spec language, compliance suite, and golden
fixtures originate upstream (CC0); this repository reuses them under their
original license.

## License

Apache-2.0 — see [`LICENSE`](LICENSE).
