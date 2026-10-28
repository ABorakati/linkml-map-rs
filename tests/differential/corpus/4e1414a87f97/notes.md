# Differential case 4e1414a87f97

- found by: port/joinalias
- `yaml`: both_error (message match: overlap)
- `json`: both_error (message match: overlap)
- `jsonl`: both_error (message match: overlap)
- `tsv`: both_error (message match: overlap)
- `csv`: both_error (message match: overlap)

port/joinalias: which table a join reads (scenario `pf_eng_single_file`). Upstream looks the table up under the join name, never `class_named`.
