# Differential case c3d3b08d87d0

- `yaml`: output_diff
- `json`: output_diff
- `jsonl`: output_diff
- `tsv`: output_diff
- `csv`: output_diff

nested class derivation over a joined table: upstream merges the parent and joined rows (`_merge_rows`, `MergedRow`) before mapping the nested object; Rust maps it against the parent row alone
