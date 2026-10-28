#!/usr/bin/env python3
"""Hypothesis generator of (source schema, specification) pairs for `invert`.

    /home/user/upstream/upvenv/bin/python tests/invert/fuzz.py --emit N --seed S [--out DIR]
        write N pairs as DIR/<id>.schema.yaml + DIR/<id>.spec.yaml (default `inputs/fuzz/`);
        `generate.py --discover` then records upstream's answers for them.
    /home/user/upstream/upvenv/bin/python tests/invert/fuzz.py --run N --seed S [--keep DIR]
        generate N pairs and compare upstream with `linkml-tr-rs invert` (binary from
        $LINKML_TR_RS, default the release build) in both modes; mismatching pairs are copied
        to DIR (default `/tmp/invert-found`) and listed.

The generator aims at every branch of `inverter.py`: renamed / implicit / expression-driven /
hidden slot derivations, source classes that exist, are unknown, empty or missing, slots that
resolve through `slot_usage`, `attributes`, slot `is_a`, mixins and `underscore()` name
normalisation, class ranges with and without identifier / key slots, inlined lists and dicts,
all four unit metaslots, structured units, stringification, and enum derivations with zero,
one or many source values.
"""

import argparse
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

import yaml
from hypothesis import HealthCheck, given, seed, settings
from hypothesis import strategies as st

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
UPSTREAM = "/home/user/upstream/upvenv/bin/linkml-map"
SCALARS = ["string", "integer", "float", "boolean", "date"]
CLASSES = ["Alpha", "Beta", "Gamma"]
SLOT_NAMES = ["a", "b", "c", "d", "e", "f", "g", "full name", "x-y"]
ENUMS = ["Kind", "Mode"]
PVS = ["P", "Q", "R", "S"]


def maybe(draw, strategy, p=0.5):
    return draw(strategy) if draw(st.floats(0, 1)) < p else None


@st.composite
def unit(draw):
    keys = draw(st.lists(st.sampled_from(["ucum_code", "symbol", "iec61360code", "abbreviation", "descriptive_name"]), min_size=1, max_size=3, unique=True))
    return {k: draw(st.sampled_from(["kg", "m", "[lb_av]", "cm", "s", "u1", ""])) for k in keys}


@st.composite
def schema(draw):
    classes = CLASSES[: draw(st.integers(1, 3))]
    slot_names = draw(st.lists(st.sampled_from(SLOT_NAMES), min_size=2, max_size=6, unique=True))
    sch = {"id": "https://example.org/fz", "name": "fz"}
    if draw(st.booleans()):
        sch["default_range"] = draw(st.sampled_from(["string", "integer"]))
    ranges = [None, *SCALARS, *ENUMS, *classes]
    slots = {}
    for i, sn in enumerate(slot_names):
        body = {}
        r = draw(st.sampled_from(ranges))
        if r:
            body["range"] = r
        if draw(st.floats(0, 1)) < 0.3:
            body["multivalued"] = True
        if r in classes:
            for key in ("inlined", "inlined_as_list"):
                if draw(st.floats(0, 1)) < 0.3:
                    body[key] = draw(st.booleans())
        if draw(st.floats(0, 1)) < 0.15:
            body[draw(st.sampled_from(["identifier", "key"]))] = True
        if draw(st.floats(0, 1)) < 0.3:
            body["unit"] = draw(unit())
        if i and draw(st.floats(0, 1)) < 0.2:
            body["is_a"] = slot_names[draw(st.integers(0, i - 1))]
        slots[sn] = body
    sch["slots"] = slots
    cls = {}
    for i, cn in enumerate(classes):
        body = {}
        if i and draw(st.floats(0, 1)) < 0.4:
            body["is_a"] = classes[draw(st.integers(0, i - 1))]
        if i and draw(st.floats(0, 1)) < 0.2:
            body["mixins"] = [classes[draw(st.integers(0, i - 1))]]
        used = draw(st.lists(st.sampled_from(slot_names), max_size=len(slot_names), unique=True))
        if used:
            body["slots"] = used
        if used and draw(st.floats(0, 1)) < 0.4:
            usage = {}
            for sn in draw(st.lists(st.sampled_from(used), min_size=1, max_size=2, unique=True)):
                u = {}
                if draw(st.booleans()):
                    u["range"] = draw(st.sampled_from(ranges[1:]))
                if draw(st.booleans()):
                    u["multivalued"] = draw(st.booleans())
                if draw(st.floats(0, 1)) < 0.3:
                    u["inlined"] = draw(st.booleans())
                if draw(st.floats(0, 1)) < 0.3:
                    u["inlined_as_list"] = draw(st.booleans())
                if draw(st.floats(0, 1)) < 0.2:
                    u["identifier"] = draw(st.booleans())
                if draw(st.floats(0, 1)) < 0.2:
                    u["unit"] = draw(unit())
                usage[sn] = u
            body["slot_usage"] = usage
        if draw(st.floats(0, 1)) < 0.4:
            attrs = {}
            for an in draw(st.lists(st.sampled_from(["att1", "att2", "k"]), min_size=1, max_size=2, unique=True)):
                a = {}
                if draw(st.booleans()):
                    a["range"] = draw(st.sampled_from(ranges))
                    if a["range"] is None:
                        del a["range"]
                if draw(st.booleans()):
                    a["multivalued"] = True
                if draw(st.floats(0, 1)) < 0.3:
                    a[draw(st.sampled_from(["identifier", "key"]))] = True
                if draw(st.floats(0, 1)) < 0.3:
                    a["unit"] = draw(unit())
                attrs[an] = a
            body["attributes"] = attrs
        cls[cn] = body
    sch["classes"] = cls
    sch["enums"] = {e: {"permissible_values": {p: {} for p in PVS}} for e in ENUMS}
    return sch


def source_slots(sch):
    names = list(sch["slots"])
    for body in sch["classes"].values():
        names += list(body.get("attributes", {}))
    return names


@st.composite
def slot_derivation(draw, sch):
    names = source_slots(sch)
    body = {}
    pf_kind = draw(st.sampled_from(["real", "real", "real", "ghost", "none", "empty", "underscored"]))
    if pf_kind == "real":
        body["populated_from"] = draw(st.sampled_from(names))
    elif pf_kind == "ghost":
        body["populated_from"] = "ghost_slot"
    elif pf_kind == "empty":
        body["populated_from"] = ""
    elif pf_kind == "underscored":
        body["populated_from"] = draw(st.sampled_from(names)).replace(" ", "_").replace("-", "_")
    ex = draw(st.sampled_from([None] * 6 + ["bare", "complex", "nl", "empty", "word_real"]))
    if ex == "bare":
        body["expr"] = "bare_name"
    elif ex == "complex":
        body["expr"] = "a + 1"
    elif ex == "nl":
        body["expr"] = draw(st.sampled_from(names)).replace(" ", "_") + "\n"
    elif ex == "empty":
        body["expr"] = ""
    elif ex == "word_real":
        body["expr"] = draw(st.sampled_from(names)).replace(" ", "_")
    if draw(st.floats(0, 1)) < 0.35:
        body["range"] = draw(st.sampled_from([*SCALARS, *CLASSES, *ENUMS, "ghost_range"]))
    if draw(st.floats(0, 1)) < 0.3:
        uc = {}
        for key, vals in [("target_unit", ["g", "cm", "", "kg"]), ("source_unit_slot", ["u", ""]), ("source_magnitude_slot", ["m", ""]),
                          ("target_unit_slot", ["tu"]), ("target_magnitude_slot", ["tm"]), ("source_unit", ["m"]), ("target_unit_scheme", ["ucum_code"]),
                          ("source_unit_scheme", ["symbol"])]:
            if draw(st.floats(0, 1)) < 0.4:
                uc[key] = draw(st.sampled_from(vals))
        if draw(st.floats(0, 1)) < 0.2:
            uc["none_if_non_numeric"] = draw(st.booleans())
        body["unit_conversion"] = uc
    if draw(st.floats(0, 1)) < 0.2:
        sf = {}
        if draw(st.booleans()):
            sf["delimiter"] = draw(st.sampled_from(["|", ",", ""]))
        if draw(st.booleans()):
            sf["reversed"] = draw(st.booleans())
        if draw(st.floats(0, 1)) < 0.3:
            sf["syntax"] = draw(st.sampled_from(["JSON", "YAML", "TURTLE"]))
        if draw(st.floats(0, 1)) < 0.3:
            sf["over_slots"] = draw(st.lists(st.sampled_from(names), max_size=2))
        body["stringification"] = sf
    if draw(st.floats(0, 1)) < 0.12:
        body["hide"] = draw(st.booleans())
    if draw(st.floats(0, 1)) < 0.08:
        body["value_mappings"] = {"x": {"key": "x", "value": "y"}}
    if draw(st.floats(0, 1)) < 0.05:
        body["cast_collection_as"] = draw(st.sampled_from(["MultiValuedList", "MultiValuedDict", "SingleValued"]))
    if draw(st.floats(0, 1)) < 0.05:
        body["class_derivations"] = [{"name": "Nested", "populated_from": draw(st.sampled_from(CLASSES)), "slot_derivations": {"n": {"populated_from": draw(st.sampled_from(names))}}}]
    return body


@st.composite
def spec(draw, sch):
    out = {}
    cds = []
    for i in range(draw(st.integers(0, 3))):
        cd = {"name": draw(st.sampled_from([*CLASSES, "Target1", "Target2"]))}
        pf = draw(st.sampled_from(["real", "real", "real", "ghost", "none", "empty"]))
        if pf == "real":
            cd["populated_from"] = draw(st.sampled_from(list(sch["classes"])))
        elif pf == "ghost":
            cd["populated_from"] = "GhostClass"
        elif pf == "empty":
            cd["populated_from"] = ""
        sds = {}
        for _ in range(draw(st.integers(0, 4))):
            key = draw(st.sampled_from(["t1", "t2", "t3", "a", "b", "c", "full name"]))
            sds[key] = draw(slot_derivation(sch)) if draw(st.floats(0, 1)) < 0.9 else None
        if sds:
            cd["slot_derivations"] = sds
        cds.append(cd)
    out["class_derivations"] = cds
    eds = {}
    for _ in range(draw(st.integers(0, 2))):
        en = draw(st.sampled_from([*ENUMS, "E3"]))
        ed = {}
        if draw(st.floats(0, 1)) < 0.8:
            ed["populated_from"] = draw(st.sampled_from([*ENUMS, "Ghost"]))
        if draw(st.floats(0, 1)) < 0.15:
            ed["expr"] = "x"
        if draw(st.floats(0, 1)) < 0.1:
            ed["hide"] = True
        pvds = {}
        for _ in range(draw(st.integers(0, 4))):
            pv = {}
            kind = draw(st.sampled_from(["none", "one", "one", "list1", "list2", "empty", "str"]))
            if kind == "one":
                pv["populated_from"] = [draw(st.sampled_from(PVS))]
            elif kind == "list1":
                pv["populated_from"] = [draw(st.sampled_from(PVS))]
            elif kind == "list2":
                pv["populated_from"] = draw(st.lists(st.sampled_from(PVS), min_size=2, max_size=3))
            elif kind == "empty":
                pv["populated_from"] = []
            elif kind == "str":
                pv["populated_from"] = draw(st.sampled_from(PVS))
            if draw(st.floats(0, 1)) < 0.1:
                pv["expr"] = "y"
            pvds[draw(st.sampled_from(["W", "X", "Y", "Z", *PVS]))] = pv or None
        if pvds:
            ed["permissible_value_derivations"] = pvds
        eds[en] = ed or None
    if eds:
        out["enum_derivations"] = eds
    return out


@st.composite
def world(draw):
    sch = draw(schema())
    return sch, draw(spec(sch))


def dump(obj):
    return yaml.safe_dump(obj, sort_keys=False, allow_unicode=True)


def generate(n, seed_value):
    found = []

    @seed(seed_value)
    @settings(max_examples=n, database=None, deadline=None, derandomize=False, suppress_health_check=list(HealthCheck))
    @given(world())
    def collect(w):
        found.append(w)

    collect()
    return found


def run(cmd):
    p = subprocess.run(cmd, capture_output=True, text=True)
    return p


def campaign(n, seed_value, keep):
    rust = os.environ.get("LINKML_TR_RS", str(Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target")) / "release/linkml-tr-rs"))
    keep = Path(keep)
    keep.mkdir(parents=True, exist_ok=True)
    bad = 0
    with tempfile.TemporaryDirectory() as tmp:
        for i, (sch, sp) in enumerate(generate(n, seed_value)):
            s, t = Path(tmp) / "s.yaml", Path(tmp) / "t.yaml"
            s.write_text(dump(sch))
            t.write_text(dump(sp))
            for flags in ([], ["--no-strict"]):
                up = run([UPSTREAM, "invert", *flags, "-T", str(t), str(s)])
                rs = run([rust, "invert", *flags, "-T", str(t), str(s)])
                same = (up.returncode == 0) == (rs.returncode == 0) and (up.returncode != 0 or up.stdout == rs.stdout)
                if same and up.returncode:
                    last = up.stderr.strip().splitlines()[-1] if up.stderr.strip() else ""
                    if any(k in last for k in ("NonInvertibleSpecificationError", "AttributeError", "ValueError:")):
                        msg = last.split(": ", 1)[-1]
                        same = msg in rs.stderr
                if not same:
                    bad += 1
                    d = keep / f"seed{seed_value}_{i}{'_nostrict' if flags else ''}"
                    d.mkdir(exist_ok=True)
                    shutil.copy(s, d / "schema.yaml")
                    shutil.copy(t, d / "spec.yaml")
                    (d / "upstream.out").write_text(up.stdout + "\n--- stderr\n" + up.stderr[-600:])
                    (d / "rust.out").write_text(rs.stdout + "\n--- stderr\n" + rs.stderr[-600:])
                    print("MISMATCH", d)
    print(f"{n} worlds, {bad} mismatches")
    return bad


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--emit", type=int)
    ap.add_argument("--run", type=int)
    ap.add_argument("--seed", type=int, default=0)
    ap.add_argument("--out", default=str(HERE / "inputs/fuzz"))
    ap.add_argument("--keep", default="/tmp/invert-found")
    a = ap.parse_args()
    if a.emit:
        out = Path(a.out)
        out.mkdir(parents=True, exist_ok=True)
        for i, (sch, sp) in enumerate(generate(a.emit, a.seed)):
            (out / f"s{a.seed}_{i:03d}.schema.yaml").write_text(dump(sch))
            (out / f"s{a.seed}_{i:03d}.spec.yaml").write_text(dump(sp))
        print(f"wrote {a.emit} pairs to {out}")
    if a.run:
        sys.exit(1 if campaign(a.run, a.seed, a.keep) else 0)


if __name__ == "__main__":
    main()
