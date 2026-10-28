# Differential case 275933bf259b

- found by: nestedjoin adhoc
- `yaml`: both_error (message match: overlap)
- `json`: both_error (message match: overlap)
- `jsonl`: both_error (message match: overlap)
- `tsv`: both_error (message match: overlap)
- `csv`: both_error (message match: overlap)

An unrestricted statement block is handed the whole row (get_ctxt_obj_and_dict()): a column the source class lacks fails it, whether or not the block reads it.
