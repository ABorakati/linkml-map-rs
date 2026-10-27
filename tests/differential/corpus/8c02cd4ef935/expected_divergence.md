category: upstream-quirk: value_mappings with an empty-string value crash spec loading

Upstream raises `TypeError: 'str' object does not support item assignment` while loading a `value_mappings` whose value is ''; Rust accepts it.

See notes.md (what differs per format) and observed_rust/ (Rust's output when found).
Delete this file once the Rust output matches expected/.
