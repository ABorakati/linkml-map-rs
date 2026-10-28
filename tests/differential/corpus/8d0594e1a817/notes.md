# Differential case 8d0594e1a817

- found by: port/joinalias
- `yaml`: both_error (message match: overlap)
- `json`: both_error (message match: overlap)
- `jsonl`: both_error (message match: overlap)
- `tsv`: both_error (message match: overlap)
- `csv`: both_error (message match: overlap)

port/joinalias: which table a join reads (scenario `pf_row_classonly`). Upstream looks the table up under the join name, never `class_named`.
