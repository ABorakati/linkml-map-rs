"""Hypothesis strategies that generate (source schema, target schema | None, spec, data) worlds.

A *world* is rendered to files by :func:`write_world`, giving a :class:`harness.Case`.

Feature flags (``feats``) switch generator families on so a campaign can focus on one area:
``core awkward enums nested joins multivalued expr inherit constants units mint typeof yamlraw rawtsv mismatch``.
"""

from __future__ import annotations

import csv
import io
from dataclasses import dataclass, field
from pathlib import Path

import yaml
from hypothesis import strategies as st

import harness as H

ALL_FEATS = frozenset(
    "core awkward enums nested joins multivalued expr inherit constants units mint typeof yamlraw rawtsv mismatch".split()
)
CLASS_NAMES = ["Person", "Order", "Item"]
TGT_NAMES = ["Agent", "Record", "Row", "Entry"]
SCALARS = ["string", "integer", "float", "boolean", "decimal", "date"]
SLOT_POOL = {
    "string": ["name", "note", "label"],
    "integer": ["age", "count"],
    "float": ["score", "ratio"],
    "boolean": ["flag"],
    "decimal": ["amount"],
    "date": ["born"],
    "MyStr": ["code"],
    "MyInt": ["rank"],
}
UNIT_PAIRS = [("kg", "g"), ("g", "kg"), ("m", "cm"), ("cm", "m"), ("s", "min"), ("L", "mL")]


class Raw(str):
    """A scalar emitted verbatim into YAML data (to probe YAML 1.1 vs 1.2 resolution)."""


@dataclass
class Slot:
    name: str
    range: str
    multivalued: bool = False
    identifier: bool = False
    unit: str | None = None


@dataclass
class Cls:
    name: str
    slots: list[Slot]
    is_a: str | None = None


@dataclass
class TSlot:
    """A target slot: its schema description plus the spec slot_derivation body (None = bare)."""

    name: str
    range: str | None
    multivalued: bool = False
    identifier: bool = False
    deriv: dict | None = None
    nested: Block | None = None  # nested class derivation (slot-level class_derivations)
    nested_list: bool = True


@dataclass
class Block:
    name: str
    populated_from: str | None
    slots: list[TSlot]
    is_a: str | None = None
    joins: dict | None = None
    mint_ids: bool | None = None
    omit_pf: bool = False
    own_class: bool = True
    #: join name -> class whose table file is also written under the join name (a join reads the table named by its
    #: *join name*, never by ``class_named``).
    alias_files: dict[str, str] = field(default_factory=dict)


@dataclass
class World:
    feats: frozenset
    classes: list[Cls]
    enums: dict[str, list[str]]
    default_range: str | None
    typeof: bool
    blocks: list[Block]
    tgt_enums: dict[str, list[str]]
    enum_derivs: dict[str, dict]
    has_target: bool
    spec_extra: dict
    list_form: bool
    rows: dict[str, list[dict]]
    input_kind: str
    source_type: str | None
    flags: list[str]
    class_by_name: dict = field(default_factory=dict)

    def induced(self, cname: str) -> list[Slot]:
        c = next(c for c in self.classes if c.name == cname)
        base = self.induced(c.is_a) if c.is_a else []
        return base + c.slots


# ── data values ──────────────────────────────────────────────────────────────

_STR_COMMON = ["a", "b", "c", "hello", "x y", "Alice", "Bob", "abc", "xyz"]
_STR_WEIRD = ["", " ", "Zoë", "日本語", "0", "007", "1e3", "-1", "True", "null", "NULL", "None", "a|b", "a,b", "'q'",
              '"d"', "#c", "~", "yes", "é", "a:b", "- x", "{k: v}", "[1]", "=", " x", "1.0", "NaN", "  pad  "]
_INT_COMMON = [0, 1, 2, 5, 42, -3, 100]
_INT_WEIRD = [2**31, 2**63, -(2**63) - 1, 10**30, "007", "1.0", "1e3", " 5", "abc", "-0", "+5", "١٢٣", "1_000", "", "0x10", 9999999999]
_INT_NUMPARSE = ["1.0", "1e3", "١٢٣", "1_000", " 5", "+5", "-0", "007", "0x10", ""]  # numeric-string parsing gaps (reported)
_FLT_COMMON = [0.5, 1.5, 2.0, 3.14, -2.5, 0.0, 10.0, 100.25]
_FLT_WEIRD = [0.1, 1e20, 1e-7, -0.0, 123456789.123456789, 5e-324, 1.7976931348623157e308, "1e3", ".5", "1.", "abc", "", "nan", "inf",
              "1e400", 0.30000000000000004, 1e16, 1e15, 123456789012345680.0, 2.5e-5]
# Extra values for the ``joins`` profile: a JSON (or CSV) primary table is read by DuckDB, which types each JSON column
# on its own (``join_rows.rs``), so integers around the 64- and 128-bit limits, floats near them, and strings DuckDB
# turns into DATE / TIME / TIMESTAMP / UUID matter.
_JOIN_INT = [2**64 - 1, 2**64, -(2**63), 2**127, -(2**127) - 1, 10**25, -(10**25), 2**53 + 1, 9223372036854775807]
_JOIN_FLT = [9223372036854775808.0, 1e21, 1e22, 1.5e300, -1e-7, 4.35, 2.0**63, 123456789012345678901234567890.0]
_JOIN_STR = ["2020-01-31T10:00:00", "2020-01-31 10:00:00.5", "2020-01-31T10:00:00Z", "2020-01-31T10:00:00+02:00", "10:00:00", "01-31-2020",
             "31-01-2020", "2020-01-31 ", "7f0dcad7-862e-5cec-4d8e-a544eb9c736f", "7F0DCAD7-862E-5CEC-4D8E-A544EB9C736F", "epoch", "24:00:00",
             "2020-1-5", "20-01-31", "01-31-20 10:11:12 PM", "2020-01-31T10:00:00.123456789Z", "1900-01-01", "9999-12-31T23:00:00-02:00"]
_BOOL_COMMON = [True, False, "true", "false"]
_BOOL_WEIRD = ["True", "False", "0", "1", "yes", "no", "", "TRUE", "t", "maybe"]
_DEC_COMMON = ["1.50", "0.10", 2.25, 10, "3", "0.5"]
_DEC_WEIRD = ["1e2", "0.1", "1234567890.123456789012", "-0.0", "1.", ".5", "abc", "", 0.1, "1E-7"]
_DATE_COMMON = ["2020-01-31", "1999-12-31", "2000-02-29"]
_DATE_WEIRD = ["2020-02-30", "not a date", "", "2020-1-5", "20200131", "0001-01-01", "2020-01-31T10:00:00", " 2020-01-31"]
_RAW_YAML = ["yes", "No", "on", "1e3", "0x1F", "1_000", "2020-01-31", "~", "null", "0o7", "010", "+1", ".5", "1:30", "12:30:45", "1e+3",
             "0b11", ".inf", "-.INF", ".NaN", "'q'", "Y", "n", "2020-01-31 10:00:00", "190:20:30", "1_0.5"]


def _mix(common, weird, awkward):
    if awkward:
        return st.one_of(st.sampled_from(common), st.sampled_from(weird), st.sampled_from(weird))
    return st.one_of(*([st.sampled_from(common)] * 6), st.sampled_from(weird))


def scalar_values(rng: str, feats, enums: dict, ids: dict):
    awk = "awkward" in feats
    jn = "joins" in feats
    if rng in ("string", "MyStr"):
        return _mix(_STR_COMMON, _STR_WEIRD + (_JOIN_STR if jn else []), awk)
    if rng in ("integer", "MyInt"):
        weird = [w for w in _INT_WEIRD if w not in _INT_NUMPARSE] \
            if "knownfree" in feats else _INT_WEIRD
        return _mix(_INT_COMMON, weird + (_JOIN_INT if jn else []), awk)
    if rng == "float":
        return _mix(_FLT_COMMON, _FLT_WEIRD + (_JOIN_FLT if jn else []), awk)
    if rng == "boolean":
        return _mix(_BOOL_COMMON, _BOOL_WEIRD, awk)
    if rng == "decimal":
        return _mix(_DEC_COMMON, _DEC_WEIRD, awk)
    if rng == "date":
        return _mix(_DATE_COMMON, _DATE_WEIRD + (_JOIN_STR if jn else []), awk)
    if rng in enums:
        return _mix(enums[rng], ["zz", "", enums[rng][0].upper(), "other"], awk)
    if rng in ids:
        return _mix(ids[rng], ["missing", ""], awk)
    return _mix(_STR_COMMON, _STR_WEIRD, awk)


@st.composite
def slot_value(draw, slot: Slot, feats, enums, ids, yaml_native: bool):
    vals = scalar_values(slot.range, feats, enums, ids)
    present = draw(st.sampled_from([True] * 9 + [False]))
    if "knownfree" in feats and not yaml_native:
        present = True  # an absent TSV cell is an empty cell (known open divergence)
    if not present:
        return None
    if slot.multivalued and yaml_native:
        v = draw(st.lists(vals, min_size=0, max_size=3))
        return v
    v = draw(vals)
    if "knownfree" in feats and not yaml_native and isinstance(v, str) and not v.strip(" "):
        v = "a"  # TSV/CSV empty cells are a known open divergence (upstream drops them)
    if yaml_native and "yamlraw" in feats and draw(st.sampled_from([False] * 9 + [True])):
        return Raw(draw(st.sampled_from(_RAW_YAML)))
    if slot.multivalued and not yaml_native and draw(st.booleans()):
        v = "|".join(str(x) for x in draw(st.lists(st.sampled_from(["a", "b", "1", "2"]), min_size=1, max_size=3)))
    return v


# ── source world ─────────────────────────────────────────────────────────────


@st.composite
def source_classes(draw, feats):
    n = draw(st.integers(1, 3 if ("joins" in feats or "nested" in feats) else 2))
    names = CLASS_NAMES[:n]
    enums: dict[str, list[str]] = {}
    if "enums" in feats or draw(st.booleans()):
        for en in draw(st.lists(st.sampled_from(["Kind", "Status"]), max_size=2, unique=True)):
            enums[en] = draw(st.lists(st.sampled_from(["A", "B", "C", "D", "x_1", "Ünï"]), min_size=2, max_size=4, unique=True))
    typeof = "typeof" in feats or draw(st.sampled_from([False, False, True]))
    kinds = list(SCALARS) + (["MyStr", "MyInt"] if typeof else []) + list(enums) + ["ref"]
    classes = []
    for i, cname in enumerate(names):
        slots = []
        id_range = draw(st.sampled_from(["string"] * 5 + ["integer"]))
        parent = None
        if "inherit" in feats and i > 0 and draw(st.booleans()):
            parent = names[0]
        if parent is None:
            slots.append(Slot("id", id_range, identifier=True))
        used = {"id"} | {x.name for x in (_induced(classes, parent) if parent else [])}
        for _ in range(draw(st.integers(1, 4))):
            kind = draw(st.sampled_from(kinds))
            if kind == "ref":
                others = [n for n in names if n != cname]
                if not others:
                    kind = "string"
                else:
                    rng = draw(st.sampled_from(others))
                    nm = f"{rng.lower()}_ref"
                    if nm in used:
                        continue
                    used.add(nm)
                    slots.append(Slot(nm, rng))
                    continue
            if kind in enums:
                nm = kind.lower()
            else:
                nm = draw(st.sampled_from(SLOT_POOL.get(kind, ["name"])))
            while nm in used:
                nm += "2"
            used.add(nm)
            mv = "multivalued" in feats and kind not in ("ref",) and draw(st.sampled_from([False, False, True]))
            unit = None
            if "units" in feats and kind in ("float", "integer", "decimal") and draw(st.booleans()):
                unit = draw(st.sampled_from(UNIT_PAIRS))[0]
            slots.append(Slot(nm, kind, multivalued=mv, unit=unit))
        classes.append(Cls(cname, slots, is_a=parent))
    default_range = draw(st.sampled_from([None, None, "string"]))
    return classes, enums, typeof, default_range


def _induced(classes: list[Cls], cname: str) -> list[Slot]:
    c = next(c for c in classes if c.name == cname)
    return (_induced(classes, c.is_a) if c.is_a else []) + c.slots


# ── expressions ──────────────────────────────────────────────────────────────


@st.composite
def expression(draw, srcslots: list[Slot], joinrefs: list[str], feats):
    """A safe expression over source slots (``{slot}`` references), returns (expr, result_range)."""
    sc = [s for s in srcslots if not s.multivalued and s.range in ("string", "MyStr")]
    nu = [s for s in srcslots if not s.multivalued and s.range in ("integer", "float", "MyInt", "decimal")]
    anyv = [s for s in srcslots if not s.multivalued]
    options = []
    if sc:
        options += [("str1", sc), ("str2", sc), ("str3", sc), ("str4", sc), ("str5", sc), ("str6", sc)]
    if nu:
        options += [("num1", nu), ("num2", nu), ("num3", nu), ("num4", nu), ("num5", nu), ("num6", nu)]
    if anyv:
        options += [("any1", anyv), ("any2", anyv), ("any3", anyv), ("any4", anyv)]
    if joinrefs:
        options += [("join", joinrefs)]
    options += [("lit", [])]
    kind, pool = draw(st.sampled_from(options))
    if kind == "str1":
        s = draw(st.sampled_from(pool)).name
        f = draw(st.sampled_from(["upper", "lower", "strlen", "strip", "title", "capitalize"]))
        return f"{f}({{{s}}})", "integer" if f == "strlen" else "string"
    if kind == "str2":
        s, t = draw(st.sampled_from(pool)).name, draw(st.sampled_from(pool)).name
        return f"{{{s}}} + '-' + {{{t}}}", "string"
    if kind == "str3":
        s = draw(st.sampled_from(pool)).name
        return f"substr({{{s}}}, {draw(st.integers(0, 3))})", "string"
    if kind == "str4":
        s = draw(st.sampled_from(pool)).name
        return f"replace({{{s}}}, 'a', 'Z')", "string"
    if kind == "str5":
        s = draw(st.sampled_from(pool)).name
        return f"{{{s}}} == 'a'", "boolean"
    if kind == "str6":
        s = draw(st.sampled_from(pool)).name
        return f"case(({{{s}}} == 'a', 'first'), (True, {{{s}}}))", "string"
    if kind == "num1":
        s = draw(st.sampled_from(pool)).name
        return f"{{{s}}} * {draw(st.sampled_from([2, 3, 10, 0.5]))}", "float"
    if kind == "num2":
        s = draw(st.sampled_from(pool)).name
        return f"{{{s}}} + {draw(st.sampled_from([1, 100, 0.1]))}", "float"
    if kind == "num3":
        s = draw(st.sampled_from(pool)).name
        return f"{{{s}}} > {draw(st.sampled_from([0, 1, 10]))}", "boolean"
    if kind == "num4":
        s = draw(st.sampled_from(pool)).name
        return f"str({{{s}}})", "string"
    if kind == "num5":
        s = draw(st.sampled_from(pool)).name
        f = draw(st.sampled_from(["abs", "round", "floor", "ceil", "int", "float"]))
        return f"{f}({{{s}}})", "integer" if f in ("round", "floor", "ceil", "int") else "float"
    if kind == "num6":
        s, t = draw(st.sampled_from(pool)).name, draw(st.sampled_from(pool)).name
        return f"{{{s}}} + {{{t}}}", "float"
    if kind == "any1":
        s = draw(st.sampled_from(pool)).name
        return f"{{{s}}}", draw(st.sampled_from([None, None, "string"]))
    if kind == "any2":
        s = draw(st.sampled_from(pool)).name
        return f"{{{s}}} or 'dflt'", "string"
    if kind == "any3":
        s = draw(st.sampled_from(pool)).name
        return f"str({{{s}}}) + '!'", "string"
    if kind == "any4":
        s = draw(st.sampled_from(pool)).name
        return f"{{{s}}} != 'zz'", "boolean"
    if kind == "join":
        return draw(st.sampled_from(pool)), None
    return draw(st.sampled_from(["'lit'", "1 + 2", "len('abc')", "NULL", "True"])), None


# ── spec / target blocks ─────────────────────────────────────────────────────

TGT_POOL = ["out_a", "out_b", "out_c", "out_d", "out_e", "out_f"]


@st.composite
def const_value(draw):
    return draw(st.sampled_from(["k", "", "7", 7, 0, 3.5, 0.1, True, False, "Zoë", "x|y", ["a", "b"], 10**30, "1e3", "null"]))


def _cv(v, feats):
    if "knownfree" in feats and isinstance(v, list):
        return "k"  # list-valued `value:` is truncated by upstream's loader (reported upstream quirk)
    return v


@st.composite
def target_range(draw, src_range: str | None, tgt_enums, feats):
    src_range = {"MyStr": "string", "MyInt": "integer"}.get(src_range, src_range)
    opts = [src_range] * 5 + SCALARS + [None]
    return draw(st.sampled_from(opts))


@st.composite
def build_block(draw, w_classes, enums, feats, src: str, has_target: bool, tgt_name: str, tgt_enums: dict, enum_derivs: dict,
                allow_nested: bool, nested_depth: int = 0):
    """Build one class derivation block deriving ``tgt_name`` from source class ``src``."""
    srcslots = _induced(w_classes, src)
    names = [c.name for c in w_classes]
    joins = None
    alias_files: dict[str, str] = {}
    joinrefs: list[str] = []
    jslots = [s for s in srcslots if s.range in names and "joins" in feats]
    if jslots and draw(st.booleans()):
        joins = {}
        for js in jslots[:2]:
            how = draw(st.sampled_from(["explicit", "explicit", "join_on"]))
            tgt_id = _induced(w_classes, js.range)[0].name
            # A join named like its class (the table is the class's file), or named otherwise with the schema class in
            # ``class_named``: with a table file of the join's name (it is what is read) or without (it fails).
            amode = draw(st.sampled_from(["same"] * 4 + ["alias_file"] * 2 + ["alias_nofile"])) if nested_depth == 0 else "same"
            alias = js.range if amode == "same" else f"j_{js.range}"
            if how == "explicit":
                joins[alias] = {"source_key": js.name, "lookup_key": tgt_id}
            else:
                joins[alias] = {"join_on": js.name} if js.name == tgt_id else {"source_key": js.name, "lookup_key": tgt_id}
            if amode != "same":
                joins[alias]["class_named"] = js.range
            if amode == "alias_file":
                alias_files[alias] = js.range
            for oc in _induced(w_classes, js.range):
                joinrefs.append(f"{{{alias}.{oc.name}}}")
    tslots: list[TSlot] = []
    used: set[str] = set()
    n = draw(st.integers(1, 4))
    for i in range(n):
        kinds = ["copy"] * 6
        if "expr" in feats:
            kinds += ["expr"] * 5
        else:
            kinds += ["expr"]
        kinds += ["value"]
        if joins:
            kinds += ["joinpf", "joinexpr", "joinexpr"]
        if "enums" in feats or any(s.range in enums for s in srcslots):
            kinds += ["enum"] * 3
        if "multivalued" in feats:
            kinds += ["cast", "cast", "mapping"]
        else:
            kinds += ["mapping"]
        if "units" in feats and any(s.unit for s in srcslots):
            kinds += ["unit"] * 4
        if allow_nested and "nested" in feats and nested_depth < 2:
            kinds += ["nested"] * 3
        if "expr" in feats:
            kinds += ["hide"]
        kind = draw(st.sampled_from(kinds))
        t = _build_tslot(draw, kind, i, srcslots, joinrefs, joins, w_classes, enums, feats, has_target, tgt_enums, enum_derivs,
                         used, nested_depth, src, tgt_name)
        if isinstance(t, tuple):
            tslots.extend(t)
        elif t is not None:
            tslots.append(t)
    if has_target and draw(st.sampled_from([True, True, True, False])) and "id" in [s.name for s in srcslots]:
        # derive the identifier like a real mapping would (bare slot, same name)
        if "id" not in used:
            idslot = next(s for s in srcslots if s.name == "id")
            tslots.insert(0, TSlot("id", idslot.range if idslot.range in SCALARS else "string", identifier=True, deriv=None))
            used.add("id")
    if not tslots:
        tslots.append(TSlot("out_a", "string", deriv={"value": "k"}))
    mint = draw(st.sampled_from([None, None, None, True, False])) if "mint" in feats else None
    return Block(tgt_name, src, tslots, joins=joins, mint_ids=mint, alias_files=alias_files)


def _build_tslot(draw, kind, i, srcslots, joinrefs, joins, classes, enums, feats, has_target, tgt_enums, enum_derivs, used,
                 nested_depth, src, tgt_name):
    def tname(default):
        nm = default
        k = 0
        while nm in used:
            k += 1
            nm = f"{default}{k}"
        used.add(nm)
        return nm

    scal = [s for s in srcslots if s.range not in [c.name for c in classes]]
    if kind == "copy" or (kind in ("mapping", "cast", "unit", "enum") and not scal):
        if not scal:
            return TSlot(tname("out_a"), "string", deriv={"value": "k"})
        s = draw(st.sampled_from(scal))
        same = draw(st.booleans())
        name = tname(s.name if same else draw(st.sampled_from(TGT_POOL)))
        if s.range in enums:
            return _enum_slot(draw, s, name, enums, feats, has_target, tgt_enums, enum_derivs, same)
        rng = draw(target_range(s.range, tgt_enums, feats)) if has_target else None
        deriv: dict = {}
        if name != s.name or draw(st.sampled_from([False, False, True])):
            deriv["populated_from"] = s.name
        if draw(st.sampled_from([False] * 6 + [True])):
            deriv["range"] = draw(st.sampled_from(SCALARS))
        if "awkward" in feats and draw(st.sampled_from([False, False, True])):
            deriv["missing_values"] = draw(st.sampled_from([[""], ["a"], [-9, "99"], "b", ["0"], [1]]))
        mv = s.multivalued
        return TSlot(name, rng if rng else s.range if not has_target else None, multivalued=mv, deriv=deriv or None)
    if kind == "mapping":
        s = draw(st.sampled_from([x for x in scal if x.range in ("string", "MyStr", "integer", "boolean")] or scal))
        name = tname(draw(st.sampled_from(TGT_POOL)))
        keys = {"string": ["a", "b", "c", "", "True", "0"], "MyStr": ["a", "b"], "integer": ["0", "1", "2", "5", "42"],
                "boolean": ["True", "False", "true", "1"]}.get(s.range, ["a", "1"])
        vm = {k: draw(st.sampled_from(["X", "Y", "1", "Zoë"] + ([] if "knownfree" in feats else [""]))) for k in draw(st.lists(st.sampled_from(keys), min_size=1, max_size=3, unique=True))}
        deriv = {"populated_from": s.name, "value_mappings": vm}
        if draw(st.sampled_from([False, False, True])):
            em = {k: draw(st.sampled_from(["'E1'", "{%s} + 'x'" % s.name, "len('ab')", "NULL"])) for k in list(vm)[:1]}
            deriv["expression_mappings"] = em
        return TSlot(name, "string" if has_target else None, multivalued=s.multivalued, deriv=deriv)
    if kind == "value":
        name = tname(draw(st.sampled_from(TGT_POOL)))
        v = _cv(draw(const_value()), feats)
        rng = draw(st.sampled_from([None, "string", "integer", "float", "boolean"])) if has_target else None
        return TSlot(name, rng, multivalued=isinstance(v, list) and has_target, deriv={"value": v})
    if kind == "expr":
        ex, rr = draw(expression(srcslots, joinrefs, feats))
        name = tname(draw(st.sampled_from(TGT_POOL)))
        rng = draw(st.sampled_from([rr, rr, None, "string", "integer"])) if has_target else None
        deriv = {"expr": ex}
        if not has_target and draw(st.sampled_from([False, False, True])):
            deriv["range"] = rr or "string"
        return TSlot(name, rng, deriv=deriv)
    if kind in ("joinpf", "joinexpr"):
        jr = draw(st.sampled_from(joinrefs))
        name = tname(draw(st.sampled_from(TGT_POOL)))
        rng = draw(st.sampled_from([None, "string", "integer"])) if has_target else None
        if kind == "joinpf":
            return TSlot(name, rng, deriv={"populated_from": jr.strip("{}")})
        return TSlot(name, rng, deriv={"expr": jr})
    if kind == "enum":
        en = [s for s in scal if s.range in enums]
        if not en:
            return None
        s = draw(st.sampled_from(en))
        return _enum_slot(draw, s, tname(draw(st.sampled_from(TGT_POOL))), enums, feats, has_target, tgt_enums, enum_derivs, False)
    if kind == "cast":
        s = draw(st.sampled_from(scal))
        name = tname(draw(st.sampled_from(TGT_POOL)))
        deriv = {"populated_from": s.name}
        if s.multivalued:
            st_choice = draw(st.sampled_from([None, {"delimiter": "|"}, {"delimiter": ","}, {"syntax": "JSON"}, {"syntax": "YAML"}]))
            if st_choice and "knownfree" in feats and st_choice.get("syntax") == "JSON":
                st_choice = {"delimiter": ";"}
            if st_choice:
                deriv["stringification"] = st_choice
            if draw(st.booleans()):
                deriv["cast_collection_as"] = "SingleValued"
            return TSlot(name, "string" if has_target else None, multivalued=False, deriv=deriv)
        st_choice = draw(st.sampled_from([None, {"delimiter": "|"}, {"delimiter": ",", "reversed": True}, {"syntax": "JSON"}]))
        if "knownfree" in feats:
            st_choice = None  # single -> multivalued stringification: reported gap
        if st_choice:
            deriv["stringification"] = st_choice
        if draw(st.booleans()):
            deriv["cast_collection_as"] = "MultiValuedList"
        return TSlot(name, s.range if has_target else None, multivalued=True, deriv=deriv)
    if kind == "unit":
        us = [s for s in srcslots if s.unit]
        s = draw(st.sampled_from(us))
        tgt_unit = next(t for f, t in UNIT_PAIRS if f == s.unit)
        name = tname(draw(st.sampled_from(TGT_POOL)))
        uc = {"target_unit": tgt_unit, "source_unit": s.unit if draw(st.booleans()) else s.unit}
        return TSlot(name, "float" if has_target else None, deriv={"populated_from": s.name, "unit_conversion": uc})
    if kind == "hide":
        hid = tname("hid")
        ex, rr = draw(expression(srcslots, joinrefs, feats))
        name = tname(draw(st.sampled_from(TGT_POOL)))
        return TSlot(hid, None, deriv={"expr": ex, "hide": True}), TSlot(name, "string" if has_target else None,
                                                                         deriv={"expr": f"slot('{hid}')"})
    if kind == "nested":
        inner_name = tname("inner")
        iname = f"Inner{tgt_name}{nested_depth}"
        inner = draw(build_block(classes, enums, feats, src, has_target, iname, tgt_enums, enum_derivs, False, nested_depth + 1))
        inner.name = iname
        inner.slots = [s for s in inner.slots if not s.identifier]
        if not inner.slots:
            return None
        inner.populated_from = None if draw(st.booleans()) else inner.populated_from
        inner.joins = None
        mv = draw(st.booleans())
        return TSlot(inner_name, inner.name, multivalued=mv, nested=inner, deriv=None)
    return None


def _enum_slot(draw, s, name, enums, feats, has_target, tgt_enums, enum_derivs, same):
    src_enum = s.range
    tgt_en = f"T{src_enum}"
    mode = draw(st.sampled_from(["pv", "pv", "pv", "mirror", "expr", "map_list"] + ([] if "knownfree" in feats else ["none"])))
    if mode != "none" and src_enum not in {d.get("populated_from") for d in enum_derivs.values()}:
        pvs = enums[src_enum]
        tvals = ["t_a", "t_b", "t_c"]
        d: dict = {"populated_from": src_enum}
        if mode in ("pv", "map_list", "mirror"):
            pvd = {}
            for tv in tvals[: draw(st.integers(1, 3))]:
                srcs = draw(st.lists(st.sampled_from(pvs), min_size=1, max_size=2, unique=True))
                pvd[tv] = {"populated_from": srcs if mode == "map_list" or len(srcs) > 1 else srcs[0]}
            d["permissible_value_derivations"] = pvd
            if mode == "mirror":
                d["mirror_source"] = True
        elif mode == "expr":
            d["expr"] = draw(st.sampled_from(["'t_a'", "case((src == 'A', 't_a'), (True, 'other'))", "NULL"]))
            d["permissible_value_derivations"] = {"t_a": {"populated_from": pvs[:1]}}
        enum_derivs[tgt_en] = d
        tgt_enums[tgt_en] = tvals + ["other"]
    deriv: dict = {}
    if name != s.name:
        deriv["populated_from"] = s.name
    return TSlot(name, tgt_en if has_target else None, multivalued=s.multivalued, deriv=deriv or None)


# ── whole world ──────────────────────────────────────────────────────────────


@st.composite
def worlds(draw, feats=ALL_FEATS):
    feats = frozenset(feats) | {"core"}
    classes, enums, typeof, default_range = draw(source_classes(feats))
    has_target = draw(st.sampled_from([True, True, True, False]))
    if "knownfree" in feats and "nested" in feats:
        has_target = True  # nested derivations without a target schema: upstream raises, Rust does not (reported)
    tgt_enums: dict = {}
    enum_derivs: dict = {}
    blocks: list[Block] = []
    list_form = False
    for i, c in enumerate(classes):
        if i > 0 and (("singleblock" in feats) or draw(st.sampled_from([True, False, False]))):
            continue  # not every source class is mapped (singleblock: only the first)
        tname = c.name if draw(st.sampled_from([False, True, True])) else TGT_NAMES[i]
        blk = draw(build_block(classes, enums, feats, c.name, has_target, tname, tgt_enums, enum_derivs, True))
        blk.omit_pf = tname == c.name and draw(st.booleans())
        blocks.append(blk)
    if "constants" in feats and draw(st.booleans()):
        if "singleblock" in feats:
            blocks = []  # constant blocks alongside table blocks are multi-block streams (port in flight)
        cn = draw(st.sampled_from(["Const", "Meta"]))
        vals = [TSlot(f"k{j}", "string" if has_target else None, deriv={"value": _cv(draw(const_value()), feats)}) for j in range(draw(st.integers(1, 2)))]
        for v in vals:
            if isinstance(v.deriv["value"], list):
                v.multivalued = has_target
        blocks.insert(draw(st.integers(0, len(blocks))), Block(cn, None, vals))
    if "inherit" in feats and blocks and draw(st.booleans()):
        # turn the first block's first slot into a shared base template
        b0 = blocks[0]
        base_slots = [b0.slots[0]]
        base = Block("BaseT", None if draw(st.booleans()) else b0.populated_from, base_slots)
        b0.slots = b0.slots[1:] or [TSlot("extra", "string" if has_target else None, deriv={"value": "e"})]
        b0.is_a = "BaseT"
        if draw(st.booleans()):
            base.populated_from = None
        blocks.insert(0, base)
    if len(blocks) >= 2 and draw(st.sampled_from([False, False, False, True])) and not any(b.is_a or b.name == "BaseT" for b in blocks):
        # two derivations for the same target class from different sources (list form)
        blocks[1].name = blocks[0].name
        blocks[1].own_class = False
        list_form = True
    spec_extra: dict = {}
    if "mint" in feats:
        if draw(st.booleans()):
            spec_extra["mint_ids"] = draw(st.booleans())
        if draw(st.booleans()) and blocks:
            spec_extra["class_defaults"] = {blocks[0].name: {"mint_ids": draw(st.booleans())}}
    if not blocks:
        blocks.append(Block("Out", classes[0].name, [TSlot("out_a", "string", deriv={"value": "k"})]))
    input_kind = draw(st.sampled_from(["tsv_dir", "tsv_dir", "csv_dir", "tsv_file", "yaml_dir", "json_dir", "yaml_single", "json_single"]))
    ids = {c.name: [f"{c.name[0].lower()}{k}" for k in range(1, 5)] for c in classes}
    yaml_native = input_kind in ("yaml_dir", "json_dir", "yaml_single", "json_single")
    rows: dict[str, list[dict]] = {}
    for c in classes:
        nrows = 1 if input_kind.endswith("single") else draw(st.integers(0, 4))
        crow = []
        for k in range(nrows):
            row = {}
            for s in _induced(classes, c.name):
                if s.identifier:
                    idv = ids[c.name][k] if s.range == "string" else k + 1
                    if "awkward" in feats and draw(st.sampled_from([False] * 8 + [True])):
                        idv = draw(scalar_values(s.range, feats, enums, ids))
                    row[s.name] = idv
                else:
                    v = draw(slot_value(s, feats, enums, {n: ids[n] for n in ids}, yaml_native))
                    if v is not None:
                        row[s.name] = v
            crow.append(row)
        rows[c.name] = crow
    st_name = None
    if input_kind in ("tsv_file", "yaml_single", "json_single"):
        st_name = draw(st.sampled_from([b.populated_from for b in blocks if b.populated_from] or [classes[0].name]))
    elif draw(st.sampled_from([False, False, False, True])) and not ("knownfree" in feats and len(blocks) > 1):
        st_name = blocks[0].populated_from  # (--source-type with several blocks: upstream forces it on all of them)
    flags = []
    if draw(st.sampled_from([True, False])):
        flags.append("--unrestricted-eval")
    if draw(st.sampled_from([False, False, True])):
        flags.append("--strict")
    return World(feats, classes, enums, default_range, typeof, blocks, tgt_enums, enum_derivs, has_target, spec_extra, list_form,
                 rows, input_kind, st_name, flags)


# ── rendering ────────────────────────────────────────────────────────────────


def schema_header(name: str) -> dict:
    return {"id": f"https://example.org/{name}", "name": name, "prefixes": {"linkml": "https://w3id.org/linkml/", "ex": "https://example.org/"},
            "default_prefix": "ex", "imports": ["linkml:types"]}


def render_source(w: World) -> dict:
    d = schema_header("src")
    if w.default_range:
        d["default_range"] = w.default_range
    if w.typeof:
        d["types"] = {"MyStr": {"typeof": "string", "description": "a string type"}, "MyInt": {"typeof": "integer"}}
    if w.enums:
        d["enums"] = {n: {"permissible_values": {pv: {} for pv in pvs}} for n, pvs in w.enums.items()}
    d["classes"] = {}
    for c in w.classes:
        attrs = {}
        for s in c.slots:
            a: dict = {"range": s.range}
            if s.identifier:
                a["identifier"] = True
            if s.multivalued:
                a["multivalued"] = True
            if s.unit:
                a["unit"] = {"ucum_code": s.unit}
            attrs[s.name] = a
        cd: dict = {"attributes": attrs}
        if c.is_a:
            cd["is_a"] = c.is_a
        d["classes"][c.name] = cd
    return d


def _tslot_attr(t: TSlot) -> dict:
    a: dict = {}
    if t.range:
        a["range"] = t.range
    if t.multivalued:
        a["multivalued"] = True
    if t.identifier:
        a["identifier"] = True
    if t.nested is not None:
        a["inlined"] = True
        if t.multivalued:
            a["inlined_as_list"] = True
    return a


def _collect_target_classes(blocks, out, w):
    for b in blocks:
        if not b.own_class and b.name in out:
            # same target class from two sources: union of attributes
            for t in b.slots:
                out[b.name]["attributes"].setdefault(t.name, _tslot_attr(t))
        else:
            cd = out.setdefault(b.name, {"attributes": {}})
            for t in b.slots:
                cd["attributes"][t.name] = _tslot_attr(t)
        for t in b.slots:
            if t.nested is not None:
                _collect_target_classes([t.nested], out, w)


def render_target(w: World) -> dict | None:
    if not w.has_target:
        return None
    d = schema_header("tgt")
    if w.tgt_enums:
        d["enums"] = {n: {"permissible_values": {pv: {} for pv in pvs}} for n, pvs in w.tgt_enums.items()}
    classes: dict = {}
    _collect_target_classes(w.blocks, classes, w)
    for b in w.blocks:
        if b.is_a and b.is_a in classes and b.name in classes:
            classes[b.name]["is_a"] = b.is_a
    d["classes"] = classes
    return d


_MAPPING_NESTED = {"on": False}


def _slot_derivs(slots: list[TSlot]) -> dict:
    out = {}
    for t in slots:
        out[t.name] = _deriv_body(t)
    return out


def _deriv_body(t: TSlot):
    body = dict(t.deriv) if t.deriv else {}
    if t.nested is not None:
        # list form is the documented one but Rust rejects it at slot level (reported); knownfree uses the mapping form
        body["class_derivations"] = ({t.nested.name: _block_body(t.nested)} if _MAPPING_NESTED["on"]
                                     else [_block_body(t.nested, with_name=True)])
    return body or None


def _block_body(b: Block, with_name=False) -> dict:
    body: dict = {}
    if with_name:
        body["name"] = b.name
    if b.is_a:
        body["is_a"] = b.is_a
    if b.populated_from and not b.omit_pf:
        body["populated_from"] = b.populated_from
    if b.joins:
        body["joins"] = b.joins
    if b.mint_ids is not None:
        body["mint_ids"] = b.mint_ids
    body["slot_derivations"] = _slot_derivs(b.slots)
    return body


def render_spec(w: World) -> dict:
    _MAPPING_NESTED["on"] = "knownfree" in w.feats
    spec: dict = {"id": "https://example.org/spec", "title": "fuzz spec"}
    spec.update(w.spec_extra)
    if w.list_form:
        spec["class_derivations"] = [_block_body(b, with_name=True) for b in w.blocks]
    else:
        spec["class_derivations"] = {b.name: _block_body(b) for b in w.blocks}
    if w.enum_derivs:
        spec["enum_derivations"] = w.enum_derivs
    return spec


def _dump(d) -> str:
    return yaml.safe_dump(d, sort_keys=False, allow_unicode=True, default_flow_style=False, width=1000)


def _yaml_value(v) -> str:
    if isinstance(v, Raw):
        return str(v)
    return yaml.safe_dump(v, allow_unicode=True, default_flow_style=True, width=10000).strip().removesuffix("...").strip()


def render_yaml_rows(rows: list[dict]) -> str:
    out = []
    for r in rows:
        if not r:
            out.append("- {}")
            continue
        first = True
        for k, v in r.items():
            out.append(("- " if first else "  ") + f"{k}: {_yaml_value(v)}")
            first = False
    return "\n".join(out) + "\n" if out else "[]\n"


def render_tsv(rows: list[dict], cols: list[str], delim: str, raw: bool) -> str:
    def cell(v):
        if v is None:
            return ""
        if isinstance(v, list):
            return "|".join(str(x) for x in v)
        return str(v)

    if raw:
        lines = [delim.join(cols)] + [delim.join(cell(r.get(c)) for c in cols) for r in rows]
        return "\n".join(lines) + "\n"
    buf = io.StringIO()
    wr = csv.writer(buf, delimiter=delim, lineterminator="\n")
    wr.writerow(cols)
    for r in rows:
        wr.writerow([cell(r.get(c)) for c in cols])
    return buf.getvalue()


def write_world(w: World, root: Path) -> H.Case:
    import json as _json

    root.mkdir(parents=True, exist_ok=True)
    (root / "schema.yaml").write_text(_dump(render_source(w)))
    (root / "spec.yaml").write_text(_dump(render_spec(w)))
    tgt = render_target(w)
    if tgt is not None:
        (root / "target.yaml").write_text(_dump(tgt))
    data = root / "data"
    data.mkdir(exist_ok=True)
    kind = w.input_kind
    cols = {c.name: [s.name for s in _induced(w.classes, c.name)] for c in w.classes}
    # The table a join is named after holds the class table's rows with their text changed (the key column apart), so
    # reading the class's file instead of the join's shows in the output.
    alias_files = {a: c for b in w.blocks for a, c in b.alias_files.items()}
    alias_rows = {
        c: [{k: ("J:" + v if isinstance(v, str) and k != cols[c][0] else v) for k, v in r.items()} for r in w.rows[c]]
        for c in set(alias_files.values())
    }
    raw = "rawtsv" in w.feats
    inp = "data"
    if kind in ("tsv_dir", "csv_dir"):
        ext, delim = ("tsv", "\t") if kind == "tsv_dir" else ("csv", ",")
        for c in w.classes:
            if w.rows[c.name] or True:
                (data / f"{c.name}.{ext}").write_text(render_tsv(w.rows[c.name], cols[c.name], delim, raw and kind == "tsv_dir"))
        for a, c in alias_files.items():
            (data / f"{a}.{ext}").write_text(render_tsv(alias_rows[c], cols[c], delim, raw and kind == "tsv_dir"))
    elif kind == "tsv_file":
        c = w.source_type or w.classes[0].name
        (data / f"{c}.tsv").write_text(render_tsv(w.rows[c], cols[c], "\t", raw))
        inp = f"data/{c}.tsv"
    elif kind == "yaml_dir":
        for c in w.classes:
            (data / f"{c.name}.yaml").write_text(render_yaml_rows(w.rows[c.name]))
        for a, c in alias_files.items():
            (data / f"{a}.yaml").write_text(render_yaml_rows(alias_rows[c]))
    elif kind == "json_dir":
        for c in w.classes:
            (data / f"{c.name}.json").write_text(_json.dumps([_plain(r) for r in w.rows[c.name]], ensure_ascii=False))
        for a, c in alias_files.items():
            (data / f"{a}.json").write_text(_json.dumps([_plain(r) for r in alias_rows[c]], ensure_ascii=False))
    elif kind == "yaml_single":
        c = w.source_type or w.classes[0].name
        r = w.rows[c][0]
        text = "\n".join(f"{k}: {_yaml_value(v)}" for k, v in r.items()) + "\n" if r else "{}\n"
        (data / f"{c}-001.yaml").write_text(text)
        inp = f"data/{c}-001.yaml"
    elif kind == "json_single":
        c = w.source_type or w.classes[0].name
        (data / f"{c}-001.json").write_text(_json.dumps(_plain(w.rows[c][0]), ensure_ascii=False))
        inp = f"data/{c}-001.json"
    return H.Case(root=str(root), schema="schema.yaml", spec="spec.yaml", input=inp, source_type=w.source_type,
                  target_schema="target.yaml" if tgt is not None else None, flags=list(w.flags))


def _plain(r: dict) -> dict:
    out = {}
    for k, v in r.items():
        if isinstance(v, Raw):
            v = str(v)
        out[k] = v
    return out
