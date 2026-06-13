# linkml-map-rs Conformance Report

**Total**: 14  **PASS**: 9  **FAIL**: 0  **SKIP**: 5

## Case Results

| Case | Status | Reason |
|------|--------|--------|
| `golden/flattening/MappingSet-001` | **PASS** |  |
| `golden/measurements/PersonQuantityValue-001` | **PASS** |  |
| `golden/measurements/PersonQuantityValue-002` | **PASS** |  |
| `golden/personinfo_basic/Container-001` | **SKIP** | SKIP(engine): transformation error in 'Container.agents': transformation error in 'Agent.death_date': expr eval failed for slot 'death_date' in class 'Person': lex error: unexpected character: '=' |
| `golden/personinfo_basic/Person-001` | **SKIP** | SKIP(engine): transformation error in 'Agent.death_date': expr eval failed for slot 'death_date' in class 'Person': lex error: unexpected character: '=' |
| `golden/type_coercion/MyRecord-001` | **PASS** |  |
| `examples/biolink/gene-001` | **SKIP** | SKIP(schema-load): "C:\\Users\\abora\\linkml-map-rs\\tests\\examples\\biolink\\source\\biolink-model.yaml": failed to load schema from C:\Users\abora\linkml-map-rs\tests\examples\biolink\source\biolink-model.yaml: slots: at `deprecated`: Invalid type boolean `true`. Expected a string |
| `examples/flattening/MappingSet-001` | **PASS** |  |
| `examples/measurements/PersonQuantityValue-001` | **PASS** |  |
| `examples/measurements/PersonQuantityValue-002` | **PASS** |  |
| `examples/personinfo_basic/Container-001` | **SKIP** | SKIP(engine): transformation error in 'Container.agents': transformation error in 'Agent.death_date': expr eval failed for slot 'death_date' in class 'Person': lex error: unexpected character: '=' |
| `examples/personinfo_basic/Person-001` | **SKIP** | SKIP(engine): transformation error in 'Agent.death_date': expr eval failed for slot 'death_date' in class 'Person': lex error: unexpected character: '=' |
| `examples/single_value_for_multivalued/Container-001` | **PASS** |  |
| `examples/type_coercion/MyRecord-001` | **PASS** |  |

## Engine Gap Punch-List

Gaps ranked by number of cases they block (SKIPs + FAILs that cite them).

### 1. Engine runtime error (map_object failure) — blocks 4 case(s)

- `golden/personinfo_basic/Container-001`
- `golden/personinfo_basic/Person-001`
- `examples/personinfo_basic/Container-001`
- `examples/personinfo_basic/Person-001`

### 2. Schema load failure (import resolution / metamodel compat) — blocks 1 case(s)

- `examples/biolink/gene-001`

