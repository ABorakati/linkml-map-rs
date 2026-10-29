category: join engine: LookupIndex SQL-keyword table name (`Order`) with an empty primary and a second, class_named join

Found by the port/bigjoin joins campaign (seed 4271), unrelated to the DuckDB typing. Upstream's `LookupIndex` interpolates
the joined table name into `CREATE OR REPLACE TABLE Order AS ...` and raises `ParserException` (already a suggested
upstream issue: "A joined table whose name is a SQL keyword"); Rust succeeds here with no rows. Delete this file once
it matches.
