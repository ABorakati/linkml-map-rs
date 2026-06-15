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

Install the command-line interface (`linkml-map`) from source.

1. Clone the repository (if you haven't already):
   ```bash
   git clone https://github.com/ABorakati/linkml-map-rs.git
   cd linkml-map-rs
   ```
2. Build and install the binary using Cargo:
   ```bash
   cargo install --path crates/linkml-map-cli
   ```

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
linkml-map map-data \
  --source-schema source.yaml --spec transform.yaml \
  --source data.jsonl --out out.jsonl --source-class Person
```

### Python (Recommended Drop-in API)

Once installed, use it as you would standard `linkml-map`:

```python
from linkml_runtime import SchemaView
from linkml_map.transformer.object_transformer import ObjectTransformer

tr = ObjectTransformer(source_schemaview=SchemaView("source.yaml"),
                       specification=open("transform.yaml").read())
out = tr.map_object({"id": "P:1", "height": {"value": 172.0, "unit": "cm"}})
```

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

**Implemented:** type coercion, list↔dict collections, the expression language,
foreign-key joins, enum/permissible-value mapping, inheritance (is_a/mixins),
stringification (delimiter + JSON/YAML), unit conversion (scheme-aware, with a
medically-extended table incl. molar↔mass via `molecular_weight` and
equivalents↔molar via `valence`), `object_derivations`, **offset / aggregation /
pivot (melt/unmelt)**, target-schema derivation, and the inverse (round-trip)
transformer.

**Not implemented** (out of scope): spec→Python/SQL compilation and JSON-schema
validation of output.

**Unit conversion** uses a finite table (SI + clinical/medical units), not
pint's full universe — unknown units error loudly rather than silently
diverging.

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

Runtime: `serde` / `serde_json` / `serde_yaml_ng`, `indexmap`, `anyhow` /
`thiserror`, `tokio` + `futures` (async I/O), `rayon` (data parallelism),
`walkdir`. Python bindings: `pyo3` (built with `maturin`); the shim needs
`linkml-runtime` + `pyyaml`. Schema introspection uses the LinkML metamodel and
SchemaView Rust crates, compiled from the unofficial [Kapernikov/rust-linkml-core](https://github.com/Kapernikov/rust-linkml-core) monorepo.

## Building

```bash
cargo build --release            # engine + CLI + pipeline
cargo test --workspace           # all tests

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
