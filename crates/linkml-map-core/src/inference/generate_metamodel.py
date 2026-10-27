#!/usr/bin/env python3
"""Regenerate `metamodel.json`, the LinkML metamodel field table used by `metamodel.rs`.

`SchemaMapper` (upstream `schema_mapper.py`) works on `linkml_runtime` dataclasses
(`SchemaDefinition`, `ClassDefinition`, `SlotDefinition`, ...). Their field order and
the normalisation their generated `__post_init__` applies decide the exact YAML
that `yaml_dumper.dumps` writes, so we read both from the installed
`linkml_runtime/linkml_model/meta.py` rather than hand-copying them:

    /home/user/upstream/upvenv/bin/python generate_metamodel.py > metamodel.json

For each metamodel class this records the dataclass field order and, per field,
one of these normalisation kinds (as `__post_init__` applies them):

* `any`               no normalisation (`Anything`, structured values);
* `str`/`int`/`float`/`bool`/`decimal`  scalar coerced with `str()`/`int()`/...
  (`Bool(...)` for `bool`);
* `list:<scalar>`     `[x]`-wrapping plus per-element coercion;
* `obj:<Class>`       `Class(**as_dict(x))`;
* `list_obj:<Class>`  list of the above;
* `dict:<Class>:<key>`/`list_inlined:<Class>:<key>`   `_normalize_inlined_as_dict` /
  `_normalize_inlined_as_list` with the key field name.
"""

import ast
import dataclasses
import json
import re
import sys

from linkml_runtime.linkml_model import annotations, extensions, meta, units
from linkml_runtime.utils.yamlutils import YAMLRoot

SCALARS = {"str": "str", "int": "int", "float": "float", "Bool": "bool", "Decimal": "decimal"}


def scalar_kind(ctor: str) -> str:
    """Map a coercion constructor name to a scalar kind (named string types are `str`)."""
    return SCALARS.get(ctor, "str")


def parse_post_init(cls):
    """Return {field: kind} from the class's own `__post_init__` source."""
    fn = cls.__dict__.get("__post_init__")
    if fn is None:
        return {}
    import inspect, textwrap

    tree = ast.parse(textwrap.dedent(inspect.getsource(fn)))
    kinds = {}
    for node in ast.walk(tree):
        text = ast.unparse(node) if isinstance(node, (ast.If, ast.Assign, ast.Expr)) else ""
        if isinstance(node, ast.Expr):
            m = re.match(
                r"self\._normalize_inlined_as_(dict|list)\(slot_name='(\w+)', slot_type=(\w+), key_name='(\w+)', keyed=(\w+)\)",
                text,
            )
            if m:
                kind, slot, typ, key, keyed = m.groups()
                kinds[slot] = f"{'dict' if kind == 'dict' else 'list_inlined'}:{typ}:{key}:{keyed}"
        elif isinstance(node, ast.Assign):
            m = re.match(r"self\.(\w+) = \[v if isinstance\(v, (\w+)\) else (\w+)\(\*\*as_dict\(v\)\) for v in self\.\1\]$", text)
            if m:
                kinds[m.group(1)] = f"list_obj:{m.group(3)}"
                continue
            m = re.match(r"self\.(\w+) = \[v if isinstance\(v, (\w+)\) else (\w+)\(v\) for v in self\.\1\]$", text)
            if m:
                kinds[m.group(1)] = f"list:{scalar_kind(m.group(3))}"
                continue
            m = re.match(r"self\.(\w+) = (\w+)\(\*\*as_dict\(self\.\1\)\)$", text)
            if m:
                kinds[m.group(1)] = f"obj:{m.group(2)}"
                continue
            m = re.match(r"self\.(\w+) = (\w+)\(self\.\1\)$", text)
            if m:
                kinds.setdefault(m.group(1), scalar_kind(m.group(2)))
    return kinds


def main():
    out = {}
    classes = [
        c
        for module in (meta, extensions, annotations, units)
        for c in vars(module).values()
        if isinstance(c, type)
        and issubclass(c, YAMLRoot)
        and c.__module__ == module.__name__
        and dataclasses.is_dataclass(c)
    ]
    for cls in classes:
        # most-derived rule wins: walk the MRO from the base up so subclasses overwrite.
        kinds = {}
        for base in reversed(cls.__mro__):
            if isinstance(base, type) and issubclass(base, YAMLRoot):
                kinds.update(parse_post_init(base))
        fields = []
        for f in dataclasses.fields(cls):
            fields.append([f.name, kinds.get(f.name, "any")])
        out[cls.__name__] = fields
    # One class per line keeps the committed table diffable.
    lines = [f"{json.dumps(name)}:{json.dumps(fields, separators=(',', ':'))}" for name, fields in out.items()]
    sys.stdout.write("{\n" + ",\n".join(lines) + "\n}\n")


if __name__ == "__main__":
    main()
