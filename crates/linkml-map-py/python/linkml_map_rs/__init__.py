"""linkml-map Rust transform engine — native API.

The compiled core lives in `linkml_map_rs._native`; it is re-exported here so
`from linkml_map_rs import Transformer` works.
"""

from ._native import Transformer, transform_object, transform_objects

__all__ = ["Transformer", "transform_object", "transform_objects"]
