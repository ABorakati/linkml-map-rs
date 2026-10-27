# Differential case 466ed494c991

- found by: fuzz.py --seed 8 profile=yaml
- `yaml`: output_diff
- `json`: output_diff
- `jsonl`: output_diff
- `tsv`: output_diff
- `csv`: output_diff

signature: `yaml:output_diff:::lines fewer|json:output_diff:::lines fewer|jsonl:output_diff:::"a#", "a_a":~"a#", "a_a":|tsv:output_diff:::a#	"[""a#""]~a#	a#|csv:output_diff:::a#,"[""a#""]~a#,a#`
