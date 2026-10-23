"""Tests for slo_check.py, standard library only.

Run: python3 -m unittest discover -s hack/loadgen -p 'test_*.py'
"""
import json
import pathlib
import subprocess
import sys
import tempfile
import unittest

HERE = pathlib.Path(__file__).resolve().parent
SLO = HERE / "slo.json"


def record(line, ok=True, offset=0.0):
    r = {"type": "request", "target": "sim", "trace_line": line,
         "context_tokens": 10, "generated_tokens": 5,
         "ts_scheduled": 1000.0 + offset, "ts_start": 1000.0 + offset,
         "send_delay_ms": 1.0, "ts_end": 1000.5 + offset, "latency_ms": 500.0}
    if ok:
        r.update(ok=True, status=200, prompt_tokens=10, completion_tokens=5,
                 ts_first_token=1000.1 + offset, ttft_ms=100.0, last_token_ms=200.0)
    else:
        r.update(ok=False, status=0, error="ClientConnectorError: connection reset")
    return r


def run(recs, *args):
    with tempfile.TemporaryDirectory() as d:
        path = pathlib.Path(d) / "r.jsonl"
        head = {"type": "run", "sequence_sha256": "abc", "requests_per_target": len(recs)}
        path.write_text("\n".join(json.dumps(x) for x in [head] + recs) + "\n")
        return subprocess.run([sys.executable, "-I", str(HERE / "slo_check.py"), str(path), *args,
                               "--slo", str(SLO), "--sequence", "abc"],
                              capture_output=True, text=True)


class ClientOnly(unittest.TestCase):
    def test_client_only_passes_without_simulator_metrics(self):
        p = run([record(1), record(2, offset=1.0)], "--client-only", "--assert-slo")
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)
        self.assertIn("client only", p.stdout)

    def test_client_only_still_asserts_the_objectives(self):
        p = run([record(1), record(2, ok=False, offset=1.0)], "--client-only", "--assert-slo")
        self.assertEqual(p.returncode, 1, p.stdout + p.stderr)
        self.assertIn("objective success missed", p.stdout)

    def test_metrics_are_required_without_client_only(self):
        p = run([record(1), record(2, offset=1.0)], "--assert-slo")
        self.assertEqual(p.returncode, 2, p.stdout + p.stderr)
        self.assertIn("required without --client-only", p.stderr)


if __name__ == "__main__":
    unittest.main()
