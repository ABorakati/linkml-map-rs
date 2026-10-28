# Expression differential fuzzer

Compares the Rust expression evaluator (`crates/linkml-map-core/src/expr`, reached
through the same gate the engine uses, `eval_parsed_gated`) with upstream
`linkml-map` v0.6.0-rc1:

* **restricted** mode: `linkml_map.utils.eval_utils.eval_expr_with_mapping` (simpleeval);
* **unrestricted** mode: `ObjectTransformer._eval_expr` with `unrestricted_eval=True`
  (statement blocks and the restricted-then-asteval fallback), called on a stub `self`
  so the real upstream code runs.

Rust side: `crates/linkml-map-conformance/examples/expr_oracle.rs` (library code in
`src/exprfuzz.rs`). It reads JSONL cases and writes one JSON result per line; a
watchdog turns a runaway case into `{"err": "hang"}`, and `oracle.py` turns a crash
into `{"err": "crash"}` and carries on.

## Layout

| file | role |
| --- | --- |
| `grammar.py` | Hypothesis strategies: literals, operators, chained comparisons, `in`, subscripts/slices, attribute paths over nested maps, comprehensions, every built-in with right/wrong arity and types, `{slot}` references, f-strings, statement blocks. Awkward bindings (numeric strings, unicode digits, `inf`, empty strings, nulls, nested maps/lists). `MASK` switches features off. |
| `pyside.py` | evaluates a case with upstream in both binding flavours (`obj` = `DynObj` as the engine's `dynamic_object` makes, `dict`); a divergence is reported only when Rust matches neither. |
| `oracle.py` | batch driver for the Rust oracle. |
| `compare.py`, `codec.py` | result comparison (`1` vs `1.0` vs `True` distinct, float bits, tuple noted) and the shared JSON encoding. |
| `shrink.py` | tree/bindings shrinker. |
| `campaign.py` | generate, evaluate, cluster, shrink, write `divergences.jsonl` / `clusters.md`. |
| `probes.py` | hand-written probes, one group per divergence class; `--write corpus/curated.jsonl`. |
| `stmtprobe.py` | block/statement probes (`----`-separated file) compared against upstream asteval: `python stmtprobe.py FILE [-v]`. |
| `leaf.py`, `chars.py` | leaf oracles (py_eq, truthiness, float repr, str/repr, Decimal, json.dumps, int()/float(), round, casing, slugify...) with random inputs, and an exhaustive per-code-point scan. |
| `corpus/*.jsonl` | stored cases with the upstream result, replayed by `cargo test -p linkml-map-conformance --test expr_differential` (no Python needed). `status: divergence` entries are skipped by the default run and replayed by `-- --ignored`. |

## Running

```bash
cargo build -p linkml-map-conformance --example expr_oracle --release
export EXPR_ORACLE=$CARGO_TARGET_DIR/release/examples/expr_oracle
PY=/home/user/upstream/upvenv/bin/python        # upstream linkml-map + hypothesis, isolated venv

$PY campaign.py run --n 30000 --seed 1 --out /tmp/run1 --mode restricted
$PY campaign.py run --n 30000 --seed 2 --out /tmp/run2 --mode unrestricted 
$PY campaign.py run --n 3000 --seed 7 --out /tmp/ci --profile clean --fail-on-divergence   # the CI profile
$PY probes.py                       # curated probes
$PY leaf.py --n 20000               # leaf oracles
$PY chars.py                        # every code point through casing/repr/slugify/...
```

`--mask a,b,c` disables grammar features that are already known to diverge so the
campaign can see what they hide (`rawint bigint slice strstyle fstr lambda method
matmul setdisp is tuple pow bitwise strescape numstr-unicode methodattr
numattr`; `in`, `div`, `strescape` and `numstr-unicode` are fixed and no longer masked). `--profile clean` is the mask set that currently yields no divergences;
shrink it as divergences are fixed.

## Value semantics decided by this harness

* **Map attributes.** The engine binds a nested map as upstream's `DynObj` (missing
  key is `None`); only a dict built inside the expression (`{...}`, a dict
  comprehension, `dict(...)`) distributes over its values like a plain `dict`.
  `Value::Map` cannot tell a bound `any`-ranged dict from a `DynObj`, so that rarer
  flavour is not reproduced. The campaign accepts a Rust result that matches
  either flavour; stored `agree.jsonl` expectations are the `DynObj` one where the
  two differ. Truthiness is the remaining gap (class `dynobj-truthiness`): an empty
  bound map is falsy in Rust, a `DynObj` is always truthy.
* **Unicode data** for `repr`, `isprintable`, case mapping, `Cased`, decimal digits
  and whitespace is generated from CPython 3.13 (Unicode 15.1) by
  `crates/linkml-map-core/tools/gen_pytables.py`, not taken from Rust's `char`.
  To follow another Python version run the script with that interpreter (for
  3.14, Unicode 16.0) and redirect it to `crates/linkml-map-core/src/expr/pytables.rs`;
  `chars.py` should then show no differing code point.
* **`sorted`** reproduces CPython 3.13's `count_run` and binary insertion exactly
  for fewer than 64 items (so NaNs land where CPython puts them); longer lists use
  a stable merge sort.
* **Not ported:** `text-unidecode`'s tables
  (Artistic/GPL, so `slugify` keeps `deunicode`), list-vs-tuple affixes of
  `startswith`/`endswith`.
