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

Write-Host "== build rust (release) =="
Push-Location $Repo
cargo build --release -q -p linkml-map-pipeline --bin bench-vs-python
Pop-Location

Write-Host "== run python ($NRows rows) =="
$PyJson = (& "$Venv/Scripts/python.exe" "$Here/bench_python.py" $NRows 2>$null | Select-Object -Last 1)

Write-Host "== run rust ($NRows rows) =="
Push-Location $Repo
$RsJson = (cargo run --release -q -p linkml-map-pipeline --bin bench-vs-python $NRows 2>$null)
Pop-Location

& "$Venv/Scripts/python.exe" - $PyJson ($RsJson -join "`n") @'
import json, sys
py = json.loads(sys.argv[1])
rs = [json.loads(l) for l in sys.argv[2].strip().splitlines()]
rows = {r["impl"]: r for r in rs}; rows["python"] = py
base = py["rows_per_sec"]
print(f'{"impl":<14}{"workers":>8}{"rows/sec":>16}{"speedup vs py":>16}')
print("-"*54)
for k in ["python","rust-1thread","rust-Ncore"]:
    r = rows[k]; sp = r["rows_per_sec"]/base
    print(f'{k:<14}{r.get("workers",1):>8}{r["rows_per_sec"]:>16,.0f}{sp:>15.1f}x')
print(f'\nN_ROWS = {py["n_rows"]:,}  (transform-only, measurements fixture)')
'@
