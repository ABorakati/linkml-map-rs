# Upstream Python Reference — linkml-map

These files are **READ-ONLY reference copies** of the upstream Python implementation.
They are NOT built, imported, compiled, or executed by this workspace.
Their sole purpose is to serve as an authoritative reference when porting behaviour
to Rust under the "mirror upstream Python" doctrine.

## Source

| Field          | Value                                                        |
|----------------|--------------------------------------------------------------|
| Upstream repo  | https://github.com/linkml/linkml-map                        |
| Pinned commit  | `66cbcc31b5169f8f95a70f164161a4cee1c6a2bb`                  |
| Fetch date     | 2026-06-24 (initial nine files); refreshed 2026-07-07 for the pin bump to `66cbcc31b5` and the three newly-added files below |

Pin-drift history: see [`docs/upstream-sync-2026-07-07.md`](../../docs/upstream-sync-2026-07-07.md).

## Raw URL pattern

```
https://raw.githubusercontent.com/linkml/linkml-map/66cbcc31b5169f8f95a70f164161a4cee1c6a2bb/src/linkml_map/<path>
```

## License

The upstream `linkml-map` project is released under **CC0-1.0** (Creative Commons Zero),
the same basis on which `tests/compliance/` already vendors upstream fixture files.

## Vendored files

| Local path (under `reference/upstream-python/`) | Upstream `src/linkml_map/<path>`            |
|--------------------------------------------------|---------------------------------------------|
| `transformer/object_transformer.py`              | `transformer/object_transformer.py`         |
| `transformer/engine.py`                          | `transformer/engine.py`                     |
| `transformer/errors.py`                          | `transformer/errors.py`                     |
| `transformer/transformer.py`                     | `transformer/transformer.py`                |
| `utils/eval_utils.py`                            | `utils/eval_utils.py`                       |
| `utils/fk_utils.py`                              | `utils/fk_utils.py`                         |
| `utils/lookup_index.py`                          | `utils/lookup_index.py`                     |
| `utils/schema_patch.py`                          | `utils/schema_patch.py`                     |
| `utils/join_utils.py`                            | `utils/join_utils.py`                       |
| `utils/expression_locations.py`                  | `utils/expression_locations.py`             |
| `datamodel/transformer_model.py`                 | `datamodel/transformer_model.py`            |
| `inference/inverter.py`                          | `inference/inverter.py`                     |

### Newly vendored (2026-07-07 pin bump)

`transformer/transformer.py`, `utils/join_utils.py`, and `utils/expression_locations.py`
were added as part of the 2026-07-07 pin bump to `66cbcc31b5169f8f95a70f164161a4cee1c6a2bb`
— they weren't previously relevant to a mirrored port, but the engine port landing
alongside this pin bump (implicit-join synthesis from expression references, and the
dotted-`populated_from` join fix) mirrors logic from them. See
[`docs/upstream-sync-2026-07-07.md`](../../docs/upstream-sync-2026-07-07.md) for context.

### Discovery note — inverter path

The inverter module was discovered by querying the GitHub API tree at the pinned
commit. It lives at `src/linkml_map/inference/inverter.py` (under `inference/`,
not `transformer/`). The Rust port is in
`crates/linkml-map-core/src/inference/`.

### Files not present at pinned commit

None of the requested files were absent at commit `66cbcc31b5169f8f95a70f164161a4cee1c6a2bb`.
All twelve modules were fetched successfully (the original nine were not re-diffed against
the new pin during this bump; only the three new files were freshly fetched).

## Do NOT

- Add `reference/` to any `Cargo.toml`, build script, or `include!` macro.
- Import or `include_str!` anything from this directory in Rust source.
- Edit these files — they are pinned snapshots; update the pin explicitly if needed.
