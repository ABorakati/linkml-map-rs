"""Type stubs for the compiled `linkml_map_rs._native` module.

This stub is part of the THREE-surface API contract enforced by the harness
sync gate (see harness-ownership.toml): any change to the PyO3 binding
(`crates/linkml-map-py/src/lib.rs`) must be reflected here so Python callers and
type checkers see the real shape. Keep signatures in lockstep with the
`#[pymethods]` / `#[pyfunction]` definitions.
"""

from typing import Any

class Transformer:
    """Reusable transformer: loads schema(s) + spec once, then transforms objects.

    Mirrors `linkml_map_core::engine::ObjectTransformer` over the PyO3 boundary.
    """

    def __init__(
        self,
        source_schema: str,
        spec: str,
        target_schema: str | None = ...,
        source_class: str | None = ...,
    ) -> None: ...
    @staticmethod
    def from_yaml(
        source_schema_yaml: str,
        spec_yaml: str,
        target_schema_yaml: str | None = ...,
        source_class: str | None = ...,
    ) -> "Transformer":
        """Build from in-memory YAML strings (used by the `linkml_map` shim)."""
        ...
    @staticmethod
    def from_python(
        source_schemaview: Any,
        spec: Any,
        target_schemaview: Any | None = ...,
        source_class: str | None = ...,
    ) -> "Transformer":
        """Build directly from Python SchemaView / specification objects."""
        ...
    def transform(self, obj: dict[str, Any]) -> dict[str, Any]:
        """Transform a single object."""
        ...
    def map_object(
        self, obj: dict[str, Any], source_type: str | None = ...
    ) -> dict[str, Any]:
        """Alias of `transform` matching `ObjectTransformer.map_object`."""
        ...
    def transform_many(self, objs: list[dict[str, Any]]) -> list[dict[str, Any]]:
        """Transform a list of objects."""
        ...
    def __repr__(self) -> str: ...

def transform_object(
    source_obj: dict[str, Any],
    *,
    source_schema: str,
    spec: str,
    target_schema: str | None = ...,
    source_class: str | None = ...,
) -> dict[str, Any]:
    """Transform one object; schema(s) + spec loaded on every call."""
    ...

def transform_objects(
    source_objs: list[dict[str, Any]],
    *,
    source_schema: str,
    spec: str,
    target_schema: str | None = ...,
    source_class: str | None = ...,
) -> list[dict[str, Any]]:
    """Transform a batch; schema(s) + spec loaded once for the batch."""
    ...
