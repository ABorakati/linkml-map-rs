# Differential case dbee105e1e6e

- found by: port/joinalias
- `yaml`: both_error (message match: overlap)
- `json`: both_error (message match: overlap)
- `jsonl`: both_error (message match: overlap)
- `tsv`: both_error (message match: overlap)
- `csv`: both_error (message match: overlap)

port/joinalias: which table a join reads (scenario `implicit_expr_with_alias_join_noalias_file`). Upstream looks the table up under the join name, never `class_named`.
