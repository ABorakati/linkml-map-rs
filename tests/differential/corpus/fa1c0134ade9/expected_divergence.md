category: upstream-bug: an ambiguous merged-row column read through slot-level `sources` leaks the _AMBIGUOUS sentinel into the output

In a nested class derivation joined to another table, a column both tables have is `_AMBIGUOUS` in the merged row.
`populated_from` and the implicit copy refuse it (`Column 'name' is ambiguous ...`); `_resolve_sources` does not check,
so with a single such source the sentinel becomes the slot's value: YAML writes
`!!python/object/apply:linkml_map.transformer.object_transformer._AmbiguousType []`, JSON raises `TypeError`, CSV/TSV write
`<AMBIGUOUS>`. Rust raises the ambiguous-column error instead (a deliberate divergence: no `Value` carries the sentinel; see
the PARITY note in `crates/linkml-map-core/src/engine/sources.rs`). Two or more present sources including an ambiguous one
agree (`Multiple sources for ...: {'name': <AMBIGUOUS>, ...}`, case 5ff6e29dfc7e).
Suggested upstream issue: see docs/suggested-upstream-issues.md.
