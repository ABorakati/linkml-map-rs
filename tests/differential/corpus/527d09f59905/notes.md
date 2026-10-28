# Differential case 527d09f59905

- found by: port/joinalias
- `yaml`: match
- `json`: match
- `jsonl`: match
- `tsv`: match
- `csv`: match

port/joinalias: which table a join reads (scenario `pf_eng_null_keys`). Upstream looks the table up under the join name, never `class_named`.
