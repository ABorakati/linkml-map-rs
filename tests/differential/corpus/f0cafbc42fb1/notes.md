# Differential case f0cafbc42fb1

- found by: fuzz.py --seed 5 profile=joins
- `yaml`: output_diff
- `json`: output_diff
- `jsonl`: output_diff
- `tsv`: output_diff
- `csv`: output_diff

signature: `yaml:output_diff:::'#'~'#'|json:output_diff:::"#"~"#"|jsonl:output_diff:::"a#", "a_a":~"a#", "a_a":|tsv:output_diff:::a#	#~a#	#|csv:output_diff:::a#,#~a#,#`
