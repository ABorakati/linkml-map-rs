"""The packages' public API.

Ports upstream `tests/test_public_api.py` (e82db08) for the `linkml_map` shim,
and applies the same check to the native `linkml_map_rs` package.
"""

import pytest

linkml_map = pytest.importorskip(
    "linkml_map",
    reason="linkml_map (Rust shim) is not importable; build and install the maturin wheel first.",
)
linkml_map_rs = pytest.importorskip(
    "linkml_map_rs",
    reason="linkml_map_rs native extension is not importable; build and install the maturin wheel first.",
)


def test_all_names_resolve():
    """Every name in ``__all__`` exists, so ``from linkml_map import *`` works."""
    assert [name for name in linkml_map.__all__ if not hasattr(linkml_map, name)] == []


def test_native_all_names_resolve():
    """Every name in ``linkml_map_rs.__all__`` exists, so ``from linkml_map_rs import *`` works."""
    assert [name for name in linkml_map_rs.__all__ if not hasattr(linkml_map_rs, name)] == []


def test_star_imports_work():
    namespace: dict = {}
    exec("from linkml_map import *", namespace)  # noqa: S102
    assert {"ObjectTransformer", "load_specification"} <= namespace.keys()
    exec("from linkml_map_rs import *", namespace)  # noqa: S102
    assert {"Transformer", "load_specification", "PathNotFoundError"} <= namespace.keys()


def test_shim_matches_upstream_all():
    """Upstream `linkml_map.__all__` at v0.6.0-rc1."""
    assert sorted(linkml_map.__all__) == ["ObjectTransformer", "load_specification"]
