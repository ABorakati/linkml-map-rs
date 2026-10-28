# Differential case fbeca95b7371

- found by: fuzz.py --seed 2 profile=joins
- `yaml`: output_diff
- `json`: output_diff
- `jsonl`: output_diff
- `tsv`: output_diff
- `csv`: output_diff

signature: `yaml:output_diff:::a#~a#|json:output_diff:::"a#",~"a#"|jsonl:output_diff:::"a#", "a_a":~"a#"}|tsv:output_diff:::a#	a#~a#	|csv:output_diff:::a#,a#~a#,`
