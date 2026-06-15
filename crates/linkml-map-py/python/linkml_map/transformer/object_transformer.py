"""`linkml_map`-compatible ObjectTransformer backed by the Rust engine.

Drop-in for the common ``ObjectTransformer(source_schemaview=..., specification=...)``
+ ``.map_object(obj)`` path. Install this package **instead of** upstream
``linkml-map`` so existing imports resolve here:

    from linkml_map.transformer.object_transformer import ObjectTransformer

Backed by the compiled ``linkml_map_rs`` extension (build with maturin).

Covered: ``map_object``, ``create_transformer_specification``, ``index`` (no-op —
the Rust engine builds any FK index internally). NOT covered: SchemaMapper,
the inverter, Python/SQL compilers, JSON-schema validation.
"""

from __future__ import annotations

import os
from typing import Any, Optional

import linkml_map_rs


def _schema_to_yaml(sv: Any) -> str:
    """Coerce a source/target schema argument to a YAML string.

    Accepts a linkml_runtime SchemaView, a file path, or a YAML string.
    """
    schema = getattr(sv, "schema", None)
    if schema is not None:  # linkml_runtime SchemaView
        from linkml_runtime.dumpers import yaml_dumper

        return yaml_dumper.dumps(schema)
    if isinstance(sv, str):
        if os.path.exists(sv):
            with open(sv) as f:
                return f.read()
        return sv  # assume it is already YAML text
    raise TypeError(f"Cannot derive schema YAML from {type(sv)!r}")


def _spec_to_yaml(spec: Any) -> str:
    """Coerce a specification argument to a YAML string.

    Accepts a dict, a file path, a YAML string, or a linkml_map
    TransformationSpecification object.
    """
    if spec is None:
        raise ValueError("specification is required")
    if isinstance(spec, str):
        if os.path.exists(spec):
            with open(spec) as f:
                return f.read()
        return spec  # YAML text
    if isinstance(spec, dict):
        import yaml

        return yaml.safe_dump(spec)
    try:  # linkml_map datamodel object
        from linkml_runtime.dumpers import yaml_dumper

        return yaml_dumper.dumps(spec)
    except Exception as e:  # noqa: BLE001
        raise TypeError(f"Cannot derive spec YAML from {type(spec)!r}: {e}") from e


class ObjectTransformer:
    """Subset of ``linkml_map.ObjectTransformer`` backed by the Rust engine.

    Parameters
    ----------
    source_schemaview :
        A linkml_runtime ``SchemaView``, a path, or YAML text.
    specification :
        A dict, a ``TransformationSpecification``, a path, or YAML text. May also
        be supplied later via :meth:`create_transformer_specification`.
    target_schemaview :
        Optional target schema (same accepted forms as ``source_schemaview``).
    """

    def __init__(
        self,
        source_schemaview: Any = None,
        specification: Any = None,
        target_schemaview: Any = None,
    ) -> None:
        self.source_schemaview = source_schemaview
        self.target_schemaview = target_schemaview
        self.specification = specification
        self._rust: Optional[Any] = None

    def create_transformer_specification(self, obj: Any) -> None:
        """Set the specification (dict or object). Mirrors the upstream method."""
        self.specification = obj
        self._rust = None  # rebuild on next call

    def index(self, source_obj: Any, target: Optional[str] = None) -> None:
        """No-op: the Rust engine builds any foreign-key index internally."""
        return None

    def _ensure(self) -> None:
        if self._rust is not None:
            return
        if self.source_schemaview is None:
            raise ValueError("source_schemaview is required before map_object")
        schema_yaml = _schema_to_yaml(self.source_schemaview)
        spec_yaml = _spec_to_yaml(self.specification)
        target_yaml = (
            _schema_to_yaml(self.target_schemaview)
            if self.target_schemaview is not None
            else None
        )
        self._rust = linkml_map_rs.Transformer.from_yaml(
            schema_yaml, spec_yaml, target_yaml, None
        )

    def map_object(self, source_obj: Any, source_type: Optional[str] = None) -> Any:
        """Transform one object (dict) → dict. Schema/spec parsed once, then cached."""
        self._ensure()
        return self._rust.map_object(source_obj, source_type)
