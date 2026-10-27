"""A lightweight stand-in for upstream's pydantic ``TransformationSpecification``.

Upstream ``linkml_map.load_specification`` returns a pydantic model from
``linkml_map.datamodel.transformer_model``. This shim must not ship (or import)
that datamodel: upstream ``linkml-map`` imports as ``linkml_map`` too, and the
Rust engine owns the real datamodel. The closest drop-in is a thin view over the
*normalised* specification dict:

* attribute access (``spec.class_derivations[0].name``) over nested mappings and
  lists, as with the pydantic model;
* ``model_dump()`` round-trips back to a plain dict that ``ObjectTransformer``
  (and ``linkml_map_rs.Transformer.from_python``) accept;
* equality against another specification or a dict.

Divergences from pydantic (deliberate, documented in the crate README):

* no validation or defaults are applied in Python (the Rust engine validates the
  specification when ``load_specification`` runs, and applies defaults when it
  transforms), so ``model_dump()`` has exactly the keys the spec spelled out;
* an unset field reads as ``None`` (as for a pydantic optional field), but so
  does a misspelt one, where pydantic raises ``AttributeError``;
* a list attribute is a fresh list of views over the same underlying dicts:
  mutating an item through attribute assignment writes through, appending to the
  returned list does not.
"""

from __future__ import annotations

import copy
from typing import Any, Iterator


def _wrap(value: Any) -> Any:
    if isinstance(value, dict):
        return _SpecNode(value)
    if isinstance(value, list):
        return [_wrap(v) for v in value]
    return value


def _unwrap(value: Any) -> Any:
    if isinstance(value, _SpecNode):
        return value._data
    if isinstance(value, list):
        return [_unwrap(v) for v in value]
    return value


class _SpecNode:
    """Attribute-style view over a (nested) specification mapping."""

    __slots__ = ("_data",)

    def __init__(self, data: dict[str, Any]) -> None:
        object.__setattr__(self, "_data", data)

    def __getattr__(self, name: str) -> Any:
        if name.startswith("_"):
            raise AttributeError(name)
        return _wrap(self._data.get(name))

    def __setattr__(self, name: str, value: Any) -> None:
        self._data[name] = _unwrap(value)

    def __getitem__(self, name: str) -> Any:
        return _wrap(self._data[name])

    def __contains__(self, name: object) -> bool:
        return name in self._data

    def __iter__(self) -> Iterator[str]:
        return iter(self._data)

    def __eq__(self, other: object) -> bool:
        if isinstance(other, _SpecNode):
            return self._data == other._data
        if isinstance(other, dict):
            return self._data == other
        return NotImplemented

    __hash__ = None  # type: ignore[assignment]  # mutable, like a pydantic model

    def __deepcopy__(self, memo: dict[int, Any]) -> _SpecNode:
        return type(self)(copy.deepcopy(self._data, memo))

    def __repr__(self) -> str:
        return f"{type(self).__name__}({self._data!r})"

    def model_dump(self, **_: Any) -> dict[str, Any]:
        """Return the specification as a plain dict (a deep copy)."""
        return copy.deepcopy(self._data)


class TransformationSpecification(_SpecNode):
    """The loaded, normalised specification (see the module docstring)."""

    __slots__ = ()

    def __init__(self, **data: Any) -> None:
        super().__init__(data)
