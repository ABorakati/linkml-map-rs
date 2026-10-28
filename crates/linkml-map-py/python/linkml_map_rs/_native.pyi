"""Type stubs for the compiled `linkml_map_rs._native` module.

This stub is part of the THREE-surface API contract enforced by the harness
sync gate (see harness-ownership.toml): any change to the PyO3 binding
(`crates/linkml-map-py/src/lib.rs`) must be reflected here so Python callers and
type checkers see the real shape. Keep signatures in lockstep with the
`#[pymethods]` / `#[pyfunction]` definitions.
"""

import os
from typing import Any

class PathNotFoundError(FileNotFoundError, ValueError):
    """A schema or specification path does not exist.

    Both a ``FileNotFoundError`` (what upstream ``open(path)`` /
    ``SchemaView(path)`` raise) and a ``ValueError`` (what this binding raised
    for a bad path before it accepted ``os.PathLike``), so either ``except``
    clause catches it.
    """

class Transformer:
    """Reusable transformer: loads schema(s) + spec once, then transforms objects.

    Mirrors `linkml_map_core::engine::ObjectTransformer` over the PyO3 boundary.

    On construction the spec is *derived*: implicit cross-table ``{Table.col}``
    references (in expressions or dotted ``populated_from``) are resolved into
    explicit joins against the source schema, mirroring upstream
    ``Transformer.derived_specification``. Top-level ``slot_derivations`` are
    supported for this check. A reference that cannot be keyed, resolved, or
    hosted raises ``ValueError`` here rather than silently yielding ``null`` at
    transform time. Expression ASTs are pre-compiled and cached once at build time.
    Synthesis runs once per constructed transformer, so ``transform`` / ``transform_many``
    reuse the derived spec and compiled expression AST cache.
    """

    def __init__(
        self,
        source_schema: str | os.PathLike[str],
        spec: str | os.PathLike[str],
        target_schema: str | os.PathLike[str] | None = ...,
        source_class: str | None = ...,
        *,
        strict: bool = ...,
        unrestricted_eval: bool = ...,
    ) -> None:
        """Load the schema(s) and spec from file paths (``str`` or ``pathlib.Path``).

        A missing file raises ``PathNotFoundError``. ``strict`` mirrors upstream
        ``ObjectTransformer.strict``: an expression reference that does not
        resolve raises instead of yielding null. ``unrestricted_eval`` mirrors
        upstream ``ObjectTransformer.unrestricted_eval`` and defaults to
        ``False`` (the safe, restricted evaluator): ``expr:`` blocks of Python
        statements then run only their first statement and see no ``src``. Set it
        to ``True`` to run such blocks with the source object as ``src``; an
        expression the restricted evaluator rejects then yields null instead of
        raising. Only enable it for trusted specifications.
        """
        ...
    @staticmethod
    def from_yaml(
        source_schema_yaml: str,
        spec_yaml: str,
        target_schema_yaml: str | None = ...,
        source_class: str | None = ...,
        *,
        strict: bool = ...,
        unrestricted_eval: bool = ...,
    ) -> "Transformer":
        """Build from in-memory YAML strings (used by the `linkml_map` shim)."""
        ...
    @staticmethod
    def from_python(
        source_schemaview: Any,
        spec: Any,
        target_schemaview: Any | None = ...,
        source_class: str | None = ...,
        *,
        strict: bool = ...,
        unrestricted_eval: bool = ...,
    ) -> "Transformer":
        """Build directly from Python SchemaView / specification objects.

        ``source_schemaview`` / ``target_schemaview``: a ``SchemaView``, a
        ``SchemaDefinition``, a schema file path (``os.PathLike``, or a ``str``
        naming an existing file) or schema YAML text. ``spec``: a dict, a
        ``linkml_runtime`` dataclass, a spec file path (``os.PathLike``, or a
        ``str`` naming an existing file) or spec YAML text. An ``os.PathLike`` is
        always a path (``PathNotFoundError`` if missing, never YAML text).
        """
        ...
    def transform(self, obj: dict[str, Any]) -> dict[str, Any]:
        """Transform a single object.

        Values cross the boundary by direct conversion (no JSON round trip):
        ``None``/``bool``/``int`` (of any size)/``float``/``complex``/``str``/
        ``list``/``dict`` keep their types and dict order. A slot with ``range: decimal`` comes back as an
        exact ``decimal.Decimal`` (``"52.30"`` -> ``Decimal("52.30")``, digits
        and exponent kept), and a ``decimal.Decimal`` in ``obj`` stays exact. Any
        other non-JSON scalar (``datetime.date``, ``pathlib.Path``, ...) is
        stringified with ``str()``.
        """
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
    source_schema: str | os.PathLike[str],
    spec: str | os.PathLike[str],
    target_schema: str | os.PathLike[str] | None = ...,
    source_class: str | None = ...,
    strict: bool = ...,
    unrestricted_eval: bool = ...,
) -> dict[str, Any]:
    """Transform one object; schema(s) + spec loaded on every call.

    Implicit cross-table joins are resolved once per call (see ``Transformer``);
    an un-keyable / un-hostable reference raises ``ValueError``.
    """
    ...

def transform_objects(
    source_objs: list[dict[str, Any]],
    *,
    source_schema: str | os.PathLike[str],
    spec: str | os.PathLike[str],
    target_schema: str | os.PathLike[str] | None = ...,
    source_class: str | None = ...,
    strict: bool = ...,
    unrestricted_eval: bool = ...,
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
    spec: str | os.PathLike[str],
    *,
    source_schema: str | os.PathLike[str] | None = ...,
    target_schema: str | os.PathLike[str] | None = ...,
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

def load_specification(spec: dict[str, Any] | str | os.PathLike[str]) -> dict[str, Any]:
    """Load a transformation specification from a YAML file or a dict.

    Native counterpart of upstream ``linkml_map.utils.loaders.load_specification``.
    A ``str`` / path-like is a file path (never YAML text; ``PathNotFoundError``
    if missing); a dict is normalised like a file's contents. The result is
    validated against the specification datamodel (``ValueError`` if invalid).

    Divergence: upstream returns a pydantic ``TransformationSpecification`` and
    normalises a dict argument in place. This returns the normalised plain dict
    as a NEW dict and leaves the argument untouched. The ``linkml_map`` shim's
    ``load_specification`` wraps it in a dict-backed stand-in and mirrors the
    in-place normalisation.
    """
    ...
