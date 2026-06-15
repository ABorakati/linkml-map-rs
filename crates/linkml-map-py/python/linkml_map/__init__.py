"""linkml_map-compatible shim backed by the Rust `linkml_map_rs` engine.

Install this package **instead of** upstream `linkml-map` to make existing
imports resolve here. Covers the common ObjectTransformer.map_object path.
"""

from .transformer.object_transformer import ObjectTransformer

__all__ = ["ObjectTransformer"]
