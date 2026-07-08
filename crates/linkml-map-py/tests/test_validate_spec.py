"""Proves `linkml_map_rs.validate_spec` cross-references a spec against its
schema(s) end-to-end via the Python API.

Background: `linkml_map_core::validate::validate_spec_semantics` (native, 29
Rust-side tests) is a read-only pre-flight check — it is not wired into the
transform path. This test exercises the PyO3 binding added for it
(`crates/linkml-map-py/src/lib.rs::validate_spec` + the `ValidationMessage`
pyclass), not the underlying Rust logic itself (already covered by
`crates/linkml-map-core/src/validate/tests.rs`).

Runtime deps: `pytest pyyaml`. Run with (after `maturin develop --release`
in `crates/linkml-map-py`):
    pytest crates/linkml-map-py/tests/test_validate_spec.py -v
"""

from __future__ import annotations

import pathlib

import pytest

linkml_map_rs = pytest.importorskip(
    "linkml_map_rs",
    reason=(
        "linkml_map_rs native extension is not importable — build/install the "
        "maturin wheel first (crates/linkml-map-py). This is an environment "
        "constraint, not a test failure."
    ),
)

from linkml_map_rs import ValidationMessage, validate_spec  # noqa: E402

# Repo root: crates/linkml-map-py/tests/test_validate_spec.py -> repo root.
REPO_ROOT = pathlib.Path(__file__).resolve().parents[3]
GOLDEN = REPO_ROOT / "tests" / "golden" / "personinfo_basic"


def test_validate_spec_known_good_fixture_has_no_errors() -> None:
    """The personinfo_basic golden fixture is a real, working transform — it
    must produce no error-severity messages when validated against both its
    source and target schema.
    """
    messages = validate_spec(
        str(GOLDEN / "transform" / "personinfo-to-agent.transform.yaml"),
        source_schema=str(GOLDEN / "source" / "personinfo.yaml"),
        target_schema=str(GOLDEN / "target" / "agent.yaml"),
    )
    assert all(isinstance(m, ValidationMessage) for m in messages)
    errors = [m for m in messages if m.severity == "error"]
    assert errors == [], f"unexpected error-severity messages: {[str(m) for m in errors]}"


def test_validate_spec_no_schemas_returns_empty() -> None:
    """Matches the Rust function: with neither schema supplied, there is
    nothing to check the spec against, so this returns [] rather than
    erroring.
    """
    messages = validate_spec(
        str(GOLDEN / "transform" / "personinfo-to-agent.transform.yaml"),
    )
    assert messages == []


def test_validate_spec_detects_unresolved_target_class(tmp_path: pathlib.Path) -> None:
    """A class_derivation naming a class absent from the target schema is a
    hard error — the runtime would otherwise silently drop the whole class.
    """
    source_schema = tmp_path / "source.yaml"
    source_schema.write_text(
        """
id: https://example.org/schemas/validate-source-test
name: validate_source_test
imports:
  - linkml:types
default_range: string
classes:
  Foo:
    tree_root: true
    attributes:
      bar:
        range: string
"""
    )
    target_schema = tmp_path / "target.yaml"
    target_schema.write_text(
        """
id: https://example.org/schemas/validate-target-test
name: validate_target_test
imports:
  - linkml:types
default_range: string
classes:
  Widget:
    attributes:
      baz:
        range: string
"""
    )
    spec_path = tmp_path / "transform.yaml"
    spec_path.write_text(
        """
id: validate-spec-test
class_derivations:
  BadClass:
    populated_from: Foo
    slot_derivations:
      baz:
        populated_from: bar
"""
    )

    messages = validate_spec(
        str(spec_path),
        source_schema=str(source_schema),
        target_schema=str(target_schema),
    )

    errors = [m for m in messages if m.severity == "error"]
    assert errors, f"expected at least one error, got: {[str(m) for m in messages]}"
    assert any(
        m.path == "class_derivations[BadClass]" and "not found in target schema" in m.message
        for m in errors
    )
    # __str__ reproduces the Rust Display impl: "{path}: [{severity}] {message}".
    assert str(errors[0]) == f"{errors[0].path}: [{errors[0].severity}] {errors[0].message}"
