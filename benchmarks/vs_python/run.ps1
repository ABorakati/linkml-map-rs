# Compare Rust linkml-map vs base Python linkml-map (measurements fixture).
# Usage: ./run.ps1 [N_ROWS]   (default 100000)
param([int]$NRows = 100000)
$ErrorActionPreference = "Stop"

$Here = Split-Path -Parent $MyInvocation.MyCommand.Path
$Repo = (Resolve-Path "$Here/../..").Path
$Venv = "$Here/.venv"

Write-Host "== setup python venv =="
if (-not (Test-Path $Venv)) { python -m venv $Venv }
& "$Venv/Scripts/python.exe" -m pip install --quiet --upgrade pip
& "$Venv/Scripts/python.exe" -m pip install --quiet linkml-map pyyaml

$Src = "$Here/source_$NRows.jsonl"

Write-Host "== build rust (release) =="
Push-Location $Repo
cargo build --release -q -p linkml-map-pipeline --bin bench-vs-python --bin bench-e2e
Pop-Location

Write-Host "== generate shared input ($NRows rows) =="
& "$Venv/Scripts/python.exe" "$Here/bench_python.py" gen $Src $NRows

Write-Host "== transform-only =="
$PyT = (& "$Venv/Scripts/python.exe" "$Here/bench_python.py" $NRows 2>$null | Select-Object -Last 1)
Push-Location $Repo
$RsT = (cargo run --release -q -p linkml-map-pipeline --bin bench-vs-python $NRows 2>$null)
Pop-Location

Write-Host "== end-to-end =="
$PyE = (& "$Venv/Scripts/python.exe" "$Here/bench_python.py" e2e $Src "$Src.py_out.jsonl" 2>$null | Select-Object -Last 1)
Push-Location $Repo
$RsE = (cargo run --release -q -p linkml-map-pipeline --bin bench-e2e $Src 2>$null)
Pop-Location

& "$Venv/Scripts/python.exe" - $PyT ($RsT -join "`n") $PyE ($RsE -join "`n") @'
import json, sys
py_t, rs_t, py_e, rs_e = sys.argv[1:5]
def table(title, py_json, rs_lines, order):
    py = json.loads(py_json)
    rows = {r["impl"]: r for r in (json.loads(l) for l in rs_lines.strip().splitlines())}
    rows["python"] = py; base = py["rows_per_sec"]
    print(f"\n== {title} ==")
    print(f'{"impl":<18}{"workers":>8}{"rows/sec":>16}{"speedup vs py":>16}')
    print("-"*58)
    for k in order:
        r = rows[k]
        print(f'{k:<18}{r.get("workers",1):>8}{r["rows_per_sec"]:>16,.0f}{r["rows_per_sec"]/base:>15.1f}x')
table("transform-only (no I/O)", py_t, rs_t, ["python","rust-1thread","rust-Ncore"])
table("end-to-end (read+transform+write)", py_e, rs_e, ["python","rust-e2e-1thread","rust-e2e-Ncore"])
print(f'\nN_ROWS = {json.loads(py_t)["n_rows"]:,}  (measurements fixture)')
'@
