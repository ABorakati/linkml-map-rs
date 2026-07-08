"""Proves `Transformer.register_join_table` resolves a cross-table join
end-to-end via the Python API.

Background: `linkml_map_core::normalize::synthesize_implicit_joins` rewrites
implicit `{Table.col}` references into explicit `joins:` entries on the spec
(see `crates/linkml-map-py/src/lib.rs::derive_spec`), but nothing previously
let a Python caller supply the *joined table's row data* — so a declared join
could never resolve to a real value at runtime through the Python API. This
test uses an explicit `joins:` block (simpler to set up than triggering
implicit-join synthesis) and asserts the joined value is a real match, not a
`joins:`-declared-but-unresolved `null`.

Mirrors the Rust-side coverage in
`crates/linkml-map-core/src/engine/mod.rs::lookup_index_tests::
test_join_populated_from_dot_notation`, but driven through the compiled
`linkml_map_rs` extension instead of calling the engine directly.

Runtime deps: `pytest pyyaml`. Run with (after `maturin develop --release`
in `crates/linkml-map-py`):
    pytest crates/linkml-map-py/tests/test_join_registration.py -v
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

from linkml_map_rs import Transformer  # noqa: E402

SOURCE_SCHEMA_YAML = """
id: https://example.org/schemas/patient-join-test
name: patient_join_test
imports:
  - linkml:types
default_range: string
classes:
  Patient:
    tree_root: true
    attributes:
      pid:
        identifier: true
"""

# Explicit `joins:` block: `demo` aliases the `demographics` table (registered
# in-memory at test time via `register_join_table`), keyed on the primary
# object's `pid` slot. `age`/`city` are populated via dotted
# `populated_from: "demo.<field>"` join bindings — exactly the resolution path
# `crates/linkml-map-core/src/engine/mod.rs` wires `LookupIndex` through.
SPEC_YAML = """
id: patient-join-spec
class_derivations:
  Patient:
    populated_from: Patient
    joins:
      demo:
        alias: demo
        class_named: demographics
        source_key: pid
    slot_derivations:
      pid:
      age:
        populated_from: demo.age
      city:
        populated_from: demo.city
"""


def test_register_join_table_resolves_join(tmp_path: pathlib.Path) -> None:
    schema_path = tmp_path / "source.yaml"
    schema_path.write_text(SOURCE_SCHEMA_YAML)
    spec_path = tmp_path / "transform.yaml"
    spec_path.write_text(SPEC_YAML)

    t = Transformer(
        source_schema=str(schema_path),
        spec=str(spec_path),
        source_class="Patient",
    )

    # Before registering the joined table, the join alias binds to Null (a
    # sparse-join miss) so the declared `populated_from: "demo.age"` yields
    # None rather than erroring — prove that's the pre-registration baseline.
    baseline = t.transform({"pid": "P:1"})
    assert baseline == {"pid": "P:1", "age": None, "city": None}

    t.register_join_table(
        "demographics",
        [
            {"patient_id": "P:1", "age": 42, "city": "London"},
            {"patient_id": "P:2", "age": 31, "city": "Leeds"},
        ],
        "patient_id",
    )

    result = t.transform({"pid": "P:1"})
    assert result == {"pid": "P:1", "age": 42, "city": "London"}

    # A different row of the same primary table joins its own matching row.
    result_2 = t.transform({"pid": "P:2"})
    assert result_2 == {"pid": "P:2", "age": 31, "city": "Leeds"}

    # A primary key with no match in the joined table is a legitimate sparse
    # miss (Null), not an error.
    result_missing = t.transform({"pid": "P:404"})
    assert result_missing == {"pid": "P:404", "age": None, "city": None}


def test_register_join_table_replaces_same_name(tmp_path: pathlib.Path) -> None:
    """Calling register_join_table again with the same name replaces the table."""
    schema_path = tmp_path / "source.yaml"
    schema_path.write_text(SOURCE_SCHEMA_YAML)
    spec_path = tmp_path / "transform.yaml"
    spec_path.write_text(SPEC_YAML)

    t = Transformer(
        source_schema=str(schema_path),
        spec=str(spec_path),
        source_class="Patient",
    )

    t.register_join_table(
        "demographics", [{"patient_id": "P:1", "age": 42, "city": "London"}], "patient_id"
    )
    assert t.transform({"pid": "P:1"}) == {"pid": "P:1", "age": 42, "city": "London"}

    # Replace with new data for the same table name.
    t.register_join_table(
        "demographics", [{"patient_id": "P:1", "age": 99, "city": "Bristol"}], "patient_id"
    )
    assert t.transform({"pid": "P:1"}) == {"pid": "P:1", "age": 99, "city": "Bristol"}
