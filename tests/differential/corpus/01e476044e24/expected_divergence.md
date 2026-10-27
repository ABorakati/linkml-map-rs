category: rust-gap: float formatting of huge floats in TSV/CSV output (1e+20)

TSV/CSV cells for floats >= 1e16 differ (upstream prints the Decimal/int form 100000000000000000000, Rust 1e+20).

See notes.md (what differs per format) and observed_rust/ (Rust's output when found).
Delete this file once the Rust output matches expected/.
