# Differential case 2786057b8201

- found by: fuzz.py --seed 5 profile=nested
- `yaml`: output_diff
- `json`: output_diff
- `jsonl`: output_diff
- `tsv`: output_diff
- `csv`: output_diff

signature: `yaml:output_diff:::lines more|json:output_diff:::lines more|jsonl:output_diff:::"a#", "a_a":~"a#", "a_a":|tsv:output_diff:::a#	a#	a#	a#	~a#	a#	a#	a#	|csv:output_diff:::a#,a#,a#,a#,~a#,a#,a#,a#,`
