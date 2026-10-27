category: rust-gap: schema slot absent from the row must evaluate to None

Upstream resolves a slot declared in the schema but absent from the row to None (even with --strict); Rust raises `name 'x' is not defined`.

See notes.md (what differs per format) and observed_rust/ (Rust's output when found).
Delete this file once the Rust output matches expected/.
