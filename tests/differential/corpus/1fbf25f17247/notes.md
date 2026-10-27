# Differential case 1fbf25f17247

- found by: fuzz.py --seed 1 profile=all
- `yaml`: output_diff
- `json`: output_diff
- `jsonl`: output_diff
- `tsv`: output_diff
- `csv`: output_diff

signature: `yaml:output_diff:::lines more|json:output_diff:::lines more|jsonl:output_diff:::["a"]}~["a", "a"]}|tsv:output_diff:::"[""a""]"~"[""a"", ""a|csv:output_diff:::"[""a""]"~"[""a"", ""a`
