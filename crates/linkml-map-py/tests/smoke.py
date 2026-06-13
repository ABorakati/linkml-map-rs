"""Smoke test for the linkml_map_rs PyO3 wheel.

Uses the PASSING golden fixture: tests/golden/measurements/PersonQuantityValue-001
  source_schema: tests/golden/measurements/source/quantity_value.yaml
  spec:          tests/golden/measurements/transform/qv-to-scalar.transform.yaml
  input:         {"id": "P:1", "height": {"value": 172.0, "unit": "cm"}}
  expected:      {"id": "P:1", "height_in_cm": 172.0}

Run from the linkml-map-rs repo root:
  python crates/linkml-map-py/tests/smoke.py
"""

import pathlib
import sys

# Locate the repo root (two levels up from this file).
REPO = pathlib.Path(__file__).resolve().parents[3]

SOURCE_SCHEMA = str(REPO / "tests/golden/measurements/source/quantity_value.yaml")
SPEC          = str(REPO / "tests/golden/measurements/transform/qv-to-scalar.transform.yaml")

INPUT_OBJ = {
    "id": "P:1",
    "height": {"value": 172.0, "unit": "cm"},
}
EXPECTED = {
    "id": "P:1",
    "height_in_cm": 172.0,
}

INPUT_OBJ_2 = {
    "id": "P:2",
    "height": {"value": 165.5, "unit": "cm"},
}
EXPECTED_2 = {
    "id": "P:2",
    "height_in_cm": 165.5,
}

def _assert_eq(label, actual, expected):
    if actual != expected:
        print(f"FAIL [{label}]")
        print(f"  expected: {expected}")
        print(f"  actual:   {actual}")
        sys.exit(1)
    print(f"  PASS [{label}]: {actual}")


def test_transformer_class():
    from linkml_map_rs import Transformer

    print("\n--- Transformer class ---")
    t = Transformer(source_schema=SOURCE_SCHEMA, spec=SPEC, source_class="Person")
    print(f"  repr: {t!r}")

    result = t.transform(INPUT_OBJ)
    _assert_eq("transform(obj1)", result, EXPECTED)

    results = t.transform_many([INPUT_OBJ, INPUT_OBJ_2])
    _assert_eq("transform_many[0]", results[0], EXPECTED)
    _assert_eq("transform_many[1]", results[1], EXPECTED_2)


def test_free_functions():
    from linkml_map_rs import transform_object, transform_objects

    print("\n--- Free functions ---")
    result = transform_object(
        INPUT_OBJ,
        source_schema=SOURCE_SCHEMA,
        spec=SPEC,
        source_class="Person",
    )
    _assert_eq("transform_object(obj1)", result, EXPECTED)

    results = transform_objects(
        [INPUT_OBJ, INPUT_OBJ_2],
        source_schema=SOURCE_SCHEMA,
        spec=SPEC,
        source_class="Person",
    )
    _assert_eq("transform_objects[0]", results[0], EXPECTED)
    _assert_eq("transform_objects[1]", results[1], EXPECTED_2)


def test_error_propagation():
    """ValueError should be raised (not a crash) for bad inputs."""
    from linkml_map_rs import Transformer

    print("\n--- Error propagation ---")
    try:
        Transformer(
            source_schema="/nonexistent/schema.yaml",
            spec=SPEC,
        )
        print("  FAIL: expected ValueError for missing schema")
        sys.exit(1)
    except ValueError as e:
        print(f"  PASS: got ValueError: {e}")


if __name__ == "__main__":
    print(f"Repo root: {REPO}")
    print(f"Source schema: {SOURCE_SCHEMA}")
    print(f"Spec: {SPEC}")

    test_transformer_class()
    test_free_functions()
    test_error_propagation()

    print("\n=== All smoke tests PASSED ===")
