category: rust-gap: dotted attribute access on partially populated inline objects

`a.b.c` in an expression must propagate None for absent keys of an inlined dict (upstream DynObj); Rust errors `value of type X has no attribute`.

See notes.md (what differs per format) and observed_rust/ (Rust's output when found).
Delete this file once the Rust output matches expected/.
