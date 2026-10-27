#!/usr/bin/env bash
# End-to-end YAML parity: upstream `linkml-map` (PyYAML) vs the Rust CLI.
#
# Runs the same map-data / derive-schema invocations through both and requires
# byte-identical YAML. Needs an environment with upstream linkml-map v0.6.0-rc1:
#
#   UPSTREAM=/path/to/venv/bin/linkml-map RUST=/path/to/linkml-tr-rs ./run.sh
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
UPSTREAM="${UPSTREAM:-linkml-map}"
RUST="${RUST:-${CARGO_TARGET_DIR:-$REPO/target}/debug/linkml-tr-rs}"
OUT="$(mktemp -d)"
trap 'rm -rf "$OUT"' EXIT
cd "$HERE"
# The upstream outputs are committed under expected/ (checked by crates/linkml-map-cli/tests/yaml_upstream_parity.rs);
# this script compares a live upstream install with the Rust CLI (and is how expected/ was made).
fail=0

check() { # name, upstream-output, rust-output
  if cmp -s "$2" "$3"; then
    echo "ok    $1"
  else
    echo "FAIL  $1"
    diff -u "$2" "$3" | head -40 || true
    fail=1
  fi
}

# Single object, YAML out: upstream `dump_output` (sort_keys=False).
"$UPSTREAM" map-data -T spec.yaml -s schema.yaml --source-type Record -o "$OUT/up_obj.yaml" data.yaml
"$RUST" map-data -T spec.yaml -s schema.yaml --source-type Record -o "$OUT/rs_obj.yaml" data.yaml
check "map-data data.yaml -> yaml (dump_output)" "$OUT/up_obj.yaml" "$OUT/rs_obj.yaml"

# Streamed TSV rows, YAML out: upstream `YAMLStreamWriter` (sorted keys, `---`).
"$UPSTREAM" map-data -T spec.yaml -s schema.yaml --source-type Record -f yaml -o "$OUT/up_stream.yaml" data.tsv
"$RUST" map-data -T spec.yaml -s schema.yaml --source-type Record -f yaml -o "$OUT/rs_stream.yaml" data.tsv
check "map-data data.tsv -> yaml (YAMLStreamWriter)" "$OUT/up_stream.yaml" "$OUT/rs_stream.yaml"

exit "$fail"
