category: port:known-open: TSV/CSV empty cell kept as ''

Upstream's loader drops empty cells (the slot is absent); Rust keeps an empty string.

See notes.md (what differs per format) and observed_rust/ (Rust's output when found).
Delete this file once the Rust output matches expected/.
