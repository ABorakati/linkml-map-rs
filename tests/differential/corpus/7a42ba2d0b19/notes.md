# Differential case 7a42ba2d0b19

- `yaml`: rust_error
- `json`: rust_error
- `jsonl`: rust_error
- `tsv`: rust_error
- `csv`: rust_error

nested class derivation over a joined table: upstream merges the parent and joined rows (`_merge_rows`, `MergedRow`) before mapping the nested object; Rust maps it against the parent row alone
