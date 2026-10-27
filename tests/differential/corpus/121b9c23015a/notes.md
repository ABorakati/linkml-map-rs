# Differential case 121b9c23015a

- found by: fuzz.py --seed 7 profile=multivalued
- `yaml`: output_diff
- `json`: output_diff
- `jsonl`: output_diff
- `tsv`: match
- `csv`: match

signature: `yaml:output_diff:::#~'#'|json:output_diff:::#~"#"|jsonl:output_diff:::"a#", "a_a#"~"a#", "a_a#"`
