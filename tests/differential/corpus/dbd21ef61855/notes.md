# Differential case dbd21ef61855

- found by: fuzz.py --seed 20261028 profile=nested
- `yaml`: output_diff
- `json`: output_diff
- `jsonl`: output_diff
- `tsv`: match
- `csv`: match

signature: `yaml:output_diff:::#~'#'|json:output_diff:::#~"#"|jsonl:output_diff:::"a#", "a_a":~"a#", "a_a":`
