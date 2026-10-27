"""Run batches of JSONL cases through the Rust oracle (``expr_oracle`` example).

Each batch goes through a file pair so a crash (stack overflow, abort) or hang
loses nothing: the oracle flushes after every line, so the first missing
response names the culprit, which is recorded as a ``crash``/``hang`` error and
skipped before the rest of the batch is rerun.
"""

from __future__ import annotations

import json
import os
import subprocess
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[3]


def oracle_binary() -> Path:
    env = os.environ.get("EXPR_ORACLE")
    if env:
        return Path(env)
    target = os.environ.get("CARGO_TARGET_DIR", str(REPO / "target"))
    for profile in ("release", "debug"):
        p = Path(target) / profile / "examples" / "expr_oracle"
        if p.exists():
            return p
    raise FileNotFoundError("build it: cargo build -p linkml-map-conformance --example expr_oracle --release")


class Oracle:
    def __init__(self, binary: Path | None = None, hang_timeout: float = 60.0) -> None:
        self.binary = binary or oracle_binary()
        self.hang_timeout = hang_timeout

    def _run_file(self, lines: list[str]) -> tuple[list[dict], str | None]:
        """Returns (responses so far, failure description or None)."""
        with tempfile.TemporaryDirectory() as d:
            inp, out = Path(d, "in.jsonl"), Path(d, "out.jsonl")
            inp.write_text("\n".join(lines) + "\n", encoding="utf-8")
            failure = None
            try:
                r = subprocess.run(
                    [str(self.binary), str(inp), str(out)],
                    capture_output=True,
                    timeout=self.hang_timeout,
                    check=False,
                )
                if r.returncode != 0:
                    failure = f"exit {r.returncode}: {r.stderr.decode(errors='replace')[-300:].strip()}"
            except subprocess.TimeoutExpired:
                failure = "hang"
            resp = []
            if out.exists():
                for ln in out.read_text(encoding="utf-8").split("\n"):
                    if ln.strip():
                        resp.append(json.loads(ln))
            return resp, failure

    def run(self, cases: list[dict]) -> list[dict]:
        """Responses aligned with ``cases`` (each gets an ``id`` = its index)."""
        results: list[dict | None] = [None] * len(cases)
        pending = list(range(len(cases)))
        while pending:
            lines = [json.dumps({**cases[i], "id": i}, ensure_ascii=False) for i in pending]
            resp, failure = self._run_file(lines)
            for r in resp:
                results[r["id"]] = r
            done = {r["id"] for r in resp}
            rest = [i for i in pending if i not in done]
            if not rest:
                break
            if failure is None and resp and resp[-1].get("err") == "hang":
                # the oracle's watchdog answered the stuck case and exited cleanly: rerun the rest
                pending = rest
                continue
            if failure is None:
                failure = "missing response"
            culprit = rest[0]
            kind = "hang" if failure == "hang" else "crash"
            results[culprit] = {"id": culprit, "err": kind, "message": failure}
            pending = rest[1:]
        return results  # type: ignore[return-value]
