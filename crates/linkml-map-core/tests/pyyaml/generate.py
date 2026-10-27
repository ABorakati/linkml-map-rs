#!/usr/bin/env python3
"""Regenerate `pyyaml_cases.json`, the differential corpus for `src/pyyaml.rs`.

Each case is `{"opts": {...}, "value": <encoded>, "expected": <text>}` where
`expected` is what upstream linkml-map's `dump_yaml` (PyYAML 6, pure-Python
`yaml.Dumper`, upstream's `_DecimalDumper` for decimals) writes. Usage:

    /home/user/upstream/upvenv/bin/python generate.py   # upstream's dump_yaml (or plain PyYAML)

For a bigger one-off fuzz run set PYYAML_SEED, PYYAML_RANDOM_CASES and
PYYAML_OUT, then run the Rust test with PYYAML_CORPUS=<that file>.

Value encoding: JSON null/bool/int/float/str/array/object map to the same
Python type; `{"$float": "inf" | "-inf" | "nan"}` and `{"$decimal": "<text>"}`
stand for values JSON cannot carry.
"""

import json
import os
import random
from decimal import Decimal
from pathlib import Path

import yaml


try:  # Prefer upstream's own function (e.g. `/home/user/upstream/upvenv/bin/python`).
    from linkml_map.utils.serialization import dump_yaml
except ImportError:  # Otherwise a copy of linkml_map/utils/serialization.py.

    class _DecimalDumper(yaml.Dumper):
        """PyYAML's default dumper, plus a plain-scalar representation for decimals."""

    def _represent_decimal(dumper, value):
        text = format(value, "f")
        tag = "tag:yaml.org,2002:float" if "." in text else "tag:yaml.org,2002:int"
        return dumper.represent_scalar(tag, text)

    _DecimalDumper.add_representer(Decimal, _represent_decimal)

    def dump_yaml(obj, **kwargs):
        kwargs.setdefault("default_flow_style", False)
        kwargs.setdefault("allow_unicode", True)
        return yaml.dump(obj, Dumper=_DecimalDumper, **kwargs)


def _fresh(v):
    return Decimal(str(v)) if isinstance(v, Decimal) else v


def encode(v):
    if isinstance(v, Decimal):
        return {"$decimal": str(v)}
    if isinstance(v, float) and (v != v or v in (float("inf"), float("-inf"))):
        return {"$float": "nan" if v != v else ("inf" if v > 0 else "-inf")}
    if isinstance(v, list):
        return [encode(x) for x in v]
    if isinstance(v, dict):
        return {k: encode(x) for k, x in v.items()}
    return v


CONFIGS = [
    # dump_yaml defaults (stream writer): block, unicode, sorted.
    {"flow": False, "unicode": True, "sort": True},
    # dump_output: sort_keys=False.
    {"flow": False, "unicode": True, "sort": False},
    # multivalued stringification: default_flow_style=True, allow_unicode=False.
    {"flow": True, "unicode": False, "sort": True},
    # yaml.dump defaults for the remaining combination.
    {"flow": False, "unicode": False, "sort": True},
    {"flow": True, "unicode": True, "sort": False},
]

CASES = []


def add(value, configs=None):
    for cfg in configs or CONFIGS:
        text = dump_yaml(
            value,
            default_flow_style=cfg["flow"],
            allow_unicode=cfg["unicode"],
            sort_keys=cfg["sort"],
        )
        CASES.append({"opts": cfg, "value": encode(value), "expected": text})


def add_string(s, configs=None):
    """A string as a root scalar, a value, a key and a list item."""
    add(s, configs)
    add({"k": s, "a": 1}, configs)
    add({s: "v", "z": [s]}, configs)
    add([s, [s, {"x": s}]], configs)


STRINGS = [
    "", " ", "  ", "a", "abc def", " lead", "trail ", " both ", "a  b", "a   b   c",
    "yes", "no", "on", "off", "Yes", "NO", "true", "False", "TRUE", "y", "n", "N",
    "null", "Null", "NULL", "~", "nul",
    "1", "0", "-1", "+1", "007", "0o7", "0b101", "0x1F", "0xg", "1_000", "1e3", "1E3",
    "1.5", ".5", "5.", "-.5", "1.0e+3", "1.0e3", "1e+3", ".inf", "-.inf", ".nan", ".NaN", ".Inf",
    "1:30", "1:30:45", "1:30.5", "190:20:30.15", "12:99", "1:60",
    "2024-01-01", "2024-1-1", "2001-12-14t21:59:43.10-05:00", "2001-12-14 21:59:43.10 -5",
    "2001-12-14 21:59:43.10 +05:30", "2001-12-14", "2001-12-14T21:59:43Z", "2001-12-14 2:5:3",
    "2024-01-01 ", "2024-01-01\n",
    "<<", "=", "!", "&", "*", "<", "~~",
    "yes\n", "null\n", "\n", "a\n", "a\nb", "a\n\nb", "a\n\n\nb", "\na", "a\n ", "a \nb", "a\n b",
    "a\n  b", "a\r\nb", "a\rb", "a\tb", "\ta", "a\t", "a\x00b", "a\x07b", "a\x1bb", "a\x7fb",
    "a\x85b", "a b", "a b", "a\xa0b", "\xa0", "a﻿b", "﻿",
    "a: b", "a:b", ":a", "a:", ": a", "a :", "key: value: more", "a #b", "a#b", "#a", " #a", "a ##",
    "- a", "-a", "-", "--", "---", "--- a", "...", "... a", "?", "? a", "?a", "a?", ",", "a,b", "[a]",
    "a[b", "{a}", "a}b", "*a", "&a", "!a", "|a", ">a", "%a", "@a", "`a", "'a'", "\"a\"", "a'b", "a\"b",
    "'", "\"", "''", "\\", "a\\b", "a\\", "\\n",
    "café", "日本語", "\U0001f600", "a\U0001f600b", "\U0010fffe", "\U0010ffff",
    "퟿", "", "�", "￾", "￿", "\U00010000",
    " x", "x ", " ", " ", "\x85",
    "Hello, World!", "It's a trap", "say \"hi\"", "it's \"both\"",
    "a" * 70, "a" * 200, "a b " * 30, ("word " * 40).strip(), "word " * 40, ("x" * 50 + " ") * 4,
    "k" * 122, "k" * 123, "k" * 127, "k" * 128, "k " * 61, ("k" * 60 + " ") * 2 + "k" * 60,
    "-" * 100, "a: " + "b " * 60, "'" * 5 + " " + "q " * 50, "\"" + "w " * 60 + "\"",
    "é " * 60, "日 " * 60, "\U0001f600 " * 60, "\n".join(["line"] * 30),
    "para one is here.\n\npara two follows with a good deal of text " * 3,
    "tab\there " * 30, "esc\x1b " * 30, ("long double quoted \\ text " * 8) + "\x01",
    ("\U0001f600 " * 40) + "\x01",
    "ends with newline\n", "ends with two\n\n", "  indented\nsecond", "x\n  y\nz",
    "http://example.org/path?x=1&y=2#frag", "P:0001", "OMOP:12345", "2 weeks", "1 2",
    "a" * 79, "a" * 80, "a" * 81, "a " * 39 + "a", "a " * 40 + "a", "a " * 41,
    "x" * 76 + " yz", "x" * 77 + " yz", "x" * 78 + " y z", "x" * 79 + " yz",
    "inline 'quoted' text that is quite long and goes past the eighty column mark for sure yes",
    "inline 'quoted' text that is quite long and goes past the eighty column mark for sure yes\nnext",
    "a" * 60 + "  " + "b" * 30, "a" * 60 + "    " + "b" * 30,
]

for _s in STRINGS:
    add_string(_s)

# ── scalars of other types ───────────────────────────────────────────────────
for _v in [
    None, True, False, 0, 1, -1, 42, 2**31, -(2**31), 2**53, 2**63 - 1, -(2**63),
    0.0, -0.0, 1.0, -1.0, 0.1, 0.5, 1.5, 3.14, 1e16, 1e15, 123456789012345.0, 1e17,
    1e20, 1e21, -1e20, 1.5e20, 1e22, 1e-4, 1e-5, 1e-7, 1.5e-7, 5e-324, 1.7976931348623157e308,
    float("inf"), float("-inf"), float("nan"), 172.5, 52.3, 0.30000000000000004, 100.0, 12345.6789,
    Decimal("52.30"), Decimal("52"), Decimal("0.0000001"), Decimal("-3.25"), Decimal("1E-7"),
    Decimal("1E+3"), Decimal("0"), Decimal("-0"), Decimal("0.000"), Decimal("100"),
    Decimal("0.123456789012345678901"), Decimal("123456789012345678901234567890"),
    Decimal("-123456789012345678901234567890.5"), Decimal("7"),
]:
    add(_v)
    # PyYAML anchors a repeated *Decimal object* (`&id001` / `*id001`) because
    # `ignore_aliases` does not exempt it; the Rust port cannot see object
    # identity (see the module docs), so each use below is a distinct object.
    add({"v": _v, "w": [_fresh(_v), _fresh(_v)]})
    add([_fresh(_v), {"x": _fresh(_v)}])

# ── containers ───────────────────────────────────────────────────────────────
for _v in [
    {}, [], [[]], [{}], {"a": {}}, {"a": []}, {"a": [[]]}, {"a": [{}]}, [[], []], [{}, {}],
    {"a": None}, {"a": [None, None]}, [None], {"": 1}, {"": ""}, {"a": ""}, {"": {}},
    {"b": 1, "a": 2, "B": 3, "A": 4, "_": 5, "1": 6, "10": 7, "2": 8, "é": 9, "z": 10, "\U0001f600": 11, "￿": 12},
    {"a": {"b": {"c": {"d": 1}}}},
    {"a": [1, 2, [3, 4, [5, 6]]]},
    {"a": [{"b": 1, "c": [1, 2]}, {"d": {"e": [3]}}]},
    [[1, 2], [3, [4, 5]], {"k": [6, 7]}],
    [[[]]], [[[1]]], [[], [[]]], [{"a": {}}, {"b": []}],
    {"list": [1, 2, 3], "map": {"x": 1}, "scalar": "s", "n": None, "t": True, "f": 1.5},
    {"id": "P:1", "name": "Alice Smith", "age": 30, "tags": ["a", "b"], "address": {"city": "London", "zip": "SW1A 1AA"}},
    {"long list": list(range(60))},
    list(range(60)),
    [str(i) for i in range(40)],
    ["word%d" % i for i in range(30)],
    {"k%d" % i: "v%d" % i for i in range(30)},
    [{"k%d" % i: "value number %d" % i for i in range(6)}],
    [["item %d of the list that is wide" % i for i in range(5)]],
    [{"a": "x" * 30, "b": "y" * 30, "c": "z" * 30}],
    {"x" * 60: {"y" * 60: "z" * 60}},
    {"key with spaces": "value with spaces", "k:colon": "v:colon", "k #hash": "v #hash"},
    {"multi\nline": 1}, {"multi\nline": [1]}, {"a b": {"c d": ["e f", "g h"]}},
    {"k" * 123: 1}, {"k" * 122: 1}, {"k" * 130: [1, 2]}, {"k" * 130: {"a": 1}}, {"k" * 130: {}},
    {"yes": 1, "no": 2, "null": 3, "1": 4, "1.5": 5, "~": 6, "2024-01-01": 7},
    {"a": Decimal("1.50"), "b": [Decimal("2"), Decimal("-0.5")], "c": {"d": Decimal("1E-7")}},
    {"mixed": [1, "1", 1.0, "1.0", True, "true", None, "null", [], {}]},
    ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q", "r", "s", "t",
     "u", "v", "w", "x", "y", "z", "aa", "bb", "cc", "dd", "ee", "ff", "gg", "hh", "ii", "jj", "kk", "ll"],
    {"a": ["a long item in a list that makes the flow line wrap past 80 columns " + str(i) for i in range(4)]},
    {"a": {"b": ["c " * 30, "d " * 30]}, "e": "f " * 50},
]:
    add(_v)

# ── random strings ───────────────────────────────────────────────────────────
PIECES = [
    "a", "b", "z", "Q", "0", "1", "9", " ", " ", " ", "  ", "-", "?", ":", ": ", " #", "#", ",", "[", "]",
    "{", "}", "'", "\"", "\\", "&", "*", "!", "|", ">", "%", "@", "`", ".", "...", "---", "\n", "\n", "\t", "\r",
    "\xe9", "日", "\U0001f600", " ", " ", "\x85", "\xa0", "﻿", "\x00", "\x1b", "\x7f", "\x01",
    "yes", "no", "null", "true", "~", "1e3", "0x1F", "2024-01-01", "1:30", ".5", ".inf", "<<", "=",
    "word", "words", "Lorem", "ipsum", "dolor", "sit", "amet", "consectetur", "adipiscing",
]
_rng = random.Random(int(os.environ.get("PYYAML_SEED", "20261027")))
_N = int(os.environ.get("PYYAML_RANDOM_CASES", "600"))


def random_string():
    n = _rng.choice([1, 2, 3, 4, 6, 8, 12, 20, 40, 80])
    return "".join(_rng.choice(PIECES) for _ in range(n))


for _ in range(_N):
    _s = random_string()
    # Every string as a block value / key and a flow item under the three main configs.
    add({"k": _s}, CONFIGS[:3])
    add([_s], CONFIGS[:3])
    add({_s: 1}, CONFIGS[:1] + CONFIGS[2:3])
for _ in range(_N // 4):
    _s = random_string() + " " + random_string() + " " + random_string()
    add({"k": _s, "l": [_s]}, CONFIGS[:3])
    add(_s, CONFIGS[:1])

out = Path(os.environ.get("PYYAML_OUT") or Path(__file__).with_name("pyyaml_cases.json"))
out.write_text(
    json.dumps({"pyyaml": yaml.__version__, "cases": CASES}, ensure_ascii=True, separators=(",", ":")) + "\n"
)
print(f"wrote {len(CASES)} cases to {out} (PyYAML {yaml.__version__})")
