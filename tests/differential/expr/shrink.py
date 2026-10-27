"""Tree shrinker: reduce a diverging case while the divergence signature is preserved."""

from __future__ import annotations

from collections.abc import Callable, Iterator
from typing import Any

SIMPLE = [("int", 0), ("int", 1), ("const", "None"), ("str", "", "repr"), ("str", "a", "repr"), ("const", "True"), ("float", "1.5"), ("name", "a")]


def children(node) -> Iterator[Any]:
    if isinstance(node, tuple) and node and isinstance(node[0], str):
        for x in node[1:]:
            if isinstance(x, tuple) and x and isinstance(x[0], str):
                yield x
            elif isinstance(x, list):
                for y in x:
                    if isinstance(y, tuple) and y and isinstance(y[0], str):
                        yield y
                    elif isinstance(y, tuple):
                        for z in y:
                            if isinstance(z, tuple) and z and isinstance(z[0], str):
                                yield z


def replace_variants(node) -> Iterator[Any]:
    """Candidate simplifications of ``node`` itself (smaller trees first)."""
    yield from SIMPLE
    yield from children(node)
    if isinstance(node, tuple) and node and isinstance(node[0], str):
        for i, x in enumerate(node):
            if isinstance(x, list) and len(x) > 0:
                for j in range(len(x)):
                    yield node[:i] + (x[:j] + x[j + 1 :],) + node[i + 1 :]
            if isinstance(x, list):
                for j, y in enumerate(x):
                    for v in replace_variants_inner(y):
                        yield node[:i] + (x[:j] + [v] + x[j + 1 :],) + node[i + 1 :]
        if node[0] == "int" and node[1] not in (0, 1):
            yield ("int", 0)
            yield ("int", 1)
        if node[0] == "str" and node[1]:
            yield ("str", node[1][:1], node[2])
            yield ("str", "", node[2])
            yield ("str", node[1], "repr")


def replace_variants_inner(y) -> Iterator[Any]:
    if isinstance(y, tuple) and y and isinstance(y[0], str):
        yield from replace_variants(y)
    elif isinstance(y, tuple):
        for k, z in enumerate(y):
            if isinstance(z, tuple) and z and isinstance(z[0], str):
                for v in replace_variants(z):
                    yield y[:k] + (v,) + y[k + 1 :]


def subtrees(node, path=()) -> Iterator[tuple[tuple, Any]]:
    """All (path, node) pairs; a path is a list of indices into tuples/lists."""
    yield path, node
    if isinstance(node, (tuple, list)):
        for i, x in enumerate(node):
            if isinstance(x, (tuple, list)):
                yield from subtrees(x, path + (i,))


def set_at(node, path, new):
    if not path:
        return new
    i = path[0]
    if isinstance(node, tuple):
        return node[:i] + (set_at(node[i], path[1:], new),) + node[i + 1 :]
    return node[:i] + [set_at(node[i], path[1:], new)] + node[i + 1 :]


def candidates(tree) -> Iterator[Any]:
    for path, sub in subtrees(tree):
        if isinstance(sub, tuple) and sub and isinstance(sub[0], str):
            for v in replace_variants(sub):
                if v != sub:
                    yield set_at(tree, path, v)
        elif isinstance(sub, list):
            for j in range(len(sub)):
                yield set_at(tree, path, sub[:j] + sub[j + 1 :])


def size(tree) -> int:
    return len(repr(tree))


def shrink_tree(tree, pred: Callable[[Any], bool], budget: int = 400):
    best = tree
    tried = 0
    improved = True
    while improved and tried < budget:
        improved = False
        for cand in candidates(best):
            if size(cand) >= size(best):
                continue
            tried += 1
            if tried > budget:
                break
            if pred(cand):
                best = cand
                improved = True
                break
    return best


def shrink_bindings(bindings: dict, pred: Callable[[dict], bool], budget: int = 150) -> dict:
    best = dict(bindings)
    for k in list(best):
        cand = {x: v for x, v in best.items() if x != k}
        if pred(cand):
            best = cand
    tried = 0

    def simpler(v):
        if isinstance(v, list):
            yield []
            for i in range(len(v)):
                yield v[:i] + v[i + 1 :]
            for i, x in enumerate(v):
                for s in simpler(x):
                    yield v[:i] + [s] + v[i + 1 :]
        elif isinstance(v, dict):
            yield {}
            for k in v:
                yield {x: y for x, y in v.items() if x != k}
            for k, x in v.items():
                for s in simpler(x):
                    yield {**v, k: s}
        elif isinstance(v, str):
            if v:
                yield ""
                yield v[:1]
                yield v[1:]
        elif isinstance(v, (int, float)) and not isinstance(v, bool):
            yield 0
            yield 1
        if v is not None:
            yield None

    changed = True
    while changed and tried < budget:
        changed = False
        for k in list(best):
            for s in simpler(best[k]):
                tried += 1
                if pred({**best, k: s}):
                    best = {**best, k: s}
                    changed = True
                    break
    return best
