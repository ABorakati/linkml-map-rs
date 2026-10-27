"""`linkml_map`-compatible ObjectTransformer backed by the Rust engine.

Drop-in for the common ``ObjectTransformer(specification=..., source_schemaview=...)``
+ ``.map_object(obj)`` path. Install this package **instead of** upstream
``linkml-map`` so existing imports resolve here:

    from linkml_map.transformer.object_transformer import ObjectTransformer

Backed by the compiled ``linkml_map_rs`` extension (build with maturin).

Covered: ``map_object``, ``create_transformer_specification``,
``load_transformer_specification``, ``load_source_schema``, ``index`` (no-op —
the Rust engine builds any FK index internally), ``strict``. NOT covered:
SchemaMapper, the inverter, Python/SQL compilers, JSON-schema validation,
``derived_specification``.

Constructor (upstream ``173121b``): ``specification`` may be a loaded
specification, a dict (deep-copied, so the caller's dict is untouched) or a YAML
file path (``str`` / ``Path``); ``source_schemaview`` / ``target_schemaview`` may
be a ``SchemaView``, a ``SchemaDefinition`` or a schema file path. After
construction ``specification`` is a loaded specification and the schema views are
``SchemaView`` objects (or ``None``).

Divergences from upstream, all deliberate:

* the loaded specification is the dict-backed stand-in described in
  ``linkml_map._specification``, not a pydantic model;
* a ``str`` that is not an existing file but contains a newline is read as YAML
  text (a superset kept from earlier releases of this shim; upstream raises
  ``FileNotFoundError``);
* ``spec_messages`` is always an empty list (the deprecation scan that fills it
  upstream is not ported);
* source-schema patches are applied inside the engine, not to the
  ``source_schemaview`` object.
"""

from __future__ import annotations

import copy
import os
from typing import Any, Optional

import linkml_map_rs

from linkml_map._specification import TransformationSpecification
from linkml_map.utils.loaders import load_specification

# Assigning one of these discards the engine built for the previous values.
_ENGINE_INPUTS = frozenset(
    {
        "specification",
        "source_schemaview",
        "target_schemaview",
        "strict",
        "unrestricted_eval",
    }
)


def _is_yaml_text(value: str) -> bool:
    """A ``str`` that names no existing file but spans lines is YAML text, not a path."""
    return not os.path.exists(value) and "\n" in value


def _missing_file(path: str) -> FileNotFoundError:
    # A FileNotFoundError, as upstream raises; also a ValueError, as this shim
    # raised (at map_object time) before it loaded schemas in the constructor.
    return linkml_map_rs.PathNotFoundError(2, "No such file or directory", path)


def _load_schemaview(schema: Any) -> Any:
    """Coerce a schema argument to a ``SchemaView`` (mirrors upstream ``__post_init__``)."""
    if schema is None:
        return None
    from linkml_runtime import SchemaView
    from linkml_runtime.linkml_model import SchemaDefinition

    if isinstance(schema, os.PathLike):
        path = os.fspath(schema)
        if not os.path.exists(path):
            raise _missing_file(path)
        return SchemaView(path)
    if isinstance(schema, str):
        if not os.path.exists(schema) and not _is_yaml_text(schema):
            raise _missing_file(schema)
        return SchemaView(schema)  # a file path, or YAML text
    if isinstance(schema, SchemaDefinition):
        return SchemaView(schema)
    return schema


def _load_specification(spec: Any) -> Any:
    """Coerce a specification argument to a loaded specification.

    A dict is deep-copied first (upstream ``__post_init__``), so the caller's dict
    is not normalised. Anything else (e.g. a ``linkml_runtime`` dataclass) is kept
    as is and handed to the engine, as earlier releases of this shim did.
    """
    if isinstance(spec, TransformationSpecification) or spec is None:
        return spec
    if isinstance(spec, dict):
        return load_specification(copy.deepcopy(spec))
    if isinstance(spec, str) and _is_yaml_text(spec):
        import yaml

        return load_specification(yaml.safe_load(spec))
    if isinstance(spec, (str, os.PathLike)):
        return load_specification(spec)
    return spec


class ObjectTransformer:
    """Subset of ``linkml_map.ObjectTransformer`` backed by the Rust engine.

    Parameters
    ----------
    specification :
        A loaded specification, a dict, or a path to a YAML specification file.
        May also be supplied later via :meth:`create_transformer_specification`
        or :meth:`load_transformer_specification`.
    source_schemaview :
        A linkml_runtime ``SchemaView``, ``SchemaDefinition`` or schema file path.
    target_schemaview :
        Optional target schema (same accepted forms; keyword-only — upstream's
        third positional dataclass field is the private ``_derived_specification``).
    strict :
        Raise on expression references that do not resolve to a schema slot,
        instead of warning and yielding null.
    unrestricted_eval :
        As upstream: ``False`` (the default) uses the restricted evaluator, which
        has no ``src``, runs only the first statement of a block and refuses
        attribute access on comprehension variables; ``True`` also runs
        statement blocks with the source object as ``src``. Forwarded to the
        engine. Only enable it for trusted specifications.
    """

    def __init__(
        self,
        specification: Any = None,
        source_schemaview: Any = None,
        *,
        target_schemaview: Any = None,
        strict: bool = False,
        unrestricted_eval: bool = False,
    ) -> None:
        self._rust: Optional[Any] = None
        self.spec_messages: list[Any] = []
        self.unrestricted_eval = unrestricted_eval
        self.strict = strict
        self.specification = _load_specification(specification)
        self.source_schemaview = _load_schemaview(source_schemaview)
        self.target_schemaview = _load_schemaview(target_schemaview)

    def __setattr__(self, name: str, value: Any) -> None:
        object.__setattr__(self, name, value)
        if name in _ENGINE_INPUTS:
            object.__setattr__(self, "_rust", None)  # rebuild on next call

    def create_transformer_specification(self, obj: Any) -> None:
        """Set the specification from a dict (or object). Mirrors the upstream method.

        PARITY: upstream normalises the dict IN PLACE (``normalize_spec``).
        """
        if isinstance(obj, dict):
            obj = load_specification(obj)
        self.specification = obj

    def load_transformer_specification(self, path: "str | os.PathLike[str]") -> None:
        """Set the specification from a YAML file path. Mirrors the upstream method."""
        self.specification = load_specification(path)

    def load_source_schema(self, path: "str | os.PathLike[str]") -> None:
        """Set ``source_schemaview`` from a schema file path. Mirrors the upstream method."""
        from linkml_runtime import SchemaView

        self.source_schemaview = SchemaView(os.fspath(path))

    def index(self, source_obj: Any, target: Optional[str] = None) -> None:
        """No-op: the Rust engine builds any foreign-key index internally."""
        return None

    def _ensure(self) -> None:
        if self._rust is not None:
            return
        if self.source_schemaview is None:
            raise ValueError("source_schemaview is required before map_object")
        if self.specification is None:
            raise ValueError("specification is required before map_object")
        spec = self.specification
        if isinstance(spec, TransformationSpecification):
            spec = spec.model_dump()
        self._rust = linkml_map_rs.Transformer.from_python(
            self.source_schemaview,
            spec,
            self.target_schemaview,
            None,
            strict=self.strict,
            unrestricted_eval=self.unrestricted_eval,
        )

    def map_object(self, source_obj: Any, source_type: Optional[str] = None) -> Any:
        """Transform one object (dict) → dict. Schema/spec parsed once, then cached."""
        self._ensure()
        return self._rust.map_object(source_obj, source_type)
