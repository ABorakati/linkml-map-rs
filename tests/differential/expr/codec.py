"""Shared JSON encoding between the Python driver and the Rust oracle.

See crates/linkml-map-conformance/src/exprfuzz.rs for the specification.
"""

from __future__ import annotations

import math
import struct
from typing import Any

I64_MIN, I64_MAX = -(2**63), 2**63 - 1


class DynObj:
    """Stand-in for upstream ``linkml_map.utils.dynamic_object.DynObj`` (imported lazily)."""


def f2h(f: float) -> str:
    return struct.pack(">d", f).hex()


def h2f(h: str) -> float:
    return struct.unpack(">d", bytes.fromhex(h))[0]


def encode(v: Any, depth: int = 0) -> Any:
    """Encode a Python value (an upstream result or a binding) as JSON-able data."""
    from linkml_map.utils.dynamic_object import DynObj as UpDynObj

    if depth > 60:
        return {"$other": "too-deep"}
    if v is None or isinstance(v, (bool, str)):
        return v
    if isinstance(v, int):
        return v if I64_MIN <= v <= I64_MAX else {"$i": str(v)}
    if isinstance(v, float):
        return {"$f": f2h(v)}
    if isinstance(v, list):
        return [encode(x, depth + 1) for x in v]
    if isinstance(v, tuple):
        return {"$tuple": [encode(x, depth + 1) for x in v]}
    if isinstance(v, (set, frozenset)):
        items = [encode(x, depth + 1) for x in v]
        items.sort(key=lambda x: repr(x))
        return {"$set": items}
    if isinstance(v, UpDynObj):
        return {k: encode(x, depth + 1) for k, x in vars(v).items()}
    if isinstance(v, dict):
        if all(isinstance(k, str) and not k.startswith("$") for k in v):
            return {k: encode(x, depth + 1) for k, x in v.items()}
        return {"$dict": [[encode(k, depth + 1), encode(x, depth + 1)] for k, x in v.items()]}
    return {"$other": type(v).__name__}


def canon(j: Any) -> Any:
    """Canonical comparable form of an encoded value.

    Leaves are tagged so ``1``, ``1.0`` and ``True`` stay distinct; tuples fold
    into lists (reported separately by :func:`has_tuple`), sets into sorted lists.
    """
    if j is None:
        return None
    if isinstance(j, bool):
        return ("b", j)
    if isinstance(j, int):
        return ("i", j)
    if isinstance(j, str):
        return ("s", j)
    if isinstance(j, list):
        return ("l", [canon(x) for x in j])
    if isinstance(j, dict):
        if "$f" in j:
            f = h2f(j["$f"])
            return ("f", "nan" if math.isnan(f) else j["$f"])
        if "$i" in j:
            return ("i", int(j["$i"]))
        if "$dec" in j:
            return ("dec", j["$dec"])
        if "$temporal" in j:
            return ("temporal", j["$temporal"])
        if "$tuple" in j:
            return ("l", [canon(x) for x in j["$tuple"]])
        if "$set" in j:
            return ("set", [canon(x) for x in j["$set"]])
        if "$dict" in j:
            return ("dict", [[canon(k), canon(x)] for k, x in j["$dict"]])
        if "$other" in j:
            return ("other", j["$other"])
        return ("m", [[k, canon(x)] for k, x in j.items()])
    raise TypeError(j)


def has_tuple(j: Any) -> bool:
    if isinstance(j, list):
        return any(has_tuple(x) for x in j)
    if isinstance(j, dict):
        if "$tuple" in j:
            return True
        return any(has_tuple(x) for x in j.values() if isinstance(x, (list, dict)))
    return False


def show(j: Any) -> str:
    """Compact human-readable rendering of an encoded value (Python-ish)."""
    if j is None:
        return "None"
    if isinstance(j, bool):
        return repr(j)
    if isinstance(j, (int, str)):
        return repr(j)
    if isinstance(j, list):
        return "[" + ", ".join(show(x) for x in j) + "]"
    if "$f" in j:
        return repr(h2f(j["$f"]))
    if "$i" in j:
        return j["$i"]
    if "$dec" in j:
        return f"Decimal({j['$dec']!r})"
    if "$temporal" in j:
        return f"Temporal({j['$temporal']!r})"
    if "$tuple" in j:
        return "(" + ", ".join(show(x) for x in j["$tuple"]) + ",)"
    if "$set" in j:
        return "{" + ", ".join(show(x) for x in j["$set"]) + "}"
    if "$dict" in j:
        return "{" + ", ".join(f"{show(k)}: {show(x)}" for k, x in j["$dict"]) + "}"
    if "$other" in j:
        return f"<{j['$other']}>"
    return "{" + ", ".join(f"{k!r}: {show(x)}" for k, x in j.items()) + "}"
