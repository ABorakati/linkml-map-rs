# Differential case 854bd9c8c579

- found by: port/fkexpr
- `yaml`: both_error (message match: overlap)
- `json`: both_error (message match: overlap)

port/fkexpr: dir_expr_boss_label. a table never builds the index: `{boss.label}` reads the bare identifier
