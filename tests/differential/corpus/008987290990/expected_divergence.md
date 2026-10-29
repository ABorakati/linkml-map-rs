category: eager induce_missing_values: a bare slot derivation the source class lacks fails before any row

Upstream's `derived_specification` runs `induce_missing_values` on every class derivation, parents (`is_a` templates) included.
A bare slot derivation (no `populated_from`/`expr`/`value`, no `range`) is induced to `populated_from = <name>`, and the range
inference then calls `induced_slot(<name>, <class>)`, which raises `ValueError: No such slot ...` even when the table is empty
or nothing reads that block. Rust induces the same `populated_from` (see `normalize/inheritance.rs`) but, as for every
slot-level error, only fails when a row reaches the slot, so it succeeds here (and for a parent block, always).
Found by the slot-level `sources` campaign (an inherited `sources` slot on a base that reads a source class); not specific
to `sources`: `out_a: {}` with an unknown name and an empty `Person.tsv` does the same. Suggested upstream issue: the eager
`ValueError` escapes the `TransformationError` wrapper and fires for blocks that never run.
