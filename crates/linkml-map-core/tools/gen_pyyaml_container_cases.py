#!/usr/bin/env python3
"""Generate crates/linkml-map-core/tests/data/pyyaml_containers.json: the YAML upstream's
`dump_yaml` writes for values holding tuples, sets and dicts with non-string keys.

Each case is an expression (evaluated by the Rust evaluator and by upstream's
`eval_expr_with_mapping`) and the text `dump_yaml` gives for the three option sets the
Rust tests replay. Run with the upstream venv:

    upvenv/bin/python tools/gen_pyyaml_container_cases.py > tests/data/pyyaml_containers.json
"""
import json
import sys

from linkml_map.utils.eval_utils import eval_expr_with_mapping
from linkml_map.utils.serialization import dump_yaml

EXPRS = [
    "()", "(1,)", "(1, 2)", "(1, 'a', None, 2.5, True)", "((1, 2), (3,))", "[(1, 2), (3,)]", "[(), []]",
    "[1, (2, 3), [4, (5,)]]", "{'a': (1, 2)}", "{'a': (), 'b': ((1,), 2)}", "{'t': (1, 2), 'l': [3, 4], 's': 'x'}",
    "set([1, 2, 3])", "set([3, 1, 2])", "set()", "set([(1, 2), (3, 4)])", "set(['b', 'a'])", "set([1, 'a'])",
    "set([None, 1])", "[set([1]), set()]", "{'s': set([2, 1])}", "{'s': set(['x'])}", "set([1.5, 0.5])",
    "set([True, 2])", "set([-1, 5, 3])", "set([(2,), (1,)])",
    "{1: 'a'}", "{1: 'a', 2: 'b'}", "{2: 'b', 1: 'a'}", "{1: 'a', 'b': 2}", "{'b': 2, 1: 'a'}", "{None: 1}", "{True: 1, 2.5: 2}",
    "{1.5: 'x', 0.5: 'y'}", "{(1, 2): 'a'}", "{(1, 2): 'a', (0, 5): 'b'}", "{(): 1}", "{1: [1, 2], 2: {'a': 1}}", "{1: {2: {3: 4}}}",
    "[{1: 2}, {'a': 1}]", "{1: (1, 2)}", "{1: set([1])}", "{1: set()}", "{1: ()}", "{1: []}", "{1: {}}", "{1: None}",
    "{(1, 2): (3, 4)}", "{('a', 'b'): 1}", "{x: x for x in [3, 1, 2]}", "{'a' * 70: 1, 2: 2}", "{12345678901234567890123: 1}" if False else "{7: 'x'}",
    "{1: 'multi\\nline'}", "{1: 'a: b'}", "{1: 'yes', 2: 'null', 3: '1'}", "{'1': 1, 2: 2}", "{'': 1, 2: 2}",
]
OPTIONS = {
    "block_sorted": dict(),
    "block_unsorted": dict(sort_keys=False),
    "flow": dict(default_flow_style=True, allow_unicode=False),
}
cases = []
for e in EXPRS:
    v = eval_expr_with_mapping(e, {})
    out = {"expr": e}
    for name, kw in OPTIONS.items():
        out[name] = dump_yaml(v, **kw)
    cases.append(out)
json.dump(cases, sys.stdout, ensure_ascii=False, indent=0)
