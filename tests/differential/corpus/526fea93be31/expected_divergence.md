category: rust-gap: int() coercion of non-ASCII digit strings

YAML `age: ١٢٣` (Arabic-Indic digits, read as a string by PyYAML) into an integer range: Python's int() accepts it (123),
Rust's integer coercion rejects it ("coercion failed ... invalid literal for int()").
See notes.md and observed_rust/. Delete this file once the Rust output matches expected/.
