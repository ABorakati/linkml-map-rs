#!/usr/bin/env python
"""Generate crates/linkml-map-core/src/expr/pyunicode.rs: CPython `str` predicate tables.

Run with CPython 3.13 (Unicode 15.1): the tables are what `str.isalpha`,
`isdigit`, `isnumeric`, `isprintable` and `isidentifier` (and the tokenizer's
identifier rule) answer, as sorted inclusive code-point ranges.

    python gen_pyunicode.py > ../../../crates/linkml-map-core/src/expr/pyunicode.rs
"""

import sys
import unicodedata


def ranges(pred):
    out, start, prev = [], None, None
    for cp in range(0x110000):
        if 0xD800 <= cp <= 0xDFFF:
            continue
        if pred(chr(cp)):
            if start is None:
                start = prev = cp
            elif cp == prev + 1:
                prev = cp
            else:
                out.append((start, prev))
                start = prev = cp
    if start is not None:
        out.append((start, prev))
    return out


def table(name, doc, pred):
    rs = ranges(pred)
    body = ", ".join(f"(0x{a:X}, 0x{b:X})" for a, b in rs)
    return f"/// {doc}\npub(super) const {name}: &[(u32, u32)] = &[{body}];\n\n"


print(f"//! CPython `str` predicate tables, generated from CPython {sys.version.split()[0]} (Unicode {unicodedata.unidata_version})")
print("//! by `tests/differential/expr/gen_pyunicode.py`; do not edit by hand.\n")
print("#![allow(clippy::unreadable_literal)]\n")
sys.stdout.write(table("ALPHA", "`str.isalpha()` (categories Lm, Lt, Lu, Ll, Lo).", str.isalpha))
sys.stdout.write(table("DIGIT", "`str.isdigit()` (Numeric_Type Digit or Decimal).", str.isdigit))
sys.stdout.write(table("NUMERIC", "`str.isnumeric()`.", str.isnumeric))
sys.stdout.write(table("NON_PRINTABLE", "Code points `str.isprintable()` rejects.", lambda c: not c.isprintable()))
sys.stdout.write(table("XID_START", "`str.isidentifier()` of the character alone (XID_Start, plus `_`).", str.isidentifier))
sys.stdout.write(table("XID_CONTINUE", "`('a' + c).isidentifier()` (XID_Continue).", lambda c: ("a" + c).isidentifier()))
