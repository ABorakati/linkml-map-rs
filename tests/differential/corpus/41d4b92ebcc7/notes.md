# Differential case 41d4b92ebcc7

- found by: fuzz.py --seed 4242 profile=all
- `yaml`: upstream_error
- `json`: upstream_error
- `jsonl`: upstream_error
- `tsv`: upstream_error
- `csv`: upstream_error

signature: `yaml:upstream_error:ParserException::|json:upstream_error:ParserException::|jsonl:upstream_error:ParserException::|tsv:upstream_error:ParserException::|csv:upstream_error:ParserException::`

port/fkexpr campaign (seed 4242, profile all): a join named like a SQL keyword (`Order`) makes upstream's `CREATE OR REPLACE TABLE Order` fail with a DuckDB ParserException; Rust accepts it. Not related to references.
