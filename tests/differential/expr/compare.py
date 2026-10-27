"""Compare an upstream result with the Rust result for one case."""

from __future__ import annotations

from codec import canon, has_tuple, show


def outcome(r: dict) -> str:
    return "ok" if "ok" in r else "err"


def compare(py: dict, rs: dict) -> dict | None:
    """``None`` when equivalent; otherwise ``{"kind": ..., ...}`` describing the divergence.

    Kinds: ``py_err_rs_ok`` (Rust accepts what upstream rejects), ``py_ok_rs_err``,
    ``value`` (both succeed with different values), ``rs_crash`` (panic/abort/hang).
    """
    if rs.get("err") in ("crash", "hang", "panic"):
        return {"kind": "rs_crash", "rs_kind": rs["err"], "rs_msg": rs.get("message", "")[:200]}
    if rs.get("err") == "unrepresentable":
        return None
    # A SyntaxError in a statement block is swallowed by asteval (the result is null); Rust
    # refuses the expression when it parses it (plan-build time), which is deliberate. The
    # restricted campaign covers parse-level agreement.
    if py.get("via") == "asteval-block" and py.get("ok", 1) is None and rs.get("err") in ("lex", "parse"):
        return None
    po, ro = outcome(py), outcome(rs)
    if po == "err" and ro == "err":
        return None
    if po == "err":
        return {"kind": "py_err_rs_ok", "py_err": py["err"], "py_msg": py["message"][:200], "rs": show(rs["ok"])}
    if ro == "err":
        return {"kind": "py_ok_rs_err", "py": show(py["ok"]), "rs_kind": rs["err"], "rs_msg": rs.get("message", "")[:200]}
    if canon(py["ok"]) != canon(rs["ok"]):
        return {"kind": "value", "py": show(py["ok"]), "rs": show(rs["ok"]), "tuple": has_tuple(py["ok"])}
    return None
