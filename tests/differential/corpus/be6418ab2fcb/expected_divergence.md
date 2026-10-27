category: rust-gap: stringification / multivalued casting edge cases

Single value into a multivalued target is split on the delimiter (upstream) / a scalar in a multivalued source slot is wrapped to a list before stringification; Rust keeps the scalar.

See notes.md (what differs per format) and observed_rust/ (Rust's output when found).
Delete this file once the Rust output matches expected/.
