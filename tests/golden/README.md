# Golden Fixtures — End-to-End Test Sets

This directory contains representative, self-contained end-to-end transform examples used as golden fixtures for integration testing the Rust port against the Python reference implementation.

## Directory Structure

Each fixture is a complete scenario containing:
- **`source/`** — LinkML source schema (YAML)
- **`target/`** — LinkML target schema (YAML)
- **`transform/`** — linkml-map transformation specification (YAML)
- **`data/`** — Sample input data instances (YAML, JSON, or TSV)
- **`output/`** — Expected transformed output (YAML, JSON)
- **`instructions.yaml`** (optional) — Setup/validation hints

## Fixtures (6 sets, upstream + LinkML-MCP)

### Upstream linkml-map Examples

#### 1. `personinfo_basic/` — Person Information Modeling
- **Purpose**: Basic LinkML schema with simple types, enums, and relationships
- **Transforms**: Schema-to-schema mapping with inheritance and composition
- **Data**: 1 person record with name, email, age, employment
- **Size**: ~122 KB (includes large target schema)
- **Source**: https://github.com/linkml/linkml-map `tests/input/examples/personinfo_basic/`

#### 2. `measurements/` — Measurement Schema
- **Purpose**: Numeric and unit handling; quantity-value patterns
- **Transforms**: Source quantity records → flattened measurement tuples
- **Data**: 2 measurement records (weight, height with units)
- **Size**: ~12 KB
- **Source**: https://github.com/linkml/linkml-map `tests/input/examples/measurements/`

#### 3. `flattening/` — Denormalization / Object Flattening
- **Purpose**: Nested object decomposition; multi-level mapping
- **Transforms**: Normalized multi-class structures → flat parent-child mappings
- **Data**: Mapping sets with multi-level nesting
- **Size**: ~35 KB
- **Source**: https://github.com/linkml/linkml-map `tests/input/examples/flattening/`

#### 4. `fair_mappings_metadata/` — FAIR Metadata Mapping
- **Purpose**: Mapping specification format itself as schema + transform
- **Transforms**: LinkML Map → FAIR mappings metadata
- **Data**: linkmlmap spec → FAIR output
- **Size**: ~16 KB
- **Source**: https://github.com/linkml/linkml-map `tests/input/examples/fair_mappings_metadata/`

#### 5. `type_coercion/` — Type Casting and Coercion
- **Purpose**: Data type conversions (string ↔ numeric, enum codes)
- **Transforms**: Loose typing → strongly typed schema
- **Data**: Mixed string/numeric values with implicit coercions
- **Size**: ~18 KB
- **Source**: https://github.com/linkml/linkml-map `tests/input/examples/type_coercion/`

### LinkML-MCP Project Fixture

#### 6. `linkml-mcp-nhsbt/` — OMOP → NHSBT (Real-World Mapping)
- **Purpose**: Clinical domain schema mapping; OMOP ↔ project-local interchange
- **Contents**: Minimal NHSBT source schema used in LinkML-MCP integration tests
- **Data**: None (schema only; no sample data due to size constraints)
- **Size**: <1 KB
- **Source**: https://github.com/ABorakati/LinkML-MCP `tests/fixtures/nhsbt_min/`

## Usage

### Running Golden Tests (Rust)

```bash
# Load all fixtures via a parametrized test
cargo test --test integration_golden -- --nocapture

# Or manually validate one fixture
cargo test --test golden_personinfo --release
```

### Data Validation

Each fixture is designed for round-trip testing:
1. Load source schema + transform spec
2. Parse input data against source schema
3. Apply transformation
4. Compare output with `output/*.yaml` (expected)

### Migration Notes (Python → Rust)

- Python test suite uses `linkml-runtime` for schema loading and value casting
- Rust port uses `linkml-runtime-rust` or equivalent SchemaProvider trait
- Type coercion (especially numeric/enum) must match Python exactly for compliance
- `flattening/` is critical for testing nested-type handling

## Not Included

The following large or incomplete fixtures are referenced but not vendored:

| Name | Reason | Size | Reference |
|---|---|---|---|
| `biolink/` | Standard BioLink model; 339 KB schema alone | 358 KB | See linkml-map upstream |
| `tabular/` | TSV → schema inference only; no transform spec | — | See linkml-map upstream |
| `single_value_for_multivalued/` | Minimal; complex cardinality patterns | — | See linkml-map upstream |

## License

All fixtures retain the original upstream license:
- **linkml-map examples**: CC0 1.0 Universal (Public Domain)
- **LinkML-MCP fixture**: Same as LinkML-MCP project

See `PROVENANCE.md` in the parent directory (`../compliance/`) for detailed source attribution.

