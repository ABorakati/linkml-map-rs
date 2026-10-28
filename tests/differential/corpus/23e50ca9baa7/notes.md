# Differential case 23e50ca9baa7

- found by: fuzz.py --seed 20261028 profile=expr
- `yaml`: output_diff
- `json`: output_diff
- `jsonl`: output_diff
- `tsv`: match
- `csv`: match

signature: `yaml:output_diff:::#~'#'|json:output_diff:::#~"#"|jsonl:output_diff:::"a#", "a_a":~"a#", "a_a":`
