import os, sys, random
d = sys.argv[1]
os.makedirs(d, exist_ok=True)
n = 0
def w(text):
    global n
    with open(os.path.join(d, f"c{n:05d}.yaml"), "w", newline='') as f: f.write(text)
    n += 1

scalars = """1 0 -0 +0 007 0o7 0x1F 0X1F 0b101 0B1 1_000 1__0 1_ _1 0_7 1:30 1:60 -1:30:15 12:30:45 1.0 1. .5 -.5 +.5 1e3 1E3 1e+3 1.0e3 1.0e+3 1.0e-3 1.0E+3 1.e+3 .5e+3 0.5e-3 1.5e3 .inf -.inf +.inf .Inf .INF .iNf .nan .NaN .NAN nan NaN inf Inf
yes Yes YES no No NO on On ON off Off OFF true True TRUE false False FALSE y n Y N t f null Null NULL nUll ~ ~~ =
<< <<< 2024-01-01 2024-1-1 2024-01-01T10:00:00 2024-01-01T10:00:00Z 2024-01-01 10:00:00 2024-01-01 10:00:00.5 2024-01-01 10:00:00.123456789 2024-01-01 10:00:00 +05:30 2024-01-01 10:00:00 -5 2024-01-01 10:00:00+05 2024-01-01t10:00:00z 2024-01-01 10:00:00 Z 2024-13-01 2024-02-30 0000-01-01 2024-01-01 25:00:00 2024-01-01 10:00:00+24:00 2024-01-01 10:00:00.Z 1999-12-31 2001-12-14t21:59:43.10-05:00 2001-12-14 21:59:43.10 -5 2001-12-15 2:59:43.10 20240101 2024/01/01 2024-01 2024-01-011
abc a:b a: b -a - a ? a !a &a *a |a >a @a `a %a #a a#b a #b "a" 'a' {a} [a] a,b a, b
é 中文 😀 a\\b a\\nb \\n
12abc 1.2.3 1,000 $5 5% 0.0.1 v1.2 1e 1e+ e5 --1 +-1 1 2 1 .
0x 0b 0o 0x_1 0xG 0b2 08 09 0_8 -08 0.5.
""".split("\n")
vals = []
for line in scalars:
    vals += [t for t in line.split(" ") if t] if line and "2024" not in line and "1999" not in line and "2001" not in line else []
# date lines need splitting by custom tokens (contain spaces)
dates = ["2024-01-01","2024-1-1","2024-01-01T10:00:00","2024-01-01T10:00:00Z","2024-01-01 10:00:00","2024-01-01 10:00:00.5","2024-01-01 10:00:00.123456789","2024-01-01 10:00:00 +05:30","2024-01-01 10:00:00 -5","2024-01-01 10:00:00+05","2024-01-01t10:00:00z","2024-01-01 10:00:00 Z","2024-13-01","2024-02-30","0000-01-01","2024-01-01 25:00:00","2024-01-01 10:00:00+24:00","2024-01-01 10:00:00.Z","1999-12-31","2001-12-14t21:59:43.10-05:00","2001-12-14 21:59:43.10 -5","2001-12-15 2:59:43.10","20240101","2024/01/01","2024-01","2024-01-011","2024-01-01  10:00:00","2024-01-01\t10:00:00","2024-01-01 10:00:60","2024-01-01 10:60:00","2024-01-01 1:00:00","2024-1-1 1:00:00", "2024-01-01 10:00:00 +5:30","2024-01-01 10:00:00 +05:3", "2024-01-01T10:00", "2023-02-29", "2024-02-29", "9999-12-31", "2024-01-01 10:00:00.", "2024-01-01 10:00:00 +23:59"]
vals += dates
vals += ["a b", "a  b", "a: b c", "x #y", "x# y", "-", "- a", "[", "]", "{", "}", ",", "a, b", "a,b", "'", '"', "'a", 'a"', "a'b", "\\", "!!str 1", "!!int 1", "!!int abc", "!!bool yes", "!!bool maybe", "!!float 1", "!!float inf", "!!null x", "!!timestamp 2024-01-01", "!!timestamp 2024-1-1", "!!str", "!! x", "!custom 1", "! 1", "&a 1", "&a", "*a", "!!binary aGk=", "!!set {a}", "!!seq [1]", "!!map {a: 1}", "!!python/none", "1 # c", "yes # c", "~ # c", "'1' # c"]
seen = set()
vals = [v for v in vals if not (v in seen or seen.add(v))]
for v in vals:
    w(f"k: {v}\n")
    w(f"- {v}\n")
    w(f"{{k: {v}}}\n")
    w(f"[{v}]\n")
    w(f"{v}\n")
    w(f"k: '{v}'\n")
    w(f'k: "{v}"\n')
    w(f"k: |\n  {v}\n")
    w(f"k: >\n  {v}\n")
    w(f"{v}: x\n")
    w(f"? {v}\n: x\n")
    w(f"{{{v}: x}}\n")
    w(f"a: &x {v}\nb: *x\n")
    w(f"k: [{v}, {v}]\n")
    w(f"k: {{a: {v}, b: {v}}}\n")
    w(f"k:\n  - {v}\n  - {v}\n")
    w(f"k: !!str {v}\n")

structs = [
"", "\n", "---\n", "...\n", "# only comment\n", "a: 1\n---\nb: 2\n", "---\na: 1\n...\n---\nb: 2\n", "%YAML 1.1\n---\na: 1\n", "%YAML 1.2\n---\na: 1\n", "%TAG !e! tag:example.com,2000:\n---\na: !e!foo 1\n",
"a:\n  b:\n    c: 1\n  d: 2\n", "a:\n- 1\n- 2\nb: 3\n", "a: [1, 2,]\n", "a: [1, , 2]\n", "a: {b: 1,}\n", "a: {b}\n", "a: {b, c}\n", "[a, b: c]\n", "{a: 1, a: 2}\n", "a: 1\na: 2\n", "{? a : 1}\n",
"a: x\n   y\n   z\n", "a: x\n\n   y\n", "a: 'x\n   y'\n", 'a: "x\n   y"\n', 'a: "x\\\n   y"\n', "a: |\n  x\n  y\n", "a: |-\n  x\n  y\n\n", "a: |+\n  x\n  y\n\n", "a: >\n  x\n  y\n\n  z\n", "a: >-\n  x\n  y\n", "a: |2\n    x\n", "a: |\n   x\n  y\n",
'a: "\\x41\\u00e9\\U0001F600\\t\\n\\0\\a\\b\\e\\f\\v\\r\\ \\"\\/\\\\\\N\\_\\L\\P"\n', 'a: "\\q"\n', 'a: "\\u12"\n', "a: 'it''s'\n", "a: 'x\\n'\n",
"a: &x [1, 2]\nb: *x\n", "a: &x {k: 1}\nb: *x\n", "a: *x\n", "a: &x\n  - 1\nb: &y\n  - *x\n", "a: &x [*x]\n", "&x a: 1\n", "a: &x b\nc: *x\nd: &x e\nf: *x\n",
"base: &b {x: 1, y: 2}\nm: {<<: *b}\n", "base: &b {x: 1, y: 2}\nm:\n  <<: *b\n  y: 3\n", "a: &a {x: 1}\nb: &b {y: 2}\nm:\n  <<: [*a, *b]\n", "a: &a {x: 1}\nb: &b {x: 2}\nm:\n  <<: [*a, *b]\n", "m:\n  <<: 1\n", "m:\n  <<: [1]\n", "a: &a {x: 1}\nm:\n  k: 1\n  <<: *a\n  x: 5\n", "a: &a {<<: {q: 1}, x: 1}\nm:\n  <<: *a\n", "<<: {a: 1}\n", "'<<': 1\n", "a: <<\n", "{<<: {a: 1}, b: 2}\n", "x: &a {p: 1}\nm: {<<: *a, <<: {q: 2}}\n",
"a: \t1\n", "a:\t1\n", "\ta: 1\n", "a:\n\t- 1\n", "a: 1\t\n", "a: [1,\t2]\n", "a:\n  b: 1\n c: 2\n", "a: 1\n  b: 2\n", " a: 1\n b: 2\n", "- a\n-b\n", "-a\n", "a:b\n", "a :b\n", "a : b\n", "a:  b\n",
"- a\n- b: 1\n  c: 2\n- - x\n  - y\n", "- - - a\n", "-\n  a: 1\n", "- \n", "a:\n-\n", "? - a\n  - b\n: c\n", "? a\n? b\n", "? {a: 1}\n: x\n", "[a]: x\n", "{a: 1}: x\n", "? |\n  block\n: x\n",
"a: !!str\nb: !!int\n", "!!map {a: 1}\n", "!!seq [a]\n", "--- !!map\na: 1\n", "--- |\n  text\n", "--- >\n  text\n", "--- a\n", "--- [a]\n", "--- {a: 1}\n", "--- # c\na: 1\n",
"\ufeffa: 1\n", "a: 1\r\nb: 2\r\n", "a: 1\rb: 2\r", "a: |\r\n  x\r\n  y\r\n", "a: \"x\r\n  y\"\r\n", "a: 1\u0085b: 2\n", "a: 1\u2028b: 2\n", "a: \u00a0x\n", "a: x\u0001y\n", "a: \u007f\n", "a: \ud7ff\n", "a: \ufffd\n", "a: \ufffe\n",
"a: é\nb: 中\nc: 😀\n", "é: 1\n", "a: \"\\xe9\"\n", "'a\\b': 1\n",
'{"a": 1, "b": [1, 2.5, "x", null, true, false], "c": {"d": 1e5, "e": 1E+5, "f": 1.5e+5, "g": -1e-3, "h": 1.5e-3}}', '{"a":1,"b":"x"}', '{"a":1, "b":[1,2]}', '[1,2,3]', '{"a": "\\u00e9\\n\\t\\/\\\\"}', '{"a": "\\ud83d\\ude00"}', '{"a": 12345678901234567890}', '{"a": -0}', '{"a": 0.0}', '{"a": 1e5}', '{"a": NaN}', '{"a": Infinity}', '{"a": true, "a": false}', '{\n  "a": 1,\n\t"b": 2\n}', '{"a": 1}\n{"b": 2}\n', '{"a": 1,}', '[1,2,]', "{'a': 1}", '{"a": 0123}', '{"a": +1}', '{"a": 1.}', '{"a": .5}',
"a: 9223372036854775807\nb: 9223372036854775808\nc: -9223372036854775808\nd: -9223372036854775809\ne: 123456789012345678901234567890\nf: 0xFFFFFFFFFFFFFFFFFF\n",
"a: 1e400\nb: 1.0e+400\nc: -1.0e+400\nd: 1.0e-400\n",
"a: 0.1\nb: 0.30000000000000004\nc: 1.7976931348623157e+308\nd: 5e-324\ne: 4.9e-324\nf: 123456789.123456789\ng: 1.0e+16\nh: 100000000000000000.0\n",
"? a\n: 1\n? b\n", "a:\n  - b\n  -\n  - c\n", "a: -\n", "a: - b\n", "a: [\n  1,\n  2\n]\n", "a: {\n  b: 1\n}\n", "a: [1,\n2]\n", "a: [1,\n 2]\n", "a:\n[1]\n", "a: 'b'c\n", "a: \"b\"c\n", "a: 'b' # c\n", "a: b: c\n", "a: b\n: c\n",
"k:\n  - &a 1\n  - *a\n  - &b [x]\n  - *b\n", "- !!str 1\n- !!int '2'\n- !!float '3'\n- !!bool 'yes'\n- !!null ''\n", "- !!str\n- !!int\n", "- !!float 1:30\n", "- !!int 0x10\n- !!int 010\n- !!int 1_0\n- !!int ' 1'\n- !!float .5\n- !!float 1e3\n",
]
for t in structs:
    w(t)

# random structure fuzz
random.seed(7)
atoms = ["a","b","1","yes","~","2024-01-01","'q'","\"r\"","1.5","x y","[1, 2]","{k: v}","&z 5","*z","!!str 1","|\n  t","<<","k: v"]
def rnd(depth=0):
    pad = "  " * depth
    kind = random.choice(["map","seq","scalar"]) if depth < 3 else "scalar"
    if kind == "scalar": return None
    return kind
def gen(depth):
    pad = "  " * depth
    kind = random.choice(["map","seq","map","seq","flat"]) if depth < 3 else "flat"
    if kind == "flat":
        return random.choice(atoms)
    lines = []
    for _ in range(random.randint(1,3)):
        v = gen(depth+1)
        if kind == "map":
            k = random.choice(["a","b","c","<<","k k"])
            if "\n" in v or v.startswith(("-", "{", "[")) and False:
                lines.append(f"{pad}{k}: {v}")
            elif isinstance(v, str) and v.startswith("\n"):
                lines.append(f"{pad}{k}:{v}")
            else:
                lines.append(f"{pad}{k}: {v}")
        else:
            lines.append(f"{pad}- {v}")
    return "\n" + "\n".join(lines)
for _ in range(1500):
    body = gen(0)
    w(body.lstrip("\n") + "\n")
print(n, "files")
