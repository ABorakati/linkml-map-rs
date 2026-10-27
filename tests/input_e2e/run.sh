#!/usr/bin/env bash
# End-to-end input parity: upstream `linkml-map` vs the Rust CLI on tricky YAML,
# JSON, TSV and CSV inputs, compared byte-for-byte in every output format.
#
#   UPSTREAM=/path/to/venv/bin/linkml-map RUST=/path/to/linkml-tr-rs ./run.sh
#   REGEN=1 UPSTREAM=... ./run.sh        # rewrite expected/ from upstream
#
# Needs upstream linkml-map v0.6.0-rc1. The upstream outputs are committed under
# expected/ and replayed without Python by
# crates/linkml-map-cli/tests/input_e2e.rs; this script compares a live install
# with the Rust CLI and is how expected/ was made.
#
# A case/format upstream cannot produce (it raises, e.g. simplejson refusing the
# `datetime.date` PyYAML builds from an unquoted `2024-01-01`) is recorded as
# expected/<case>.<format>.error holding upstream's last stderr line, and is
# reported here as "skip" rather than compared.
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
UPSTREAM="${UPSTREAM:-linkml-map}"
RUST="${RUST:-${CARGO_TARGET_DIR:-$REPO/target}/debug/linkml-tr-rs}"
FORMATS="${FORMATS:-yaml json jsonl tsv}"
OUT="$(mktemp -d)"
trap 'rm -rf "$OUT"' EXIT
cd "$HERE"
mkdir -p expected
fail=0
compared=0
skipped=0

for input in cases/*; do
  name="$(basename "$input")"
  for fmt in $FORMATS; do
    exp="expected/$name.$fmt"
    up="$OUT/up.$fmt"
    rs="$OUT/rs.$fmt"
    rm -f "$up" "$rs"
    if [ -n "${REGEN:-}" ] || [ ! -e "$exp" -a ! -e "$exp.error" ]; then
      rm -f "$exp" "$exp.error"
      if "$UPSTREAM" map-data -T spec.yaml -s schema.yaml --source-type Rec -f "$fmt" -o "$up" "$input" 2>"$OUT/up.err"; then
        cp "$up" "$exp"
      else
        tail -n 1 "$OUT/up.err" >"$exp.error"
      fi
    fi
    if [ -e "$exp.error" ]; then
      echo "skip  $name -> $fmt (upstream: $(cat "$exp.error"))"
      skipped=$((skipped + 1))
      continue
    fi
    if ! "$RUST" map-data -T spec.yaml -s schema.yaml --source-type Rec -f "$fmt" -o "$rs" "$input" 2>"$OUT/rs.err"; then
      echo "FAIL  $name -> $fmt (rust: $(tail -n 1 "$OUT/rs.err"))"
      fail=1
      continue
    fi
    compared=$((compared + 1))
    if cmp -s "$exp" "$rs"; then
      echo "ok    $name -> $fmt"
    else
      echo "FAIL  $name -> $fmt"
      diff -u "$exp" "$rs" | head -30
      fail=1
    fi
  done
done
echo "compared $compared, skipped $skipped"
exit "$fail"
