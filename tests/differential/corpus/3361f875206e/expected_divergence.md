category: rust-gap: offset result not coerced to the integer target range

Upstream coerces `offset` results through the target slot range (25); Rust emits 25.0.

See notes.md (what differs per format) and observed_rust/ (Rust's output when found).
Delete this file once the Rust output matches expected/.
