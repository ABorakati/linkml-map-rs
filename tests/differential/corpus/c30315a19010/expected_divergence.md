category: upstream-quirk: list-valued `value:` truncated to its first element

Upstream's spec loader keeps only the first item of `value: [a, b]`; Rust keeps the list.

See notes.md (what differs per format) and observed_rust/ (Rust's output when found).
Delete this file once the Rust output matches expected/.
