# linkml-map-rs Conformance Report

**Total**: 55  **PASS**: 55  **FAIL**: 0  **SKIP**: 0

## Case Results

| Case | Status | Reason |
|------|--------|--------|
| `compliance/map_types/string->string` | **PASS** |  |
| `compliance/map_types/integer->integer` | **PASS** |  |
| `compliance/map_types/string->integer` | **PASS** |  |
| `compliance/map_types/integer->float` | **PASS** |  |
| `compliance/map_types/float->integer` | **PASS** |  |
| `compliance/map_types/float->integer` | **PASS** |  |
| `compliance/map_types/integer->boolean` | **PASS** |  |
| `compliance/map_types/integer->boolean` | **PASS** |  |
| `compliance/map_collections/list->dict` | **PASS** |  |
| `compliance/map_collections/dict->list` | **PASS** |  |
| `compliance/expr/0:s1 + s2` | **PASS** |  |
| `compliance/expr/1:{s1} + {s2}` | **PASS** |  |
| `compliance/expr/2:{s1} + {s2}` | **PASS** |  |
| `compliance/expr/3:s1 + s2.s3` | **PASS** |  |
| `compliance/expr/4:s1 + s2.s3.s4` | **PASS** |  |
| `compliance/expr/5:s1 + s2` | **PASS** |  |
| `compliance/expr/6:s1 + s2` | **PASS** |  |
| `compliance/expr/7:len(s1)` | **PASS** |  |
| `compliance/expr/8:s1 < s2` | **PASS** |  |
| `compliance/simple_unit_conversion/s1 m->cm [ucum_code]` | **PASS** |  |
| `compliance/simple_unit_conversion/s1 m->cm [symbol]` | **PASS** |  |
| `compliance/simple_unit_conversion/s1 m->m [ucum_code]` | **PASS** |  |
| `compliance/simple_unit_conversion/s1 a->mo [ucum_code]` | **PASS** |  |
| `compliance/simple_unit_conversion/s1 a->mo [symbol]` | **PASS** |  |
| `compliance/simple_unit_conversion/s1 m->ml [ucum_code]` | **PASS** |  |
| `compliance/simple_unit_conversion/s1 m->pinknoodles [ucum_code]` | **PASS** |  |
| `compliance/simple_unit_conversion/s1 ml->m [ucum_code]` | **PASS** |  |
| `compliance/simple_unit_conversion/s1 pinknoodles->m [ucum_code]` | **PASS** |  |
| `compliance/simple_unit_conversion/s1 m/s->cm/s [ucum_code]` | **PASS** |  |
| `compliance/simple_unit_conversion/s1 m.s-1->cm.s-1 [ucum_code]` | **PASS** |  |
| `compliance/simple_unit_conversion/height_in_m m->cm [ucum_code]` | **PASS** |  |
| `compliance/simple_unit_conversion/s1 m[H2O]{35Cel}->m[H2O]{35Cel} [ucum_code]` | **PASS** |  |
| `compliance/complex_unit_conversion/m->cm` | **PASS** |  |
| `compliance/complex_unit_conversion/cm->cm` | **PASS** |  |
| `compliance/complex_unit_conversion/cm->ml` | **PASS** |  |
| `compliance/complex_unit_conversion/cm->pinknoodles` | **PASS** |  |
| `compliance/stringify/0:None/Some(",")` | **PASS** |  |
| `compliance/stringify/1:None/Some("|")` | **PASS** |  |
| `compliance/stringify/2:None/Some("|")` | **PASS** |  |
| `compliance/stringify/3:None/Some("|")` | **PASS** |  |
| `compliance/stringify/4:Some("JSON")/None` | **PASS** |  |
| `compliance/stringify/5:Some("JSON")/None` | **PASS** |  |
| `compliance/stringify/6:Some("YAML")/None` | **PASS** |  |
| `compliance/isomorphic/use_expr=true` | **PASS** |  |
| `compliance/isomorphic/use_expr=false` | **PASS** |  |
| `compliance/join/inlined=true` | **PASS** |  |
| `compliance/join/inlined=false` | **PASS** |  |
| `compliance/map_enum/A->Some("B")` | **PASS** |  |
| `compliance/map_enum/Z->None` | **PASS** |  |
| `compliance/map_enum/A->Some("B")` | **PASS** |  |
| `compliance/map_enum/C->Some("B")` | **PASS** |  |
| `compliance/inheritance/is_a=true,flatten=false` | **PASS** |  |
| `compliance/inheritance/is_a=true,flatten=true` | **PASS** |  |
| `compliance/inheritance/is_a=false,flatten=false` | **PASS** |  |
| `compliance/inheritance/is_a=false,flatten=true` | **PASS** |  |

## Engine Gap Punch-List

Gaps ranked by number of cases they block (SKIPs + FAILs that cite them).

