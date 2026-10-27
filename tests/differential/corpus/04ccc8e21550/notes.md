# Differential case 04ccc8e21550

- found by: fuzz.py --seed 6 profile=nested
- `yaml`: output_diff
- `json`: output_diff
- `jsonl`: output_diff
- `tsv`: match
- `csv`: match

signature: `yaml:output_diff:::#.#~'#.#'|json:output_diff:::#.#,~"#.#",|jsonl:output_diff:::"a#", "a": #~"a#", "a": "`
