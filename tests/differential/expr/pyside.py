"""Evaluate a case with upstream linkml-map (``/home/user/upstream/upvenv``).

``restricted`` is ``eval_expr_with_mapping`` (simpleeval). ``unrestricted`` is
``ObjectTransformer._eval_expr`` with ``unrestricted_eval=True`` (statement blocks
and the restricted-then-asteval fallback), called with a stub ``self`` so the
real upstream code runs, not a re-implementation.
"""

from __future__ import annotations

import logging
import signal
import types
from collections.abc import Mapping
from typing import Any

from linkml_map.transformer.object_transformer import ObjectTransformer
from linkml_map.utils.dynamic_object import DynObj
from linkml_map.utils.eval_utils import eval_expr_with_mapping

from codec import encode

logging.disable(logging.CRITICAL)


class Timeout(BaseException):
    pass


def _alarm(signum, frame):  # noqa: ARG001
    raise Timeout


signal.signal(signal.SIGALRM, _alarm)


def convert(v: Any, flavour: str) -> Any:
    """Decode binding data; ``obj`` flavour turns maps into ``DynObj`` like the engine's ``dynamic_object``."""
    if isinstance(v, list):
        return [convert(x, flavour) for x in v]
    if isinstance(v, dict):
        d = {k: convert(x, flavour) for k, x in v.items()}
        return DynObj(**d) if flavour == "obj" else d
    return v


def decode_binding(j: Any) -> Any:
    """Inverse of codec.encode for binding inputs."""
    from codec import h2f

    if isinstance(j, list):
        return [decode_binding(x) for x in j]
    if isinstance(j, dict):
        if "$f" in j:
            return h2f(j["$f"])
        if "$i" in j:
            return int(j["$i"])
        return {k: decode_binding(x) for k, x in j.items()}
    return j


class FakeBindings(Mapping):
    """What ``Bindings`` offers ``_eval_expr``: a mapping plus ``get_ctxt_obj_and_dict``."""

    def __init__(self, source: dict) -> None:
        self.source = source
        self.names = {"NULL": None, **source}

    def __getitem__(self, k):
        return self.names[k]

    def __iter__(self):
        return iter(self.names)

    def __len__(self):
        return len(self.names)

    def get_ctxt_obj_and_dict(self, source_obj=None):  # noqa: ARG002
        do = DynObj(**self.source)
        return do, vars(do)


def run_py(expr: str, bindings_json: dict, mode: str, strict: bool, flavour: str, timeout: float = 3.0) -> dict:
    """Return ``{"ok": enc}`` or ``{"err": ExcName, "message": str}`` plus ``via`` for unrestricted."""
    source = {k: convert(decode_binding(v), flavour) for k, v in bindings_json.items()}
    fb = FakeBindings(source)
    via = ["restricted"]
    stub = types.SimpleNamespace(unrestricted_eval=(mode == "unrestricted"), strict=strict, _warned_unbound_names=set())

    def unres(expr, src, functions=None, raise_undefined=False):
        via[0] = "asteval-block" if raise_undefined else "fallback"
        return ObjectTransformer._unrestricted_eval(stub, expr, src, functions=functions, raise_undefined=raise_undefined)

    stub._unrestricted_eval = unres
    signal.setitimer(signal.ITIMER_REAL, timeout)
    try:
        if mode == "restricted":
            v = eval_expr_with_mapping(expr, fb, strict=strict)
        else:
            v = ObjectTransformer._eval_expr(stub, expr, fb)
        out = {"ok": encode(v)}
    except Timeout:
        out = {"err": "Timeout", "message": "timeout"}
    except RecursionError as e:
        out = {"err": "RecursionError", "message": str(e)[:200]}
    except BaseException as e:  # noqa: BLE001
        out = {"err": type(e).__name__, "message": str(e)[:300]}
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
    out["via"] = via[0]
    return out
