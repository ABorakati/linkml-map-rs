category: rust-gap: int() coercion of non-ASCII digit strings

Python's int() accepts e.g. '١٢٣' (123); Rust's integer coercion rejects it. (Also covers delimited-file integer columns holding '1.0'.)

See notes.md (what differs per format) and observed_rust/ (Rust's output when found).
Delete this file once the Rust output matches expected/.
