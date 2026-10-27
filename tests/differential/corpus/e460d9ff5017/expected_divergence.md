category: rust-gap: float formatting is not Python's shortest round-trip repr

17-significant-digit floats print differently (123456789.12345679 vs 123456789.1234568).

See notes.md (what differs per format) and observed_rust/ (Rust's output when found).
Delete this file once the Rust output matches expected/.
