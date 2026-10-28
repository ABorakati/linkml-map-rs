category: join-engine JSON primary: integer beyond 64 bits (DuckDB infers DOUBLE)

Found by the port/joinalias joins campaign (unrelated to alias vs class_named). A block with joins over a JSON
primary table is read by upstream's DuckDB join engine: -9223372036854775809 (below the BIGINT range) is inferred
as DOUBLE, cast to the text -9.223372036854775808e18 and parsed back as -9223372036854775808. `join_rows::Ty::of`
keeps such integers as their digits ("not modelled" in `join_rows.rs`). Delete this file once it matches.
