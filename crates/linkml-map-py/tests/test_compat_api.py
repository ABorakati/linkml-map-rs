"""Python drop-in compat test corpus for the linkml-map-rs PyO3 bindings.

Exercises the Rust-backed `linkml_map_rs` native extension AND the
`linkml_map`-compatible shim (`linkml_map.transformer.object_transformer.
ObjectTransformer`) against the SAME golden fixtures the Rust conformance
runner uses (`crates/linkml-map-conformance/src/lib.rs::discover_fixtures`,
`tests/golden/*/{data,output,transform,source}`).

Those golden `output/*.transformed.yaml` files are byte-identical to
upstream Python `linkml-map` (see repo CLAUDE.md — "Correct = byte-identical
to upstream Python"). Driving the Rust-backed engine through the same inputs
and asserting equality against those files is therefore a BEHAVIORAL parity
check against Python, not merely a shape/schema check.

Import hygiene
---------------
This module imports ONLY:
  - `linkml_map_rs` (the compiled Rust extension / native package)
  - `linkml_map`    (the Rust-backed shim under crates/linkml-map-py/python/linkml_map)
  - stdlib, pyyaml, linkml_runtime (SchemaView convenience only)

It must NEVER import `linkml_map.compiler`, `linkml_map.datamodel`,
`linkml_map.inference`, or any other upstream-`linkml-map`-only submodule —
those do not exist in the Rust shim, and if upstream `linkml-map` is
co-installed such an import would silently start testing upstream Python
instead of this wheel (see `_workspace/00_contract.md`).

Running
-------
    pytest crates/linkml-map-py/tests/test_compat_api.py -v

Runtime deps for this leg: `pytest pyyaml linkml-runtime`. Do **NOT** install
upstream `linkml-map` / `linkml` alongside these tests — `import linkml_map`
must resolve to the Rust shim in `crates/linkml-map-py/python/linkml_map`,
not to upstream.
"""

from __future__ import annotations

import pathlib
from typing import Any

import pytest

yaml = pytest.importorskip("yaml", reason="pyyaml is required for this test leg")

linkml_map_rs = pytest.importorskip(
    "linkml_map_rs",
    reason=(
        "linkml_map_rs native extension is not importable — build/install the "
        "maturin wheel first (crates/linkml-map-py). This is an environment "
        "constraint, not a test failure."
    ),
)
linkml_map = pytest.importorskip(
    "linkml_map",
    reason=(
        "linkml_map (Rust shim) is not importable. If upstream `linkml-map` is "
        "installed instead of this repo's shim, UNINSTALL it — this leg must "
        "never test upstream Python."
    ),
)

# Guard against accidentally resolving to upstream `linkml-map`: the upstream
# package exposes `linkml_map.compiler` / `linkml_map.datamodel` /
# `linkml_map.inference` submodules that the Rust shim intentionally does not
# provide. If any of those import cleanly, `linkml_map` is NOT this repo's
# shim and every downstream assertion here would be meaningless.
_UPSTREAM_ONLY_SUBMODULES = ("compiler", "datamodel", "inference")


def _looks_like_upstream_linkml_map() -> bool:
    import importlib

    for sub in _UPSTREAM_ONLY_SUBMODULES:
        try:
            importlib.import_module(f"linkml_map.{sub}")
        except ImportError:
            continue
        else:
            return True
    return False


if _looks_like_upstream_linkml_map():
    pytest.skip(
        "`linkml_map` resolves to upstream Python linkml-map (found "
        f"{_UPSTREAM_ONLY_SUBMODULES!r} submodule(s)), not the Rust shim in "
        "crates/linkml-map-py/python/linkml_map. Uninstall upstream "
        "`linkml-map`/`linkml` from this environment before running this leg.",
        allow_module_level=True,
    )

from linkml_map.transformer.object_transformer import ObjectTransformer  # noqa: E402
from linkml_map_rs import Transformer, transform_object, transform_objects  # noqa: E402

# ── Fixture discovery (mirrors crates/linkml-map-conformance/src/lib.rs) ──────
#
# Layout per tests/README.md and the Rust `discover_fixtures`:
#   tests/golden/<fixture>/
#     data/           input YAML/JSON, filenames like "<Stem>-001.yaml"
#     output/         expected output, "<stem>.transformed.yaml" (or .json)
#     source/ | model/  source schema YAML (first YAML file found)
#     transform/      transform spec YAML (first YAML file found)
#
# `source_class_hint` = stem split on '-' before the first '-', e.g.
# "Person-001" -> "Person". The engine (and this corpus) only actually USE
# the hint as `source_type` when it matches a class derivation's
# `populated_from` (or bare `name` when `populated_from` is absent) — exactly
# mirroring `run_case_inner`'s `resolved_source_type` logic in lib.rs. When it
# doesn't match, `source_type=None` and the engine infers from the schema's
# `tree_root` class / the spec's sole class derivation.

REPO_ROOT = pathlib.Path(__file__).resolve().parents[3]
GOLDEN_ROOT = REPO_ROOT / "tests" / "golden"


def _find_yaml_file(directory: pathlib.Path) -> pathlib.Path | None:
    if not directory.is_dir():
        return None
    candidates = sorted(
        p for p in directory.iterdir() if p.is_file() and p.suffix in (".yaml", ".yml", ".json")
    )
    return candidates[0] if candidates else None


def _collect_yaml_files(directory: pathlib.Path) -> list[pathlib.Path]:
    if not directory.is_dir():
        return []
    return sorted(
        p for p in directory.iterdir() if p.is_file() and p.suffix in (".yaml", ".yml", ".json")
    )


def _best_source_schema(directory: pathlib.Path) -> pathlib.Path | None:
    """Pick the schema YAML with the most top-level `classes:` — mirrors the
    Rust runner's `load_source_schema` fallback (crates/linkml-map-conformance
    /src/lib.rs), which scans the sibling directory and prefers whichever
    candidate yields the most classes. Some fixtures (e.g. `flattening/`) ship
    a truncated/alternate schema (`cmdr_normalized.yaml`, 0 classes) alongside
    the real one (`normalized.yaml`) — naively taking the alphabetically-first
    file would silently pick the empty stub.
    """
    candidates = _collect_yaml_files(directory)
    if not candidates:
        return None
    if len(candidates) == 1:
        return candidates[0]

    best_path: pathlib.Path | None = None
    best_count = -1
    for cand in candidates:
        try:
            doc = yaml.safe_load(cand.read_text(encoding="utf-8"))
        except yaml.YAMLError:
            continue
        n_classes = len((doc or {}).get("classes") or {}) if isinstance(doc, dict) else 0
        if n_classes > best_count:
            best_count = n_classes
            best_path = cand
    return best_path or candidates[0]


class GoldenCase:
    """One (input, expected-output, schema, transform) tuple from tests/golden/."""

    def __init__(
        self,
        name: str,
        input_path: pathlib.Path,
        expected_path: pathlib.Path,
        source_schema_path: pathlib.Path | None,
        transform_path: pathlib.Path,
        source_class_hint: str | None,
    ) -> None:
        self.name = name
        self.input_path = input_path
        self.expected_path = expected_path
        self.source_schema_path = source_schema_path
        self.transform_path = transform_path
        self.source_class_hint = source_class_hint

    def __repr__(self) -> str:  # pytest id readability
        return self.name

    def load_input(self) -> Any:
        with open(self.input_path, encoding="utf-8") as f:
            if self.input_path.suffix == ".json":
                import json

                return json.load(f)
            return yaml.safe_load(f)

    def load_expected(self) -> Any:
        with open(self.expected_path, encoding="utf-8") as f:
            if self.expected_path.suffix == ".json":
                import json

                return json.load(f)
            return yaml.safe_load(f)

    def resolve_source_type(self, spec_dict: dict) -> str | None:
        """Mirror `run_case_inner`'s `resolved_source_type` (lib.rs) exactly:
        use the filename-derived hint only if it matches a class derivation's
        `populated_from` (or bare name when `populated_from` is absent).
        """
        hint = self.source_class_hint
        if hint is None:
            return None
        class_derivations = spec_dict.get("class_derivations") or {}
        for cd_name, cd_body in class_derivations.items():
            cd_body = cd_body or {}
            populated_from = cd_body.get("populated_from")
            if populated_from == hint:
                return hint
            if populated_from is None and cd_name == hint:
                return hint
        return None


def discover_golden_cases(root: pathlib.Path) -> list[GoldenCase]:
    """Python re-implementation of `discover_fixtures` in
    crates/linkml-map-conformance/src/lib.rs — walks one level of
    subdirectories under `root`, requiring data/, output/, transform/.
    """
    cases: list[GoldenCase] = []
    if not root.is_dir():
        return cases

    for fixture_dir in sorted(p for p in root.iterdir() if p.is_dir()):
        data_dir = fixture_dir / "data"
        output_dir = fixture_dir / "output"
        transform_dir = fixture_dir / "transform"
        if not (data_dir.is_dir() and output_dir.is_dir() and transform_dir.is_dir()):
            continue

        transform_path = _find_yaml_file(transform_dir)
        if transform_path is None:
            continue

        source_schema_path = None
        for sub in ("source", "model"):
            candidate_dir = fixture_dir / sub
            if candidate_dir.is_dir():
                source_schema_path = _best_source_schema(candidate_dir)
                if source_schema_path is not None:
                    break

        for input_path in _collect_yaml_files(data_dir):
            stem = input_path.stem
            expected_path = output_dir / f"{stem}.transformed.yaml"
            if not expected_path.exists():
                expected_json = output_dir / f"{stem}.transformed.json"
                if not expected_json.exists():
                    continue
                expected_path = expected_json

            source_class_hint = stem.split("-")[0] if "-" in stem else stem

            cases.append(
                GoldenCase(
                    name=f"golden/{fixture_dir.name}/{stem}",
                    input_path=input_path,
                    expected_path=expected_path,
                    source_schema_path=source_schema_path,
                    transform_path=transform_path,
                    source_class_hint=source_class_hint,
                )
            )
    return cases


GOLDEN_CASES = discover_golden_cases(GOLDEN_ROOT)

# Fixtures the shim's single-object `ObjectTransformer.map_object` path
# genuinely cannot express, keyed by case `.name`. Every entry MUST carry a
# reason; skip must be explicit (contract: "NEVER omit silently").
SHIM_UNSUPPORTED: dict[str, str] = {
    # (none identified: every discovered golden case is a single source
    # object -> single target object with a source schema on disk, which
    # `ObjectTransformer(source_schemaview=<path>, specification=<path>)` +
    # `.map_object(obj, source_type)` expresses directly.)
}


def _case_id(case: GoldenCase) -> str:
    return case.name


# ── 1. Behavioral parity via golden fixtures ──────────────────────────────────


@pytest.mark.skipif(not GOLDEN_CASES, reason="no golden fixtures discovered under tests/golden/")
@pytest.mark.parametrize("case", GOLDEN_CASES, ids=_case_id)
def test_golden_native_transformer_matches_expected(case: GoldenCase) -> None:
    """Drive `linkml_map_rs.Transformer` (native entry point) through a golden
    fixture and assert the result equals the byte-identical-to-Python
    expected output.
    """
    if case.source_schema_path is None:
        pytest.skip(f"{case.name}: no source schema discovered (schema-optional fixture)")

    spec_dict = yaml.safe_load(case.transform_path.read_text(encoding="utf-8"))
    source_type = case.resolve_source_type(spec_dict)

    transformer = Transformer(
        source_schema=str(case.source_schema_path),
        spec=str(case.transform_path),
        source_class=source_type,
    )
    input_obj = case.load_input()
    actual = transformer.map_object(input_obj, source_type)
    expected = case.load_expected()

    assert actual == expected, (
        f"{case.name}: native Transformer output diverges from byte-identical "
        f"Python golden output.\nexpected: {expected!r}\nactual:   {actual!r}"
    )


@pytest.mark.skipif(not GOLDEN_CASES, reason="no golden fixtures discovered under tests/golden/")
@pytest.mark.parametrize("case", GOLDEN_CASES, ids=_case_id)
def test_golden_shim_object_transformer_matches_expected(case: GoldenCase) -> None:
    """Drive the `linkml_map`-compatible shim's `ObjectTransformer.map_object`
    through the SAME golden fixture and assert the same byte-identical-to-
    Python expected output. This is what upstream `linkml-map` consumers
    actually call, so it is the more important of the two parity checks.
    """
    if case.name in SHIM_UNSUPPORTED:
        pytest.skip(f"{case.name}: shim cannot express this fixture — {SHIM_UNSUPPORTED[case.name]}")
    if case.source_schema_path is None:
        pytest.skip(f"{case.name}: no source schema discovered (schema-optional fixture)")

    spec_dict = yaml.safe_load(case.transform_path.read_text(encoding="utf-8"))
    source_type = case.resolve_source_type(spec_dict)

    ot = ObjectTransformer(
        source_schemaview=str(case.source_schema_path),
        specification=str(case.transform_path),
    )
    input_obj = case.load_input()
    actual = ot.map_object(input_obj, source_type)
    expected = case.load_expected()

    assert actual == expected, (
        f"{case.name}: shim ObjectTransformer.map_object output diverges from "
        f"byte-identical Python golden output.\nexpected: {expected!r}\nactual:   {actual!r}"
    )


@pytest.mark.skipif(not GOLDEN_CASES, reason="no golden fixtures discovered under tests/golden/")
@pytest.mark.parametrize("case", GOLDEN_CASES, ids=_case_id)
def test_golden_shim_specification_as_dict_matches_expected(case: GoldenCase) -> None:
    """Same fixture, but exercise `ObjectTransformer(specification=<dict>)` +
    `create_transformer_specification` (the in-memory dict path upstream
    callers commonly use instead of a spec file path).
    """
    if case.name in SHIM_UNSUPPORTED:
        pytest.skip(f"{case.name}: shim cannot express this fixture — {SHIM_UNSUPPORTED[case.name]}")
    if case.source_schema_path is None:
        pytest.skip(f"{case.name}: no source schema discovered (schema-optional fixture)")

    spec_dict = yaml.safe_load(case.transform_path.read_text(encoding="utf-8"))
    source_type = case.resolve_source_type(spec_dict)

    ot = ObjectTransformer(source_schemaview=str(case.source_schema_path))
    ot.create_transformer_specification(spec_dict)

    input_obj = case.load_input()
    actual = ot.map_object(input_obj, source_type)
    expected = case.load_expected()

    assert actual == expected, (
        f"{case.name}: shim with dict specification diverges from expected.\n"
        f"expected: {expected!r}\nactual:   {actual!r}"
    )


# ── 2. Public-surface coverage ────────────────────────────────────────────────


def _measurements_case() -> GoldenCase:
    for case in GOLDEN_CASES:
        if case.name == "golden/measurements/PersonQuantityValue-001":
            return case
    pytest.skip("golden/measurements/PersonQuantityValue-001 fixture not found")


class TestTransformerClass:
    """`linkml_map_rs.Transformer` construction + `.transform` / `.transform_many` / `__repr__`."""

    def test_construction_and_transform(self) -> None:
        case = _measurements_case()
        t = Transformer(
            source_schema=str(case.source_schema_path),
            spec=str(case.transform_path),
            source_class="Person",
        )
        result = t.transform(case.load_input())
        assert result == case.load_expected()

    def test_transform_many(self) -> None:
        case = _measurements_case()
        second_input_path = case.input_path.parent / "PersonQuantityValue-002.yaml"
        second_expected_path = case.expected_path.parent / "PersonQuantityValue-002.transformed.yaml"
        if not (second_input_path.exists() and second_expected_path.exists()):
            pytest.skip("PersonQuantityValue-002 fixture files not found")

        t = Transformer(
            source_schema=str(case.source_schema_path),
            spec=str(case.transform_path),
            source_class="Person",
        )
        obj1 = case.load_input()
        obj2 = yaml.safe_load(second_input_path.read_text(encoding="utf-8"))
        results = t.transform_many([obj1, obj2])

        expected1 = case.load_expected()
        expected2 = yaml.safe_load(second_expected_path.read_text(encoding="utf-8"))
        assert results == [expected1, expected2]

    def test_repr(self) -> None:
        case = _measurements_case()
        t = Transformer(
            source_schema=str(case.source_schema_path),
            spec=str(case.transform_path),
            source_class="Person",
        )
        r = repr(t)
        assert "Transformer(" in r
        assert "source_class" in r


class TestFreeFunctions:
    """Module-level `transform_object` / `transform_objects`."""

    def test_transform_object(self) -> None:
        case = _measurements_case()
        result = transform_object(
            case.load_input(),
            source_schema=str(case.source_schema_path),
            spec=str(case.transform_path),
            source_class="Person",
        )
        assert result == case.load_expected()

    def test_transform_objects_batch(self) -> None:
        case = _measurements_case()
        second_input_path = case.input_path.parent / "PersonQuantityValue-002.yaml"
        second_expected_path = case.expected_path.parent / "PersonQuantityValue-002.transformed.yaml"
        if not (second_input_path.exists() and second_expected_path.exists()):
            pytest.skip("PersonQuantityValue-002 fixture files not found")

        obj1 = case.load_input()
        obj2 = yaml.safe_load(second_input_path.read_text(encoding="utf-8"))
        results = transform_objects(
            [obj1, obj2],
            source_schema=str(case.source_schema_path),
            spec=str(case.transform_path),
            source_class="Person",
        )
        expected1 = case.load_expected()
        expected2 = yaml.safe_load(second_expected_path.read_text(encoding="utf-8"))
        assert results == [expected1, expected2]


class TestObjectTransformerShim:
    """`linkml_map.transformer.object_transformer.ObjectTransformer` surface."""

    def test_construct_with_path_specification(self) -> None:
        case = _measurements_case()
        ot = ObjectTransformer(
            source_schemaview=str(case.source_schema_path),
            specification=str(case.transform_path),
        )
        result = ot.map_object(case.load_input(), "Person")
        assert result == case.load_expected()

    def test_construct_with_dict_specification(self) -> None:
        case = _measurements_case()
        spec_dict = yaml.safe_load(case.transform_path.read_text(encoding="utf-8"))
        ot = ObjectTransformer(
            source_schemaview=str(case.source_schema_path),
            specification=spec_dict,
        )
        result = ot.map_object(case.load_input(), "Person")
        assert result == case.load_expected()

    def test_create_transformer_specification(self) -> None:
        case = _measurements_case()
        spec_dict = yaml.safe_load(case.transform_path.read_text(encoding="utf-8"))
        ot = ObjectTransformer(source_schemaview=str(case.source_schema_path))
        ot.create_transformer_specification(spec_dict)
        result = ot.map_object(case.load_input(), "Person")
        assert result == case.load_expected()

    def test_index_is_noop(self) -> None:
        """Upstream `.index(...)` builds an FK lookup index; the Rust engine
        builds any FK index internally, so the shim's `.index` is a documented
        no-op. Assert it doesn't raise and doesn't affect `.map_object`.
        """
        case = _measurements_case()
        ot = ObjectTransformer(
            source_schemaview=str(case.source_schema_path),
            specification=str(case.transform_path),
        )
        assert ot.index(case.load_input()) is None
        result = ot.map_object(case.load_input(), "Person")
        assert result == case.load_expected()

    def test_source_schemaview_missing_raises_value_error(self) -> None:
        ot = ObjectTransformer(specification={"class_derivations": {}})
        with pytest.raises(ValueError):
            ot.map_object({"id": "x"})


class TestDictValueBridge:
    """dict<->Value round-trip: nested dict, list, int, float, str, bool, null."""

    def test_nested_structure_round_trips_faithfully(self) -> None:
        # A minimal identity-shaped transform spec (populated_from == attribute
        # name for every field) so the ONLY thing under test is the dict<->Value
        # JSON bridge, not derivation logic. Uses the measurements source schema
        # for a valid SchemaView but supplies its own class derivation.
        case = _measurements_case()
        spec_dict = {
            "id": "bridge-roundtrip",
            "class_derivations": {
                "Person": {
                    "populated_from": "Person",
                    "slot_derivations": {
                        "id": {"populated_from": "id"},
                        "height": {"populated_from": "height"},
                    },
                }
            },
        }
        ot = ObjectTransformer(
            source_schemaview=str(case.source_schema_path),
            specification=spec_dict,
        )
        obj = {
            "id": "P:1",
            "height": {
                "value": 172.5,
                "unit": "cm",
                # extra nested/typed leaves to exercise every scalar kind;
                # they are not declared on QuantityValue so copy-through of
                # `height` as a whole (via populated_from) is what preserves
                # them faithfully.
                "meta": {
                    "flags": [True, False, None],
                    "count": 3,
                    "ratio": 0.5,
                    "label": "nested",
                },
            },
        }
        result = ot.map_object(obj, "Person")
        assert result["id"] == "P:1"
        assert result["height"] == obj["height"]
        # Type fidelity, not just equality (bool must not become 0/1, etc.)
        nested = result["height"]["meta"]
        assert nested["flags"] == [True, False, None]
        assert isinstance(nested["flags"][0], bool)
        assert isinstance(nested["flags"][1], bool)
        assert nested["flags"][2] is None
        assert isinstance(nested["count"], int)
        assert isinstance(nested["ratio"], float)
        assert isinstance(nested["label"], str)


class TestErrorPropagation:
    """Bad schema path / malformed spec raises `ValueError`, never a crash."""

    def test_bad_schema_path_raises_value_error_native(self) -> None:
        case = _measurements_case()
        with pytest.raises(ValueError):
            Transformer(
                source_schema="/nonexistent/schema.yaml",
                spec=str(case.transform_path),
            )

    def test_bad_spec_path_raises_value_error_native(self) -> None:
        case = _measurements_case()
        with pytest.raises(ValueError):
            Transformer(
                source_schema=str(case.source_schema_path),
                spec="/nonexistent/spec.yaml",
            )

    def test_malformed_spec_text_raises_value_error_native(self) -> None:
        case = _measurements_case()
        with pytest.raises(ValueError):
            Transformer.from_yaml(
                source_schema_yaml=case.source_schema_path.read_text(encoding="utf-8"),
                spec_yaml="not: [valid, transform, spec: : :",
            )

    def test_shim_bad_schema_path_raises_value_error(self) -> None:
        case = _measurements_case()
        ot = ObjectTransformer(
            source_schemaview="/nonexistent/schema.yaml",
            specification=str(case.transform_path),
        )
        with pytest.raises(ValueError):
            ot.map_object({"id": "P:1"})

    def test_free_function_bad_schema_path_raises_value_error(self) -> None:
        case = _measurements_case()
        with pytest.raises(ValueError):
            transform_object(
                {"id": "P:1"},
                source_schema="/nonexistent/schema.yaml",
                spec=str(case.transform_path),
            )
