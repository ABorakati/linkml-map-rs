category: rust-lenient: nested derivation without a target schema

Upstream raises (`'NoneType' object has no attribute 'induced_slot'`) when slot-level class_derivations run with no target schema; Rust emits the nested object.

See notes.md (what differs per format) and observed_rust/ (Rust's output when found).
Delete this file once the Rust output matches expected/.
