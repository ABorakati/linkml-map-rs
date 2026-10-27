category: rust-gap: integers beyond 64 bits

Rust's YAML/spec reader rejects integers outside i64/u64 (`JSON number out of range`) and TSV integers above i64 stay strings; Python ints are arbitrary precision.

See notes.md (what differs per format) and observed_rust/ (Rust's output when found).
Delete this file once the Rust output matches expected/.
