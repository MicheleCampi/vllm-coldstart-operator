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
SLO_L4 = HERE / "slo-l4-qwen7b-bf16.json"


def record(line, ok=True, offset=0.0, ctx=10, ttft=100.0):
    r = {"type": "request", "target": "sim", "trace_line": line,
         "context_tokens": ctx, "generated_tokens": 5,
         "ts_scheduled": 1000.0 + offset, "ts_start": 1000.0 + offset,
         "send_delay_ms": 1.0, "ts_end": 1000.5 + offset, "latency_ms": 500.0}
    if ok:
        r.update(ok=True, status=200, prompt_tokens=ctx, completion_tokens=5,
                 ts_first_token=1000.1 + offset, ttft_ms=ttft, last_token_ms=ttft + 100.0)
    else:
        r.update(ok=False, status=0, error="ClientConnectorError: connection reset")
    return r


def run(recs, *args, slo=SLO):
    with tempfile.TemporaryDirectory() as d:
        path = pathlib.Path(d) / "r.jsonl"
        head = {"type": "run", "sequence_sha256": "abc", "requests_per_target": len(recs)}
        path.write_text("\n".join(json.dumps(x) for x in [head] + recs) + "\n")
        return subprocess.run([sys.executable, "-I", str(HERE / "slo_check.py"), str(path), *args,
                               "--slo", str(slo), "--sequence", "abc"],
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


class TtftPerPromptLength(unittest.TestCase):
    """ADR-0022: above ttft_long_above_tokens the first token may take ttft_long_ms."""

    def judge(self, ctx, ttft, slo=SLO_L4):
        return run([record(1, ctx=ctx, ttft=ttft)], "--client-only", "--assert-slo", slo=slo)

    def test_a_long_prompt_meets_its_own_threshold(self):
        p = self.judge(3000, 1500.0)
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)

    def test_a_long_prompt_over_its_threshold_misses(self):
        p = self.judge(3000, 2100.0)
        self.assertEqual(p.returncode, 1, p.stdout + p.stderr)
        self.assertIn("objective ttft missed", p.stdout)

    def test_the_boundary_takes_the_short_threshold(self):
        p = self.judge(2048, 1500.0)
        self.assertEqual(p.returncode, 1, p.stdout + p.stderr)
        self.assertIn("objective ttft missed", p.stdout)

    def test_without_the_keys_one_threshold_holds(self):
        p = self.judge(3000, 1500.0, slo=SLO)
        self.assertEqual(p.returncode, 1, p.stdout + p.stderr)
        self.assertIn("objective ttft missed", p.stdout)


if __name__ == "__main__":
    unittest.main()
