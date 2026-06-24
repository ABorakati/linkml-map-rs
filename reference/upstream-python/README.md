# Upstream Python Reference — linkml-map

These files are **READ-ONLY reference copies** of the upstream Python implementation.
They are NOT built, imported, compiled, or executed by this workspace.
Their sole purpose is to serve as an authoritative reference when porting behaviour
to Rust under the "mirror upstream Python" doctrine.

## Source

| Field          | Value                                                        |
|----------------|--------------------------------------------------------------|
| Upstream repo  | https://github.com/linkml/linkml-map                        |
| Pinned commit  | `19b1985889ec0fc247145e9aa03eb74a4d7b3588`                  |
| Fetch date     | 2026-06-24                                                   |

## Raw URL pattern

```
https://raw.githubusercontent.com/linkml/linkml-map/19b1985889ec0fc247145e9aa03eb74a4d7b3588/src/linkml_map/<path>
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
| `utils/eval_utils.py`                            | `utils/eval_utils.py`                       |
| `utils/fk_utils.py`                              | `utils/fk_utils.py`                         |
| `utils/lookup_index.py`                          | `utils/lookup_index.py`                     |
| `utils/schema_patch.py`                          | `utils/schema_patch.py`                     |
| `datamodel/transformer_model.py`                 | `datamodel/transformer_model.py`            |
| `inference/inverter.py`                          | `inference/inverter.py`                     |

### Discovery note — inverter path

The inverter module was discovered by querying the GitHub API tree at the pinned
commit. It lives at `src/linkml_map/inference/inverter.py` (under `inference/`,
not `transformer/`). The Rust port is in
`crates/linkml-map-core/src/inference/`.

### Files not present at pinned commit

None of the requested files were absent at commit `19b1985889ec0fc247145e9aa03eb74a4d7b3588`.
All nine modules were fetched successfully.

## Do NOT

- Add `reference/` to any `Cargo.toml`, build script, or `include!` macro.
- Import or `include_str!` anything from this directory in Rust source.
- Edit these files — they are pinned snapshots; update the pin explicitly if needed.
