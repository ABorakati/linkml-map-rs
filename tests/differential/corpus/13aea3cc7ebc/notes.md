# Differential case 13aea3cc7ebc

- found by: fuzz.py --seed 4 profile=multivalued
- `yaml`: output_diff
- `json`: output_diff
- `jsonl`: output_diff
- `tsv`: match
- `csv`: match

signature: `yaml:output_diff:::#~'#'|json:output_diff:::#~"#"|jsonl:output_diff:::#}~"#"}`
