"""Record end-to-end corpus cases for the join engine's DuckDB read of a JSON primary table.

Each case is a ``Person.json`` table (plus an empty ``Order.json``, so the block runs through the set-based join
engine) mapped by a spec that copies every column to a target slot.  Upstream's output is recorded with
``harness.save_corpus_case``; the Rust replay (``cargo test -p linkml-map-cli --test differential_corpus``) then
guards ``crates/linkml-map-pipeline/src/join_rows.rs``.  Run with the upstream venv::

    /home/user/upstream/upvenv/bin/python tests/differential/record_join_json_cases.py [--corpus DIR]
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

import yaml

sys.path.insert(0, str(Path(__file__).resolve().parent))
import harness as H  # noqa: E402

SCHEMA = {
    "id": "https://example.org/src", "name": "src",
    "prefixes": {"linkml": "https://w3id.org/linkml/", "ex": "https://example.org/"}, "default_prefix": "ex",
    "imports": ["linkml:types"],
    "classes": {
        "Person": {"attributes": {
            "id": {"range": "string", "identifier": True},
            "age": {"range": "integer"}, "score": {"range": "float"}, "flag": {"range": "boolean"},
            "when": {"range": "string"}, "note": {"range": "string"}, "order_ref": {"range": "Order"},
        }},
        "Order": {"attributes": {"id": {"range": "string", "identifier": True}, "name": {"range": "string"}}},
    },
}
TARGET = {
    "id": "https://example.org/tgt", "name": "tgt",
    "prefixes": {"linkml": "https://w3id.org/linkml/", "ex": "https://example.org/"}, "default_prefix": "ex",
    "imports": ["linkml:types"],
    "classes": {"Agent": {"attributes": {
        "id": {"range": "string", "identifier": True},
        "out_age": {"range": "integer"}, "out_score": {"range": "float"}, "out_flag": {"range": "boolean"},
        "out_when": {"range": "string"}, "out_note": {"range": "string"},
    }}},
}
SPEC = {
    "id": "https://example.org/spec", "title": "join json cells",
    "class_derivations": {"Agent": {
        "populated_from": "Person",
        "joins": {"Order": {"source_key": "order_ref", "lookup_key": "id"}},
        "slot_derivations": {
            "id": None,
            "out_age": {"populated_from": "age"}, "out_score": {"populated_from": "score"},
            "out_flag": {"populated_from": "flag"}, "out_when": {"populated_from": "when"},
            "out_note": {"populated_from": "note"},
        },
    }},
}

CASES = {
    # integers beyond 64 bits: a HUGEINT up to 2^64 - 1, a DOUBLE below -2^63 and above
    "hugeint_up_to_u64": [{"id": "a", "age": 2**63}, {"id": "b", "age": 2**64 - 1}, {"id": "c", "age": 1}],
    "double_beyond_u64": [{"id": "a", "age": 2**64}, {"id": "b", "age": -(2**63) - 1}, {"id": "c", "age": 7}],
    "bigint_in_float_column": [{"id": "a", "score": 2**63}, {"id": "b", "score": 1.5}, {"id": "c", "score": -(2**63) - 1}],
    "mixed_int_and_bigint": [{"id": "a", "age": -5}, {"id": "b", "age": 2**63}, {"id": "c", "age": 10**30}],
    # strings DuckDB reads as dates, times, timestamps and UUIDs
    "dates": [{"id": "a", "when": "2020-1-5"}, {"id": "b", "when": "2021-02-03"}, {"id": "c", "when": " 1999-12-31"}],
    "timestamps": [{"id": "a", "when": "2020-01-01T10:00:00Z"}, {"id": "b", "when": "2021-06-30T23:59:59Z"}],
    "timestamps_with_offsets": [{"id": "a", "when": "2020-01-01T10:00:00+02:00"}, {"id": "b", "when": "2021-01-01T00:30:00-0100"}],
    "fractions_and_times": [{"id": "a", "when": "2020-01-01 10:00:00.50", "note": "10:00"},
                            {"id": "b", "when": "2020-01-01 10:00:00.123456", "note": "23:59:59.999999"}],
    "uuids": [{"id": "a", "note": "7F0DCAD7-862E-5CEC-4D8E-A544EB9C736F"}, {"id": "b", "note": "7f0dcad7-862e-5cec-4d8e-a544eb9c736f"}],
    "date_and_text_stay_text": [{"id": "a", "when": "2020-01-01"}, {"id": "b", "when": "x"}, {"id": "c", "note": "2020-01-01T10:00:00.123456789Z"}],
    "mixed_column_keeps_json_text": [{"id": "a", "note": "2020-01-01"}, {"id": "b", "note": 5}, {"id": "c", "note": True}],
    "booleans_and_floats": [{"id": "a", "flag": True, "score": 1}, {"id": "b", "flag": False, "score": 2.5}],
    "epoch_and_infinity": [{"id": "a", "when": "epoch"}, {"id": "b", "when": "2020-01-01"}],
    # a UUID alone in the second scan vector (2048 records) is what the column becomes: the dates then fail
    "uuid_alone_in_second_vector": [{"id": f"p{i}", "note": "2020-01-01"} for i in range(2048)]
    + [{"id": "last", "note": "7f0dcad7-862e-5cec-4d8e-a544eb9c736f"}],
    # raw text (a str) is written as it is
    "negative_zero_and_big_float": '[{"id": "a", "age": -0, "score": 1e22}, {"id": "b", "age": 3, "score": 123456789012345678901234567890.0}]',
    "float_beyond_f64": '[{"id": "a", "score": 1e400}, {"id": "b", "score": 2.5}]',
}


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--corpus", default=str(Path(__file__).resolve().parent / "corpus"))
    a = ap.parse_args()
    corpus = Path(a.corpus)
    corpus.mkdir(parents=True, exist_ok=True)
    for name, rows in CASES.items():
        with H.tmp_case_dir() as d:
            (d / "schema.yaml").write_text(yaml.safe_dump(SCHEMA, sort_keys=False))
            (d / "target.yaml").write_text(yaml.safe_dump(TARGET, sort_keys=False))
            (d / "spec.yaml").write_text(yaml.safe_dump(SPEC, sort_keys=False))
            data = d / "data"
            data.mkdir()
            (data / "Person.json").write_text(rows if isinstance(rows, str) else json.dumps(rows))
            (data / "Order.json").write_text("[]")
            case = H.Case(root=str(d), schema="schema.yaml", spec="spec.yaml", input="data", source_type=None,
                          target_schema="target.yaml", flags=["--unrestricted-eval"])
            res = H.compare_case(case, "inproc")
            dest = H.save_corpus_case(case, res, corpus, note=f"recorded by record_join_json_cases.py: {name}",
                                      seed="record_join_json_cases.py")
            kinds = ",".join(sorted({r.kind for r in res}))
            print(f"{name:40} {dest.name} {kinds}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
