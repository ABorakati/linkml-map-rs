category: rust-gap: nested class_derivations list with repeated names keeps only the last

Two entries with the same `name` in a slot-level `class_derivations:` list must both emit an object; Rust (mapping-keyed) drops all but the last.

See notes.md (what differs per format) and observed_rust/ (Rust's output when found).
Delete this file once the Rust output matches expected/.
