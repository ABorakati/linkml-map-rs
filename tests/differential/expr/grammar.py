"""Hypothesis grammar for LinkML expression cases.

A *case* is plain data: ``{"tree": ..., "bindings": {...}, "mode": ..., "strict": bool}``.
Trees are nested tuples ``(kind, ...)``; :func:`render` turns one into source text
with precedence-aware parenthesisation, so the parser's precedence handling is
exercised, plus redundant ``("paren", e)`` nodes. Keeping the tree separate from
the text lets the shrinker (``shrink.py``) delete and replace sub-trees.

Types used to steer generation (not enforced; ill-typed draws are deliberate):
``int float num str bool list map any``.
"""

from __future__ import annotations

import sys

from hypothesis import strategies as st

# ─── value pools ─────────────────────────────────────────────────────────────

INT_EDGES = [0, 1, -1, 2, 3, 7, 10, 12, 100, 255, 2**31, 2**53, 2**53 + 1, 2**63 - 1, -(2**63), 2**63, 10**20, -(10**20),
             2**64, -(2**63) - 1, 2**100 + 1, -(2**100), 10**30, 2**1023, 2**1024, 2**1100, 10**400]
FLOAT_EDGE_TEXT = [
    "0.0", "-0.0", "1.0", "0.5", "1.5", "2.5", "3.14", "0.1", "0.2", "0.3", "1e16", "1e15", "1e22", "1.5e300", "1e308", "1e309",
    "5e-324", "1e-5", "1e-4", "0.0001", "123456789.123456789", "1e-320", "2.675", "0.5e1", "100.0", "1_0.5", ".5", "5.",
    "9007199254740993.0", "1.7976931348623157e308", "4.35", "0.045", "1.005", "-1.5", "-2.5",
]
STR_EDGES = [
    "", " ", "a", "abc", "Hello World", "hello world", "HELLO", "x", "1", "0", "-1", "+1", "1.0", "1.", ".5", "-.5", " 2 ", "\t3\n",
    "1e3", "1E3", "1e-3", "1_000", "1__0", "0x10", "0b1", "0o7", "inf", "-inf", "+Infinity", "nan", "NaN", "infinity", "True",
    "None", "null", "٣", "١٢٣", "１２３", "²", "½", "Ⅷ", "1,5", "1.2.3", "--1", "1e", "e5", "１.５", "9" * 25, "1" + "0" * 400, "0.1", "12abc",
    "é", "ǆ", "ß", "İ", "ﬁ", "straße", "ŉ", "ΣΑΣ", "σ", "ǅ", "o'neil", "mcDonald's", "foo_bar", "FooBar", "HTTPResponse", "snake_case",
    "a-b c", "  padded  ", "\u00a0nbsp\u00a0", "\u2003em\u2003", "tab\there", "new\nline", "quote'\"", "back\\slash", "😀", "a\u0301",
    "日本語", "Ünïcödé", "hello, world!", "123 hits", "!!!", "i̇", "ΐ", "ǰ", "A.B", "a b", "A,B,C", "a,b,,c", ",", "ab", "aa", "aaa",
]
KEY_EDGES = ["a", "b", "c", "name", "id", "x", "y", "items", "keys", "values", "upper", "1", "", "A", "key with space", "é", "_p", "get", "NULL"]
NAMES = ["a", "b", "c", "n", "s", "f", "l", "m", "x", "y", "z", "t"]
NAME_TYPES = {"a": "any", "b": "any", "c": "any", "n": "int", "s": "str", "f": "float", "l": "list", "m": "map", "x": "any", "y": "any", "z": "any", "t": "str"}
UNKNOWN_NAMES = ["zz", "undefined_name", "Person", "_hidden", "__import__", "True_", "pi", "e", "len_", "src", "target", "uuid5x"]

FUNCS: dict[str, tuple[list[str], str]] = {
    "max": (["list"], "any"), "min": (["list"], "any"), "len": (["list"], "int"), "sum": (["list"], "num"),
    "sorted": (["list"], "list"), "any": (["list"], "bool"), "all": (["list"], "bool"), "reversed": (["list"], "list"),
    "first": (["list"], "any"), "last": (["list"], "any"),
    "str": (["any"], "str"), "int": (["any"], "int"), "float": (["any"], "float"), "bool": (["any"], "bool"),
    "abs": (["num"], "num"), "round": (["num", "int?"], "num"), "floor": (["num"], "int"), "ceil": (["num"], "int"),
    "strlen": (["str"], "int"), "uuid5": (["str", "str"], "str"), "substr": (["str", "int", "int?"], "str"),
    "upper": (["str"], "str"), "lower": (["str"], "str"), "title": (["str"], "str"), "capitalize": (["str"], "str"),
    "strip": (["str", "str?"], "str"), "lstrip": (["str", "str?"], "str"), "rstrip": (["str", "str?"], "str"),
    "replace": (["str", "str", "str", "int?"], "str"), "startswith": (["str", "str"], "bool"), "endswith": (["str", "str"], "bool"),
    "contains": (["str", "str"], "bool"), "split": (["str", "str?", "int?"], "list"), "join": (["str", "list"], "str"),
    "slugify": (["str", "str?"], "str"), "to_snake": (["str"], "str"), "to_camel": (["str"], "str"), "to_pascal": (["str"], "str"),
    "is_str": (["any"], "bool"), "is_int": (["any"], "bool"), "is_float": (["any"], "bool"), "is_bool": (["any"], "bool"),
    "is_list": (["any"], "bool"), "coalesce": (["any*"], "any"), "is_numeric": (["any"], "bool"), "case": (["pair*"], "any"),
    "list": (["any?"], "list"), "tuple": (["any?"], "list"),
}
# Constructs another agent is porting right now: generated, but their failures are classified "pending in-flight port".
PENDING_FUNCS = {"dict": (["any?"], "map"), "set": (["any?"], "list")}
# `open`, `eval`, `exec`, `__import__`, `print`, `input` are deliberately absent: asteval (unrestricted mode)
# really runs them -- open(1) closes the fuzzer's own stdout and open(name, "w") creates files.
UNKNOWN_FUNCS = ["foo", "length", "Upper", "_private", "getattr_", "type", "isinstance", "range", "enumerate", "zip", "map", "filter", "ord", "chr", "pow", "divmod", "repr", "format", "hash", "id", "dir", "vars", "sum_", "uuid", "pi", "sqrt"]
METHODS = ["upper", "lower", "strip", "title", "split", "join", "replace", "format", "startswith", "items", "keys", "values", "get", "append", "count", "index", "isdigit", "encode", "capitalize", "zfill", "__class__", "mro"]
BINOPS = ["+", "-", "*", "/", "//", "%", "**", "<<", ">>", "^", "|", "&", "@"]
CMPOPS = ["==", "!=", "<", "<=", ">", ">=", "in", "not in", "is", "is not"]

# Features switched off to expose divergences hidden behind an already-known one
# (``campaign.py --mask rawint,bigint,...``). Known names: rawint bigint slice strstyle
# fstr lambda method matmul setdisp emptymap stmt (for/aug/unpack/del/pass statements).
MASK: set[str] = set()


def on(feature: str) -> bool:
    return feature not in MASK


def str_edges() -> list[str]:
    out = STR_EDGES
    if "strescape" in MASK:  # keep only strings Python's repr leaves as plain printable ASCII
        out = [s for s in out if all(0x20 <= ord(c) < 0x7F for c in s)]
    if "numstr-unicode" in MASK:  # unicode digits / \x1c..\x1f whitespace around numbers
        out = [s for s in out if all(ord(c) < 0x80 and not 0x1C <= ord(c) <= 0x1F for c in s) and s not in ("²", "½", "Ⅷ")]
    return out


def float_edges() -> list[str]:
    if "bigint" in MASK:  # floats whose floor/int/round would not fit an i64
        return [s for s in FLOAT_EDGE_TEXT if abs(float(s.replace("_", ""))) < 1e15]
    return FLOAT_EDGE_TEXT


def int_edges() -> list[int]:
    lim = 2**31 if "bigint" in MASK else None
    return [i for i in INT_EDGES if lim is None or -lim <= i <= lim]


# ─── tiny weighted-choice helper ─────────────────────────────────────────────


def pick(draw, options):
    """Weighted choice: ``options`` is a list of ``(weight, value)``."""
    total = sum(w for w, _ in options)
    r = draw(st.integers(0, total - 1))
    for w, v in options:
        if r < w:
            return v
        r -= w
    raise AssertionError


def chance(draw, p_percent: int) -> bool:
    return draw(st.integers(0, 99)) < p_percent


# ─── bindings ────────────────────────────────────────────────────────────────


def gen_scalar(draw, ty: str = "any"):
    if ty == "any":
        ty = pick(draw, [(3, "int"), (3, "float"), (6, "str"), (2, "bool"), (3, "null")])
    if ty == "null":
        return None
    if ty == "bool":
        return draw(st.booleans())
    if ty in ("int", "num"):
        return draw(st.one_of(st.sampled_from(int_edges()), st.integers(-1000, 1000)))
    if ty == "float":
        t = draw(st.sampled_from(float_edges()))
        return float(t.replace("_", ""))
    if ty == "str":
        rnd = st.text(st.characters(min_codepoint=0x20, max_codepoint=0x7E), max_size=8) if "strescape" in MASK else st.text(st.characters(blacklist_categories=("Cs",)), max_size=8)
        return pick(draw, [(8, draw(st.sampled_from(str_edges()))), (3, draw(rnd)), (1, draw(st.text(alphabet="abc ABC_-", max_size=6)))])
    raise ValueError(ty)


def gen_value(draw, depth: int = 0, ty: str = "any"):
    """A JSON-able binding value (floats as Python floats, which ``codec.encode`` tags)."""
    if ty in ("int", "float", "str", "bool", "num"):
        # natural type most of the time, otherwise an awkward other one
        if chance(draw, 75):
            return gen_scalar(draw, ty)
        return gen_value(draw, depth, "any")
    if ty == "list" or (ty == "any" and depth < 3 and chance(draw, 15)):
        n = draw(st.integers(0, 4))
        elem_ty = pick(draw, [(3, "any"), (2, "int"), (2, "str"), (1, "float"), (1, "map")])
        return [gen_value(draw, depth + 1, elem_ty) if depth < 2 else gen_scalar(draw) for _ in range(n)]
    if ty == "map" or (ty == "any" and depth < 3 and chance(draw, 12)):
        # `emptymap`: an empty bound map is a DynObj (always truthy) upstream, but the
        # other operations on it (`in`, `len`, `==`) are those of a plain dict here.
        n = draw(st.integers(1 if "emptymap" in MASK else 0, 3))
        keys = draw(st.lists(st.sampled_from(KEY_EDGES), min_size=n, max_size=n, unique=True))
        return {k: gen_value(draw, depth + 1) if depth < 2 else gen_scalar(draw) for k in keys}
    return gen_scalar(draw, "any")


def gen_bindings(draw):
    out = {}
    for name in NAMES:
        if chance(draw, 15):
            continue  # absent -> unbound name
        out[name] = gen_value(draw, 0, NAME_TYPES[name])
    return out


# ─── expression trees ────────────────────────────────────────────────────────


def literal(draw, ty: str):
    if ty in ("int", "num"):
        if on("rawint") and chance(draw, 5):
            return ("rawint", draw(st.sampled_from(["0x1f", "0b101", "0o17", "1_000", "00", "1e3", "0xFF", "1_0"])))
        return ("int", draw(st.one_of(st.sampled_from(int_edges()), st.integers(-20, 20))))
    if ty == "float":
        return ("float", draw(st.sampled_from(float_edges())))
    if ty == "str":
        return ("str", draw(st.sampled_from(str_edges())), pick(draw, [(10, "repr"), (2, "dq")] + ([(1, "triple"), (1, "raw"), (1, "u"), (1, "adj")] if on("strstyle") else [])))
    if ty == "bool":
        return ("const", draw(st.sampled_from(["True", "False"])))
    if ty == "list":
        return ("list", [literal(draw, pick(draw, [(3, "int"), (2, "str"), (1, "float"), (1, "bool")])) for _ in range(draw(st.integers(0, 4)))])
    if ty == "map":
        return ("dict", [(("str", k, "repr"), literal(draw, "int")) for k in draw(st.lists(st.sampled_from(KEY_EDGES), max_size=3, unique=True))])
    if ty == "any":
        return literal(draw, pick(draw, [(3, "int"), (2, "float"), (4, "str"), (2, "bool"), (1, "list"), (2, "none")]))
    return ("const", "None")


def name_ref(draw, ty: str):
    cands = [n for n in NAMES if NAME_TYPES[n] in (ty, "any")] or NAMES
    if chance(draw, 7):
        return ("name", draw(st.sampled_from(UNKNOWN_NAMES)))
    n = draw(st.sampled_from(cands))
    r = ("name", n)
    if chance(draw, 30):
        r = ("brace", r)
    return r


def path_ref(draw):
    """``m.a``-style attribute chains over the nested bindings."""
    base = draw(st.sampled_from(["m", "l", "x", "y", "src"]))
    r = ("name", base)
    attrs = ["a", "b", "c", "name", "id", "x", "_p", "NULL", "e"] + ([] if "methodattr" in MASK else ["items", "keys", "upper"])
    for _ in range(draw(st.integers(1, 3))):
        r = ("attr", r, draw(st.sampled_from(attrs)))
    if chance(draw, 30):
        r = ("brace", r)
    return r


def expr(draw, ty: str, depth: int, mode: str = "restricted"):
    leaf_p = 25 + 25 * (3 - min(depth, 3))
    if depth <= 0 or chance(draw, max(5, 70 - leaf_p)):
        return leaf(draw, ty)
    kinds = [
            (6, "leaf"),
            (6, "binop"),
            (3, "cmp"),
            (3, "bool"),
            (2, "not"),
            (2, "unary"),
            (10, "call"),
            (2, "ifexp"),
            (3, "sub"),
            (3, "attr"),
            (2, "comp"),
            (1, "list"),
            (1 if on("tuple") else 0, "tuple"),
            (1, "dict"),
            (1 if on("setdisp") else 0, "set"),
            (1, "paren"),
            (1, "fstr"),
            (1, "method"),
            (1, "lambda"),
            (1, "cmpchain"),
    ]
    masked = {"slice": "sub", "fstr": "fstr", "lambda": "lambda", "method": "method"}
    kinds = [(w, k) for w, k in kinds if not (k in masked.values() and any(f in MASK and masked[f] == k for f in masked))]
    if "slice" in MASK:
        kinds.append((3, "subidx"))
    kind = pick(draw, kinds)
    d = depth - 1
    sub = lambda t: expr(draw, t, d, mode)  # noqa: E731
    if kind == "leaf":
        return leaf(draw, ty)
    if kind == "binop":
        opty = "str" if ty == "str" else ("list" if ty == "list" else pick(draw, [(6, "num"), (2, "int"), (1, "str"), (1, "any"), (1, "list")]))
        ops = [(6, "+"), (4, "-"), (5, "*"), (3, "/"), (2, "//"), (2, "%"), (2, "**"), (1, "<<"), (1, ">>"), (1, "^"), (1, "|"), (1, "&")] + ([(1, "@")] if on("matmul") else [])
        ops = [(w, o) for w, o in ops if not (o in ("/", "//", "%") and "div" in MASK) and not (o == "**" and "pow" in MASK) and not (o in ("<<", ">>", "^", "|", "&") and "bitwise" in MASK)]
        op = pick(draw, ops)
        if chance(draw, 20):
            return ("bin", op, sub("any"), sub("any"))
        return ("bin", op, sub(opty), sub(opty if chance(draw, 70) else "any"))
    if kind == "cmp":
        opty = pick(draw, [(3, "num"), (3, "str"), (2, "any"), (1, "list")])
        cmps = [(3, "=="), (2, "!="), (3, "<"), (2, "<="), (3, ">"), (2, ">=")] + ([(2, "in"), (2, "not in")] if on("in") else []) + ([(1, "is"), (1, "is not")] if on("is") else [])
        op = pick(draw, cmps)
        if op in ("in", "not in") and on("in"):
            return ("cmp", sub("any"), [(op, sub(pick(draw, [(2, "list"), (2, "str"), (1, "map"), (1, "any")])))])
        return ("cmp", sub(opty), [(op, sub(opty if chance(draw, 75) else "any"))])
    if kind == "cmpchain":
        return ("cmp", sub("num"), [(draw(st.sampled_from(["<", "<=", "==", ">", "!="] + (["in"] if on("in") else []))), sub("num")) for _ in range(draw(st.integers(2, 3)))])
    if kind == "bool":
        return ("boolop", draw(st.sampled_from(["and", "or"])), [sub("any" if chance(draw, 50) else "bool") for _ in range(draw(st.integers(2, 3)))])
    if kind == "not":
        return ("not", sub("any"))
    if kind == "unary":
        return ("unary", draw(st.sampled_from(["-", "+"] + ([] if "bitwise" in MASK else ["~"]))), sub("num" if chance(draw, 80) else "any"))
    if kind == "call":
        return call(draw, ty, d, mode)
    if kind == "ifexp":
        return ("ifexp", sub("bool" if chance(draw, 60) else "any"), sub(ty), sub(ty))
    if kind in ("sub", "subidx"):
        base = sub(pick(draw, [(3, "list"), (3, "str"), (1, "map"), (1, "any")]))
        if kind == "subidx" or chance(draw, 50):
            return ("sub", base, sub("int" if chance(draw, 80) else "any"))
        parts = [sub("int") if chance(draw, 55) else None for _ in range(draw(st.integers(2, 3)))]
        return ("sub", base, ("slice", parts))
    if kind == "attr":
        if chance(draw, 50):
            return path_ref(draw)
        base = sub("any")
        if "numattr" in MASK and base[0] in ("int", "float", "rawint"):
            base = ("paren", base)
        return ("attr", base, draw(st.sampled_from(["a", "b", "name", "id", "x", "_p"] + ([] if "methodattr" in MASK else ["upper", "items"]))))
    if kind == "comp":
        var = draw(st.sampled_from(["i", "v", "k", "it"]))
        ck = pick(draw, [(6, "listcomp"), (1, "gen"), (1, "dictcomp")] + ([(1, "setcomp")] if on("setdisp") else []))
        iter_e = sub(pick(draw, [(4, "list"), (1, "str"), (1, "map"), (1, "any")])) if chance(draw, 70) else ("call", "range", [("int", draw(st.integers(0, 5)))])
        cond = sub("bool") if chance(draw, 30) else None
        elt_e = ("name", var) if chance(draw, 25) else comp_elt(draw, var, d, mode)
        if ck == "dictcomp":
            return ("dictcomp", ("name", var), elt_e, var, iter_e, cond)
        if ck == "setcomp":
            return ("setcomp", elt_e, var, iter_e, cond)
        if ck == "gen":
            return ("call", "list", [("gen", elt_e, var, iter_e, cond)])
        return ("listcomp", elt_e, var, iter_e, cond)
    if kind == "list":
        return ("list", [sub(ty if ty != "list" else "any") for _ in range(draw(st.integers(0, 4)))])
    if kind == "tuple":
        return ("tuple", [sub("any") for _ in range(draw(st.integers(0, 3)))])
    if kind == "dict":
        return ("dict", [(literal(draw, "str"), sub("any")) for _ in range(draw(st.integers(0, 3)))])
    if kind == "set":
        return ("set", [sub("any") for _ in range(draw(st.integers(0, 3)))])
    if kind == "paren":
        return ("paren", sub(ty))
    if kind == "fstr":
        return ("fstr", draw(st.sampled_from(["", "x", "{} items", "{}!r", "{}:>5", "{}:.2f", "{}:d", "{}:,"])), sub("any"))
    if kind == "method":
        return ("method", sub(pick(draw, [(4, "str"), (1, "list"), (1, "map"), (1, "any")])), draw(st.sampled_from(METHODS)), [sub("any") for _ in range(draw(st.integers(0, 2)))])
    if kind == "lambda":
        return ("call", "list", [("lambda", sub("any"))])
    raise AssertionError(kind)


def comp_elt(draw, var, d, mode):
    r = expr(draw, "any", min(d, 1), mode)
    return ("bin", "+", ("name", var), r) if chance(draw, 40) else r


def leaf(draw, ty: str):
    kind = pick(draw, [(5, "lit"), (5, "name"), (1, "path"), (1, "null"), (1, "none")])
    if kind == "lit":
        return literal(draw, ty)
    if kind == "name":
        return name_ref(draw, ty)
    if kind == "path":
        return path_ref(draw)
    if kind == "null":
        return ("name", "NULL")
    return ("const", "None")


def call(draw, ty: str, d: int, mode: str):
    sub = lambda t: expr(draw, t, d, mode)  # noqa: E731
    table = {**FUNCS, **{k: v for k, v in PENDING_FUNCS.items() if not (k == "set" and not on("setdisp"))}}
    if on("unknownfn") and chance(draw, 6):
        return ("call", draw(st.sampled_from(UNKNOWN_FUNCS)), [sub("any") for _ in range(draw(st.integers(0, 2)))])
    cands = [n for n, (_, r) in table.items() if r == ty or ty == "any" or r == "any"] if chance(draw, 70) else list(table)
    name = draw(st.sampled_from(sorted(cands or table)))
    params, _ = table[name]
    args = []
    wrong = chance(draw, 25)
    for p in params:
        if p == "any*":
            args += [sub("any") for _ in range(draw(st.integers(0, 3)))]
        elif p == "pair*":
            args += [("tuple", [sub("bool"), sub("any")]) for _ in range(draw(st.integers(0, 3)))]
        elif p.endswith("?"):
            if chance(draw, 40):
                args.append(sub("any" if wrong else p[:-1]))
        else:
            args.append(sub("any" if wrong and chance(draw, 60) else p))
    if chance(draw, 8):
        if args and chance(draw, 50):
            args.pop()
        else:
            args.append(sub("any"))
    return ("call", name, args)


# ─── statement blocks ────────────────────────────────────────────────────────


def stmts(draw, depth: int, budget: int):
    out = []
    for _ in range(draw(st.integers(1, budget))):
        k = pick(draw, [(8, "assign_target"), (3, "assign_tmp"), (3, "if"), (1, "expr"), (1, "ifelse")] + ([(2, "aug"), (1, "for"), (1, "pass"), (1, "unpack"), (1, "del")] if on("stmt") else [])) if depth > 0 else pick(draw, [(8, "assign_target"), (2, "assign_tmp"), (1, "expr")])
        rhs = lambda: stmt_expr(draw)  # noqa: E731
        if k == "assign_target":
            out.append(("assign", "target", rhs()))
        elif k == "assign_tmp":
            out.append(("assign", draw(st.sampled_from(["tmp", "t1", "r", "x2"])), rhs()))
        elif k == "aug":
            out.append(("aug", draw(st.sampled_from(["target", "tmp"])), draw(st.sampled_from(["+=", "-=", "*=", "//="])), rhs()))
        elif k == "if":
            out.append(("if", rhs(), stmts(draw, depth - 1, 2), None))
        elif k == "ifelse":
            out.append(("if", rhs(), stmts(draw, depth - 1, 2), stmts(draw, depth - 1, 2)))
        elif k == "expr":
            out.append(("sexpr", rhs()))
        elif k == "for":
            out.append(("for", draw(st.sampled_from(["i", "v"])), stmt_expr(draw, "list"), stmts(draw, depth - 1, 2)))
        elif k == "pass":
            out.append(("pass",))
        elif k == "unpack":
            out.append(("unpack", ["tmp", "target"], stmt_expr(draw, "list")))
        elif k == "del":
            out.append(("del", draw(st.sampled_from(["tmp", "target", "src"]))))
    return out


def stmt_expr(draw, ty="any"):
    e = expr(draw, ty, draw(st.integers(1, 3)), "unrestricted")
    return e


# ─── the case strategy ───────────────────────────────────────────────────────


@st.composite
def cases(draw, mode: str | None = None):
    mode = mode or pick(draw, [(55, "restricted"), (45, "unrestricted")])
    strict = chance(draw, 15)
    if mode == "unrestricted" and on("block") and chance(draw, 65):
        tree = ("block", stmts(draw, 2, 3), draw(st.sampled_from(["  ", "    ", "\t"])))
        tree = ("block", tree[1], tree[2])
    else:
        tree = expr(draw, pick(draw, [(3, "any"), (3, "num"), (2, "str"), (1, "bool"), (1, "list"), (1, "int")]), draw(st.integers(1, 4)), mode)
    return {"tree": tree, "bindings": gen_bindings(draw), "mode": mode, "strict": strict}


# ─── rendering ───────────────────────────────────────────────────────────────

PREC = {"ifexp": 1, "lambda": 1, "or": 2, "and": 3, "not": 4, "cmp": 5, "|": 6, "^": 7, "&": 8, "<<": 9, ">>": 9, "+": 10, "-": 10, "*": 11, "/": 11, "//": 11, "%": 11, "@": 11, "unary": 12, "**": 13, "atom": 15}


def _str_lit(s: str, style: str) -> str:
    if style == "dq":
        body = s.replace("\\", "\\\\").replace('"', '\\"').replace("\n", "\\n").replace("\r", "\\r").replace("\t", "\\t")
        if any(ord(c) < 32 for c in body):
            return repr(s)
        return f'"{body}"'
    if style == "triple" and '"""' not in s and not s.endswith('"') and "\\" not in s and "\r" not in s:
        return f'"""{s}"""'
    if style == "raw" and "\\" not in s and "'" not in s and "\n" not in s and "\r" not in s:
        return f"r'{s}'"
    if style == "u":
        return "u" + repr(s)
    if style == "adj" and len(s) > 1:
        return repr(s[:1]) + " " + repr(s[1:])
    return repr(s)


def prec_of(node) -> int:
    k = node[0]
    if k == "bin":
        return PREC[node[1]]
    if k == "boolop":
        return PREC[node[1]]
    if k == "not":
        return PREC["not"]
    if k == "cmp":
        return PREC["cmp"]
    if k == "unary":
        return PREC["unary"]
    if k == "ifexp":
        return PREC["ifexp"]
    if k == "lambda":
        return PREC["lambda"]
    if k == "int" and node[1] < 0:
        return PREC["unary"]
    if k == "float" and node[1].startswith("-"):
        return PREC["unary"]
    return PREC["atom"]


def r(node, ctx: int = 0) -> str:
    """Render ``node`` for a context needing precedence >= ``ctx``."""
    s = _r(node)
    return f"({s})" if prec_of(node) < ctx else s


def _r(node) -> str:  # noqa: C901, PLR0911, PLR0912
    k = node[0]
    if k == "int":
        return str(node[1])
    if k in ("rawint", "float"):
        return node[1]
    if k == "str":
        return _str_lit(node[1], node[2])
    if k == "const":
        return node[1]
    if k == "name":
        return node[1]
    if k == "brace":
        return "{" + _r(node[1]) + "}"
    if k == "paren":
        return "(" + _r(node[1]) + ")"
    if k == "list":
        return "[" + ", ".join(r(x) for x in node[1]) + "]"
    if k == "tuple":
        items = [r(x) for x in node[1]]
        return "(" + ", ".join(items) + ("," if len(items) == 1 else "") + ")"
    if k == "set":
        return "{" + ", ".join(r(x) for x in node[1]) + "}" if node[1] else "set()"
    if k == "dict":
        return "{" + ", ".join(f"{r(a)}: {r(b)}" for a, b in node[1]) + "}"
    if k == "unary":
        return node[1] + r(node[2], PREC["unary"])
    if k == "not":
        return "not " + r(node[1], PREC["not"])
    if k == "bin":
        op, a, b = node[1], node[2], node[3]
        p = PREC[op]
        if op == "**":
            return f"{r(a, p + 1)} ** {r(b, PREC['unary'])}"
        return f"{r(a, p)} {op} {r(b, p + 1)}"
    if k == "boolop":
        p = PREC[node[1]]
        return f" {node[1]} ".join(r(x, p + 1) for x in node[2])
    if k == "cmp":
        parts = [r(node[1], PREC["cmp"] + 1)]
        for op, e in node[2]:
            parts += [op, r(e, PREC["cmp"] + 1)]
        return " ".join(parts)
    if k == "ifexp":
        return f"{r(node[2], 2)} if {r(node[1], 2)} else {r(node[3], 1)}"
    if k == "call":
        return f"{node[1]}({', '.join(_arg(a) for a in node[2])})"
    if k == "method":
        return f"{r(node[1], PREC['atom'])}.{node[2]}({', '.join(r(a) for a in node[3])})"
    if k == "attr":
        return f"{r(node[1], PREC['atom'])}.{node[2]}"
    if k == "sub":
        return f"{r(node[1], PREC['atom'])}[{_idx(node[2])}]"
    if k == "listcomp":
        return "[" + _comp(node[1:]) + "]"
    if k == "setcomp":
        return "{" + _comp(node[1:]) + "}"
    if k == "dictcomp":
        return "{" + r(node[1]) + ": " + _comp(node[2:]) + "}"
    if k == "gen":
        return _comp(node[1:])
    if k == "lambda":
        return "lambda q: " + r(node[1])
    if k == "fstr":
        spec, e = node[1], node[2]
        body = spec.replace("{}", "{" + r(e).replace('"', "'") + "}") if "{}" in spec else spec + "{" + r(e).replace('"', "'") + "}"
        if "!r" in spec or ":" in spec:
            head, _, tail = spec.partition("{}")
            fmt = tail
            body = head + "{" + r(e).replace('"', "'") + fmt + "}"
        return 'f"' + body.replace('"', "'") + '"'
    raise AssertionError(node)


def _arg(a) -> str:
    if a[0] == "gen":
        return _comp(a[1:])
    if a[0] == "lambda":
        return _r(a)
    return r(a)


def _idx(i) -> str:
    if i[0] == "slice":
        return ":".join("" if p is None else r(p) for p in i[1])
    return r(i)


def _comp(parts) -> str:
    elt, var, it, cond = parts
    s = f"{r(elt)} for {var} in {r(it, 2)}"
    if cond is not None:
        s += f" if {r(cond, 3)}"
    return s


def render(tree) -> str:
    if tree[0] == "block":
        return "\n".join(_rs_block(tree[1], tree[2], 0))
    return r(tree)


def _rs_block(body, indent: str, level: int) -> list[str]:
    pad = indent * level
    out: list[str] = []
    for s in body:
        k = s[0]
        if k == "assign":
            out.append(f"{pad}{s[1]} = {r(s[2])}")
        elif k == "aug":
            out.append(f"{pad}{s[1]} {s[2]} {r(s[3])}")
        elif k == "sexpr":
            out.append(f"{pad}{r(s[1])}")
        elif k == "pass":
            out.append(f"{pad}pass")
        elif k == "unpack":
            out.append(f"{pad}{', '.join(s[1])} = {r(s[2])}")
        elif k == "del":
            out.append(f"{pad}del {s[1]}")
        elif k == "if":
            out.append(f"{pad}if {r(s[1])}:")
            out += _rs_block(s[2], indent, level + 1) or [pad + indent + "pass"]
            if s[3] is not None:
                out.append(f"{pad}else:")
                out += _rs_block(s[3], indent, level + 1) or [pad + indent + "pass"]
        elif k == "for":
            out.append(f"{pad}for {s[1]} in {r(s[2])}:")
            out += _rs_block(s[3], indent, level + 1) or [pad + indent + "pass"]
        else:
            raise AssertionError(s)
    return out


# ─── constructs owned by in-flight ports ─────────────────────────────────────


def tree_kinds(tree) -> set[str]:
    """Node kinds and function names present in a tree (for classification)."""
    out: set[str] = set()

    def walk(n):
        if isinstance(n, tuple):
            if n and isinstance(n[0], str):
                out.add(n[0])
                if n[0] == "call":
                    out.add("fn:" + n[1])
                if n[0] in ("bin", "unary", "boolop"):
                    out.add("op:" + n[1])
            for x in n:
                walk(x)
        elif isinstance(n, list):
            for x in n:
                walk(x)

    walk(tree)
    return out


if __name__ == "__main__":
    from hypothesis import given, settings

    sys.setrecursionlimit(10000)

    @settings(max_examples=30, database=None)
    @given(cases())
    def show(c):
        print(c["mode"], repr(render(c["tree"])), c["bindings"])

    show()
