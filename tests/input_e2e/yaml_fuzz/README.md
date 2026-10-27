# YAML loader differential fuzz

Compares `linkml_map_io::yaml_load::safe_load_all` with PyYAML's pure-Python
`yaml.safe_load_all` on ~5,300 generated documents (scalar spellings in every
context, structure edge cases, merge keys, anchors, JSON documents).

```bash
python3 gen_corpus.py /tmp/yaml_corpus
cargo build -p linkml-map-io --example yaml_canon
/path/to/upstream/venv/bin/python yaml_diff.py /tmp/yaml_corpus target/debug/examples/yaml_canon
```

Known remaining mismatches (all exotic): libyaml rejects `[?]`, `[a:]` and
`=: x` where PyYAML accepts them, and accepts `!!str` with an empty value and
a surrogate-pair escape (`"😀"`) differently; PyYAML's two lone
surrogates then crash upstream's writers, so the JSON fallback is used here.
