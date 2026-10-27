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
| personinfo_basic/personinfo-to-agent.transform.yaml | data/Container-001.yaml | unrestricted | DIFF | UP-ERR | UP-ERR | DIFF | DIFF |
| personinfo_basic/personinfo-to-agent.transform.yaml | data/Person-001.yaml | restricted | E(d) | E(d) | E(d) | E(d) | E(d) |
| personinfo_basic/personinfo-to-agent.transform.yaml | data/Person-001.yaml | unrestricted | DIFF | UP-ERR | UP-ERR | ok | ok |
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
| biolink/biolink-example-profile.transform.yaml | derive-schema | DIFF |
| biolink/biolink-example-profile.transform.yaml | validate-spec | DIFF |
| fair_mappings_metadata/linkmlmap-to-fair.transformation.yaml | derive-schema | DIFF |
| fair_mappings_metadata/linkmlmap-to-fair.transformation.yaml | validate-spec | E(d) |
| flattening/denormalize.transform.yaml | derive-schema | DIFF |
| flattening/denormalize.transform.yaml | validate-spec | DIFF |
| measurements/qv-to-scalar.transform.yaml | derive-schema | DIFF |
| measurements/qv-to-scalar.transform.yaml | validate-spec | DIFF |
| personinfo_basic/personinfo-to-agent.transform.yaml | derive-schema | DIFF |
| personinfo_basic/personinfo-to-agent.transform.yaml | validate-spec | DIFF |
| single_value_for_multivalued/greetings-to-languages.transform.yaml | derive-schema | DIFF |
| single_value_for_multivalued/greetings-to-languages.transform.yaml | validate-spec | DIFF |
| tabular/person_to_agent.transform.yaml | derive-schema | DIFF |
| tabular/person_to_agent.transform.yaml | validate-spec | DIFF |
| type_coercion/type_coercion.transform.yaml | derive-schema | DIFF |
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
| personinfo_basic/personinfo-to-agent.transform.yaml | data/Container-001.yaml | DIFF | UP-ERR | UP-ERR | DIFF | DIFF |
| personinfo_basic/personinfo-to-agent.transform.yaml | data/Person-001.yaml | DIFF | UP-ERR | UP-ERR | ok | ok |
| single_value_for_multivalued/greetings-to-languages.transform.yaml | data/Container-001.yaml | ok | ok | ok | ok | ok |
| tabular/person_to_agent.transform.yaml | data/Person.csv | ok | ok | ok | ok | ok |
| tabular/person_to_agent.transform.yaml | data/Person.tsv | ok | ok | ok | ok | ok |
| tabular/person_to_agent.transform.yaml | data/ (directory) | ok | ok | ok | ok | ok |
| type_coercion/type_coercion.transform.yaml | data/MyRecord-001.yaml | UP-ERR | UP-ERR | UP-ERR | UP-ERR | UP-ERR |
| attic/transform-s1-to-s2.yaml | personinfo-s1-example-data-01.yaml | RS-ERR | RS-ERR | RS-ERR | RS-ERR | RS-ERR |

## Totals

both_error: 14, match: 124, output_diff: 22, rust_error: 15, upstream_error: 23

## Divergence detail

### personinfo_basic/personinfo-to-agent.transform.yaml data/Container-001.yaml unrestricted yaml [output_diff]

```diff
--- upstream
+++ rust
@@ -5,4 +5,4 @@
   primary_email: fred.bloggs@example.com
-  driving_since: 2014-03-31
-  first_known_event: 1990-01-01
+  driving_since: '2014-03-31'
+  first_known_event: '1990-01-01'
   current_address:
```

### personinfo_basic/personinfo-to-agent.transform.yaml data/Container-001.yaml unrestricted json [upstream_error]

upstream rc=1 TypeError: Object of type date is not JSON serializable

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/personinfo_basic/data/Container-001.yaml -> - (spec: /home/user/upstream/linkml-map/tests/input/examples/personinfo_basic/transform/personinfo-to-agent.transform.yaml)
rows_in=1 rows_out=1 elapsed=0.027s throughput=37 rows/s


### personinfo_basic/personinfo-to-agent.transform.yaml data/Container-001.yaml unrestricted jsonl [upstream_error]

upstream rc=1 TypeError: Object of type date is not JSON serializable

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/personinfo_basic/data/Container-001.yaml -> - (spec: /home/user/upstream/linkml-map/tests/input/examples/personinfo_basic/transform/personinfo-to-agent.transform.yaml)
rows_in=1 rows_out=1 elapsed=0.068s throughput=15 rows/s


### personinfo_basic/personinfo-to-agent.transform.yaml data/Container-001.yaml unrestricted tsv [output_diff]

```diff
--- upstream
+++ rust
@@ -1,2 +1,2 @@
 agents
-[{'id': 'P:001', 'label': 'fred bloggs', 'age': '33 years', 'primary_email': 'fred.bloggs@example.com', 'secondary_email': None, 'gender': None, 'driving_since': datetime.date(2014, 3, 31), 'first_known_event': datetime.date(1990, 1, 1), 'death_date': None, 'current_address': {'address_of': 'foo', 'street': '1 oak street', 'city': 'oaktown'}, 'has_familial_relationships': [{'type': 'SIBLING_OF', 'related_to': 'P:002'}, {'type': 'CHILD_OF', 'related_to': 'P:003'}]}, {'id': 'P:002', 'label': 'Alison Wu', 'age': None, 'primary_email': None, 'secondary_email': None, 'gender': None, 'driving_since': None, 'first_known_event': None, 'death_date': None, 'current_address': None, 'has_familial_relationships': [{'type': 'SIBLING_OF', 'related_to': 'P:001'}, {'type': 'CHILD_OF', 'related_to': 'P:003'}]}, {'id': 'P:003', 'label': 'Mr. Blobby', 'age': None, 'primary_email': None, 'secondary_email': None, 'gender': None, 'driving_since': None, 'first_known_event': None, 'death_date': None, 'current_address': None, 'has_familial_relationships': [{'type': 'PARENT_OF', 'related_to': 'P:001'}, {'type': 'PARENT_OF', 'related_to': 'P:002'}]}]
+[{'id': 'P:001', 'label': 'fred bloggs', 'age': '33 years', 'primary_email': 'fred.bloggs@example.com', 'secondary_email': None, 'gender': None, 'driving_since': '2014-03-31', 'first_known_event': '1990-01-01', 'death_date': None, 'current_address': {'address_of': 'foo', 'street': '1 oak street', 'city': 'oaktown'}, 'has_familial_relationships': [{'type': 'SIBLING_OF', 'related_to': 'P:002'}, {'type': 'CHILD_OF', 'related_to': 'P:003'}]}, {'id': 'P:002', 'label': 'Alison Wu', 'age': None, 'primary_email': None, 'secondary_email': None, 'gender': None, 'driving_since': None, 'first_known_event': None, 'death_date': None, 'current_address': None, 'has_familial_relationships': [{'type': 'SIBLING_OF', 'related_to': 'P:001'}, {'type': 'CHILD_OF', 'related_to': 'P:003'}]}, {'id': 'P:003', 'label': 'Mr. Blobby', 'age': None, 'primary_email': None, 'secondary_email': None, 'gender': None, 'driving_since': None, 'first_known_event': None, 'death_date': None, 'current_address': None, 'has_familial_relationships': [{'type': 'PARENT_OF', 'related_to': 'P:001'}, {'type': 'PARENT_OF', 'related_to': 'P:002'}]}]
```

### personinfo_basic/personinfo-to-agent.transform.yaml data/Container-001.yaml unrestricted csv [output_diff]

```diff
--- upstream
+++ rust
@@ -1,2 +1,2 @@
 agents
-[{'id': 'P:001', 'label': 'fred bloggs', 'age': '33 years', 'primary_email': 'fred.bloggs@example.com', 'secondary_email': None, 'gender': None, 'driving_since': datetime.date(2014, 3, 31), 'first_known_event': datetime.date(1990, 1, 1), 'death_date': None, 'current_address': {'address_of': 'foo', 'street': '1 oak street', 'city': 'oaktown'}, 'has_familial_relationships': [{'type': 'SIBLING_OF', 'related_to': 'P:002'}, {'type': 'CHILD_OF', 'related_to': 'P:003'}]}, {'id': 'P:002', 'label': 'Alison Wu', 'age': None, 'primary_email': None, 'secondary_email': None, 'gender': None, 'driving_since': None, 'first_known_event': None, 'death_date': None, 'current_address': None, 'has_familial_relationships': [{'type': 'SIBLING_OF', 'related_to': 'P:001'}, {'type': 'CHILD_OF', 'related_to': 'P:003'}]}, {'id': 'P:003', 'label': 'Mr. Blobby', 'age': None, 'primary_email': None, 'secondary_email': None, 'gender': None, 'driving_since': None, 'first_known_event': None, 'death_date': None, 'current_address': None, 'has_familial_relationships': [{'type': 'PARENT_OF', 'related_to': 'P:001'}, {'type': 'PARENT_OF', 'related_to': 'P:002'}]}]
+[{'id': 'P:001', 'label': 'fred bloggs', 'age': '33 years', 'primary_email': 'fred.bloggs@example.com', 'secondary_email': None, 'gender': None, 'driving_since': '2014-03-31', 'first_known_event': '1990-01-01', 'death_date': None, 'current_address': {'address_of': 'foo', 'street': '1 oak street', 'city': 'oaktown'}, 'has_familial_relationships': [{'type': 'SIBLING_OF', 'related_to': 'P:002'}, {'type': 'CHILD_OF', 'related_to': 'P:003'}]}, {'id': 'P:002', 'label': 'Alison Wu', 'age': None, 'primary_email': None, 'secondary_email': None, 'gender': None, 'driving_since': None, 'first_known_event': None, 'death_date': None, 'current_address': None, 'has_familial_relationships': [{'type': 'SIBLING_OF', 'related_to': 'P:001'}, {'type': 'CHILD_OF', 'related_to': 'P:003'}]}, {'id': 'P:003', 'label': 'Mr. Blobby', 'age': None, 'primary_email': None, 'secondary_email': None, 'gender': None, 'driving_since': None, 'first_known_event': None, 'death_date': None, 'current_address': None, 'has_familial_relationships': [{'type': 'PARENT_OF', 'related_to': 'P:001'}, {'type': 'PARENT_OF', 'related_to': 'P:002'}]}]
```

### personinfo_basic/personinfo-to-agent.transform.yaml data/Person-001.yaml unrestricted yaml [output_diff]

```diff
--- upstream
+++ rust
@@ -4,4 +4,4 @@
 primary_email: fred.bloggs@example.com
-driving_since: 2014-03-31
-first_known_event: 1990-01-01
+driving_since: '2014-03-31'
+first_known_event: '1990-01-01'
 current_address:
```

### personinfo_basic/personinfo-to-agent.transform.yaml data/Person-001.yaml unrestricted json [upstream_error]

upstream rc=1 TypeError: Object of type date is not JSON serializable

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/personinfo_basic/data/Person-001.yaml -> - (spec: /home/user/upstream/linkml-map/tests/input/examples/personinfo_basic/transform/personinfo-to-agent.transform.yaml)
rows_in=1 rows_out=1 elapsed=0.057s throughput=17 rows/s


### personinfo_basic/personinfo-to-agent.transform.yaml data/Person-001.yaml unrestricted jsonl [upstream_error]

upstream rc=1 TypeError: Object of type date is not JSON serializable

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/personinfo_basic/data/Person-001.yaml -> - (spec: /home/user/upstream/linkml-map/tests/input/examples/personinfo_basic/transform/personinfo-to-agent.transform.yaml)
rows_in=1 rows_out=1 elapsed=0.043s throughput=23 rows/s


### type_coercion/type_coercion.transform.yaml data/MyRecord-001.yaml restricted yaml [upstream_error]

upstream rc=1 TransformationError: No such slot id_as_url as an attribute of MyRecord ancestors or as a slot definition in the schema; class_derivation=MyRecord (from MyRecord); slot_derivation=id_as_url (from id)

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/data/MyRecord-001.yaml -> - (spec: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/transform/type_coercion.transform.yaml)
rows_in=1 rows_out=1 elapsed=0.008s throughput=121 rows/s


### type_coercion/type_coercion.transform.yaml data/MyRecord-001.yaml restricted json [upstream_error]

upstream rc=1 TransformationError: No such slot id_as_url as an attribute of MyRecord ancestors or as a slot definition in the schema; class_derivation=MyRecord (from MyRecord); slot_derivation=id_as_url (from id)

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/data/MyRecord-001.yaml -> - (spec: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/transform/type_coercion.transform.yaml)
rows_in=1 rows_out=1 elapsed=0.005s throughput=210 rows/s


### type_coercion/type_coercion.transform.yaml data/MyRecord-001.yaml restricted jsonl [upstream_error]

upstream rc=1 TransformationError: No such slot id_as_url as an attribute of MyRecord ancestors or as a slot definition in the schema; class_derivation=MyRecord (from MyRecord); slot_derivation=id_as_url (from id)

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/data/MyRecord-001.yaml -> - (spec: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/transform/type_coercion.transform.yaml)
rows_in=1 rows_out=1 elapsed=0.017s throughput=60 rows/s


### type_coercion/type_coercion.transform.yaml data/MyRecord-001.yaml restricted tsv [upstream_error]

upstream rc=1 TransformationError: No such slot id_as_url as an attribute of MyRecord ancestors or as a slot definition in the schema; class_derivation=MyRecord (from MyRecord); slot_derivation=id_as_url (from id)

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/data/MyRecord-001.yaml -> - (spec: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/transform/type_coercion.transform.yaml)
rows_in=1 rows_out=1 elapsed=0.004s throughput=266 rows/s


### type_coercion/type_coercion.transform.yaml data/MyRecord-001.yaml restricted csv [upstream_error]

upstream rc=1 TransformationError: No such slot id_as_url as an attribute of MyRecord ancestors or as a slot definition in the schema; class_derivation=MyRecord (from MyRecord); slot_derivation=id_as_url (from id)

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/data/MyRecord-001.yaml -> - (spec: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/transform/type_coercion.transform.yaml)
rows_in=1 rows_out=1 elapsed=0.017s throughput=61 rows/s


### type_coercion/type_coercion.transform.yaml data/MyRecord-001.yaml unrestricted yaml [upstream_error]

upstream rc=1 TransformationError: No such slot id_as_url as an attribute of MyRecord ancestors or as a slot definition in the schema; class_derivation=MyRecord (from MyRecord); slot_derivation=id_as_url (from id)

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/data/MyRecord-001.yaml -> - (spec: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/transform/type_coercion.transform.yaml)
rows_in=1 rows_out=1 elapsed=0.002s throughput=475 rows/s


### type_coercion/type_coercion.transform.yaml data/MyRecord-001.yaml unrestricted json [upstream_error]

upstream rc=1 TransformationError: No such slot id_as_url as an attribute of MyRecord ancestors or as a slot definition in the schema; class_derivation=MyRecord (from MyRecord); slot_derivation=id_as_url (from id)

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/data/MyRecord-001.yaml -> - (spec: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/transform/type_coercion.transform.yaml)
rows_in=1 rows_out=1 elapsed=0.002s throughput=489 rows/s


### type_coercion/type_coercion.transform.yaml data/MyRecord-001.yaml unrestricted jsonl [upstream_error]

upstream rc=1 TransformationError: No such slot id_as_url as an attribute of MyRecord ancestors or as a slot definition in the schema; class_derivation=MyRecord (from MyRecord); slot_derivation=id_as_url (from id)

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/data/MyRecord-001.yaml -> - (spec: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/transform/type_coercion.transform.yaml)
rows_in=1 rows_out=1 elapsed=0.005s throughput=192 rows/s


### type_coercion/type_coercion.transform.yaml data/MyRecord-001.yaml unrestricted tsv [upstream_error]

upstream rc=1 TransformationError: No such slot id_as_url as an attribute of MyRecord ancestors or as a slot definition in the schema; class_derivation=MyRecord (from MyRecord); slot_derivation=id_as_url (from id)

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/data/MyRecord-001.yaml -> - (spec: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/transform/type_coercion.transform.yaml)
rows_in=1 rows_out=1 elapsed=0.006s throughput=167 rows/s


### type_coercion/type_coercion.transform.yaml data/MyRecord-001.yaml unrestricted csv [upstream_error]

upstream rc=1 TransformationError: No such slot id_as_url as an attribute of MyRecord ancestors or as a slot definition in the schema; class_derivation=MyRecord (from MyRecord); slot_derivation=id_as_url (from id)

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/data/MyRecord-001.yaml -> - (spec: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/transform/type_coercion.transform.yaml)
rows_in=1 rows_out=1 elapsed=0.055s throughput=18 rows/s


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

### biolink/biolink-example-profile.transform.yaml derive-schema [output_diff]

```diff
--- upstream
+++ rust
@@ -1,383 +1,6 @@
-name: Biolink-Model-derived
-id: https://w3id.org/biolink/biolink-model-derived
-imports:
-- linkml:types
+id: example-biolink-profile
+name: example-biolink-profile
+default_range: string
 prefixes:
-  AGRKB:
-    prefix_prefix: AGRKB
-    prefix_reference: https://www.alliancegenome.org/
-  apollo:
-    prefix_prefix: apollo
-    prefix_reference: https://github.com/GMOD/Apollo
-  AspGD:
-    prefix_prefix: AspGD
-    prefix_reference: http://www.aspergillusgenome.org/cgi-bin/locus.pl?dbid=
-  biolink:
-    prefix_prefix: biolink
-    prefix_reference: https://w3id.org/biolink/vocab/
-  bioschemas:
-    prefix_prefix: bioschemas
-    prefix_reference: https://bioschemas.org/
-  linkml:
-    prefix_prefix: linkml
-    prefix_reference: https://w3id.org/linkml/
-  CAID:
-    prefix_prefix: CAID
-    prefix_reference: http://reg.clinicalgenome.org/redmine/projects/registry/genboree_registry/by_caid?caid=
-  CHADO:
-    prefix_prefix: CHADO
-    prefix_reference: http://gmod.org/wiki/Chado/
-  ChemBank:
-    prefix_prefix: ChemBank
-    prefix_reference: http://chembank.broadinstitute.org/chemistry/viewMolecule.htm?cbid=
-  CHEMBL.MECHANISM:
-    prefix_prefix: CHEMBL.MECHANISM
```

### biolink/biolink-example-profile.transform.yaml validate-spec [output_diff]

```diff
--- upstream
+++ rust
@@ -1 +0,0 @@
-/home/user/upstream/linkml-map/tests/input/examples/biolink/transform/biolink-example-profile.transform.yaml: ok
```

### fair_mappings_metadata/linkmlmap-to-fair.transformation.yaml derive-schema [output_diff]

```diff
--- upstream
+++ rust
@@ -1,31 +1,7 @@
-name: linkml-map-derived
-id: https://w3id.org/linkml/transformer-derived
-imports:
-- linkml:types
+id: https://w3id.org/linkml/transformer/linkml-map-to-fair-mappings
+name: linkml-map-to-fair-mappings
+default_range: string
 prefixes:
-  linkml:
-    prefix_prefix: linkml
-    prefix_reference: https://w3id.org/linkml/
-  linkmlmap:
-    prefix_prefix: linkmlmap
-    prefix_reference: https://w3id.org/linkml/transformer/
-  dcterms:
-    prefix_prefix: dcterms
-    prefix_reference: http://purl.org/dc/terms/
-  schema:
-    prefix_prefix: schema
-    prefix_reference: http://schema.org/
-  rdfs:
-    prefix_prefix: rdfs
-    prefix_reference: http://www.w3.org/2000/01/rdf-schema#
-  sh:
-    prefix_prefix: sh
-    prefix_reference: http://www.w3.org/ns/shacl#
-  STATO:
-    prefix_prefix: STATO
-    prefix_reference: http://purl.obolibrary.org/obo/STATO_
-  sssom:
-    prefix_prefix: sssom
-    prefix_reference: https://w3id.org/sssom/
-default_prefix: https://w3id.org/linkml/transformer-derived/
+  linkmlmap: https://w3id.org/linkml/transformer/
+  fair_mappings_schema: https://w3id.org/mapping-commons/fair-mappings-schema/
 classes:
@@ -33,3 +9,4 @@
```

### flattening/denormalize.transform.yaml derive-schema [output_diff]

```diff
--- upstream
+++ rust
@@ -1,13 +1,3 @@
-name: mappings_norm-derived
-id: https://w3id.org/linkml/examples/mappings-norm-derived
-imports:
-- linkml:types
-prefixes:
-  mappings:
-    prefix_prefix: mappings
-    prefix_reference: https://w3id.org/linkml/examples/mappings-norm/
-  linkml:
-    prefix_prefix: linkml
-    prefix_reference: https://w3id.org/linkml/
-default_prefix: https://w3id.org/linkml/examples/mappings-norm-derived/
+id: denormalize-mapping
+name: denormalize-mapping
 default_range: string
@@ -16,2 +6,3 @@
     name: MappingSet
+    tree_root: true
     attributes:
@@ -22,3 +13,2 @@
         inlined: true
-    tree_root: true
   Mapping:
```

### flattening/denormalize.transform.yaml validate-spec [output_diff]

```diff
--- upstream
+++ rust
@@ -1 +0,0 @@
-/home/user/upstream/linkml-map/tests/input/examples/flattening/transform/denormalize.transform.yaml: ok
```

### measurements/qv-to-scalar.transform.yaml derive-schema [output_diff]

```diff
--- upstream
+++ rust
@@ -1,13 +1,3 @@
-name: quantity_value-derived
-id: https://w3id.org/linkml/examples/quantity_value-derived
-imports:
-- linkml:types
-prefixes:
-  personinfo:
-    prefix_prefix: personinfo
-    prefix_reference: https://w3id.org/linkml/examples/personinfo/
-  linkml:
-    prefix_prefix: linkml
-    prefix_reference: https://w3id.org/linkml/
-default_prefix: https://w3id.org/linkml/examples/quantity_value-derived/
+id: qv-to-scalar
+name: qv-to-scalar
 default_range: string
@@ -16,2 +6,3 @@
     name: Person
+    tree_root: true
     attributes:
@@ -22,2 +13 @@
         name: height_in_cm
-    tree_root: true
```

### measurements/qv-to-scalar.transform.yaml validate-spec [output_diff]

```diff
--- upstream
+++ rust
@@ -1 +0,0 @@
-/home/user/upstream/linkml-map/tests/input/examples/measurements/transform/qv-to-scalar.transform.yaml: ok
```

### personinfo_basic/personinfo-to-agent.transform.yaml derive-schema [output_diff]

```diff
--- upstream
+++ rust
@@ -1,56 +1,6 @@
-name: personinfo-derived
-id: https://w3id.org/linkml/examples/personinfo-derived
-imports:
-- linkml:types
+id: https://w3id.org/linkml/map/example/personinfo-to-agent.transform.yaml
+name: personinfo-to-agent.transform.yaml
+default_range: string
 prefixes:
-  personinfo:
-    prefix_prefix: personinfo
-    prefix_reference: https://w3id.org/linkml/examples/personinfo/
-  linkml:
-    prefix_prefix: linkml
-    prefix_reference: https://w3id.org/linkml/
-  schema:
-    prefix_prefix: schema
-    prefix_reference: http://schema.org/
-  rdfs:
-    prefix_prefix: rdfs
-    prefix_reference: http://www.w3.org/2000/01/rdf-schema#
-  prov:
-    prefix_prefix: prov
-    prefix_reference: http://www.w3.org/ns/prov#
-  GSSO:
-    prefix_prefix: GSSO
-    prefix_reference: http://purl.obolibrary.org/obo/GSSO_
-  famrel:
-    prefix_prefix: famrel
-    prefix_reference: https://example.org/FamilialRelations#
-  P:
-    prefix_prefix: P
-    prefix_reference: http://example.org/P/
-  ROR:
-    prefix_prefix: ROR
-    prefix_reference: http://example.org/ror/
-  CODE:
-    prefix_prefix: CODE
```

### personinfo_basic/personinfo-to-agent.transform.yaml validate-spec [output_diff]

```diff
--- upstream
+++ rust
@@ -1 +0,0 @@
-/home/user/upstream/linkml-map/tests/input/examples/personinfo_basic/transform/personinfo-to-agent.transform.yaml: ok
```

### single_value_for_multivalued/greetings-to-languages.transform.yaml derive-schema [output_diff]

```diff
--- upstream
+++ rust
@@ -1,23 +1,4 @@
-name: greetings-derived
-id: greetings-derived
-imports:
-- linkml:types
-default_prefix: greetings-derived/
+id: https://example.org/derived
+name: derived
 default_range: string
-enums:
-  languages:
-    name: languages
-    permissible_values:
-      bonjour:
-        text: bonjour
-      ciao:
-        text: ciao
-      hello:
-        text: hello
-      hola:
-        text: hola
-      konnichiwa:
-        text: konnichiwa
-      namaste:
-        text: namaste
 classes:
@@ -33,2 +14,3 @@
     name: Container
+    tree_root: true
     attributes:
@@ -39,2 +21,10 @@
         inlined_as_list: true
-    tree_root: true
+enums:
+  languages:
+    permissible_values:
+      English: {}
+      French: {}
```

### single_value_for_multivalued/greetings-to-languages.transform.yaml validate-spec [output_diff]

```diff
--- upstream
+++ rust
@@ -1 +0,0 @@
-/home/user/upstream/linkml-map/tests/input/examples/single_value_for_multivalued/transform/greetings-to-languages.transform.yaml: ok
```

### tabular/person_to_agent.transform.yaml derive-schema [output_diff]

```diff
--- upstream
+++ rust
@@ -1,13 +1,3 @@
-name: person_flat-derived
-id: https://example.org/person_flat-derived
-imports:
-- linkml:types
-prefixes:
-  linkml:
-    prefix_prefix: linkml
-    prefix_reference: https://w3id.org/linkml/
-  P:
-    prefix_prefix: P
-    prefix_reference: http://example.org/P/
-default_prefix: https://example.org/person_flat-derived/
+id: person-to-agent-flat
+name: person-to-agent-flat
 default_range: string
```

### tabular/person_to_agent.transform.yaml validate-spec [output_diff]

```diff
--- upstream
+++ rust
@@ -1 +0,0 @@
-/home/user/upstream/linkml-map/tests/input/examples/tabular/transform/person_to_agent.transform.yaml: ok
```

### type_coercion/type_coercion.transform.yaml derive-schema [output_diff]

```diff
--- upstream
+++ rust
@@ -1,19 +1,3 @@
-name: quantity_value-derived
-id: https://w3id.org/linkml/examples/type_coercion-derived
-imports:
-- linkml:types
-prefixes:
-  tc:
-    prefix_prefix: tc
-    prefix_reference: https://w3id.org/linkml/examples/type_coercion/
-  linkml:
-    prefix_prefix: linkml
-    prefix_reference: https://w3id.org/linkml/
-  example:
-    prefix_prefix: example
-    prefix_reference: https://example.org/
-  P:
-    prefix_prefix: P
-    prefix_reference: https://example.org/person/
-default_prefix: https://w3id.org/linkml/examples/type_coercion-derived/
+id: qv-to-scalar
+name: qv-to-scalar
 default_range: string
@@ -22,2 +6,3 @@
     name: MyRecord
+    tree_root: true
     attributes:
@@ -25,12 +10,12 @@
         name: id
+        range: uriorcurie
         identifier: true
-        range: uriorcurie
       id_as_string:
         name: id_as_string
+        range: string
         identifier: true
-        range: string
       id_as_url:
         name: id_as_url
```

### personinfo_basic/personinfo-to-agent.transform.yaml data/Container-001.yaml -o yaml [output_diff]

```diff
--- upstream
+++ rust
@@ -5,4 +5,4 @@
   primary_email: fred.bloggs@example.com
-  driving_since: 2014-03-31
-  first_known_event: 1990-01-01
+  driving_since: '2014-03-31'
+  first_known_event: '1990-01-01'
   current_address:
```

### personinfo_basic/personinfo-to-agent.transform.yaml data/Container-001.yaml -o json [upstream_error]

upstream rc=1 TypeError: Object of type date is not JSON serializable

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/personinfo_basic/data/Container-001.yaml -> /tmp/tmpnzi1ct9i/rs.json (spec: /home/user/upstream/linkml-map/tests/input/examples/personinfo_basic/transform/personinfo-to-agent.transform.yaml)
rows_in=1 rows_out=1 elapsed=0.095

### personinfo_basic/personinfo-to-agent.transform.yaml data/Container-001.yaml -o jsonl [upstream_error]

upstream rc=1 TypeError: Object of type date is not JSON serializable

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/personinfo_basic/data/Container-001.yaml -> /tmp/tmpz8xgjagb/rs.jsonl (spec: /home/user/upstream/linkml-map/tests/input/examples/personinfo_basic/transform/personinfo-to-agent.transform.yaml)
rows_in=1 rows_out=1 elapsed=0.07

### personinfo_basic/personinfo-to-agent.transform.yaml data/Container-001.yaml -o tsv [output_diff]

```diff
--- upstream
+++ rust
@@ -1,2 +1,2 @@
 agents
-[{'id': 'P:001', 'label': 'fred bloggs', 'age': '33 years', 'primary_email': 'fred.bloggs@example.com', 'secondary_email': None, 'gender': None, 'driving_since': datetime.date(2014, 3, 31), 'first_known_event': datetime.date(1990, 1, 1), 'death_date': None, 'current_address': {'address_of': 'foo', 'street': '1 oak street', 'city': 'oaktown'}, 'has_familial_relationships': [{'type': 'SIBLING_OF', 'related_to': 'P:002'}, {'type': 'CHILD_OF', 'related_to': 'P:003'}]}, {'id': 'P:002', 'label': 'Alison Wu', 'age': None, 'primary_email': None, 'secondary_email': None, 'gender': None, 'driving_since': None, 'first_known_event': None, 'death_date': None, 'current_address': None, 'has_familial_relationships': [{'type': 'SIBLING_OF', 'related_to': 'P:001'}, {'type': 'CHILD_OF', 'related_to': 'P:003'}]}, {'id': 'P:003', 'label': 'Mr. Blobby', 'age': None, 'primary_email': None, 'secondary_email': None, 'gender': None, 'driving_since': None, 'first_known_event': None, 'death_date': None, 'current_address': None, 'has_familial_relationships': [{'type': 'PARENT_OF', 'related_to': 'P:001'}, {'type': 'PARENT_OF', 'related_to': 'P:002'}]}]
+[{'id': 'P:001', 'label': 'fred bloggs', 'age': '33 years', 'primary_email': 'fred.bloggs@example.com', 'secondary_email': None, 'gender': None, 'driving_since': '2014-03-31', 'first_known_event': '1990-01-01', 'death_date': None, 'current_address': {'address_of': 'foo', 'street': '1 oak street', 'city': 'oaktown'}, 'has_familial_relationships': [{'type': 'SIBLING_OF', 'related_to': 'P:002'}, {'type': 'CHILD_OF', 'related_to': 'P:003'}]}, {'id': 'P:002', 'label': 'Alison Wu', 'age': None, 'primary_email': None, 'secondary_email': None, 'gender': None, 'driving_since': None, 'first_known_event': None, 'death_date': None, 'current_address': None, 'has_familial_relationships': [{'type': 'SIBLING_OF', 'related_to': 'P:001'}, {'type': 'CHILD_OF', 'related_to': 'P:003'}]}, {'id': 'P:003', 'label': 'Mr. Blobby', 'age': None, 'primary_email': None, 'secondary_email': None, 'gender': None, 'driving_since': None, 'first_known_event': None, 'death_date': None, 'current_address': None, 'has_familial_relationships': [{'type': 'PARENT_OF', 'related_to': 'P:001'}, {'type': 'PARENT_OF', 'related_to': 'P:002'}]}]
```

### personinfo_basic/personinfo-to-agent.transform.yaml data/Container-001.yaml -o csv [output_diff]

```diff
--- upstream
+++ rust
@@ -1,2 +1,2 @@
 agents
-[{'id': 'P:001', 'label': 'fred bloggs', 'age': '33 years', 'primary_email': 'fred.bloggs@example.com', 'secondary_email': None, 'gender': None, 'driving_since': datetime.date(2014, 3, 31), 'first_known_event': datetime.date(1990, 1, 1), 'death_date': None, 'current_address': {'address_of': 'foo', 'street': '1 oak street', 'city': 'oaktown'}, 'has_familial_relationships': [{'type': 'SIBLING_OF', 'related_to': 'P:002'}, {'type': 'CHILD_OF', 'related_to': 'P:003'}]}, {'id': 'P:002', 'label': 'Alison Wu', 'age': None, 'primary_email': None, 'secondary_email': None, 'gender': None, 'driving_since': None, 'first_known_event': None, 'death_date': None, 'current_address': None, 'has_familial_relationships': [{'type': 'SIBLING_OF', 'related_to': 'P:001'}, {'type': 'CHILD_OF', 'related_to': 'P:003'}]}, {'id': 'P:003', 'label': 'Mr. Blobby', 'age': None, 'primary_email': None, 'secondary_email': None, 'gender': None, 'driving_since': None, 'first_known_event': None, 'death_date': None, 'current_address': None, 'has_familial_relationships': [{'type': 'PARENT_OF', 'related_to': 'P:001'}, {'type': 'PARENT_OF', 'related_to': 'P:002'}]}]
+[{'id': 'P:001', 'label': 'fred bloggs', 'age': '33 years', 'primary_email': 'fred.bloggs@example.com', 'secondary_email': None, 'gender': None, 'driving_since': '2014-03-31', 'first_known_event': '1990-01-01', 'death_date': None, 'current_address': {'address_of': 'foo', 'street': '1 oak street', 'city': 'oaktown'}, 'has_familial_relationships': [{'type': 'SIBLING_OF', 'related_to': 'P:002'}, {'type': 'CHILD_OF', 'related_to': 'P:003'}]}, {'id': 'P:002', 'label': 'Alison Wu', 'age': None, 'primary_email': None, 'secondary_email': None, 'gender': None, 'driving_since': None, 'first_known_event': None, 'death_date': None, 'current_address': None, 'has_familial_relationships': [{'type': 'SIBLING_OF', 'related_to': 'P:001'}, {'type': 'CHILD_OF', 'related_to': 'P:003'}]}, {'id': 'P:003', 'label': 'Mr. Blobby', 'age': None, 'primary_email': None, 'secondary_email': None, 'gender': None, 'driving_since': None, 'first_known_event': None, 'death_date': None, 'current_address': None, 'has_familial_relationships': [{'type': 'PARENT_OF', 'related_to': 'P:001'}, {'type': 'PARENT_OF', 'related_to': 'P:002'}]}]
```

### personinfo_basic/personinfo-to-agent.transform.yaml data/Person-001.yaml -o yaml [output_diff]

```diff
--- upstream
+++ rust
@@ -4,4 +4,4 @@
 primary_email: fred.bloggs@example.com
-driving_since: 2014-03-31
-first_known_event: 1990-01-01
+driving_since: '2014-03-31'
+first_known_event: '1990-01-01'
 current_address:
```

### personinfo_basic/personinfo-to-agent.transform.yaml data/Person-001.yaml -o json [upstream_error]

upstream rc=1 TypeError: Object of type date is not JSON serializable

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/personinfo_basic/data/Person-001.yaml -> /tmp/tmp51lkvke8/rs.json (spec: /home/user/upstream/linkml-map/tests/input/examples/personinfo_basic/transform/personinfo-to-agent.transform.yaml)
rows_in=1 rows_out=1 elapsed=0.028s t

### personinfo_basic/personinfo-to-agent.transform.yaml data/Person-001.yaml -o jsonl [upstream_error]

upstream rc=1 TypeError: Object of type date is not JSON serializable

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/personinfo_basic/data/Person-001.yaml -> /tmp/tmpbsj3gy07/rs.jsonl (spec: /home/user/upstream/linkml-map/tests/input/examples/personinfo_basic/transform/personinfo-to-agent.transform.yaml)
rows_in=1 rows_out=1 elapsed=0.021s 

### type_coercion/type_coercion.transform.yaml data/MyRecord-001.yaml -o yaml [upstream_error]

upstream rc=1 TransformationError: No such slot id_as_url as an attribute of MyRecord ancestors or as a slot definition in the schema; class_derivation=MyRecord (from MyRecord); slot_derivation=id_as_url (from id)

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/data/MyRecord-001.yaml -> /tmp/tmpgsbetxep/rs.yaml (spec: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/transform/type_coercion.transform.yaml)
rows_in=1 rows_out=1 elapsed=0.002s throughput=

### type_coercion/type_coercion.transform.yaml data/MyRecord-001.yaml -o json [upstream_error]

upstream rc=1 TransformationError: No such slot id_as_url as an attribute of MyRecord ancestors or as a slot definition in the schema; class_derivation=MyRecord (from MyRecord); slot_derivation=id_as_url (from id)

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/data/MyRecord-001.yaml -> /tmp/tmp3o6s0462/rs.json (spec: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/transform/type_coercion.transform.yaml)
rows_in=1 rows_out=1 elapsed=0.004s throughput=

### type_coercion/type_coercion.transform.yaml data/MyRecord-001.yaml -o jsonl [upstream_error]

upstream rc=1 TransformationError: No such slot id_as_url as an attribute of MyRecord ancestors or as a slot definition in the schema; class_derivation=MyRecord (from MyRecord); slot_derivation=id_as_url (from id)

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/data/MyRecord-001.yaml -> /tmp/tmp4ars6po_/rs.jsonl (spec: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/transform/type_coercion.transform.yaml)
rows_in=1 rows_out=1 elapsed=0.003s throughput

### type_coercion/type_coercion.transform.yaml data/MyRecord-001.yaml -o tsv [upstream_error]

upstream rc=1 TransformationError: No such slot id_as_url as an attribute of MyRecord ancestors or as a slot definition in the schema; class_derivation=MyRecord (from MyRecord); slot_derivation=id_as_url (from id)

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/data/MyRecord-001.yaml -> /tmp/tmpcf185dkr/rs.tsv (spec: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/transform/type_coercion.transform.yaml)
rows_in=1 rows_out=1 elapsed=0.004s throughput=2

### type_coercion/type_coercion.transform.yaml data/MyRecord-001.yaml -o csv [upstream_error]

upstream rc=1 TransformationError: No such slot id_as_url as an attribute of MyRecord ancestors or as a slot definition in the schema; class_derivation=MyRecord (from MyRecord); slot_derivation=id_as_url (from id)

rust rc=0: linkml-tr-rs map-data: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/data/MyRecord-001.yaml -> /tmp/tmp0zfhrapz/rs.csv (spec: /home/user/upstream/linkml-map/tests/input/examples/type_coercion/transform/type_coercion.transform.yaml)
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

224 distinct map_object invocations x 5 formats: both_error: 155, match: 810, output_diff: 45, rust_error: 110

| case | origin test | divergent formats |
|---|---|---|
| 0759dc334566 | test_transformer/test_multi_enum.py::test_enum_expr_unrestricted_fallback | yaml:DIFF json:DIFF jsonl:DIFF tsv:DIFF csv:DIFF |
| 1efbc4a901e2 | test_transformer/test_content_id.py::test_null_and_empty_count_as_absent[absent0] | yaml:RS-ERR json:RS-ERR jsonl:RS-ERR tsv:RS-ERR csv:RS-ERR |
| 22b03ca460ec | test_transformer/test_content_id.py::test_equivalent_content_same_id[changes2] | yaml:RS-ERR json:RS-ERR jsonl:RS-ERR tsv:RS-ERR csv:RS-ERR |
| 2a3e4922c2fd | test_transformer/test_range_override.py::test_parse_expr_null_input_yields_none | yaml:RS-ERR json:RS-ERR jsonl:RS-ERR tsv:RS-ERR csv:RS-ERR |
| 3cb7c04887b5 | test_transformer/test_pivot_operation.py::TestEdgeCases::test_unmelt_missing_variable | yaml:DIFF json:DIFF jsonl:DIFF tsv:DIFF csv:DIFF |
| 42175371c7cd | test_transformer/test_expr_strict_mode.py::test_schema_slot_absent_from_row_resolves_to_none[True] | yaml:RS-ERR json:RS-ERR jsonl:RS-ERR tsv:RS-ERR csv:RS-ERR |
| 42f8aeb349ad | test_transformer/test_content_id.py::test_null_and_empty_count_as_absent[absent0] | yaml:RS-ERR json:RS-ERR jsonl:RS-ERR tsv:RS-ERR csv:RS-ERR |
| 4d8e0addb392 | test_transformer/test_object_transformer.py::test_offset_reverse_subtraction | yaml:DIFF json:DIFF jsonl:DIFF tsv:DIFF csv:DIFF |
| 53323ba2d2c2 | test_transformer/test_pivot_operation.py::TestEdgeCases::test_unmelt_missing_value | yaml:DIFF json:DIFF jsonl:DIFF tsv:DIFF csv:DIFF |
| 549c49e209bb | test_transformer/test_content_id.py::test_explicit_id_derivation_wins | yaml:RS-ERR json:RS-ERR jsonl:RS-ERR tsv:RS-ERR csv:RS-ERR |
| 5b02b0aceb8c | test_transformer/test_content_id.py::test_different_content_different_id[changes1] | yaml:RS-ERR json:RS-ERR jsonl:RS-ERR tsv:RS-ERR csv:RS-ERR |
| 612b35fd2571 | test_transformer/test_pivot_operation.py::TestEdgeCases::test_melt_null_values_skipped | yaml:DIFF json:DIFF jsonl:DIFF tsv:DIFF csv:DIFF |
| 624bf4746c9c | test_transformer/test_object_transformer.py::test_offset_applied_addition | yaml:DIFF json:DIFF jsonl:DIFF tsv:DIFF csv:DIFF |
| 6912e082ae37 | test_transformer/test_content_id.py::test_same_content_same_id_across_transformers | yaml:RS-ERR json:RS-ERR jsonl:RS-ERR tsv:RS-ERR csv:RS-ERR |
| 74055f70a60c | test_transformer/test_flatten_inlined.py::test_flatten_two_level_dotpath_null_propagation[env_broad_scale1-middle | yaml:RS-ERR json:RS-ERR jsonl:RS-ERR tsv:RS-ERR csv:RS-ERR |
| 8ba729cc95cd | test_transformer/test_multi_enum.py::test_enum_expr_restricted_evaluates | yaml:DIFF json:DIFF jsonl:DIFF tsv:DIFF csv:DIFF |
| a24a07db0802 | test_transformer/test_slot_derivation_features.py::test_missing_values_applies_within_nested_class_derivation | yaml:RS-ERR json:RS-ERR jsonl:RS-ERR tsv:RS-ERR csv:RS-ERR |
| a348eb4e07e6 | test_transformer/test_content_id.py::test_different_content_different_id[changes3] | yaml:RS-ERR json:RS-ERR jsonl:RS-ERR tsv:RS-ERR csv:RS-ERR |
| a4807fb6da0b | test_transformer/test_content_id.py::test_equivalent_content_same_id[changes1] | yaml:RS-ERR json:RS-ERR jsonl:RS-ERR tsv:RS-ERR csv:RS-ERR |
| a8c78e8ca56d | test_transformer/test_slot_derivation_features.py::test_slot_fn_with_unrestricted_eval | yaml:RS-ERR json:RS-ERR jsonl:RS-ERR tsv:RS-ERR csv:RS-ERR |
| b18ac8678d14 | test_transformer/test_range_override.py::test_parse_string_into_object | yaml:RS-ERR json:RS-ERR jsonl:RS-ERR tsv:RS-ERR csv:RS-ERR |
| bf88321f41a3 | test_transformer/test_content_id.py::test_null_and_empty_count_as_absent[absent1] | yaml:RS-ERR json:RS-ERR jsonl:RS-ERR tsv:RS-ERR csv:RS-ERR |
| c14a3ce239b7 | test_transformer/test_content_id.py::test_nested_derivation_follows_the_spec_not_its_parent | yaml:RS-ERR json:RS-ERR jsonl:RS-ERR tsv:RS-ERR csv:RS-ERR |
| d5d814e375b4 | test_transformer/test_content_id.py::test_equivalent_content_same_id[changes0] | yaml:RS-ERR json:RS-ERR jsonl:RS-ERR tsv:RS-ERR csv:RS-ERR |
| df16cf8b3044 | test_transformer/test_content_id.py::test_different_content_different_id[changes0] | yaml:RS-ERR json:RS-ERR jsonl:RS-ERR tsv:RS-ERR csv:RS-ERR |
| e525872c0623 | test_transformer/test_content_id.py::test_different_content_different_id[changes2] | yaml:RS-ERR json:RS-ERR jsonl:RS-ERR tsv:RS-ERR csv:RS-ERR |
| e89e6495d1ef | test_transformer/test_content_id.py::test_synthesized_id_is_pinned | yaml:RS-ERR json:RS-ERR jsonl:RS-ERR tsv:RS-ERR csv:RS-ERR |
| ed14d3b728ab | test_transformer/test_pivot_operation.py::TestMeltBasic::test_melt_with_id_slots | yaml:DIFF json:DIFF jsonl:DIFF tsv:DIFF csv:DIFF |
| ed15ebb53262 | test_transformer/test_object_transformer.py::test_nested_class_derivations | yaml:RS-ERR json:RS-ERR jsonl:RS-ERR tsv:RS-ERR csv:RS-ERR |
| f2ba6e0e0ae9 | test_transformer/test_pivot_operation.py::TestMeltBasic::test_melt_basic | yaml:DIFF json:DIFF jsonl:DIFF tsv:DIFF csv:DIFF |
| fec9877c167d | test_transformer/test_flatten_inlined.py::test_flatten_partial_quantity_value | yaml:RS-ERR json:RS-ERR jsonl:RS-ERR tsv:RS-ERR csv:RS-ERR |
