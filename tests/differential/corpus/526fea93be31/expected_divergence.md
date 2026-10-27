category: rust-gap: TSV/CSV numeric-column parsing (e.g. '1.0' in an integer column)

Upstream's delimited loader tries int then float on numeric-range columns ('1.0' -> 1.0 -> 1); Rust keeps the string and int('1.0') fails.

See notes.md (what differs per format) and observed_rust/ (Rust's output when found).
Delete this file once the Rust output matches expected/.
