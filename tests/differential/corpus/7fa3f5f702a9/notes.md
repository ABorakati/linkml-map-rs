# Differential case 7fa3f5f702a9

- found by: port/joinalias
- `yaml`: both_error (message match: differs)
- `json`: both_error (message match: differs)
- `jsonl`: both_error (message match: differs)
- `tsv`: both_error (message match: differs)
- `csv`: both_error (message match: differs)

port/joinalias: which table a join reads (scenario `pf_eng_alias_yaml`). Upstream looks the table up under the join name, never `class_named`.
