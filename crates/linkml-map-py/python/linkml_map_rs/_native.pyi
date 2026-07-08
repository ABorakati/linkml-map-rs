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

    On construction the spec is *derived*: implicit cross-table ``{Table.col}``
    references (in expressions or dotted ``populated_from``) are resolved into
    explicit joins against the source schema, mirroring upstream
    ``Transformer.derived_specification``. Top-level ``slot_derivations`` are
    supported for this check. A reference that cannot be keyed, resolved, or
    hosted raises ``ValueError`` here rather than silently yielding ``null`` at
    transform time. Synthesis runs once per constructed transformer, so
    ``transform`` / ``transform_many`` reuse the derived spec.
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
    def register_join_table(
        self, name: str, rows: list[dict[str, Any]], key_column: str
    ) -> None:
        """Register (or replace) a joined table's rows for cross-table joins.

        Mirrors upstream ``Transformer.lookup_index``
        (``utils/lookup_index.py``), except rows are supplied directly as
        in-memory dicts rather than loaded from a CSV/TSV/JSON file. Lets a
        spec's ``joins:`` block (explicit, or synthesized from an implicit
        ``{Table.col}`` reference) resolve ``populated_from: "alias.field"`` /
        ``expr: "{alias.field}"`` bindings against real data instead of
        always yielding ``null``.

        ``key_column`` names the column in ``rows`` that join lookups match
        against. Calling again with the same ``name`` replaces that table;
        different names accumulate. Call before ``transform`` /
        ``map_object`` / ``transform_many`` — registrations are not
        retroactive for a call that already ran.
        """
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
    """Transform one object; schema(s) + spec loaded on every call.

    Implicit cross-table joins are resolved once per call (see ``Transformer``);
    an un-keyable / un-hostable reference raises ``ValueError``.
    """
    ...

def transform_objects(
    source_objs: list[dict[str, Any]],
    *,
    source_schema: str,
    spec: str,
    target_schema: str | None = ...,
    source_class: str | None = ...,
) -> list[dict[str, Any]]:
    """Transform a batch; schema(s) + spec loaded once for the batch.

    Implicit cross-table joins are resolved once for the batch (see
    ``Transformer``); an un-keyable / un-hostable reference raises ``ValueError``.
    """
    ...

class ValidationMessage:
    """A single finding from `validate_spec`.

    Mirrors `linkml_map_core::validate::ValidationMessage`. `severity` is the
    lowercased string form (``"error"`` / ``"warning"`` / ``"info"``).
    `__str__`/`__repr__` reproduce the Rust `Display` impl:
    ``"{path}: [{severity}] {message}"``.
    """

    @property
    def severity(self) -> str: ...
    @property
    def path(self) -> str: ...
    @property
    def message(self) -> str: ...
    @property
    def category(self) -> str | None: ...
    def __repr__(self) -> str: ...
    def __str__(self) -> str: ...

def validate_spec(
    spec: str,
    *,
    source_schema: str | None = ...,
    target_schema: str | None = ...,
    strict: bool = ...,
) -> list[ValidationMessage]:
    """Cross-reference a spec against its schema(s) WITHOUT running any transform.

    Native binding for `linkml_map_core::validate::validate_spec_semantics` —
    a read-only pre-flight check that catches an unresolved class/slot/enum
    name, an unresolvable expression reference, a misconfigured join, an
    unresolved ``is_a``/``mixins``, or a required target slot with no
    derivation.

    Returns an empty list when neither `source_schema` nor `target_schema` is
    given — matching the Rust function, there is nothing to check the spec
    against. Validation runs on the spec exactly as parsed: no implicit-join
    synthesis (``Transformer``'s spec derivation) runs first, since the whole
    point of this check is to catch spec problems *before* that stage.
    """
    ...
