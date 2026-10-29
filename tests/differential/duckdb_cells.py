"""Oracle for ``crates/linkml-map-pipeline/src/join_rows.rs``: random JSON tables through upstream's DuckDB read.

Upstream's set-based join engine reads a JSON primary table with
``SELECT CAST(columns(*) AS VARCHAR) FROM read_json_auto(?)`` and runs ``_parse_numeric`` over every cell
(``linkml_map.transformer.join_engine``).  This script writes random JSON tables (big integers, floats of every
magnitude, booleans, ISO dates/times/timestamps in every format DuckDB tries, UUIDs, numeric-looking strings,
lists, objects, mixed columns) through exactly that query, and records the raw JSON text with the cells upstream
ends up with.  The Rust unit test ``join_rows::tests::replays_duckdb_cells`` replays the fixture.

Run it with the upstream venv (the DuckDB version upstream pins is the oracle)::

    /home/user/upstream/upvenv/bin/python tests/differential/duckdb_cells.py --n 3000 --seed 1 \
        --out crates/linkml-map-pipeline/src/join_rows/duckdb_cells.jsonl

Fixture line: ``{"json": <file text>, "rows": [{col: cell}, ...]}`` or ``{"json": ..., "error": <message>}``.
A cell is ``null``, ``["i", "<digits>"]`` (Python ``int``), ``["f", "<f64 bits in hex>"]`` (``float``) or
``["s", "<text>"]`` (``str``).  ``--check FILE`` re-runs a fixture against the installed DuckDB (to see whether
a DuckDB upgrade changes anything).
"""

from __future__ import annotations

import argparse
import json
import os
import random
import struct
import sys
import tempfile

import duckdb
from linkml_map.utils.lookup_index import _parse_numeric, make_connection


class Raw(str):
    """A JSON token written exactly as given (``-0``, ``1E+2``, ...)."""


def enc_json(v) -> str:
    if isinstance(v, Raw):
        return str(v)
    if isinstance(v, bool):
        return "true" if v else "false"
    if v is None:
        return "null"
    if isinstance(v, int):
        return str(v)
    if isinstance(v, float):
        return repr(v)
    if isinstance(v, str):
        return json.dumps(v, ensure_ascii=random.random() < 0.5)
    if isinstance(v, list):
        return "[" + ", ".join(enc_json(x) for x in v) + "]"
    if isinstance(v, dict):
        return "{" + ", ".join(json.dumps(k) + ": " + enc_json(x) for k, x in v.items()) + "}"
    raise TypeError(v)


def enc_cell(v):
    if v is None:
        return None
    if isinstance(v, bool):
        raise TypeError("bool cell")
    if isinstance(v, int):
        return ["i", str(v)]
    if isinstance(v, float):
        return ["f", struct.pack(">d", v).hex()]
    return ["s", v]


def read(text: str):
    """What upstream's join engine sees for a JSON primary table with this text."""
    fd, path = tempfile.mkstemp(suffix=".json")
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as f:
            f.write(text)
        con = make_connection()
        try:
            sql = "SELECT * FROM (SELECT CAST(columns(*) AS VARCHAR) FROM read_json_auto(?)) m"
            cur = con.execute(sql, [path])
            names = [d[0] for d in cur.description]
            rows = [{c: enc_cell(_parse_numeric(v)) for c, v in zip(names, r)} for r in cur.fetchall()]
            return {"rows": rows, "names": names}
        except duckdb.Error as e:
            return {"error": type(e).__name__ + ": " + str(e).splitlines()[0][:160]}
        finally:
            con.close()
    finally:
        os.unlink(path)


# ── value generators ─────────────────────────────────────────────────────────

I64_MIN, I64_MAX, U64_MAX = -(2**63), 2**63 - 1, 2**64 - 1


def gen_int(r: random.Random):
    k = r.random()
    if k < 0.25:
        return r.randint(-1000, 1000)
    if k < 0.4:
        return r.choice([I64_MIN, I64_MAX, I64_MIN + 1, I64_MAX - 1, 0, 1, -1]) + r.choice([0, 0, 0, 1, -1])
    if k < 0.6:
        return r.choice([I64_MAX, U64_MAX, 2**63, I64_MIN]) + r.randint(-3, 3)
    if k < 0.7:
        return r.randint(I64_MAX, U64_MAX)
    if k < 0.8:
        return -r.randint(2**63, 2**64)
    if k < 0.9:
        return r.randint(-(10 ** r.randint(19, 45)), 10 ** r.randint(19, 45))
    if k < 0.95:
        return r.choice([2**127, 2**127 - 1, -(2**127), 2**128, 10**38, -(10**38), 2**53 + 1, 2**64, -(2**64)])
    return r.choice([1, -1]) * 10 ** r.randint(300, 330) + r.randint(0, 9)


def gen_float(r: random.Random):
    k = r.random()
    if k < 0.2:
        return r.randint(-100000, 100000) / r.choice([1, 10, 100, 8, 3])
    if k < 0.3:
        return float(r.randint(-(10**18), 10**18))
    if k < 0.45:
        return r.choice([1e15, 1e16, 1e17, 1e-4, 1e-5, 1e22, 1e21, 123456789012345678.0, 0.1, 0.30000000000000004,
                         5e-324, 1.7976931348623157e308, 2.2250738585072014e-308, 9007199254740993.0, 2.0**63, 2.0**64,
                         -(2.0**63), 4.35, 100.0, 1e100, -0.0, 0.0])
    if k < 0.6:
        return r.uniform(-1, 1) * 10 ** r.randint(-30, 30)
    if k < 0.75:
        return struct.unpack(">d", struct.pack(">Q", r.getrandbits(64)))[0] if r.random() < 0.9 else 0.5
    return round(r.uniform(-1000, 1000), r.randint(0, 6))


def raw_number(r: random.Random, nested=False):
    # A float beyond f64 is raw text for DuckDB: inside a JSON-typed list or object it keeps its spelling, which a
    # parsed value cannot, so those only appear as whole cells.
    if not nested and r.random() < 0.15:
        return Raw(r.choice(["1e400", "-1e400", "1E400", "-1.5e+999"]))
    return Raw(r.choice(["-0", "1E2", "1e+2", "1.0E-5", "0.0", "-0.0", "2.50", "1E0", "12e-1", "0e5",
                         "123456789012345678901234567890.0", "4e2", "1.5e300", "100000000000000000000", "-0.5e-3"]))


def pad(n, w):
    return str(n).zfill(w)


def gen_date_str(r: random.Random):
    y, m, d = r.randint(1, 9999), r.randint(1, 12), r.randint(1, 31)
    if r.random() < 0.5:
        y = r.randint(1900, 2100)
    if r.random() < 0.7:
        d = min(d, 28)
    yy = y % 100
    f = r.choice(["ymd", "ymd", "ymd", "mdy", "dmy", "ymd2", "mdy2", "dmy2", "slash", "dots", "compact", "unpad", "us"])
    s = {
        "ymd": f"{pad(y, 4)}-{pad(m, 2)}-{pad(d, 2)}",
        "mdy": f"{pad(m, 2)}-{pad(d, 2)}-{pad(y, 4)}",
        "dmy": f"{pad(d, 2)}-{pad(m, 2)}-{pad(y, 4)}",
        "ymd2": f"{pad(yy, 2)}-{pad(m, 2)}-{pad(d, 2)}",
        "mdy2": f"{pad(m, 2)}-{pad(d, 2)}-{pad(yy, 2)}",
        "dmy2": f"{pad(d, 2)}-{pad(m, 2)}-{pad(yy, 2)}",
        "slash": f"{pad(y, 4)}/{pad(m, 2)}/{pad(d, 2)}",
        "dots": f"{pad(d, 2)}.{pad(m, 2)}.{pad(y, 4)}",
        "compact": f"{pad(y, 4)}{pad(m, 2)}{pad(d, 2)}",
        "unpad": f"{y}-{m}-{d}",
        "us": f"{m}/{d}/{y}",
    }[f]
    return mutate_space(r, s)


def mutate_space(r: random.Random, s: str) -> str:
    k = r.random()
    if k < 0.04:
        return " " + s
    if k < 0.08:
        return s + " "
    if k < 0.1:
        return s + "\t"
    return s


def gen_time_part(r: random.Random, ampm=False):
    h, mi, s = r.randint(0, 23), r.randint(0, 59), r.randint(0, 59)
    if ampm:
        h12 = r.randint(1, 12)
        return f"{pad(h12, 2)}:{pad(mi, 2)}:{pad(s, 2)} {r.choice(['AM', 'PM', 'am', 'pm', 'Pm'])}"
    base = f"{pad(h, 2)}:{pad(mi, 2)}:{pad(s, 2)}"
    k = r.random()
    if k < 0.3:
        base += "." + "".join(r.choice("0123456789") for _ in range(r.randint(1, 9)))
    elif k < 0.35:
        base = f"{h}:{mi}:{s}"
    return base


def gen_ts_str(r: random.Random):
    d = gen_date_str(r).strip()
    sep = r.choice([" ", " ", "T", "T", "t", "  "])
    t = gen_time_part(r, ampm=r.random() < 0.15)
    tz = r.choice(["", "", "", "Z", "z", "+02:00", "-08:00", "+0530", "+05", "-0000", " UTC", "+14:00", "+02:00:30", "+2"])
    return mutate_space(r, d + sep + t + tz)


def gen_time_str(r: random.Random):
    k = r.random()
    if k < 0.1:
        return r.choice(["24:00:00", "24:00:01", "00:00", "1:2", "23:59:59.999999", "10:00:00Z", "99:00:00", "12:00:60", "7",
                         "10:00:00.1234567", "12:00:00 PM", "T10:00:00", "10:00:00+02", "-1:00:00", "010:00:00"])
    return mutate_space(r, gen_time_part(r, ampm=False)[: r.choice([5, 8, 99, 99])])


def gen_uuid_str(r: random.Random):
    h = "".join(r.choice("0123456789abcdefABCDEF") for _ in range(32))
    k = r.random()
    if k < 0.5:
        return f"{h[:8]}-{h[8:12]}-{h[12:16]}-{h[16:20]}-{h[20:]}"
    if k < 0.65:
        return f"{h[:8]}-{h[8:12]}-{h[12:16]}-{h[16:20]}-{h[20:]}".lower()
    if k < 0.75:
        return h
    if k < 0.85:
        return "{" + f"{h[:8]}-{h[8:12]}-{h[12:16]}-{h[16:20]}-{h[20:]}" + "}"
    return f"{h[:8]}-{h[8:12]}-{h[12:16]}-{h[16:20]}-{h[20:31]}"


def gen_plain_str(r: random.Random):
    return r.choice(["", "x", "hello world", "a,b", "O'Brien", 'say "hi"', "back\\slash", "tab\there", "line\nbreak",
                     "naïve", "日本", " ", "null", "NULL", "true", "False", "007", "1e3", "-5", "1_000", " 12 ", "inf",
                     "nan", "0x10", "3.14", "1,5", "+7", ".5", "5.", "epoch", "infinity", "-infinity", "Epoch", "EPOCH",
                     "Infinity", "2020-02-30", "2020-13-01", "0000-01-01", "10000-01-01", "-2020-01-01", "20200101",
                     "now", "today", "12345678901234567890", "-9223372036854775809", "[1,2]", "{}", "a" * 40, "😀"])


def gen_numstr(r: random.Random):
    return r.choice(["007", "1e3", "-5", "1_000", " 12 ", "3.14", "+7", ".5", "5.", "12345678901234567890", "-0", "0.0",
                     str(r.randint(-100, 100)), str(gen_int(r)), "-1e-400", "٣", "1__0"])


def gen_str(r: random.Random):
    k = r.random()
    if k < 0.25:
        return gen_date_str(r)
    if k < 0.45:
        return gen_ts_str(r)
    if k < 0.55:
        return gen_time_str(r)
    if k < 0.65:
        return gen_uuid_str(r)
    if k < 0.75:
        return gen_numstr(r)
    return gen_plain_str(r)


def gen_scalar(r: random.Random, family: str, nested=False):
    if family == "int":
        return gen_int(r)
    if family == "smallint":
        return r.randint(-50, 50)
    if family == "float":
        return gen_float(r)
    if family == "num":
        return gen_int(r) if r.random() < 0.5 else gen_float(r)
    if family == "bool":
        return r.random() < 0.5
    if family == "date":
        return gen_date_str(r)
    if family == "ts":
        return gen_ts_str(r) if r.random() < 0.7 else gen_date_str(r)
    if family == "time":
        return gen_time_str(r)
    if family == "uuid":
        return gen_uuid_str(r)
    if family == "numstr":
        return gen_numstr(r)
    if family == "str":
        return gen_str(r)
    if family == "raw":
        return raw_number(r, nested)
    if family == "mix":
        return gen_scalar(r, r.choice(["int", "float", "bool", "str", "date", "num"]))
    raise ValueError(family)


FAMILIES = ["int"] * 6 + ["smallint"] * 2 + ["float"] * 4 + ["num"] * 3 + ["bool"] + ["date"] * 4 + ["ts"] * 5 + ["time"] * 2 + \
    ["uuid"] * 2 + ["numstr"] * 2 + ["str"] * 4 + ["raw"] * 2 + ["mix"] * 2


def gen_value(r: random.Random, family: str, depth=0):
    k = r.random()
    if k < 0.06:
        return None
    if depth < 2:
        if k < 0.10:
            return [gen_value(r, family, depth + 1) for _ in range(r.randint(0, 3))]
        if k < 0.13:
            obj = {"k": gen_value(r, family, depth + 1), "n": r.randint(0, 3)}
            if r.random() < 0.3:
                obj["e"] = {} if r.random() < 0.5 else [None]
            return obj
    return gen_scalar(r, family, depth > 0)


def gen_table(r: random.Random) -> str:
    ncols = r.randint(1, 4)
    nrows = r.choice([1, 1, 2, 2, 3, 4, 6, 10])
    cols = []
    for i in range(ncols):
        name = r.choice(["a", "b", "c", "id", "v", "A", "x y", "é", "d"]) if r.random() < 0.3 else f"c{i}"
        fam = r.choice(FAMILIES)
        cols.append((name, fam, r.random() < 0.15))
    rows = []
    for _ in range(nrows):
        row = {}
        for name, fam, sparse in cols:
            if sparse and r.random() < 0.5:
                continue
            row[name] = gen_value(r, fam)
        rows.append(row)
    # Arrays only: the loader reads a `.json` table with `json.load`.  (DuckDB also reads newline-delimited
    # text, but its last line, with no final newline, is then sampled as a vector of its own.)
    return enc_json(rows) if r.random() < 0.5 else "[\n" + ",\n".join(enc_json(x) for x in rows) + "\n]"


# ── tables past one scan vector (2048 records) or the type-detection sample (20480) ──────────────────

BASE = {
    "bool": True, "int": 7, "double": 1.5, "str": "abc", "date": "2020-01-01", "ts": "2020-01-01 10:00:00",
    "time": "10:00:00", "uuid": "7f0dcad7-862e-5cec-4d8e-a544eb9c736f", "list": [1], "obj": {"a": 1}, "bigint": 2**63,
    "dates": ["2020-01-01"],
}
LATER = [
    None, True, 0, 1, -1, 2**63, 2**64, -(2**63) - 1, 1.5, 2.5, 2.0, 1e300, 1e-7, "5", " 5", "x", "true", "yes", "1.5", "0x10", "2020-01-02",
    "01-02-2020", "2020-1-2", "epoch", "11:00", "2020-01-02 11:00:00", "2020-01-02T11:00:00Z", "8f0dcad7-862e-5cec-4d8e-a544eb9c736f", [2],
    ["x"], {"a": 2}, {"a": "x"}, [], {},
]


def table_text(runs) -> str:
    """``runs``: ``[(count, record), ...]``; the records as JSON text, comma-joined in an array."""
    return "[" + ",".join(",".join([rec] * n) for n, rec in runs) + "]"


def rle(rows):
    out = []
    for row in rows:
        if out and out[-1][1] == row:
            out[-1][0] += 1
        else:
            out.append([1, row])
    return out


def big_case(runs):
    text = table_text(runs)
    res = read(text)
    case = {"runs": [[n, rec] for n, rec in runs]}
    if "error" in res:
        case["error"] = res["error"]
    else:
        case["rows_rle"] = rle(res["rows"])
    return case


def gen_big(r: random.Random, n: int, stride: int = 1):
    """The cases: values of every kind after the sample (every ``stride``-th kind); kinds that change at a vector
    boundary."""
    for bn, bv in BASE.items():
        for lv in LATER[::stride]:
            yield big_case([(20480, enc_json({"a": bv})), (1, enc_json({"a": lv}))])
    kinds = ["date", "ts", "time", "uuid", "str", "int", "double", "bool", "bigint"]
    for _ in range(n):
        a, b = r.choice(kinds), r.choice(kinds)
        va, vb = gen_scalar(r, {"date": "date", "ts": "ts", "time": "time", "uuid": "uuid", "str": "str", "int": "int",
                                "double": "float", "bool": "bool", "bigint": "int"}[a]), None
        vb = gen_scalar(r, {"date": "date", "ts": "ts", "time": "time", "uuid": "uuid", "str": "str", "int": "int",
                            "double": "float", "bool": "bool", "bigint": "int"}[b])
        k = r.choice([1, 2047, 2048, 2049, 4095, 4096, 4097, 6000, 20479, 20480, 20481, 20482])
        m = r.choice([1, 1, 2, 5, 2047, 2048, 3000])
        runs = [(k, enc_json({"a": va})), (m, enc_json({"a": vb}))]
        if r.random() < 0.3:
            runs.append((r.choice([1, 2, 2048]), enc_json({"a": gen_scalar(r, r.choice(["date", "ts", "uuid", "time"]))})))
        yield big_case(runs)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--n", type=int, default=1000)
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--out")
    ap.add_argument("--stride", type=int, default=1, help="with --big: use every n-th value kind after the sample")
    ap.add_argument("--big", action="store_true", help="tables past one scan vector / the sample size (run-length encoded)")
    ap.add_argument("--check", help="re-run a fixture against the installed DuckDB")
    a = ap.parse_args()
    if a.check:
        bad = 0
        for line in open(a.check, encoding="utf-8"):
            c = json.loads(line)
            got = read(c["json"])
            want = {k: c[k] for k in ("rows", "error") if k in c}
            got = {k: got[k] for k in ("rows", "error") if k in got}
            if got != want:
                bad += 1
                print("CHANGED", c["json"][:200])
        print("changed:", bad)
        return 1 if bad else 0
    r = random.Random(a.seed)
    if a.big:
        out = open(a.out, "w", encoding="utf-8") if a.out else sys.stdout
        for case in gen_big(r, a.n, a.stride):
            out.write(json.dumps(case, ensure_ascii=False) + "\n")
        return 0
    out = open(a.out, "w", encoding="utf-8") if a.out else sys.stdout
    seen = 0
    for _ in range(a.n):
        text = gen_table(r)
        res = read(text)
        res.pop("names", None)
        out.write(json.dumps({"json": text, **res}, ensure_ascii=False) + "\n")
        seen += 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
