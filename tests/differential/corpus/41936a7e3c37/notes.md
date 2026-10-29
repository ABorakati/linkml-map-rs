# Differential case 41936a7e3c37

- found by: port/fkexpr
- `yaml`: both_error (message match: overlap)
- `json`: both_error (message match: overlap)

port/fkexpr: single_stray_key_root. single-object input is indexed whatever the spec reads: dynamic_object rejects a key the schema does not know
