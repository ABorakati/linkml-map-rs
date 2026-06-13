# Test Fixtures Index — linkml-map-rs

## Overview

This directory contains three tiers of test data:

1. **Compliance Suite** (`compliance/`) — Vendored upstream regression data
2. **Examples** (`examples/`) — All 8 upstream example sets (full upstream archive)
3. **Golden Fixtures** (`golden/`) — Curated subset for integration testing

## Compliance Suite (`tests/compliance/`)

**Status**: Vendored from https://github.com/linkml/linkml-map  
**Upstream Commit**: 19b1985889ec0fc247145e9aa03eb74a4d7b3588  
**Files**: 10 (8 YAML data files + test harness + provenance)  
**Size**: ~56 KB

### Contents
- `attic/` — 8 legacy compliance test YAML files (source/target/transform/data)
- `test_compliance_suite.py` — Python test harness (39 KB)
- `PROVENANCE.md` — Source attribution and license

### Usage
Reference for edge cases and regression testing. The Python test harness is documentation-only for the Rust port; implement equivalent compliance runner as `cargo test`.

---

## Examples Archive (`tests/examples/`)

**Status**: Complete upstream archive  
**Source**: https://github.com/linkml/linkml-map `tests/input/examples/` + `docs/examples/`  
**Files**: 58 (+ README, __init__)  
**Size**: 585 KB

### Fixture Sets (8)
| Name | Type | Size | Purpose |
|---|---|---|---|
| `biolink/` | Schema subset | 358 KB | BioLink model sample transforms |
| `fair_mappings_metadata/` | Metadata | 16 KB | FAIR metadata format mapping |
| `flattening/` | Denormalization | 35 KB | Nested → flat transforms |
| `measurements/` | Quantities | 12 KB | Numeric values + units |
| `personinfo_basic/` | Basic Modeling | 122 KB | Person schema example |
| `single_value_for_multivalued/` | Cardinality | — | Scalar ← array transforms |
| `tabular/` | Format | — | TSV → schema inference |
| `type_coercion/` | Type Casting | 18 KB | String ↔ numeric coercions |

### Usage
Full reference set. Use `examples/` for extended testing and edge-case research. Individual examples are copied into `golden/` for core integration tests.

---

## Golden Fixtures (`tests/golden/`)

**Status**: Curated integration test set  
**Files**: 49 (5 upstream + 1 LinkML-MCP project fixture)  
**Size**: 228 KB

### Fixtures (6)

#### 1. **fair_mappings_metadata/** (21 KB)
- Source/target/transform/data/output YAML
- Transform a linkml-map spec to FAIR metadata format
- 5 files

#### 2. **flattening/** (36 KB)
- Denormalization example; nested → flat transformation
- 12 files including schema variants

#### 3. **linkml-mcp-nhsbt/** (4 KB)
- OMOP ↔ NHSBT clinical domain schema
- Minimal source-only fixture from LinkML-MCP project
- 1 file

#### 4. **measurements/** (13 KB)
- Quantity value transforms; unit handling
- 2 input records, 2 expected outputs
- 9 files

#### 5. **personinfo_basic/** (123 KB)
- Standard person/organization schema
- Large target schema; inheritance and composition
- 15 files

#### 6. **type_coercion/** (19 KB)
- Type casting and implicit conversions
- String, numeric, enum test data
- 6 files

### Provenance
- **personinfo_basic, measurements, flattening, fair_mappings_metadata, type_coercion**: https://github.com/linkml/linkml-map
- **linkml-mcp-nhsbt**: https://github.com/ABorakati/LinkML-MCP

Each has a local `README.md` pointing to source.

### Usage
```bash
cargo test --test integration_golden -- --nocapture
cargo test --test golden_personinfo --release
```

Each fixture is a self-contained test scenario: load schemas, apply transform, compare output.

---

## Coverage Summary

### What's Covered
✓ Basic type mapping (simple types, enums, relationships)  
✓ Nested object handling & denormalization  
✓ Type coercion and casting  
✓ Quantity/measurement domain  
✓ Multi-level composition  
✓ Legacy compliance patterns (attic/)  

### Gaps (Not Vendored)
✗ **Large schemas**: BioLink model (339 KB) referenced but not core to golden tests  
✗ **TSV inference**: tabular example (schema induction only; no transform spec)  
✗ **Complex cardinality**: single_value_for_multivalued (minimal; not critical path)  

## License

All fixtures retain upstream licenses:
- **linkml-map examples**: CC0 1.0 Universal (Public Domain)
- **LinkML-MCP fixture**: LinkML-MCP project license

See `compliance/PROVENANCE.md` for detailed attribution.

## Notes

- **No Rust code in tests/**: Only data files. Implement test runners in `crates/*/tests/`.
- **Test harness**: `compliance/test_compliance_suite.py` is reference documentation for Python test structure; reimplement in Rust.
- **Size constraint**: biolink/ (358 KB) excluded from golden/ but available in examples/ for research.

