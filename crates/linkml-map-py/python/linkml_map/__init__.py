"""linkml_map-compatible shim backed by the Rust `linkml_map_rs` engine.

Install this package **instead of** upstream `linkml-map` to make existing
imports resolve here. Covers the common ObjectTransformer.map_object path and
`load_specification`.
"""

from .transformer.object_transformer import ObjectTransformer
from .utils.loaders import load_specification

__all__ = ["ObjectTransformer", "load_specification"]
