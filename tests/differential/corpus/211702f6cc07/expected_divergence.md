category: rust-gap: value_mappings key for booleans is Python's str(True) ('True'), not 'true'

Upstream looks up `str(v)` ('True'/'False'); Rust's key for a YAML/JSON boolean differs, so the mapping misses.

See notes.md (what differs per format) and observed_rust/ (Rust's output when found).
Delete this file once the Rust output matches expected/.
