"""Mirror of upstream ``linkml_map.utils.loaders``."""

from __future__ import annotations

import copy
import os
from typing import Any

import linkml_map_rs

from linkml_map._specification import TransformationSpecification


def load_specification(
    spec: TransformationSpecification | dict[str, Any] | os.PathLike[str] | str,
) -> TransformationSpecification:
    """Load a transformation specification from a YAML file, a dict, or an existing specification.

    Mirrors upstream ``linkml_map.utils.loaders.load_specification``. A ``str``
    or ``Path`` is a file path (``FileNotFoundError`` if missing), a dict is
    normalised IN PLACE, as a file's contents are, and an existing specification
    is returned as is.

    Divergence: upstream returns a pydantic ``TransformationSpecification``; this
    shim returns a dict-backed stand-in (see ``linkml_map._specification``) with
    attribute access and ``model_dump()``, which ``ObjectTransformer`` accepts.

    >>> spec = load_specification({"class_derivations": {"Agent": {"populated_from": "Person"}}})
    >>> spec.class_derivations[0].name
    'Agent'
    >>> load_specification(spec) is spec
    True
    """
    if isinstance(spec, TransformationSpecification):
        return spec
    # PARITY: upstream linkml_map.utils.loaders.load_specification runs
    # normalize_spec(spec) on the caller's dict, so the dict is rewritten in place.
    normalised = linkml_map_rs.load_specification(spec)
    if isinstance(spec, dict):
        spec.clear()
        spec.update(copy.deepcopy(normalised))
    return TransformationSpecification(**normalised)
