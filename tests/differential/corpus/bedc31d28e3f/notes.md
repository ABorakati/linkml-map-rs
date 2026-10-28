# Differential case bedc31d28e3f

- found by: port/joinalias
- `yaml`: both_error (message match: overlap)
- `json`: both_error (message match: overlap)
- `jsonl`: both_error (message match: overlap)
- `tsv`: both_error (message match: overlap)
- `csv`: both_error (message match: overlap)

port/joinalias: which table a join reads (scenario `nested_alias_classonly`). Upstream looks the table up under the join name, never `class_named`.
