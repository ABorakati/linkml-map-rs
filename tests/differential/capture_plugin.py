"""pytest plugin: record every top-level ``ObjectTransformer.map_object`` call made by upstream's own test-suite.

Run from the upstream checkout (upstream venv):

    CAPTURE_DIR=/tmp/captured PYTHONPATH=<repo>/tests/differential \
        python -m pytest -p capture_plugin tests/test_compliance tests/test_transformer -q

Each distinct (source schema, spec, target schema, source type, input object, eval flags) becomes
``$CAPTURE_DIR/<hash>/{schema.yaml,spec.yaml,[target.yaml],data/<Type>-001.yaml,case.json}``, ready for
``examples_sweep.py --captured $CAPTURE_DIR``.  Only dict inputs are captured (the CLI's single-object path).
"""

from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path

import yaml

_DEPTH = {"n": 0}
_SEEN: set[str] = set()


def _dump_schema(sv) -> str | None:
    if sv is None:
        return None
    from linkml_runtime.dumpers import yaml_dumper

    try:
        return yaml_dumper.dumps(sv.schema)
    except Exception:  # noqa: BLE001
        return None


def _dump_spec(spec) -> str | None:
    if spec is None:
        return None
    try:
        d = spec.model_dump(mode="json", exclude_none=True)
    except Exception:  # noqa: BLE001
        return None
    return yaml.safe_dump(d, sort_keys=False, allow_unicode=True)


def pytest_configure(config) -> None:  # noqa: ANN001
    out = os.environ.get("CAPTURE_DIR")
    if not out:
        return
    out_dir = Path(out)
    out_dir.mkdir(parents=True, exist_ok=True)
    from linkml_map.transformer.object_transformer import ObjectTransformer

    orig = ObjectTransformer.map_object

    def wrapper(self, source_obj, source_type=None, target_type=None, class_derivation=None):  # noqa: ANN001
        top = _DEPTH["n"] == 0
        if top and isinstance(source_obj, dict) and source_type and class_derivation is None:
            try:
                record(self, source_obj, source_type)
            except Exception:  # noqa: BLE001
                pass
        _DEPTH["n"] += 1
        try:
            return orig(self, source_obj, source_type, target_type, class_derivation)
        finally:
            _DEPTH["n"] -= 1

    def record(tr, source_obj, source_type) -> None:  # noqa: ANN001
        schema = _dump_schema(tr.source_schemaview)
        spec = _dump_spec(tr.specification)
        if schema is None or spec is None:
            return
        target = _dump_schema(tr.target_schemaview)
        data = yaml.safe_dump(json.loads(json.dumps(source_obj, default=str)), sort_keys=False, allow_unicode=True)
        flags = []
        if getattr(tr, "unrestricted_eval", False):
            flags.append("--unrestricted-eval")
        if getattr(tr, "strict", False):
            flags.append("--strict")
        h = hashlib.sha1("\0".join([schema, spec, target or "", data, source_type, *flags]).encode()).hexdigest()[:12]
        if h in _SEEN:
            return
        _SEEN.add(h)
        d = out_dir / h
        (d / "data").mkdir(parents=True, exist_ok=True)
        (d / "schema.yaml").write_text(schema)
        (d / "spec.yaml").write_text(spec)
        if target:
            (d / "target.yaml").write_text(target)
        (d / "data" / f"{source_type}-001.yaml").write_text(data)
        (d / "case.json").write_text(
            json.dumps(
                {"schema": "schema.yaml", "spec": "spec.yaml", "input": f"data/{source_type}-001.yaml", "source_type": source_type,
                 "target_schema": "target.yaml" if target else None, "flags": flags, "origin": os.environ.get("PYTEST_CURRENT_TEST", "")},
                indent=2,
            )
        )

    ObjectTransformer.map_object = wrapper
