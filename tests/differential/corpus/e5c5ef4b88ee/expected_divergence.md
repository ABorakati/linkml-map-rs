category: rust-lenient: joins on YAML tables / single-object input

Upstream raises (`LookupIndex does not yet support 'yaml' files`, `lookup_index has not been initialized`) for joins outside TSV/CSV streaming; Rust resolves them.

See notes.md (what differs per format) and observed_rust/ (Rust's output when found).
Delete this file once the Rust output matches expected/.
