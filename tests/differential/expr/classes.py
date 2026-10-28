"""Divergence classes: name, reason, and a predicate over a (minimised) case.

Order matters; the first match wins. ``classify`` returns ``"unclassified"`` when
nothing matches, which ``build_corpus.py`` reports so new classes get noticed.
"""

from __future__ import annotations

import re
from collections.abc import Callable
from dataclasses import dataclass


@dataclass
class Ctx:
    expr: str
    mode: str
    py: dict
    rs: dict
    diff: dict

    @property
    def py_err(self) -> str:
        return self.py.get("err", "")

    @property
    def py_msg(self) -> str:
        return self.py.get("message", "")

    @property
    def rs_msg(self) -> str:
        return self.rs.get("message", "")

    @property
    def rs_err(self) -> str:
        return self.rs.get("err", "")

    @property
    def via(self) -> str:
        return self.py.get("via", "restricted")


def has(rx: str, s: str) -> bool:
    return re.search(rx, s) is not None


R: list[tuple[str, str, Callable[[Ctx], bool]]] = [
    ("hang-or-crash", "a case hangs or aborts the Rust evaluator (unbounded allocation/recursion)", lambda c: c.rs_err in ("hang", "crash", "panic")),
    ("div-zero", "upstream lets ZeroDivisionError escape the null-propagating wrapper; Rust returns null", lambda c: c.py_err == "ZeroDivisionError"),
    ("resource-limits", "simpleeval caps (MAX_STRING_LENGTH, MAX_POWER, MAX_SHIFT, iterable length) are not reproduced", lambda c: c.py_err in ("NumberTooHigh", "IterableTooLong", "MemoryError", "OverflowError")),
    ("unrestricted-syntax-error", "asteval swallows SyntaxError/IndentationError (result null); Rust raises, or accepts bad indentation", lambda c: c.mode == "unrestricted" and c.via in ("asteval-block", "fallback") and "ok" in c.py and c.py["ok"] is None and (c.rs_err in ("parse", "lex") or "ok" in c.rs) and has(r"^\s|:\n[^\s]|\n\s+\S", c.expr) and False),
    ("unrestricted-statements", "statement forms beyond assignment / if / expression statements (for, while, aug-assign, del, pass, unpack, try, def, elif, chained assignment)", lambda c: c.mode == "unrestricted" and c.rs_err in ("parse", "undefined", "eval") and c.via == "asteval-block" and (has(r"\b(for|while|del|pass|try|def|elif|assert|import|global|lambda)\b|[-+*/]=|^\s*\w+\s*,|=.*=", c.expr) or has(r"invalid assignment target|unexpected keyword|expected Eof, found (Name|Assign|Colon)", c.rs_msg))),
    ("unrestricted-namespace", "asteval namespace is `src`/`target`/`uuid5` plus Python builtins and math; LinkML built-ins, bare slots and NULL are undefined there, Python builtins (range, type, ...) are not in Rust", lambda c: c.mode == "unrestricted" and c.via == "asteval-block"),
    ("unrestricted-fallback", "restricted rejection retried by asteval (result null) differs", lambda c: c.mode == "unrestricted" and c.via == "fallback"),
    ("is-operator", "`is` / `is not` are not parsed", lambda c: c.rs_err == "parse" and has(r'found Name\("is"\)', c.rs_msg)),
    ("int-literal-forms", "hex/octal/binary int literals are not lexed", lambda c: has(r"\b0[xXbBoO][0-9a-fA-F_]", c.expr) and c.rs_err in ("parse", "lex")),
    ("matmul", "`@` rejected at lex time even in an untaken branch", lambda c: "'@'" in c.rs_msg),
    ("slicing", "subscript slices `a[x:y:z]` are not parsed", lambda c: c.rs_err in ("parse", "lex") and has(r"Colon|character: ':'", c.rs_msg) and "[" in c.expr),
    ("string-literal-forms", "triple-quoted / raw / u- / b- / adjacent string literals are not lexed", lambda c: c.rs_err == "parse" and has(r'found Str\(', c.rs_msg)),
    ("string-escapes", "\\x \\u \\U \\N{} octal escapes and unknown-escape backslashes are not decoded in string literals", lambda c: "ok" in c.py and "ok" in c.rs and "\\" in c.expr and not has(r"\\[ntr'\"\\]", c.expr) or (c.rs_err == "lex" and "unterminated" in c.rs_msg and "\\" in c.expr)),
    ("fstring", "f-strings are not supported", lambda c: has(r"\bf['\"]", c.expr)),
    ("number-attr-lex", "`1.5.name` is mis-lexed as a malformed float", lambda c: "invalid float literal" in c.rs_msg),
    ("method-calls", "method calls on values (`s.upper()`) and lambda are not parsed", lambda c: c.rs_err in ("parse", "lex") and has(r"found (LParen|Colon)|RParen", c.rs_msg)),
    ("method-attr", "attribute access resolving to a Python method (`s.upper`) or builtin attribute (`x.real`)", lambda c: has(r"has no attribute", c.rs_msg)),
    ("bigint", "arbitrary-precision int vs i64 (literals above i64 become floats, overflow wraps or errors)", lambda c: has(r"big integers|out of range", c.rs_msg) or has(r"\d{19,}", c.expr + c.diff.get("py", "")) or (has(r"\*\*|<<|\*", c.expr) and c.diff.get("kind") == "value")),
    ("sequence-repeat-bool", "list * bool: Python repeats the list (bool is an int); Rust returns null", lambda c: c.diff.get("kind") == "value" and has(r"\*", c.expr) and has(r"True|False", c.expr) and "[" in c.diff.get("py", "")),
    ("bitwise-on-float", "bitwise operators on floats: upstream raises TypeError, Rust returns null", lambda c: c.py_err == "TypeError" and has(r"unsupported operand type\(s\) for (\||&|\^|<<|>>)", c.py_msg)),
    ("shift-negative-count", "negative shift count: upstream ValueError, Rust returns a value", lambda c: "negative shift count" in c.py_msg),
    ("in-not-iterable", "`x in y` with a non-container y: upstream null after a warning, Rust raises", lambda c: "is not iterable" in c.rs_msg and "ok" in c.py),
    ("comprehension-none", "comprehension over None / a scalar: upstream TypeError, Rust yields []", lambda c: "not iterable" in c.py_msg),
    ("list-ordering", "ordering comparison of lists/tuples (`[1] < [2]`) returns null in Rust", lambda c: c.diff.get("kind") == "value" and c.diff.get("rs") == "None" and has(r"[<>]", c.expr) or has(r"\[.*\]\s*(<|>|<=|>=)", c.expr) or has(r"(<|>|<=|>=)\s*\[", c.expr)),
    ("dynobj-flavour", "a bound map is a DynObj upstream (always truthy, getattr-only) or a plain dict (an `any`-ranged or inlined-as-dict value); Value::Map takes the DynObj side for truthiness and attributes and the dict side for everything else (assigned by hand: not detectable from one result)", lambda c: False),
    ("map-attr-hybrid", "attribute access on a nested map: Rust resolves a present key directly and distributes over values otherwise; upstream is DynObj getattr (null when missing) or dict distribution, never both", lambda c: c.diff.get("kind") == "value" and "." in c.expr and c.diff.get("py") == "None" and c.diff.get("rs", "").startswith("[")),
    ("repr-non-printable", "str()/repr() of a container escapes every non-printable code point (\\xNN, \\uNNNN, \\UNNNNNNNN); Rust escapes only ASCII control characters", lambda c: c.diff.get("kind") == "value" and has(r"\\\\[xuU][0-9a-f]", c.diff.get("py", "")) and not has(r"\\\\[xuU][0-9a-f]", c.diff.get("rs", ""))),
    ("decimal-or-unicode-numstr", "numeric strings with unicode digits / underscores / \\x1c-\\x1f whitespace coerce differently", lambda c: has(r"[^\x00-\x7f]|_", c.expr + str(c.diff))),
]


def classify(expr: str, mode: str, py: dict, rs: dict, diff: dict) -> tuple[str, str]:
    c = Ctx(expr, mode, py, rs, diff)
    for name, reason, pred in R:
        try:
            if pred(c):
                return name, reason
        except Exception:  # noqa: BLE001
            continue
    return "unclassified", "no class matched; triage and add a rule to classes.py"
