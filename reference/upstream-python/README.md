# Upstream Python Reference — linkml-map

These files are **READ-ONLY reference copies** of the upstream Python implementation.
They are NOT built, imported, compiled, or executed by this workspace.
Their sole purpose is to serve as an authoritative reference when porting behaviour
to Rust under the "mirror upstream Python" doctrine.

## Source

| Field          | Value                                                        |
|----------------|--------------------------------------------------------------|
| Upstream repo  | https://github.com/linkml/linkml-map                        |
| Pinned commit  | `v0.6.0-rc1` tag (`1323cc8`)                                |
| Fetch date     | 2026-06-24 (initial); 2026-07-07; 2026-07-25 (pin bump to `v0.5.3`); 2026-10-27 (pin bump to `v0.6.0-rc1`) |

Pin-drift history: see [`docs/upstream-sync-2026-10-27.md`](../../docs/upstream-sync-2026-10-27.md).

## Raw URL pattern

```
https://raw.githubusercontent.com/linkml/linkml-map/1323cc8b73311c4774abac7337748d212ab76a32/src/linkml_map/<path>
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
| `transformer/content_id.py`                      | `transformer/content_id.py`                 |
| `transformer/pivot.py`                           | `transformer/pivot.py`                      |
| `utils/enum_index.py`                            | `utils/enum_index.py`                       |
| `utils/serialization.py`                         | `utils/serialization.py`                    |
| `functions/unit_conversion.py`                   | `functions/unit_conversion.py`              |
| `spec_normalizer.py`                             | `spec_normalizer.py`                        |
| `writers/output_streams.py`                      | `writers/output_streams.py`                 |
| `utils/loaders.py`                               | `utils/loaders.py`                          |

### Newly vendored (2026-10-27 pin bump to `v0.6.0-rc1`)

Eight files were added when the pin moved to `1323cc8`, because the Rust port now
mirrors them: `transformer/content_id.py` (content-hash ids, #342),
`transformer/pivot.py` and `functions/unit_conversion.py` (blocks extracted from
`object_transformer.py`), `spec_normalizer.py` (extracted from `Transformer`),
`utils/enum_index.py` (target-enum resolution), `utils/serialization.py`
(exact decimals), `utils/loaders.py` (`load_specification`) and
`writers/output_streams.py` (the stream writers, including Parquet/DuckDB). The
other twelve files were re-fetched and now match `1323cc8` byte for byte.
`transformer/transformer.py`, `utils/join_utils.py` and
`utils/expression_locations.py` were first vendored in the 2026-07-07 bump; see
[`docs/upstream-sync-2026-07-07.md`](../../docs/upstream-sync-2026-07-07.md).

### Discovery note — inverter path

The inverter module was discovered by querying the GitHub API tree at the pinned
commit. It lives at `src/linkml_map/inference/inverter.py` (under `inference/`,
not `transformer/`). The Rust port is in
`crates/linkml-map-core/src/inference/`.

### Files not present at pinned commit

All twenty files in the table above exist at `1323cc8`. The only deliberate gaps
are upstream modules that are **not vendored**, although some are mirrored by the
Rust port: `validator.py`, `cli/cli.py`, `inference/inference.py`,
`loaders/data_loaders.py`, `spec_scan.py`, `utils/spec_merge.py`,
`utils/multi_file_transformer.py`, `session.py`, `transformer/join_engine.py`,
`transformer/duckdb_transformer.py` and the `compiler/` package. Read them from an
upstream checkout at `1323cc8` when porting.

## Do NOT

- Add `reference/` to any `Cargo.toml`, build script, or `include!` macro.
- Import or `include_str!` anything from this directory in Rust source.
- Edit these files — they are pinned snapshots; update the pin explicitly if needed.
