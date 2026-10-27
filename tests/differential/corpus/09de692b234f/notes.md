# Differential case 09de692b234f

- found by: fuzz.py --seed 11 profile=inherit
- a decimal `1e2` prints as `100` in CSV/TSV cells (`format_decimal`), not `1E+2`; fixed in `dump_delimited`
