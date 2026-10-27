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
| type_coercion/type_coercion.transform.yaml | data/MyRecord-001.yaml | restricted | UP-ERR | UP-ERR | UP-ERR | UP-ERR | UP-ERR |
| type_coercion/type_coercion.transform.yaml | data/MyRecord-001.yaml | unrestricted | UP-ERR | UP-ERR | UP-ERR | UP-ERR | UP-ERR |
| attic/transform-s1-to-s2.yaml | personinfo-s1-example-data-01.yaml | restricted | RS-ERR | RS-ERR | RS-ERR | RS-ERR | RS-ERR |
| attic/transform-s1-to-s2.yaml | personinfo-s1-example-data-01.yaml | unrestricted | RS-ERR | RS-ERR | RS-ERR | RS-ERR | RS-ERR |

## derive-schema / validate-spec

| fixture | command | result |
|---|---|---|
| biolink/biolink-example-profile.transform.yaml | derive-schema | ok |
| biolink/biolink-example-profile.transform.yaml | validate-spec | DIFF |
| fair_mappings_metadata/linkmlmap-to-fair.transformation.yaml | derive-schema | ok |
| fair_mappings_metadata/linkmlmap-to-fair.transformation.yaml | validate-spec | E(d) |
| flattening/denormalize.transform.yaml | derive-schema | ok |
| flattening/denormalize.transform.yaml | validate-spec | DIFF |
| measurements/qv-to-scalar.transform.yaml | derive-schema | ok |
| measurements/qv-to-scalar.transform.yaml | validate-spec | DIFF |
| personinfo_basic/personinfo-to-agent.transform.yaml | derive-schema | ok |
| personinfo_basic/personinfo-to-agent.transform.yaml | validate-spec | DIFF |
| single_value_for_multivalued/greetings-to-languages.transform.yaml | derive-schema | ok |
| single_value_for_multivalued/greetings-to-languages.transform.yaml | validate-spec | DIFF |
| tabular/person_to_agent.transform.yaml | derive-schema | ok |
| tabular/person_to_agent.transform.yaml | validate-spec | DIFF |
| type_coercion/type_coercion.transform.yaml | derive-schema | ok |
| type_coercion/type_coercion.transform.yaml | validate-spec | E(d) |
| attic/transform-s1-to-s2.yaml | derive-schema | E(d) |
| attic/transform-s1-to-s2.yaml | validate-spec | E(d) |

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
| type_coercion/type_coercion.transform.yaml | data/MyRecord-001.yaml | UP-ERR | UP-ERR | UP-ERR | UP-ERR | UP-ERR |
| attic/transform-s1-to-s2.yaml | personinfo-s1-example-data-01.yaml | RS-ERR | RS-ERR | RS-ERR | RS-ERR | RS-ERR |

## Totals

both_error: 14, match: 140, output_diff: 6, rust_error: 15, upstream_error: 23

## Divergence detail

### personinfo_basic/personinfo-to-agent.transform.yaml data/Container-001.yaml unrestricted json [upstream_error]

upstream rc=1 TypeError: Object of type date is not JSON serializable

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/personinfo_basic/data/Container-001.yaml -> - (spec: /home/user/upstream/linkml-map/tests/input/examples/personinfo_basic/transform/personinfo-to-agent.transform.yaml)
rows_in=1 rows_out=1 elapsed=0.018s throughput=56 rows/s


### personinfo_basic/personinfo-to-agent.transform.yaml data/Container-001.yaml unrestricted jsonl [upstream_error]

upstream rc=1 TypeError: Object of type date is not JSON serializable

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/personinfo_basic/data/Container-001.yaml -> - (spec: /home/user/upstream/linkml-map/tests/input/examples/personinfo_basic/transform/personinfo-to-agent.transform.yaml)
rows_in=1 rows_out=1 elapsed=0.014s throughput=70 rows/s


### personinfo_basic/personinfo-to-agent.transform.yaml data/Person-001.yaml unrestricted json [upstream_error]

upstream rc=1 TypeError: Object of type date is not JSON serializable

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/personinfo_basic/data/Person-001.yaml -> - (spec: /home/user/upstream/linkml-map/tests/input/examples/personinfo_basic/transform/personinfo-to-agent.transform.yaml)
rows_in=1 rows_out=1 elapsed=0.013s throughput=75 rows/s


### personinfo_basic/personinfo-to-agent.transform.yaml data/Person-001.yaml unrestricted jsonl [upstream_error]

upstream rc=1 TypeError: Object of type date is not JSON serializable

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/personinfo_basic/data/Person-001.yaml -> - (spec: /home/user/upstream/linkml-map/tests/input/examples/personinfo_basic/transform/personinfo-to-agent.transform.yaml)
rows_in=1 rows_out=1 elapsed=0.010s throughput=100 rows/s


### type_coercion/type_coercion.transform.yaml data/MyRecord-001.yaml restricted yaml [upstream_error]

upstream rc=1 TransformationError: No such slot id_as_url as an attribute of MyRecord ancestors or as a slot definition in the schema; class_derivation=MyRecord (from MyRecord); slot_derivation=id_as_url (from id)

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/data/MyRecord-001.yaml -> - (spec: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/transform/type_coercion.transform.yaml)
rows_in=1 rows_out=1 elapsed=0.002s throughput=443 rows/s


### type_coercion/type_coercion.transform.yaml data/MyRecord-001.yaml restricted json [upstream_error]

upstream rc=1 TransformationError: No such slot id_as_url as an attribute of MyRecord ancestors or as a slot definition in the schema; class_derivation=MyRecord (from MyRecord); slot_derivation=id_as_url (from id)

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/data/MyRecord-001.yaml -> - (spec: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/transform/type_coercion.transform.yaml)
rows_in=1 rows_out=1 elapsed=0.002s throughput=442 rows/s


### type_coercion/type_coercion.transform.yaml data/MyRecord-001.yaml restricted jsonl [upstream_error]

upstream rc=1 TransformationError: No such slot id_as_url as an attribute of MyRecord ancestors or as a slot definition in the schema; class_derivation=MyRecord (from MyRecord); slot_derivation=id_as_url (from id)

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/data/MyRecord-001.yaml -> - (spec: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/transform/type_coercion.transform.yaml)
rows_in=1 rows_out=1 elapsed=0.003s throughput=366 rows/s


### type_coercion/type_coercion.transform.yaml data/MyRecord-001.yaml restricted tsv [upstream_error]

upstream rc=1 TransformationError: No such slot id_as_url as an attribute of MyRecord ancestors or as a slot definition in the schema; class_derivation=MyRecord (from MyRecord); slot_derivation=id_as_url (from id)

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/data/MyRecord-001.yaml -> - (spec: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/transform/type_coercion.transform.yaml)
rows_in=1 rows_out=1 elapsed=0.002s throughput=429 rows/s


### type_coercion/type_coercion.transform.yaml data/MyRecord-001.yaml restricted csv [upstream_error]

upstream rc=1 TransformationError: No such slot id_as_url as an attribute of MyRecord ancestors or as a slot definition in the schema; class_derivation=MyRecord (from MyRecord); slot_derivation=id_as_url (from id)

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/data/MyRecord-001.yaml -> - (spec: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/transform/type_coercion.transform.yaml)
rows_in=1 rows_out=1 elapsed=0.002s throughput=450 rows/s


### type_coercion/type_coercion.transform.yaml data/MyRecord-001.yaml unrestricted yaml [upstream_error]

upstream rc=1 TransformationError: No such slot id_as_url as an attribute of MyRecord ancestors or as a slot definition in the schema; class_derivation=MyRecord (from MyRecord); slot_derivation=id_as_url (from id)

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/data/MyRecord-001.yaml -> - (spec: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/transform/type_coercion.transform.yaml)
rows_in=1 rows_out=1 elapsed=0.002s throughput=441 rows/s


### type_coercion/type_coercion.transform.yaml data/MyRecord-001.yaml unrestricted json [upstream_error]

upstream rc=1 TransformationError: No such slot id_as_url as an attribute of MyRecord ancestors or as a slot definition in the schema; class_derivation=MyRecord (from MyRecord); slot_derivation=id_as_url (from id)

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/data/MyRecord-001.yaml -> - (spec: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/transform/type_coercion.transform.yaml)
rows_in=1 rows_out=1 elapsed=0.002s throughput=479 rows/s


### type_coercion/type_coercion.transform.yaml data/MyRecord-001.yaml unrestricted jsonl [upstream_error]

upstream rc=1 TransformationError: No such slot id_as_url as an attribute of MyRecord ancestors or as a slot definition in the schema; class_derivation=MyRecord (from MyRecord); slot_derivation=id_as_url (from id)

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/data/MyRecord-001.yaml -> - (spec: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/transform/type_coercion.transform.yaml)
rows_in=1 rows_out=1 elapsed=0.002s throughput=479 rows/s


### type_coercion/type_coercion.transform.yaml data/MyRecord-001.yaml unrestricted tsv [upstream_error]

upstream rc=1 TransformationError: No such slot id_as_url as an attribute of MyRecord ancestors or as a slot definition in the schema; class_derivation=MyRecord (from MyRecord); slot_derivation=id_as_url (from id)

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/data/MyRecord-001.yaml -> - (spec: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/transform/type_coercion.transform.yaml)
rows_in=1 rows_out=1 elapsed=0.002s throughput=503 rows/s


### type_coercion/type_coercion.transform.yaml data/MyRecord-001.yaml unrestricted csv [upstream_error]

upstream rc=1 TransformationError: No such slot id_as_url as an attribute of MyRecord ancestors or as a slot definition in the schema; class_derivation=MyRecord (from MyRecord); slot_derivation=id_as_url (from id)

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/data/MyRecord-001.yaml -> - (spec: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/transform/type_coercion.transform.yaml)
rows_in=1 rows_out=1 elapsed=0.002s throughput=453 rows/s


### attic/transform-s1-to-s2.yaml personinfo-s1-example-data-01.yaml restricted yaml [rust_error]

upstream rc=0 None: 

rust rc=1: compiling plan: loading spec /home/user/upstream/linkml-map/tests/input/attic/transform-s1-to-s2.yaml: parsing spec /home/user/upstream/linkml-map/tests/input/attic/transform-s1-to-s2.yaml: missing field `element_name`

### attic/transform-s1-to-s2.yaml personinfo-s1-example-data-01.yaml restricted json [rust_error]

upstream rc=0 None: 

rust rc=1: compiling plan: loading spec /home/user/upstream/linkml-map/tests/input/attic/transform-s1-to-s2.yaml: parsing spec /home/user/upstream/linkml-map/tests/input/attic/transform-s1-to-s2.yaml: missing field `element_name`

### attic/transform-s1-to-s2.yaml personinfo-s1-example-data-01.yaml restricted jsonl [rust_error]

upstream rc=0 None: 

rust rc=1: compiling plan: loading spec /home/user/upstream/linkml-map/tests/input/attic/transform-s1-to-s2.yaml: parsing spec /home/user/upstream/linkml-map/tests/input/attic/transform-s1-to-s2.yaml: missing field `element_name`

### attic/transform-s1-to-s2.yaml personinfo-s1-example-data-01.yaml restricted tsv [rust_error]

upstream rc=0 None: 

rust rc=1: compiling plan: loading spec /home/user/upstream/linkml-map/tests/input/attic/transform-s1-to-s2.yaml: parsing spec /home/user/upstream/linkml-map/tests/input/attic/transform-s1-to-s2.yaml: missing field `element_name`

### attic/transform-s1-to-s2.yaml personinfo-s1-example-data-01.yaml restricted csv [rust_error]

upstream rc=0 None: 

rust rc=1: compiling plan: loading spec /home/user/upstream/linkml-map/tests/input/attic/transform-s1-to-s2.yaml: parsing spec /home/user/upstream/linkml-map/tests/input/attic/transform-s1-to-s2.yaml: missing field `element_name`

### attic/transform-s1-to-s2.yaml personinfo-s1-example-data-01.yaml unrestricted yaml [rust_error]

upstream rc=0 None: 

rust rc=1: compiling plan: loading spec /home/user/upstream/linkml-map/tests/input/attic/transform-s1-to-s2.yaml: parsing spec /home/user/upstream/linkml-map/tests/input/attic/transform-s1-to-s2.yaml: missing field `element_name`

### attic/transform-s1-to-s2.yaml personinfo-s1-example-data-01.yaml unrestricted json [rust_error]

upstream rc=0 None: 

rust rc=1: compiling plan: loading spec /home/user/upstream/linkml-map/tests/input/attic/transform-s1-to-s2.yaml: parsing spec /home/user/upstream/linkml-map/tests/input/attic/transform-s1-to-s2.yaml: missing field `element_name`

### attic/transform-s1-to-s2.yaml personinfo-s1-example-data-01.yaml unrestricted jsonl [rust_error]

upstream rc=0 None: 

rust rc=1: compiling plan: loading spec /home/user/upstream/linkml-map/tests/input/attic/transform-s1-to-s2.yaml: parsing spec /home/user/upstream/linkml-map/tests/input/attic/transform-s1-to-s2.yaml: missing field `element_name`

### attic/transform-s1-to-s2.yaml personinfo-s1-example-data-01.yaml unrestricted tsv [rust_error]

upstream rc=0 None: 

rust rc=1: compiling plan: loading spec /home/user/upstream/linkml-map/tests/input/attic/transform-s1-to-s2.yaml: parsing spec /home/user/upstream/linkml-map/tests/input/attic/transform-s1-to-s2.yaml: missing field `element_name`

### attic/transform-s1-to-s2.yaml personinfo-s1-example-data-01.yaml unrestricted csv [rust_error]

upstream rc=0 None: 

rust rc=1: compiling plan: loading spec /home/user/upstream/linkml-map/tests/input/attic/transform-s1-to-s2.yaml: parsing spec /home/user/upstream/linkml-map/tests/input/attic/transform-s1-to-s2.yaml: missing field `element_name`

### biolink/biolink-example-profile.transform.yaml validate-spec [output_diff]

```diff
--- upstream
+++ rust
@@ -1 +0,0 @@
-/home/user/upstream/linkml-map/tests/input/examples/biolink/transform/biolink-example-profile.transform.yaml: ok
```

### flattening/denormalize.transform.yaml validate-spec [output_diff]

```diff
--- upstream
+++ rust
@@ -1 +0,0 @@
-/home/user/upstream/linkml-map/tests/input/examples/flattening/transform/denormalize.transform.yaml: ok
```

### measurements/qv-to-scalar.transform.yaml validate-spec [output_diff]

```diff
--- upstream
+++ rust
@@ -1 +0,0 @@
-/home/user/upstream/linkml-map/tests/input/examples/measurements/transform/qv-to-scalar.transform.yaml: ok
```

### personinfo_basic/personinfo-to-agent.transform.yaml validate-spec [output_diff]

```diff
--- upstream
+++ rust
@@ -1 +0,0 @@
-/home/user/upstream/linkml-map/tests/input/examples/personinfo_basic/transform/personinfo-to-agent.transform.yaml: ok
```

### single_value_for_multivalued/greetings-to-languages.transform.yaml validate-spec [output_diff]

```diff
--- upstream
+++ rust
@@ -1 +0,0 @@
-/home/user/upstream/linkml-map/tests/input/examples/single_value_for_multivalued/transform/greetings-to-languages.transform.yaml: ok
```

### tabular/person_to_agent.transform.yaml validate-spec [output_diff]

```diff
--- upstream
+++ rust
@@ -1 +0,0 @@
-/home/user/upstream/linkml-map/tests/input/examples/tabular/transform/person_to_agent.transform.yaml: ok
```

### personinfo_basic/personinfo-to-agent.transform.yaml data/Container-001.yaml -o json [upstream_error]

upstream rc=1 TypeError: Object of type date is not JSON serializable

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/personinfo_basic/data/Container-001.yaml -> /tmp/tmpn30h8crl/rs.json (spec: /home/user/upstream/linkml-map/tests/input/examples/personinfo_basic/transform/personinfo-to-agent.transform.yaml)
rows_in=1 rows_out=1 elapsed=0.017

### personinfo_basic/personinfo-to-agent.transform.yaml data/Container-001.yaml -o jsonl [upstream_error]

upstream rc=1 TypeError: Object of type date is not JSON serializable

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/personinfo_basic/data/Container-001.yaml -> /tmp/tmp0ft34era/rs.jsonl (spec: /home/user/upstream/linkml-map/tests/input/examples/personinfo_basic/transform/personinfo-to-agent.transform.yaml)
rows_in=1 rows_out=1 elapsed=0.01

### personinfo_basic/personinfo-to-agent.transform.yaml data/Person-001.yaml -o json [upstream_error]

upstream rc=1 TypeError: Object of type date is not JSON serializable

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/personinfo_basic/data/Person-001.yaml -> /tmp/tmp1payjwqp/rs.json (spec: /home/user/upstream/linkml-map/tests/input/examples/personinfo_basic/transform/personinfo-to-agent.transform.yaml)
rows_in=1 rows_out=1 elapsed=0.010s t

### personinfo_basic/personinfo-to-agent.transform.yaml data/Person-001.yaml -o jsonl [upstream_error]

upstream rc=1 TypeError: Object of type date is not JSON serializable

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/personinfo_basic/data/Person-001.yaml -> /tmp/tmp95x_le8q/rs.jsonl (spec: /home/user/upstream/linkml-map/tests/input/examples/personinfo_basic/transform/personinfo-to-agent.transform.yaml)
rows_in=1 rows_out=1 elapsed=0.011s 

### type_coercion/type_coercion.transform.yaml data/MyRecord-001.yaml -o yaml [upstream_error]

upstream rc=1 TransformationError: No such slot id_as_url as an attribute of MyRecord ancestors or as a slot definition in the schema; class_derivation=MyRecord (from MyRecord); slot_derivation=id_as_url (from id)

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/data/MyRecord-001.yaml -> /tmp/tmp4qtdgj3b/rs.yaml (spec: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/transform/type_coercion.transform.yaml)
rows_in=1 rows_out=1 elapsed=0.002s throughput=

### type_coercion/type_coercion.transform.yaml data/MyRecord-001.yaml -o json [upstream_error]

upstream rc=1 TransformationError: No such slot id_as_url as an attribute of MyRecord ancestors or as a slot definition in the schema; class_derivation=MyRecord (from MyRecord); slot_derivation=id_as_url (from id)

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/data/MyRecord-001.yaml -> /tmp/tmp7bxsvc5_/rs.json (spec: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/transform/type_coercion.transform.yaml)
rows_in=1 rows_out=1 elapsed=0.002s throughput=

### type_coercion/type_coercion.transform.yaml data/MyRecord-001.yaml -o jsonl [upstream_error]

upstream rc=1 TransformationError: No such slot id_as_url as an attribute of MyRecord ancestors or as a slot definition in the schema; class_derivation=MyRecord (from MyRecord); slot_derivation=id_as_url (from id)

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/data/MyRecord-001.yaml -> /tmp/tmpwg2gm42d/rs.jsonl (spec: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/transform/type_coercion.transform.yaml)
rows_in=1 rows_out=1 elapsed=0.003s throughput

### type_coercion/type_coercion.transform.yaml data/MyRecord-001.yaml -o tsv [upstream_error]

upstream rc=1 TransformationError: No such slot id_as_url as an attribute of MyRecord ancestors or as a slot definition in the schema; class_derivation=MyRecord (from MyRecord); slot_derivation=id_as_url (from id)

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/data/MyRecord-001.yaml -> /tmp/tmpgb7zukqx/rs.tsv (spec: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/transform/type_coercion.transform.yaml)
rows_in=1 rows_out=1 elapsed=0.003s throughput=3

### type_coercion/type_coercion.transform.yaml data/MyRecord-001.yaml -o csv [upstream_error]

upstream rc=1 TransformationError: No such slot id_as_url as an attribute of MyRecord ancestors or as a slot definition in the schema; class_derivation=MyRecord (from MyRecord); slot_derivation=id_as_url (from id)

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/data/MyRecord-001.yaml -> /tmp/tmp82ylt7e_/rs.csv (spec: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/transform/type_coercion.transform.yaml)
rows_in=1 rows_out=1 elapsed=0.002s throughput=4

### attic/transform-s1-to-s2.yaml personinfo-s1-example-data-01.yaml -o yaml [rust_error]

upstream rc=0 None: 

rust rc=1: compiling plan: loading spec /home/user/upstream/linkml-map/tests/input/attic/transform-s1-to-s2.yaml: parsing spec /home/user/upstream/linkml-map/tests/input/attic/transform-s1-to-s2.yaml: missing field `element_name`

### attic/transform-s1-to-s2.yaml personinfo-s1-example-data-01.yaml -o json [rust_error]

upstream rc=0 None: 

rust rc=1: compiling plan: loading spec /home/user/upstream/linkml-map/tests/input/attic/transform-s1-to-s2.yaml: parsing spec /home/user/upstream/linkml-map/tests/input/attic/transform-s1-to-s2.yaml: missing field `element_name`

### attic/transform-s1-to-s2.yaml personinfo-s1-example-data-01.yaml -o jsonl [rust_error]

upstream rc=0 None: 

rust rc=1: compiling plan: loading spec /home/user/upstream/linkml-map/tests/input/attic/transform-s1-to-s2.yaml: parsing spec /home/user/upstream/linkml-map/tests/input/attic/transform-s1-to-s2.yaml: missing field `element_name`

### attic/transform-s1-to-s2.yaml personinfo-s1-example-data-01.yaml -o tsv [rust_error]

upstream rc=0 None: 

rust rc=1: compiling plan: loading spec /home/user/upstream/linkml-map/tests/input/attic/transform-s1-to-s2.yaml: parsing spec /home/user/upstream/linkml-map/tests/input/attic/transform-s1-to-s2.yaml: missing field `element_name`

### attic/transform-s1-to-s2.yaml personinfo-s1-example-data-01.yaml -o csv [rust_error]

upstream rc=0 None: 

rust rc=1: compiling plan: loading spec /home/user/upstream/linkml-map/tests/input/attic/transform-s1-to-s2.yaml: parsing spec /home/user/upstream/linkml-map/tests/input/attic/transform-s1-to-s2.yaml: missing field `element_name`

## Cases captured from upstream's own test-suite

224 distinct map_object invocations x 5 formats: both_error: 155, match: 885, output_diff: 50, rust_error: 30

| case | origin test | divergent formats |
|---|---|---|
| 0759dc334566 | test_transformer/test_multi_enum.py::test_enum_expr_unrestricted_fallback | yaml:DIFF json:DIFF jsonl:DIFF tsv:DIFF csv:DIFF |
| 2a3e4922c2fd | test_transformer/test_range_override.py::test_parse_expr_null_input_yields_none | yaml:RS-ERR json:RS-ERR jsonl:RS-ERR tsv:RS-ERR csv:RS-ERR |
| 3cb7c04887b5 | test_transformer/test_pivot_operation.py::TestEdgeCases::test_unmelt_missing_variable | yaml:DIFF json:DIFF jsonl:DIFF tsv:DIFF csv:DIFF |
| 42175371c7cd | test_transformer/test_expr_strict_mode.py::test_schema_slot_absent_from_row_resolves_to_none[True] | yaml:RS-ERR json:RS-ERR jsonl:RS-ERR tsv:RS-ERR csv:RS-ERR |
| 4d8e0addb392 | test_transformer/test_object_transformer.py::test_offset_reverse_subtraction | yaml:DIFF json:DIFF jsonl:DIFF tsv:DIFF csv:DIFF |
| 53323ba2d2c2 | test_transformer/test_pivot_operation.py::TestEdgeCases::test_unmelt_missing_value | yaml:DIFF json:DIFF jsonl:DIFF tsv:DIFF csv:DIFF |
| 612b35fd2571 | test_transformer/test_pivot_operation.py::TestEdgeCases::test_melt_null_values_skipped | yaml:DIFF json:DIFF jsonl:DIFF tsv:DIFF csv:DIFF |
| 624bf4746c9c | test_transformer/test_object_transformer.py::test_offset_applied_addition | yaml:DIFF json:DIFF jsonl:DIFF tsv:DIFF csv:DIFF |
| 74055f70a60c | test_transformer/test_flatten_inlined.py::test_flatten_two_level_dotpath_null_propagation[env_broad_scale1-middle | yaml:RS-ERR json:RS-ERR jsonl:RS-ERR tsv:RS-ERR csv:RS-ERR |
| 8ba729cc95cd | test_transformer/test_multi_enum.py::test_enum_expr_restricted_evaluates | yaml:DIFF json:DIFF jsonl:DIFF tsv:DIFF csv:DIFF |
| a8c78e8ca56d | test_transformer/test_slot_derivation_features.py::test_slot_fn_with_unrestricted_eval | yaml:RS-ERR json:RS-ERR jsonl:RS-ERR tsv:RS-ERR csv:RS-ERR |
| b18ac8678d14 | test_transformer/test_range_override.py::test_parse_string_into_object | yaml:RS-ERR json:RS-ERR jsonl:RS-ERR tsv:RS-ERR csv:RS-ERR |
| ed14d3b728ab | test_transformer/test_pivot_operation.py::TestMeltBasic::test_melt_with_id_slots | yaml:DIFF json:DIFF jsonl:DIFF tsv:DIFF csv:DIFF |
| ed15ebb53262 | test_transformer/test_object_transformer.py::test_nested_class_derivations | yaml:DIFF json:DIFF jsonl:DIFF tsv:DIFF csv:DIFF |
| f2ba6e0e0ae9 | test_transformer/test_pivot_operation.py::TestMeltBasic::test_melt_basic | yaml:DIFF json:DIFF jsonl:DIFF tsv:DIFF csv:DIFF |
| fec9877c167d | test_transformer/test_flatten_inlined.py::test_flatten_partial_quantity_value | yaml:RS-ERR json:RS-ERR jsonl:RS-ERR tsv:RS-ERR csv:RS-ERR |
