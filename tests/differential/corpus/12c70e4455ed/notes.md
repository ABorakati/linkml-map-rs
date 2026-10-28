# Differential case 12c70e4455ed

- found by: port/joinalias
- `yaml`: both_error (message match: overlap)
- `json`: both_error (message match: overlap)
- `jsonl`: both_error (message match: overlap)
- `tsv`: both_error (message match: overlap)
- `csv`: both_error (message match: overlap)

port/joinalias: which table a join reads (scenario `self_expr_dir_boss`). Upstream looks the table up under the join name, never `class_named`.
