#!/usr/bin/env bash
# Differential testing: upstream Python linkml-map vs the Rust CLI (linkml-tr-rs).
#
#   tests/differential/run.sh [--examples] [--fuzz N] [--seed S] [--time-budget SECS] [--profiles a,b] [--corpus DIR]
#                             [--build] [--fail-on-diff] [--report FILE]
#
#   --examples        deterministic sweep over upstream's examples (all formats, both eval modes, derive-schema, validate-spec)
#   --fuzz N          Hypothesis campaign of ~N generated cases; each distinct divergence is shrunk and written to corpus/
#   --seed S          Hypothesis seed (default 1); with a fixed seed and no time budget a run is reproducible
#   --time-budget T   stop the fuzz campaign after T seconds
#   --profiles        comma list of generator profiles (all,core,awkward,enums,expr,joins,nested,multivalued,inherit,units,mint,yaml,tsv,sources)
#   --corpus DIR      where counterexamples go (default tests/differential/corpus)
#   --known-from-corpus  do not re-report divergences whose signature is already in corpus/
#   --avoid-known     do not generate inputs that hit divergences already reported as open (empty TSV cells, >64-bit ints, ...)
#   --build           cargo build --release -p linkml-map-cli first
#   --fail-on-diff    exit 1 if the examples sweep has a divergent cell or the fuzzer found a new divergence
# No flags: --examples --fuzz 300.
#
# Environment: UPSTREAM_PY (default /home/user/upstream/upvenv/bin/python; must have linkml-map + hypothesis installed),
#              UPSTREAM_BIN (the venv's linkml-map script), UPSTREAM_DIR (upstream checkout; default /home/user/upstream/linkml-map),
#              LINKML_TR_RS (path to the Rust binary; default $CARGO_TARGET_DIR/release/linkml-tr-rs or target/release/...).
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
PY="${UPSTREAM_PY:-/home/user/upstream/upvenv/bin/python}"
export UPSTREAM_BIN="${UPSTREAM_BIN:-$(dirname "$PY")/linkml-map}"
UPSTREAM_DIR="${UPSTREAM_DIR:-/home/user/upstream/linkml-map}"

do_examples=0; fuzz=0; seed=1; budget=""; profiles=""; corpus=""; build=0; known=0; avoid=0; gate=0; report=""
while [ $# -gt 0 ]; do
  case "$1" in
    --examples) do_examples=1 ;;
    --fuzz) fuzz="$2"; shift ;;
    --seed) seed="$2"; shift ;;
    --time-budget) budget="$2"; shift ;;
    --profiles) profiles="$2"; shift ;;
    --corpus) corpus="$2"; shift ;;
    --build) build=1 ;;
    --known-from-corpus) known=1 ;;
    --avoid-known) avoid=1 ;;
    --fail-on-diff) gate=1 ;;
    --report) report="$2"; shift ;;
    -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
  shift
done
if [ "$do_examples" = 0 ] && [ "$fuzz" = 0 ]; then do_examples=1; fuzz=300; fi

if [ "$build" = 1 ]; then (cd "$repo" && cargo build --release -p linkml-map-cli); fi
status=0

if [ "$do_examples" = 1 ]; then
  # Record every map_object call upstream's own pytest suite makes, and replay those too.
  cap="$(mktemp -d)"
  (cd "$UPSTREAM_DIR" && CAPTURE_DIR="$cap" PYTHONPATH="$here" "$PY" -m pytest -p capture_plugin tests -q -p no:cacheprovider -x \
      --deselect tests/test_compiler/test_graphviz_compiler.py >/dev/null 2>&1) || echo "note: capturing upstream test inputs failed; continuing" >&2
  args=(--upstream-dir "$UPSTREAM_DIR" --report "${report:-$here/EXAMPLES_REPORT.md}" --captured "$cap")
  [ "$gate" = 1 ] && args+=(--fail-on-diff)
  "$PY" "$here/examples_sweep.py" "${args[@]}" > /dev/null || status=1
  echo "examples sweep report: ${report:-$here/EXAMPLES_REPORT.md}"
fi

if [ "$fuzz" != 0 ]; then
  args=(--examples "$fuzz" --seed "$seed")
  [ -n "$budget" ] && args+=(--time-budget "$budget")
  [ -n "$profiles" ] && args+=(--profiles "$profiles")
  [ -n "$corpus" ] && args+=(--corpus "$corpus")
  [ "$known" = 1 ] && args+=(--known-from-corpus)
  [ "$avoid" = 1 ] && args+=(--avoid-known)
  out="$("$PY" "$here/fuzz.py" "${args[@]}" | tee /dev/stderr | tail -1)"
  if [ "$gate" = 1 ] && [ "$out" != "[]" ]; then status=1; fi
fi
exit $status
