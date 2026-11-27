# Examples sweep: upstream linkml-map vs linkml-tr-rs

Legend: ok = byte-identical; E(s|o|d) = both failed (messages same/overlap/differ); DIFF = both succeeded, bytes differ;
RS-ERR = upstream ok, Rust failed; UP-ERR = upstream failed, Rust succeeded; PANIC = Rust panic/timeout.

## map-data (stdout)

| fixture | input | eval | yaml | json | jsonl | tsv | csv |
|---|---|---|---|---|---|---|---|
| fair_mappings_metadata/linkmlmap-to-fair.transformation.yaml | data/sample-linkmlmap-spec.yaml | restricted | ok | ok | ok | ok | ok |
| fair_mappings_metadata/linkmlmap-to-fair.transformation.yaml | data/sample-linkmlmap-spec.yaml | unrestricted | ok | ok | ok | ok | ok |
| flattening/denormalize.transform.yaml | data/MappingSet-001.yaml | restricted | ok | ok | ok | ok | ok |
| flattening/denormalize.transform.yaml | data/MappingSet-001.yaml | unrestricted | ok | ok | ok | ok | ok |
| measurements/qv-to-scalar.transform.yaml | data/PersonQuantityValue-001.yaml | restricted | ok | ok | ok | ok | ok |
| measurements/qv-to-scalar.transform.yaml | data/PersonQuantityValue-001.yaml | unrestricted | ok | ok | ok | ok | ok |
| measurements/qv-to-scalar.transform.yaml | data/PersonQuantityValue-002.yaml | restricted | ok | ok | ok | ok | ok |
| measurements/qv-to-scalar.transform.yaml | data/PersonQuantityValue-002.yaml | unrestricted | ok | ok | ok | ok | ok |
| personinfo_basic/personinfo-to-agent.transform.yaml | data/Container-001.yaml | restricted | E(d) | E(d) | E(d) | E(d) | E(d) |
| personinfo_basic/personinfo-to-agent.transform.yaml | data/Container-001.yaml | unrestricted | ok | UP-ERR | UP-ERR | ok | ok |
| personinfo_basic/personinfo-to-agent.transform.yaml | data/Person-001.yaml | restricted | E(d) | E(d) | E(d) | E(d) | E(d) |
| personinfo_basic/personinfo-to-agent.transform.yaml | data/Person-001.yaml | unrestricted | ok | UP-ERR | UP-ERR | ok | ok |
| single_value_for_multivalued/greetings-to-languages.transform.yaml | data/Container-001.yaml | restricted | ok | ok | ok | ok | ok |
| single_value_for_multivalued/greetings-to-languages.transform.yaml | data/Container-001.yaml | unrestricted | ok | ok | ok | ok | ok |
| tabular/person_to_agent.transform.yaml | data/Person.csv | restricted | ok | ok | ok | ok | ok |
| tabular/person_to_agent.transform.yaml | data/Person.csv | unrestricted | ok | ok | ok | ok | ok |
| tabular/person_to_agent.transform.yaml | data/Person.tsv | restricted | ok | ok | ok | ok | ok |
| tabular/person_to_agent.transform.yaml | data/Person.tsv | unrestricted | ok | ok | ok | ok | ok |
| tabular/person_to_agent.transform.yaml | data/ (directory) | restricted | ok | ok | ok | ok | ok |
| tabular/person_to_agent.transform.yaml | data/ (directory) | unrestricted | ok | ok | ok | ok | ok |
| type_coercion/type_coercion.transform.yaml | data/MyRecord-001.yaml | restricted | E(o) | E(o) | E(o) | E(o) | E(o) |
| type_coercion/type_coercion.transform.yaml | data/MyRecord-001.yaml | unrestricted | E(o) | E(o) | E(o) | E(o) | E(o) |
| attic/transform-s1-to-s2.yaml | personinfo-s1-example-data-01.yaml | restricted | ok | ok | ok | ok | ok |
| attic/transform-s1-to-s2.yaml | personinfo-s1-example-data-01.yaml | unrestricted | ok | ok | ok | ok | ok |

## derive-schema / validate-spec

| fixture | command | result |
|---|---|---|
| biolink/biolink-example-profile.transform.yaml | derive-schema | ok |
| biolink/biolink-example-profile.transform.yaml | validate-spec | ok |
| fair_mappings_metadata/linkmlmap-to-fair.transformation.yaml | derive-schema | ok |
| fair_mappings_metadata/linkmlmap-to-fair.transformation.yaml | validate-spec | E(s) |
| flattening/denormalize.transform.yaml | derive-schema | ok |
| flattening/denormalize.transform.yaml | validate-spec | ok |
| measurements/qv-to-scalar.transform.yaml | derive-schema | ok |
| measurements/qv-to-scalar.transform.yaml | validate-spec | ok |
| personinfo_basic/personinfo-to-agent.transform.yaml | derive-schema | ok |
| personinfo_basic/personinfo-to-agent.transform.yaml | validate-spec | ok |
| single_value_for_multivalued/greetings-to-languages.transform.yaml | derive-schema | ok |
| single_value_for_multivalued/greetings-to-languages.transform.yaml | validate-spec | ok |
| tabular/person_to_agent.transform.yaml | derive-schema | ok |
| tabular/person_to_agent.transform.yaml | validate-spec | ok |
| type_coercion/type_coercion.transform.yaml | derive-schema | ok |
| type_coercion/type_coercion.transform.yaml | validate-spec | E(o) |
| attic/transform-s1-to-s2.yaml | derive-schema | UP-ERR |
| attic/transform-s1-to-s2.yaml | validate-spec | E(o) |

## map-data with `-o FILE` (unrestricted eval)

| fixture | input | yaml | json | jsonl | tsv | csv |
|---|---|---|---|---|---|---|
| fair_mappings_metadata/linkmlmap-to-fair.transformation.yaml | data/sample-linkmlmap-spec.yaml | ok | ok | ok | ok | ok |
| flattening/denormalize.transform.yaml | data/MappingSet-001.yaml | ok | ok | ok | ok | ok |
| measurements/qv-to-scalar.transform.yaml | data/PersonQuantityValue-001.yaml | ok | ok | ok | ok | ok |
| measurements/qv-to-scalar.transform.yaml | data/PersonQuantityValue-002.yaml | ok | ok | ok | ok | ok |
| personinfo_basic/personinfo-to-agent.transform.yaml | data/Container-001.yaml | ok | UP-ERR | UP-ERR | ok | ok |
| personinfo_basic/personinfo-to-agent.transform.yaml | data/Person-001.yaml | ok | UP-ERR | UP-ERR | ok | ok |
| single_value_for_multivalued/greetings-to-languages.transform.yaml | data/Container-001.yaml | ok | ok | ok | ok | ok |
| tabular/person_to_agent.transform.yaml | data/Person.csv | ok | ok | ok | ok | ok |
| tabular/person_to_agent.transform.yaml | data/Person.tsv | ok | ok | ok | ok | ok |
| tabular/person_to_agent.transform.yaml | data/ (directory) | ok | ok | ok | ok | ok |
| type_coercion/type_coercion.transform.yaml | data/MyRecord-001.yaml | E(o) | E(o) | E(o) | E(o) | E(o) |
| attic/transform-s1-to-s2.yaml | personinfo-s1-example-data-01.yaml | ok | ok | ok | ok | ok |

## Totals

both_error: 28, match: 161, upstream_error: 9

## Divergence detail

### personinfo_basic/personinfo-to-agent.transform.yaml data/Container-001.yaml unrestricted json [upstream_error]

upstream rc=1 TypeError: Object of type date is not JSON serializable

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/personinfo_basic/data/Container-001.yaml -> - (spec: /home/user/upstream/linkml-map/tests/input/examples/personinfo_basic/transform/personinfo-to-agent.transform.yaml)
rows_in=1 rows_out=1 elapsed=0.008s throughput=133 rows/s


### personinfo_basic/personinfo-to-agent.transform.yaml data/Container-001.yaml unrestricted jsonl [upstream_error]

upstream rc=1 TypeError: Object of type date is not JSON serializable

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/personinfo_basic/data/Container-001.yaml -> - (spec: /home/user/upstream/linkml-map/tests/input/examples/personinfo_basic/transform/personinfo-to-agent.transform.yaml)
rows_in=1 rows_out=1 elapsed=0.007s throughput=143 rows/s


### personinfo_basic/personinfo-to-agent.transform.yaml data/Person-001.yaml unrestricted json [upstream_error]

upstream rc=1 TypeError: Object of type date is not JSON serializable

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/personinfo_basic/data/Person-001.yaml -> - (spec: /home/user/upstream/linkml-map/tests/input/examples/personinfo_basic/transform/personinfo-to-agent.transform.yaml)
rows_in=1 rows_out=1 elapsed=0.007s throughput=136 rows/s


### personinfo_basic/personinfo-to-agent.transform.yaml data/Person-001.yaml unrestricted jsonl [upstream_error]

upstream rc=1 TypeError: Object of type date is not JSON serializable

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/personinfo_basic/data/Person-001.yaml -> - (spec: /home/user/upstream/linkml-map/tests/input/examples/personinfo_basic/transform/personinfo-to-agent.transform.yaml)
rows_in=1 rows_out=1 elapsed=0.007s throughput=147 rows/s


### attic/transform-s1-to-s2.yaml derive-schema [upstream_error]

upstream rc=1 AttributeError: 'NoneType' object has no attribute 'values'

rust rc=0: WARN: No such class Entity
WARN: No such class Job
WARN: No such class SequenceFeature
WARN: No such class Mapping


### personinfo_basic/personinfo-to-agent.transform.yaml data/Container-001.yaml -o json [upstream_error]

upstream rc=1 TypeError: Object of type date is not JSON serializable

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/personinfo_basic/data/Container-001.yaml -> /tmp/tmp1csogzuu/rs.json (spec: /home/user/upstream/linkml-map/tests/input/examples/personinfo_basic/transform/personinfo-to-agent.transform.yaml)
rows_in=1 rows_out=1 elapsed=0.008s throughput=120 rows/s


### personinfo_basic/personinfo-to-agent.transform.yaml data/Container-001.yaml -o jsonl [upstream_error]

upstream rc=1 TypeError: Object of type date is not JSON serializable

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/personinfo_basic/data/Container-001.yaml -> /tmp/tmp5xu7a0x1/rs.jsonl (spec: /home/user/upstream/linkml-map/tests/input/examples/personinfo_basic/transform/personinfo-to-agent.transform.yaml)
rows_in=1 rows_out=1 elapsed=0.008s throughput=125 rows/s


### personinfo_basic/personinfo-to-agent.transform.yaml data/Person-001.yaml -o json [upstream_error]

upstream rc=1 TypeError: Object of type date is not JSON serializable

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/personinfo_basic/data/Person-001.yaml -> /tmp/tmpryp4cb4r/rs.json (spec: /home/user/upstream/linkml-map/tests/input/examples/personinfo_basic/transform/personinfo-to-agent.transform.yaml)
rows_in=1 rows_out=1 elapsed=0.010s throughput=101 rows/s


### personinfo_basic/personinfo-to-agent.transform.yaml data/Person-001.yaml -o jsonl [upstream_error]

upstream rc=1 TypeError: Object of type date is not JSON serializable

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/personinfo_basic/data/Person-001.yaml -> /tmp/tmpsjoz0ip1/rs.jsonl (spec: /home/user/upstream/linkml-map/tests/input/examples/personinfo_basic/transform/personinfo-to-agent.transform.yaml)
rows_in=1 rows_out=1 elapsed=0.011s throughput=92 rows/s


## Cases captured from upstream's own test-suite

230 distinct map_object invocations x 5 formats: both_error: 155, match: 995

| case | origin test | divergent formats |
|---|---|---|
