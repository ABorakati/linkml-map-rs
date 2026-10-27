"""Deterministic examples sweep: every upstream example/test fixture through both engines, all output formats.

Usage (upstream venv):
    /home/user/upstream/upvenv/bin/python tests/differential/examples_sweep.py [--upstream-dir DIR]
        [--upstream-mode subprocess|inproc] [--report FILE] [--json FILE]

For every fixture with a spec + schema + input it runs ``map-data`` for yaml/json/jsonl/tsv/csv in both eval
modes (upstream default, restricted; and ``--unrestricted-eval``), plus ``-o FILE`` output, and compares bytes.
For every spec + schema it also compares ``derive-schema`` bytes and ``validate-spec`` exit status/text.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

import yaml

sys.path.insert(0, str(Path(__file__).resolve().parent))
import harness as H  # noqa: E402

DEFAULT_UPSTREAM = Path("/home/user/upstream/linkml-map")


def discover(up: Path) -> list[dict]:
    """Build the list of fixtures: dicts with name, schema, spec, target, inputs=[(label, path, source_type)]."""
    ex = up / "tests/input/examples"
    fixtures: list[dict] = []
    for d in sorted(p for p in ex.iterdir() if p.is_dir() and not p.name.startswith("_")):
        schemas = sorted((d / "source").glob("*.yaml")) if (d / "source").exists() else []
        specs = sorted((d / "transform").glob("*.yaml")) if (d / "transform").exists() else []
        targets = sorted((d / "target").glob("*.yaml")) if (d / "target").exists() else []
        if d.name == "fair_mappings_metadata":
            schemas = [up / "src/linkml_map/datamodel/transformer_model.yaml"]
        for schema in schemas:
            for spec in specs:
                if len(schemas) > 1 and schema.stem.split("_")[0] not in spec.name and schema.stem not in spec.name:
                    continue
                target = None
                if len(targets) == 1:
                    target = targets[0]
                inputs = []
                for f in sorted((d / "data").glob("*")) if (d / "data").exists() else []:
                    if f.suffix in (".yaml", ".json"):
                        parts = f.stem.split("-")
                        st = parts[-2] if len(parts) >= 2 else None
                        if d.name == "fair_mappings_metadata":
                            st = "TransformationSpecification"
                        if (d / "instructions.yaml").exists():
                            st = None  # upstream's MultiFileTransformer then resolves the tree_root class
                        inputs.append((f"data/{f.name}", f, st))
                    elif f.suffix in (".tsv", ".csv"):
                        inputs.append((f"data/{f.name}", f, f.stem))
                if (d / "data").exists() and any(f.suffix in (".tsv", ".csv") for f in (d / "data").iterdir()):
                    inputs.append(("data/ (directory)", d / "data", None))
                fixtures.append(
                    {"name": f"{d.name}/{spec.name}", "schema": schema, "spec": spec, "target": target, "inputs": inputs}
                )
    # extra pairs the upstream tests refer to outside the example tree
    attic = up / "tests/input/attic"
    if (attic / "personinfo_s1.yaml").exists():
        fixtures.append(
            {
                "name": "attic/transform-s1-to-s2.yaml",
                "schema": attic / "personinfo_s1.yaml",
                "spec": attic / "transform-s1-to-s2.yaml",
                "target": attic / "personinfo_s2.yaml",
                "inputs": [("personinfo-s1-example-data-01.yaml", attic / "personinfo-s1-example-data-01.yaml", None)],
            }
        )
    return fixtures


SYMBOLS = {
    "match": "ok",
    "both_error": "E",
    "output_diff": "DIFF",
    "rust_error": "RS-ERR",
    "upstream_error": "UP-ERR",
    "rust_panic": "PANIC",
}


def cell(r: H.FormatResult) -> str:
    s = SYMBOLS[r.kind]
    if r.kind == "both_error":
        s += f"({r.msg_level[0]})"
    return s


def sweep(up: Path, mode: str) -> tuple[list[dict], list[dict]]:
    rows: list[dict] = []
    other: list[dict] = []
    for fx in discover(up):
        for label, inp, st in fx["inputs"]:
            for evalmode, flags in (("restricted", []), ("unrestricted", ["--unrestricted-eval"])):
                case = H.Case(
                    root="/", schema=str(fx["schema"]), spec=str(fx["spec"]), input=str(inp), source_type=st,
                    target_schema=str(fx["target"]) if fx["target"] else None, flags=flags,
                )
                res = H.compare_case(case, mode)
                rows.append({"fixture": fx["name"], "input": label, "eval": evalmode, "results": res})
                # -o FILE variant (one format that exercises the file writer path)
        # derive-schema
        up_run = H.run_upstream(["derive-schema", "-T", str(fx["spec"]), str(fx["schema"])], mode)
        rs_run = H.run_rust(["derive-schema", "-T", str(fx["spec"]), "-s", str(fx["schema"])])
        other.append({"fixture": fx["name"], "cmd": "derive-schema", "res": H.classify("-", up_run, rs_run)})
        vargs_up = ["validate-spec", str(fx["spec"]), "--source-schema", str(fx["schema"])]
        vargs_rs = ["validate-spec", str(fx["spec"]), "--source-schema", str(fx["schema"])]
        if fx["target"]:
            vargs_up += ["--target-schema", str(fx["target"])]
            vargs_rs += ["--target-schema", str(fx["target"])]
        up_run = H.run_upstream(vargs_up, mode)
        rs_run = H.run_rust(vargs_rs)
        other.append({"fixture": fx["name"], "cmd": "validate-spec", "res": H.classify("-", up_run, rs_run)})
    return rows, other


def file_mode_check(up: Path, mode: str, rows_out: list[dict]) -> None:
    """Re-run each ok fixture with ``-o FILE`` for each format and compare file bytes (writer path vs stdout path)."""
    import tempfile

    for fx in discover(up):
        for label, inp, st in fx["inputs"]:
            for fmt in H.FORMATS:
                with tempfile.TemporaryDirectory() as td:
                    fo_up, fo_rs = str(Path(td) / f"up.{fmt}"), str(Path(td) / f"rs.{fmt}")
                    case = H.Case(root="/", schema=str(fx["schema"]), spec=str(fx["spec"]), input=str(inp), source_type=st,
                                  target_schema=str(fx["target"]) if fx["target"] else None, flags=["--unrestricted-eval"])
                    ru = H.run_upstream(case.map_data_args(fmt, output=fo_up), mode)
                    rr = H.run_rust(case.map_data_args(fmt, output=fo_rs))
                    if ru.ok and Path(fo_up).exists():
                        ru.out = Path(fo_up).read_bytes()
                    if rr.ok and Path(fo_rs).exists():
                        rr.out = Path(fo_rs).read_bytes()
                    r = H.classify(fmt, ru, rr)
                    rows_out.append({"fixture": fx["name"], "input": label, "fmt": fmt, "res": r})


def captured_sweep(cdir: Path, mode: str) -> list[dict]:
    """Run every case recorded by ``capture_plugin.py`` (inputs/specs that upstream's own pytest suite exercised)."""
    out = []
    for d in sorted(p for p in cdir.iterdir() if (p / "case.json").is_file()):
        meta = json.loads((d / "case.json").read_text())
        case = H.Case(root=str(d), schema=meta["schema"], spec=meta["spec"], input=meta["input"], source_type=meta["source_type"],
                      target_schema=meta.get("target_schema"), flags=meta.get("flags", []))
        out.append({"dir": d.name, "origin": meta.get("origin", ""), "results": H.compare_case(case, mode)})
    return out


def render_captured(caps) -> str:
    tally: dict = {}
    for c in caps:
        for r in c["results"]:
            tally[r.kind] = tally.get(r.kind, 0) + 1
    out = ["", "## Cases captured from upstream's own test-suite", "",
           f"{len(caps)} distinct map_object invocations x {len(H.FORMATS)} formats: " + ", ".join(f"{k}: {v}" for k, v in sorted(tally.items())), "",
           "| case | origin test | divergent formats |", "|---|---|---|"]
    for c in caps:
        bad = [f"{r.fmt}:{SYMBOLS[r.kind]}" for r in c["results"] if not r.ok]
        if bad:
            out.append(f"| {c['dir']} | {c['origin'].split(' ')[0].replace('tests/', '')} | {' '.join(bad)} |")
    return "\n".join(out) + "\n"


def render(rows, other, files) -> str:
    out = ["# Examples sweep: upstream linkml-map vs linkml-tr-rs", ""]
    out += ["Legend: ok = byte-identical; E(s|o|d) = both failed (messages same/overlap/differ); DIFF = both succeeded, bytes differ;",
            "RS-ERR = upstream ok, Rust failed; UP-ERR = upstream failed, Rust succeeded; PANIC = Rust panic/timeout.", ""]
    out += ["## map-data (stdout)", "", "| fixture | input | eval | " + " | ".join(H.FORMATS) + " |", "|---|---|---|" + "---|" * len(H.FORMATS)]
    for r in rows:
        out.append(f"| {r['fixture']} | {r['input']} | {r['eval']} | " + " | ".join(cell(x) for x in r["results"]) + " |")
    out += ["", "## derive-schema / validate-spec", "", "| fixture | command | result |", "|---|---|---|"]
    for o in other:
        out.append(f"| {o['fixture']} | {o['cmd']} | {cell(o['res'])} |")
    if files:
        out += ["", "## map-data with `-o FILE` (unrestricted eval)", "", "| fixture | input | " + " | ".join(H.FORMATS) + " |", "|---|---|" + "---|" * len(H.FORMATS)]
        byk: dict = {}
        for f in files:
            byk.setdefault((f["fixture"], f["input"]), {})[f["fmt"]] = f["res"]
        for (fx, inp), d in byk.items():
            out.append(f"| {fx} | {inp} | " + " | ".join(cell(d[f]) for f in H.FORMATS if f in d) + " |")
    allr = [x for r in rows for x in r["results"]] + [o["res"] for o in other] + [f["res"] for f in files]
    tally: dict = {}
    for x in allr:
        tally[x.kind] = tally.get(x.kind, 0) + 1
    out += ["", "## Totals", "", ", ".join(f"{k}: {v}" for k, v in sorted(tally.items())), ""]
    return "\n".join(out)


def detail(rows, other, files) -> str:
    """Per-divergence detail with a short diff excerpt."""
    import difflib

    out = []
    def one(title, r):
        if r.ok:
            return
        out.append(f"### {title} [{r.kind}]")
        if r.kind == "output_diff":
            a = r.up.out.decode("utf-8", "replace").splitlines()
            b = r.rs.out.decode("utf-8", "replace").splitlines()
            out.append("```diff\n" + "\n".join(list(difflib.unified_diff(a, b, "upstream", "rust", lineterm="", n=1))[:40]) + "\n```")
        else:
            out.append(f"upstream rc={r.up.rc} {r.up.exc_class}: {r.up.exc_msg[:300]}\n\nrust rc={r.rs.rc}: {(r.rs.exc_msg or r.rs.err)[:300]}")
    for r in rows:
        for x in r["results"]:
            one(f"{r['fixture']} {r['input']} {r['eval']} {x.fmt}", x)
    for o in other:
        one(f"{o['fixture']} {o['cmd']}", o["res"])
    for f in files:
        one(f"{f['fixture']} {f['input']} -o {f['fmt']}", f["res"])
    return "\n\n".join(out)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--upstream-dir", type=Path, default=DEFAULT_UPSTREAM)
    ap.add_argument("--upstream-mode", default="inproc", choices=["subprocess", "inproc"])
    ap.add_argument("--report", type=Path)
    ap.add_argument("--no-file-mode", action="store_true")
    ap.add_argument("--captured", type=Path, help="directory written by capture_plugin.py")
    ap.add_argument("--fail-on-diff", action="store_true", help="exit 1 if any cell is not ok/E (CI gate)")
    ap.add_argument("--allow-file", type=Path, default=Path(__file__).with_name("known_sweep_divergences.txt"),
                    help="cells (key|kind) already known to diverge; only others fail --fail-on-diff")
    ap.add_argument("--write-allow", type=Path, help="write the current divergent cells as the allow-list")
    a = ap.parse_args()
    rows, other = sweep(a.upstream_dir, a.upstream_mode)
    files: list[dict] = []
    if not a.no_file_mode:
        file_mode_check(a.upstream_dir, a.upstream_mode, files)
    text = render(rows, other, files) + "\n## Divergence detail\n\n" + detail(rows, other, files) + "\n"
    caps = captured_sweep(a.captured, a.upstream_mode) if a.captured else []
    if caps:
        text += render_captured(caps)
    if a.report:
        a.report.write_text(text)
    print(text)
    cells: list[tuple[str, H.FormatResult]] = []
    for r in rows:
        cells += [(f"map-data|{r['fixture']}|{r['input']}|{r['eval']}|{x.fmt}", x) for x in r["results"]]
    cells += [(f"{o['cmd']}|{o['fixture']}", o["res"]) for o in other]
    cells += [(f"file|{f['fixture']}|{f['input']}|{f['fmt']}", f["res"]) for f in files]
    # captured upstream-test cases are reported above but gated by the committed corpus replay (cargo test), not here
    bad = [(k, x) for k, x in cells if not x.ok]
    if a.write_allow:
        a.write_allow.write_text("# known divergent sweep cells (regenerate: examples_sweep.py --write-allow FILE)\n"
                                 + "".join(f"{k}|{x.kind}\n" for k, x in sorted(bad)))
    allow = set()
    if a.allow_file and a.allow_file.exists():
        allow = {ln.strip() for ln in a.allow_file.read_text().splitlines() if ln.strip() and not ln.startswith("#")}
    new = [(k, x) for k, x in bad if f"{k}|{x.kind}" not in allow]
    print(f"\nsweep: {len(bad)} divergent cells, {len(new)} not in allow-list", file=sys.stderr)
    for k, x in new[:50]:
        print(f"  NEW {k} [{x.kind}]", file=sys.stderr)
    return 1 if (new and a.fail_on_diff) else 0


if __name__ == "__main__":
    sys.exit(main())
