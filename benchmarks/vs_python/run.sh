#!/usr/bin/env bash
# Compare the Rust linkml-map engine against base Python linkml-map on the same
# workload (measurements fixture, transform-only). Sets up a venv, installs the
# Python package, runs both, prints a speedup table.
#
# Usage: ./run.sh [N_ROWS]   (default 100000)
set -euo pipefail

N_ROWS="${1:-100000}"
HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
VENV="$HERE/.venv"

echo "== setup python venv =="
if [ ! -d "$VENV" ]; then
  python -m venv "$VENV"
fi
# shellcheck disable=SC1091
if [ -f "$VENV/Scripts/activate" ]; then source "$VENV/Scripts/activate"; else source "$VENV/bin/activate"; fi
python -m pip install --quiet linkml-map pyyaml

echo "== build rust (release) =="
( cd "$REPO" && cargo build --release -q -p linkml-map-pipeline --bin bench-vs-python )

echo "== run python ($N_ROWS rows) =="
PY_JSON="$(python "$HERE/bench_python.py" "$N_ROWS" 2>/dev/null | tail -1)"

echo "== run rust ($N_ROWS rows) =="
RS_JSON="$(cd "$REPO" && cargo run --release -q -p linkml-map-pipeline --bin bench-vs-python "$N_ROWS" 2>/dev/null)"

echo
python - "$PY_JSON" "$RS_JSON" <<'PYEOF'
import json, sys
py = json.loads(sys.argv[1])
rs = [json.loads(l) for l in sys.argv[2].strip().splitlines()]
rows = {r["impl"]: r for r in rs}
rows["python"] = py
order = ["python", "rust-1thread", "rust-Ncore"]
print(f'{"impl":<14}{"workers":>8}{"rows/sec":>16}{"speedup vs py":>16}')
print("-"*54)
base = py["rows_per_sec"]
for k in order:
    r = rows[k]
    w = r.get("workers", 1)
    sp = r["rows_per_sec"]/base
    print(f'{k:<14}{w:>8}{r["rows_per_sec"]:>16,.0f}{sp:>15.1f}x')
print()
print(f'N_ROWS = {py["n_rows"]:,}  (transform-only, measurements fixture)')
PYEOF
