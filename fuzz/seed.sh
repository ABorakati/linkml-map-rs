#!/usr/bin/env bash
# Seed fuzz/corpus/<target>/ from the repository's fixtures (idempotent; safe to re-run).
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
repo="$(dirname "$here")"
corpus="$here/corpus"
mkdir -p "$corpus"/{expr_parse_eval,spec_load_compile,loader_yaml,loader_jsonl,loader_tsv,decimal_parse}

# Specifications: every transform spec under tests/.
i=0
while IFS= read -r f; do
  i=$((i + 1)); cp "$f" "$corpus/spec_load_compile/fixture-$i.yaml"
done < <(find "$repo/tests" -path '*/transform/*' \( -name '*.yaml' -o -name '*.yml' \) | sort)

# Expressions: curated seeds plus every `expr:` line found in those specs.
cp "$here"/seeds/expr/* "$corpus/expr_parse_eval/"
grep -rhoE '^\s*expr:\s*.+$' "$repo/tests" --include='*.yaml' 2>/dev/null \
  | sed -E 's/^\s*expr:\s*//; s/^"(.*)"$/\1/; s/^'"'"'(.*)'"'"'$/\1/' \
  | awk -v dir="$corpus/expr_parse_eval" 'length($0) > 0 && length($0) < 300 { n++; print > (dir "/spec-expr-" n ".txt") }' || true

# Loaders: the fixture data files.
i=0
while IFS= read -r f; do
  i=$((i + 1)); cp "$f" "$corpus/loader_yaml/fixture-$i.yaml"
done < <(find "$repo/tests" \( -path '*/data/*' -o -path '*/output/*' \) -name '*.yaml' | sort)
cp "$here"/seeds/jsonl/* "$corpus/loader_jsonl/"
for f in "$repo"/tests/examples/tabular/data/*.tsv "$repo"/tests/examples/tabular/data/*.csv; do
  [ -f "$f" ] && cp "$f" "$corpus/loader_tsv/$(basename "$f")"
done
cp "$here"/seeds/decimal/* "$corpus/decimal_parse/"
echo "seeded $(find "$corpus" -type f | wc -l) files under $corpus"
