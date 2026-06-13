# Vendored Test Fixtures Report — linkml-map-rs

**Completed**: 2026-06-13  
**Task**: Vendor upstream compliance suite + golden fixtures  
**Repository**: https://github.com/ABorakati/linkml-map-rs

## Summary

Successfully vendored **118 test data files (903 KB)** from two sources:

1. **Upstream linkml-map repository** — compliance suite + full examples archive
2. **LinkML-MCP project** — real-world OMOP/NHSBT schema fixture

---

## Goal A: Upstream Compliance Suite

### Status: COMPLETE

**Source**: https://github.com/linkml/linkml-map  
**Upstream Commit Hash**: `19b1985889ec0fc247145e9aa03eb74a4d7b3588`  
**Location**: `C:\Users\abora\linkml-map-rs\tests\compliance\`

### Vendored Assets

#### Compliance Data (`tests/compliance/attic/`)
**8 YAML files** (16.4 KB total) — legacy transformation test cases

| File | Size | Purpose |
|---|---|---|
| `denormalize-mapping.yaml` | 810 B | Transform spec for denormalization |
| `mappingset-norm-example-01.yaml` | 123 B | Normalized mapping set example |
| `mapping_denorm.yaml` | 986 B | Denormalized mapping reference |
| `mapping_norm.yaml` | 1.0 KB | Normalized mapping reference |
| `personinfo-s1-example-data-01.yaml` | 242 B | Sample person data (source) |
| `personinfo_s1.yaml` | 5.7 KB | PersonInfo source schema |
| `personinfo_s2.yaml` | 5.6 KB | PersonInfo target schema |
| `transform-s1-to-s2.yaml` | 2.8 KB | Complete transform spec |

**Total**: 8 files, 16.4 KB

#### Test Harness
**File**: `test_compliance_suite.py` (39 KB)  
**Status**: Reference documentation (Python test runner; reimplemented in Rust)

#### Documentation
**File**: `PROVENANCE.md` (2.7 KB)  
**Contents**: Source attribution, license, upstream path mapping

### Not Vendored (Size Constraint)
- Full `tests/test_compliance/` Python test suite — reference only; to be reimplemented in Rust as `cargo test`
- Individual concept lookups — test environment-specific

---

## Goal B: Golden Test Fixtures

### Status: COMPLETE

**Location**: `C:\Users\abora\linkml-map-rs\tests\golden\`  
**Scope**: 6 self-contained transformation scenarios  
**Total Files**: 49 (including per-fixture READMEs)  
**Total Size**: 228 KB

### Fixture Sets

#### 1. `fair_mappings_metadata/` (5 files, 21 KB)
- **Purpose**: FAIR metadata mapping from LinkML-map spec
- **Files**: source schema, target schema, transform spec, input data, expected output
- **Source**: https://github.com/linkml/linkml-map `tests/input/examples/fair_mappings_metadata/`

#### 2. `flattening/` (12 files, 36 KB)
- **Purpose**: Denormalization; nested → flat object transformation
- **Files**: 2 source schemas, 2 target schemas, transform, 3 mapping sets
- **Source**: https://github.com/linkml/linkml-map `tests/input/examples/flattening/`
- **Test Patterns**: Multi-level nesting, cardinality changes

#### 3. `linkml-mcp-nhsbt/` (1 file, 4.0 KB)
- **Purpose**: OMOP↔NHSBT clinical domain schema (real-world example)
- **Contents**: Minimal NHSBT source schema
- **Source**: https://github.com/ABorakati/LinkML-MCP `tests/fixtures/nhsbt_min/`
- **Note**: Schema only (no data due to clinical confidentiality)

#### 4. `measurements/` (9 files, 13 KB)
- **Purpose**: Quantity value transforms with units
- **Files**: source schema, target schema, transform spec, 2 input samples, 2 expected outputs
- **Source**: https://github.com/linkml/linkml-map `tests/input/examples/measurements/`
- **Test Patterns**: Numeric + unit slot handling

#### 5. `personinfo_basic/` (15 files, 123 KB)
- **Purpose**: Standard person/organization schema with inheritance
- **Files**: source schema, target schema, transform, 1 input, expected output, supporting schemas
- **Source**: https://github.com/linkml/linkml-map `tests/input/examples/personinfo_basic/`
- **Test Patterns**: Multi-class composition, relationship mapping, large target schema

#### 6. `type_coercion/` (6 files, 19 KB)
- **Purpose**: Type casting and implicit conversions
- **Files**: source schema, target schema, transform spec, 1 input, expected output
- **Source**: https://github.com/linkml/linkml-map `tests/input/examples/type_coercion/`
- **Test Patterns**: String↔numeric, enum code resolution

### Fixture Organization
Each fixture directory contains a local README.md pointing to the upstream source and describing the test contract.

---

## Bonus: Full Examples Archive

### Status: VENDORED (for reference)

**Location**: `C:\Users\abora\linkml-map-rs\tests\examples\`  
**Source**: https://github.com/linkml/linkml-map  
**Contents**: All 8 upstream example sets  
**Total Files**: 58  
**Total Size**: 585 KB

### Sets Included

| Name | Size | Files | Purpose |
|---|---|---|---|
| `biolink/` | 358 KB | 5 | BioLink model transform sample (large schema) |
| `fair_mappings_metadata/` | 16 KB | 5 | FAIR metadata format |
| `flattening/` | 35 KB | 12 | Denormalization patterns |
| `measurements/` | 12 KB | 9 | Quantity + unit handling |
| `personinfo_basic/` | 122 KB | 15 | Person schema example |
| `single_value_for_multivalued/` | — | 6 | Cardinality: scalar ← array |
| `tabular/` | 4 KB | 4 | TSV → schema inference |
| `type_coercion/` | 18 KB | 6 | Type casting |

**Note**: `biolink/` (358 KB) is in `examples/` for research but not duplicated in `golden/` (core tests).

---

## Coverage Analysis

### What's Covered

- **Type mapping**: Simple types, enums, relationships
- **Structure handling**: Multi-level composition, inheritance, denormalization
- **Type coercion**: String↔numeric, enum code lookups
- **Domain modeling**: Quantity/unit, person/organization, clinical (NHSBT)
- **Legacy patterns**: Compliance suite (attic/) for regression
- **Transform specs**: Full linkml-map format (class_derivations, value_mappings, etc.)

### What's NOT Covered (Gaps)

| Feature | Reason | Size | Reference |
|---|---|---|---|
| **BioLink Large Schema** | 339 KB; included in examples/ for research | 358 KB | `examples/biolink/` |
| **TSV Inference** | Schema induction only; no transform spec | — | `examples/tabular/` |
| **Complex Cardinality** | Single_value_for_multivalued; minimal test patterns | — | `examples/single_value_for_multivalued/` |

---

## File Organization

```
tests/
├── compliance/                    # Vendored compliance regression data
│   ├── attic/                    # 8 legacy transformation YAML files
│   ├── test_compliance_suite.py  # Python test harness (reference)
│   └── PROVENANCE.md             # Source attribution + license
├── examples/                      # Full upstream archive (8 sets, 58 files, 585 KB)
│   ├── biolink/
│   ├── fair_mappings_metadata/
│   ├── flattening/
│   ├── measurements/
│   ├── personinfo_basic/
│   ├── single_value_for_multivalued/
│   ├── tabular/
│   └── type_coercion/
├── golden/                        # Core integration test fixtures (6 sets, 49 files, 228 KB)
│   ├── fair_mappings_metadata/
│   ├── flattening/
│   ├── linkml-mcp-nhsbt/
│   ├── measurements/
│   ├── personinfo_basic/
│   ├── type_coercion/
│   ├── README.md                 # Golden fixture overview
│   └── (per-fixture README.md)
├── INDEX.md                       # Complete test data index
└── [test_*.rs files in crates/]  # Rust test harness (to implement)
```

---

## Upstream Source Mapping

### Compliance Suite
| Upstream | Local | Type | Count |
|---|---|---|---|
| `tests/input/attic/` | `tests/compliance/attic/` | YAML | 7 |
| `tests/test_compliance/test_compliance_suite.py` | `tests/compliance/test_compliance_suite.py` | Python | 1 |
| — | `tests/compliance/PROVENANCE.md` | Markdown | 1 |

### Golden Fixtures
| Upstream Set | Local | Type | Count |
|---|---|---|---|
| `tests/input/examples/personinfo_basic/` | `tests/golden/personinfo_basic/` | Mixed | 15 |
| `tests/input/examples/measurements/` | `tests/golden/measurements/` | Mixed | 9 |
| `tests/input/examples/flattening/` | `tests/golden/flattening/` | Mixed | 12 |
| `tests/input/examples/fair_mappings_metadata/` | `tests/golden/fair_mappings_metadata/` | Mixed | 5 |
| `tests/input/examples/type_coercion/` | `tests/golden/type_coercion/` | Mixed | 6 |
| (LinkML-MCP) | `tests/golden/linkml-mcp-nhsbt/` | YAML | 1 |

---

## License

### Upstream linkml-map
- **License**: CC0 1.0 Universal (Public Domain Dedication)
- **URL**: https://github.com/linkml/linkml-map/blob/main/LICENSE
- **Scope**: All compliance suite + examples fixture data

### LinkML-MCP Fixture
- **License**: Same as LinkML-MCP project
- **Source**: https://github.com/ABorakati/LinkML-MCP/tests/fixtures/nhsbt_min/

---

## Implementation Guidance

### For Rust Test Harness

The compliance suite is intended as a reference set. The Python test_compliance_suite.py uses dynamic test loading (pytest parameterization). Implement equivalent tests in Rust via parametrized tests or integration tests.

### For Golden Test Integration

Reference tests/INDEX.md and tests/golden/README.md for test scenarios. Each golden fixture is self-contained and can be tested independently.

---

## Summary Statistics

| Metric | Count |
|---|---|
| **Total files vendored** | 118 |
| **Total data size** | 903 KB |
| **Compliance YAML files** | 8 |
| **Example sets** | 8 |
| **Golden fixtures** | 6 |
| **Golden files** | 49 |
| **Upstream commit** | 19b1985889ec0fc247145e9aa03eb74a4d7b3588 |
| **License** | CC0 1.0 (public domain) |

---

## Completion Checklist

- [x] Clone upstream linkml-map repository (shallow)
- [x] Locate and copy compliance test data (8 YAML files)
- [x] Copy test harness (test_compliance_suite.py)
- [x] Vendor full examples archive (8 sets, 58 files)
- [x] Create golden fixtures subset (5 upstream + 1 LinkML-MCP)
- [x] Create per-directory provenance/README files
- [x] Retrieve upstream commit hash
- [x] Verify no Rust code in test data (data files only)
- [x] Clean up temporary clone directory
- [x] Create comprehensive INDEX and provenance documentation

**Status**: COMPLETE
