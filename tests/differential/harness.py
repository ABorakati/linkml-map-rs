"""Core of the differential harness: run upstream ``linkml-map`` and the Rust CLI on the same case.

A :class:`Case` is a directory-relative description of one ``map-data`` invocation (schema, spec, optional
target schema, input file or directory, source type, shared flags).  :func:`compare_case` runs it through both
engines for each output format and classifies every format as one of:

``match``            both succeeded with byte-identical stdout
``both_error``       both failed (non-zero exit); ``msg_level`` grades how similar the messages are
``output_diff``      both succeeded but the bytes differ
``rust_error``       upstream succeeded, Rust failed (non-zero exit)
``upstream_error``   upstream failed, Rust succeeded
``rust_panic``       Rust panicked (exit 101) or timed out; always a bug

This module must run under the upstream venv (it imports ``linkml_map`` for the in-process runner).
"""

from __future__ import annotations

import contextlib
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from dataclasses import asdict, dataclass, field
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
FORMATS = ["yaml", "json", "jsonl", "tsv", "csv"]
DEFAULT_UPSTREAM_BIN = "/home/user/upstream/upvenv/bin/linkml-map"
TIMEOUT = 60

OK_KINDS = {"match", "both_error"}


def rust_bin() -> Path:
    """Locate the release ``linkml-tr-rs`` binary (``LINKML_TR_RS`` overrides)."""
    env = os.environ.get("LINKML_TR_RS")
    if env:
        return Path(env)
    for base in (os.environ.get("CARGO_TARGET_DIR"), str(REPO / "target")):
        if base and (Path(base) / "release" / "linkml-tr-rs").exists():
            return Path(base) / "release" / "linkml-tr-rs"
    raise FileNotFoundError("linkml-tr-rs not found: cargo build --release -p linkml-map-cli, or set LINKML_TR_RS")


# ── cases ────────────────────────────────────────────────────────────────────


@dataclass
class Case:
    """One ``map-data`` invocation, with paths relative to ``root`` (absolute paths also work)."""

    root: str
    schema: str = "schema.yaml"
    spec: str = "spec.yaml"
    input: str = "data"
    source_type: str | None = None
    target_schema: str | None = None
    flags: list[str] = field(default_factory=list)
    formats: list[str] = field(default_factory=lambda: list(FORMATS))

    def p(self, rel: str) -> str:
        return str(Path(self.root) / rel)

    def map_data_args(self, fmt: str, *, spec_flag: str = "-T", output: str | None = None) -> list[str]:
        args = ["map-data", spec_flag, self.p(self.spec), "-s", self.p(self.schema)]
        if self.target_schema:
            args += ["--target-schema", self.p(self.target_schema)]
        if self.source_type:
            args += ["--source-type", self.source_type]
        args += ["-f", fmt, *self.flags]
        if output:
            args += ["-o", output]
        args.append(self.p(self.input))
        return args

    def to_json(self) -> dict:
        d = asdict(self)
        d.pop("root")
        return d


# ── running ──────────────────────────────────────────────────────────────────


@dataclass
class Run:
    rc: int
    out: bytes
    err: str
    exc_class: str | None = None
    exc_msg: str = ""

    @property
    def ok(self) -> bool:
        return self.rc == 0


def run_rust(args: list[str], timeout: int = TIMEOUT) -> Run:
    env = {**os.environ, "RUST_BACKTRACE": "0"}
    try:
        p = subprocess.run([str(rust_bin()), *args], capture_output=True, timeout=timeout, env=env)
    except subprocess.TimeoutExpired:
        return Run(124, b"", "timeout", "Timeout", "timeout")
    err = p.stderr.decode("utf-8", "replace")
    cls = None
    msg = ""
    if p.returncode != 0:
        cls = "panic" if p.returncode == 101 or "panicked at" in err else "error"
        msg = rust_error_message(err)
    return Run(p.returncode, p.stdout, err, cls, msg)


def rust_error_message(err: str) -> str:
    lines = [ln for ln in err.splitlines() if ln.strip()]
    for ln in lines:
        if ln.startswith("Error:"):
            return ln[len("Error:") :].strip()
    return lines[-1].strip() if lines else ""


_INPROC = {}


def _inproc_main():
    if "main" not in _INPROC:
        from click.testing import CliRunner
        from linkml_map.cli.cli import main

        _INPROC["main"] = main
        _INPROC["runner"] = CliRunner()
    return _INPROC["main"], _INPROC["runner"]


def run_upstream(args: list[str], mode: str = "inproc", timeout: int = TIMEOUT) -> Run:
    """Run upstream ``linkml-map``: ``inproc`` (click's CliRunner, fast) or ``subprocess`` (true end to end)."""
    if mode == "subprocess":
        exe = os.environ.get("UPSTREAM_BIN", DEFAULT_UPSTREAM_BIN)
        try:
            p = subprocess.run([exe, *args], capture_output=True, timeout=timeout)
        except subprocess.TimeoutExpired:
            return Run(124, b"", "timeout", "Timeout", "timeout")
        err = p.stderr.decode("utf-8", "replace")
        cls = msg = None
        if p.returncode != 0:
            cls, msg = parse_upstream_traceback(err)
        return Run(p.returncode, p.stdout, err, cls, msg or "")
    main, runner = _inproc_main()
    res = runner.invoke(main, args, catch_exceptions=True)
    exc = res.exception
    cls = msg = None
    if res.exit_code != 0:
        if exc is None or isinstance(exc, SystemExit):
            # click.ClickException or explicit SystemExit: message is on stderr as "Error: ..."
            cls = "ClickException" if "Error:" in res.stderr else "SystemExit"
            msg = rust_error_message(res.stderr) or res.stderr.strip()
        else:
            cls, msg = type(exc).__name__, str(exc)
    return Run(res.exit_code, res.stdout_bytes, res.stderr, cls, msg or "")


def parse_upstream_traceback(err: str) -> tuple[str, str]:
    lines = [ln for ln in err.splitlines() if ln.strip()]
    for ln in reversed(lines):
        m = re.match(r"^([A-Za-z_][\w.]*(?:Error|Exception|Exit)?):? ?(.*)$", ln)
        if m and ("Error" in m.group(1) or "Exception" in m.group(1)):
            return m.group(1).split(".")[-1], m.group(2)
    for ln in lines:
        if ln.startswith("Error:"):
            return "ClickException", ln[6:].strip()
    return "SystemExit", lines[-1] if lines else ""


# ── comparison ───────────────────────────────────────────────────────────────

_WORD = re.compile(r"[A-Za-z_][A-Za-z0-9_]{2,}")
_STOP = {"error", "the", "for", "not", "with", "and", "row", "class", "slot", "from", "derivation", "value", "got"}


def msg_level(up: Run, rs: Run) -> str:
    """Grade error-message similarity: ``same`` / ``overlap`` (shared salient words) / ``differs``."""
    a, b = norm_msg(up.exc_msg), norm_msg(rs.exc_msg or rs.err)
    if a and a == b:
        return "same"
    wa = {w.lower() for w in _WORD.findall(a)} - _STOP
    wb = {w.lower() for w in _WORD.findall(b)} - _STOP
    return "overlap" if wa & wb else "differs"


def norm_msg(m: str) -> str:
    m = re.sub(r"/[^\s'\"]+/", "<path>/", m)
    return re.sub(r"\s+", " ", m).strip()


@dataclass
class FormatResult:
    fmt: str
    kind: str
    msg_level: str | None = None
    up: Run | None = None
    rs: Run | None = None

    @property
    def ok(self) -> bool:
        return self.kind in OK_KINDS


def classify(fmt: str, up: Run, rs: Run) -> FormatResult:
    if rs.exc_class in ("panic", "Timeout"):
        return FormatResult(fmt, "rust_panic", None, up, rs)
    if up.ok and rs.ok:
        return FormatResult(fmt, "match" if up.out == rs.out else "output_diff", None, up, rs)
    if not up.ok and not rs.ok:
        return FormatResult(fmt, "both_error", msg_level(up, rs), up, rs)
    return FormatResult(fmt, "upstream_error" if not up.ok else "rust_error", None, up, rs)


def compare_case(case: Case, upstream_mode: str = "inproc") -> list[FormatResult]:
    out = []
    for fmt in case.formats:
        up = run_upstream(case.map_data_args(fmt, spec_flag="-T"), upstream_mode)
        rs = run_rust(case.map_data_args(fmt, spec_flag="-T"))
        out.append(classify(fmt, up, rs))
    return out


def divergences(results: list[FormatResult]) -> list[FormatResult]:
    return [r for r in results if not r.ok]


def signature(results: list[FormatResult]) -> str:
    """Coarse divergence signature used to de-duplicate counterexamples during a campaign."""
    parts = []
    for r in divergences(results):
        cls = (r.up.exc_class if r.up and not r.up.ok else "") or ""
        rcls = (r.rs.exc_class if r.rs and not r.rs.ok else "") or ""
        parts.append(f"{r.fmt}:{r.kind}:{cls}:{rcls}")
    return "|".join(parts)


# ── corpus ───────────────────────────────────────────────────────────────────


def case_hash(case: Case) -> str:
    h = hashlib.sha1()
    for rel in (case.schema, case.spec, case.target_schema):
        if rel:
            h.update(Path(case.p(rel)).read_bytes())
    inp = Path(case.p(case.input))
    files = sorted(p for p in inp.rglob("*") if p.is_file()) if inp.is_dir() else [inp]
    for f in files:
        h.update(f.name.encode())
        h.update(f.read_bytes())
    h.update(json.dumps(case.to_json(), sort_keys=True).encode())
    return h.hexdigest()[:12]


def save_corpus_case(case: Case, results: list[FormatResult], corpus_dir: Path, *, note: str = "", category: str | None = None,
                     seed: str = "", allow_overwrite: bool = False) -> Path:
    """Persist ``case`` with upstream's expected output for every format.

    Layout: ``schema.yaml spec.yaml [target.yaml] data/ case.json expected/<fmt>.out|.error observed_rust/<fmt>.out|.error
    notes.md [expected_divergence.md]``.
    """
    dest = corpus_dir / case_hash(case)
    if dest.exists() and not allow_overwrite:
        return dest
    if dest.exists():
        shutil.rmtree(dest)
    (dest / "expected").mkdir(parents=True)
    root = Path(case.root)
    rel = {"schema": "schema.yaml", "spec": "spec.yaml", "target": "target.yaml"}
    shutil.copy(root / case.schema, dest / rel["schema"])
    shutil.copy(root / case.spec, dest / rel["spec"])
    if case.target_schema:
        shutil.copy(root / case.target_schema, dest / rel["target"])
    src_in = root / case.input
    if src_in.is_dir():
        shutil.copytree(src_in, dest / "data")
        new_input = "data"
    else:
        (dest / "data").mkdir()
        shutil.copy(src_in, dest / "data" / src_in.name)
        new_input = f"data/{src_in.name}"
    saved = Case(
        root=str(dest), schema=rel["schema"], spec=rel["spec"], input=new_input, source_type=case.source_type,
        target_schema=rel["target"] if case.target_schema else None, flags=case.flags, formats=case.formats,
    )
    (dest / "case.json").write_text(json.dumps(saved.to_json(), indent=2) + "\n")
    for r in results:
        write_run(dest / "expected", r.fmt, r.up)
        if not r.ok:
            write_run(dest / "observed_rust", r.fmt, r.rs)
    lines = [f"# Differential case {dest.name}", ""]
    if seed:
        lines.append(f"- found by: {seed}")
    for r in results:
        lines.append(f"- `{r.fmt}`: {r.kind}" + (f" (message match: {r.msg_level})" if r.msg_level else ""))
    if note:
        lines += ["", note]
    (dest / "notes.md").write_text("\n".join(lines) + "\n")
    if any(not r.ok for r in results):
        cat = category or "untriaged"
        (dest / "expected_divergence.md").write_text(
            f"category: {cat}\n\nStatus: known divergence from upstream (see notes.md and observed_rust/).\n"
            "Edit this file when triaging; delete it once the Rust output matches `expected/`.\n"
        )
    return dest


def write_run(d: Path, fmt: str, run: Run | None) -> None:
    if run is None:
        return
    d.mkdir(parents=True, exist_ok=True)
    if run.ok:
        (d / f"{fmt}.out").write_bytes(run.out)
    else:
        (d / f"{fmt}.error").write_text(
            json.dumps({"exit_code": run.rc, "class": run.exc_class, "message": run.exc_msg}, indent=2, ensure_ascii=False) + "\n"
        )


@contextlib.contextmanager
def tmp_case_dir():
    d = tempfile.mkdtemp(prefix="dfz-")
    try:
        yield Path(d)
    finally:
        shutil.rmtree(d, ignore_errors=True)


def require_upvenv() -> None:
    try:
        import linkml_map  # noqa: F401
    except ImportError:
        sys.exit("run under the upstream venv: /home/user/upstream/upvenv/bin/python")
