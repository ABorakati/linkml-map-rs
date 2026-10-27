category: upstream-quirk: --source-type forces one source class on every class derivation

With --source-type X upstream maps rows of every block as X (`source_type or table_name`), so a second block over another table fails with `No such slot`; Rust types each block by its own table.

See notes.md (what differs per format) and observed_rust/ (Rust's output when found).
Delete this file once the Rust output matches expected/.
