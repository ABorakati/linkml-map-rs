# linkml-map-rs Conformance Report

**Total**: 14  **PASS**: 5  **FAIL**: 4  **SKIP**: 5

## Case Results

| Case | Status | Reason |
|------|--------|--------|
| `golden/flattening/MappingSet-001` | **FAIL** | FAIL(mismatch): key 'mappings': [0]: key 'object_id' missing in actual (expected "\"Y:1\"") |
| `golden/measurements/PersonQuantityValue-001` | **PASS** |  |
| `golden/measurements/PersonQuantityValue-002` | **PASS** |  |
| `golden/personinfo_basic/Container-001` | **SKIP** | SKIP(engine): transformation error in 'Container.agents': transformation error in 'Agent.death_date': expr eval failed for slot 'death_date' in class 'Person': lex error: unexpected character: '=' |
| `golden/personinfo_basic/Person-001` | **SKIP** | SKIP(engine): transformation error in 'Agent.death_date': expr eval failed for slot 'death_date' in class 'Person': lex error: unexpected character: '=' |
| `golden/type_coercion/MyRecord-001` | **FAIL** | FAIL(mismatch): key 'id_as_url': expected "\"https://example.org/person/1\"", got "\"P:1\"" |
| `examples/biolink/gene-001` | **SKIP** | SKIP(schema-load): failed to load schema from C:\Users\abora\linkml-map-rs\tests\examples\biolink\source\biolink-model.yaml: slots: at `deprecated`: Invalid type boolean `true`. Expected a string |
| `examples/flattening/MappingSet-001` | **FAIL** | FAIL(mismatch): key 'mappings': [0]: key 'object_id' missing in actual (expected "\"Y:1\"") |
| `examples/measurements/PersonQuantityValue-001` | **PASS** |  |
| `examples/measurements/PersonQuantityValue-002` | **PASS** |  |
| `examples/personinfo_basic/Container-001` | **SKIP** | SKIP(engine): transformation error in 'Container.agents': transformation error in 'Agent.death_date': expr eval failed for slot 'death_date' in class 'Person': lex error: unexpected character: '=' |
| `examples/personinfo_basic/Person-001` | **SKIP** | SKIP(engine): transformation error in 'Agent.death_date': expr eval failed for slot 'death_date' in class 'Person': lex error: unexpected character: '=' |
| `examples/single_value_for_multivalued/Container-001` | **PASS** |  |
| `examples/type_coercion/MyRecord-001` | **FAIL** | FAIL(mismatch): key 'id_as_url': expected "\"https://example.org/person/1\"", got "\"P:1\"" |

## Engine Gap Punch-List

Gaps ranked by number of cases they block (SKIPs + FAILs that cite them).

### 1. Output mismatch (engine ran but result differs from expected) — blocks 4 case(s)

- `golden/flattening/MappingSet-001`
- `golden/type_coercion/MyRecord-001`
- `examples/flattening/MappingSet-001`
- `examples/type_coercion/MyRecord-001`

### 2. Engine runtime error (map_object failure) — blocks 4 case(s)

- `golden/personinfo_basic/Container-001`
- `golden/personinfo_basic/Person-001`
- `examples/personinfo_basic/Container-001`
- `examples/personinfo_basic/Person-001`

### 3. Schema load failure (import resolution / metamodel compat) — blocks 1 case(s)

- `examples/biolink/gene-001`

## Uncategorised Failures (Engine Ran, Output Mismatch)

- `golden/flattening/MappingSet-001`: FAIL(mismatch): key 'mappings': [0]: key 'object_id' missing in actual (expected "\"Y:1\"")
- `golden/type_coercion/MyRecord-001`: FAIL(mismatch): key 'id_as_url': expected "\"https://example.org/person/1\"", got "\"P:1\""
- `examples/flattening/MappingSet-001`: FAIL(mismatch): key 'mappings': [0]: key 'object_id' missing in actual (expected "\"Y:1\"")
- `examples/type_coercion/MyRecord-001`: FAIL(mismatch): key 'id_as_url': expected "\"https://example.org/person/1\"", got "\"P:1\""

