#!/usr/bin/env bash
# Compare the Rust linkml-map engine against base Python linkml-map on the same
# workload (measurements fixture). Reports BOTH:
#   - transform-only (per-row engine CPU, no I/O)
#   - end-to-end     (read JSONL file -> transform -> write JSONL file)
# Sets up a venv, installs the Python package, runs both, prints speedup tables.
#
# Usage: ./run.sh [N_ROWS]   (default 100000)
set -euo pipefail

N_ROWS="${1:-100000}"
HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
VENV="$HERE/.venv"
SRC="$HERE/source_${N_ROWS}.jsonl"

echo "== setup python venv =="
[ -d "$VENV" ] || python -m venv "$VENV"
# shellcheck disable=SC1091
if [ -f "$VENV/Scripts/activate" ]; then source "$VENV/Scripts/activate"; else source "$VENV/bin/activate"; fi
python -m pip install --quiet linkml-map pyyaml

echo "== build rust (release) =="
( cd "$REPO" && cargo build --release -q -p linkml-map-pipeline --bin bench-vs-python --bin bench-e2e )

echo "== generate shared input ($N_ROWS rows) =="
python "$HERE/bench_python.py" gen "$SRC" "$N_ROWS"

echo "== transform-only: python =="
PY_T="$(python "$HERE/bench_python.py" "$N_ROWS" 2>/dev/null | tail -1)"
echo "== transform-only: rust =="
RS_T="$(cd "$REPO" && cargo run --release -q -p linkml-map-pipeline --bin bench-vs-python "$N_ROWS" 2>/dev/null)"

echo "== end-to-end: python (read+transform+write) =="
PY_E="$(python "$HERE/bench_python.py" e2e "$SRC" "$SRC.py_out.jsonl" 2>/dev/null | tail -1)"
echo "== end-to-end: rust (read+transform+write) =="
RS_E="$(cd "$REPO" && cargo run --release -q -p linkml-map-pipeline --bin bench-e2e "$SRC" 2>/dev/null)"

echo
python - "$PY_T" "$RS_T" "$PY_E" "$RS_E" <<'PYEOF'
import json, sys
py_t, rs_t, py_e, rs_e = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]

def table(title, py_json, rs_lines, order):
    py = json.loads(py_json)
    rows = {r["impl"]: r for r in (json.loads(l) for l in rs_lines.strip().splitlines())}
    rows["python"] = py
    base = py["rows_per_sec"]
    print(f"\n== {title} ==")
    print(f'{"impl":<18}{"workers":>8}{"rows/sec":>16}{"speedup vs py":>16}')
    print("-" * 58)
    for k in order:
        r = rows[k]
        print(f'{k:<18}{r.get("workers",1):>8}{r["rows_per_sec"]:>16,.0f}{r["rows_per_sec"]/base:>15.1f}x')

table("transform-only (no I/O)", py_t, rs_t, ["python", "rust-1thread", "rust-Ncore"])
table("end-to-end (read+transform+write)", py_e, rs_e, ["python", "rust-e2e-1thread", "rust-e2e-Ncore"])
print(f'\nN_ROWS = {json.loads(py_t)["n_rows"]:,}  (measurements fixture)')
PYEOF
