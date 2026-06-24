# Compliance Suite Provenance

## Source Repository
- **URL**: https://github.com/linkml/linkml-map
- **Upstream Commit**: 19b1985889ec0fc247145e9aa03eb74a4d7b3588
- **Clone Date**: 2026-06-13

## Contents

This directory contains vendored compliance and test data files from the upstream linkml-map repository, used to ensure conformance of the Rust port with the reference Python implementation.

### Vendored Data Structure

#### `/tests/compliance/attic/`
Legacy compliance test data (6 YAML files):
- Source → mapping → transform specs from older LinkML Map versions
- Used for regression testing and historical reference
- Files:
  - `denormalize-mapping.yaml` — transform specification
  - `mapping_denorm.yaml`, `mapping_norm.yaml` — mapping examples
  - `mappingset-norm-example-01.yaml` — normalized mapping set
  - `personinfo_s1.yaml`, `personinfo_s2.yaml`, `transform-s1-to-s2.yaml` — complete example with source, target, and transform
  - `personinfo-s1-example-data-01.yaml` — sample input data

**Upstream Source**: `tests/input/attic/` → `tests/compliance/attic/`

#### `/tests/compliance/test_compliance_suite.py`
Python compliance test harness (39.6 KB).

**Upstream Source**: `tests/test_compliance/test_compliance_suite.py`

## Not Vendored

### Size Constraints (>2MB excluded)
- `biolink/` example set: 358 KB of schema + transforms (biolink-model.yaml alone: 339 KB)
  - See `/tests/golden/` for representative examples instead
  - Original path: `tests/input/examples/biolink/`

### Compliance Test Harness
- Full `tests/test_compliance/` directory uses Python unittest and linkml-runtime
- The Rust port will implement an equivalent compliance runner via cargo test
- See `tests/test_*.rs` in the Rust crates for port-equivalent tests

## License

All vendored content retains the original upstream license from https://github.com/linkml/linkml-map:
- **License File**: [LICENSE](../../LICENSE) in the linkml-map-rs repository root
- **License Type**: CC0 1.0 Universal (Public Domain Dedication)

## Usage

The compliance suite is intended for:
1. **Conformance Testing**: Rust implementation should produce identical output on the same input specs + data
2. **Edge Case Regression**: Attic files preserve older transformation patterns
3. **Interoperability**: Python ↔ Rust round-trip validation

## Mapping: Upstream Path → Local Path

| Upstream Path | Local Path | Type | File Count |
|---|---|---|---|
| `tests/input/attic/` | `tests/compliance/attic/` | Compliance | 7 |
| `tests/test_compliance/test_compliance_suite.py` | `tests/compliance/test_compliance_suite.py` | Test Harness | 1 |

Total vendored: **8 files** from compliance suite.

## Additional Vendored Upstream Source

`reference/upstream-python/` contains nine Python source modules from the same pinned commit
(`19b1985889ec0fc247145e9aa03eb74a4d7b3588`), covering `transformer/`, `utils/`, `datamodel/`,
and `inference/inverter.py`. These are read-only porting references — not built or executed.
See [`reference/upstream-python/README.md`](../../reference/upstream-python/README.md) for the
full file listing and fetch details.
