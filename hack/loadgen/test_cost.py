"""Tests for cost.py, standard library only.

Run: python3 -m unittest discover -s hack/loadgen -p 'test_*.py'
"""
import json
import pathlib
import subprocess
import sys
import tempfile
import unittest

HERE = pathlib.Path(__file__).resolve().parent


def record(start, end, ok=True, gen=5):
    r = {"type": "request", "context_tokens": 10, "generated_tokens": gen, "ts_start": start, "ts_end": end}
    if ok:
        r.update(ok=True, status=200, prompt_tokens=10, completion_tokens=gen)
    else:
        r.update(ok=False, status=0)
    return r


def run(recs, *args, head=None, rate=("--eur-per-hour", "3.6", "--rate-source", "test")):
    head = head or {"type": "run_config", "ts_start": 1000.0}
    with tempfile.TemporaryDirectory() as d:
        path = pathlib.Path(d) / "r.jsonl"
        path.write_text("\n".join(json.dumps(x) for x in [head] + recs) + "\n")
        return subprocess.run([sys.executable, "-I", str(HERE / "cost.py"), str(path), *rate, *args],
                              capture_output=True, text=True)


class Cost(unittest.TestCase):
    def test_overlapping_requests_count_once(self):
        # 3.6 EUR/h is 0.001 EUR/s: the window 1000-1010 costs 0.01 EUR.
        out = run([record(1000.0, 1006.0), record(1004.0, 1010.0)]).stdout
        self.assertIn("window 10.0 s, of which 0.0 s (0%)", out)
        self.assertIn("cost of the window 0.0100 EUR", out)

    def test_a_gap_is_idle_and_paid(self):
        out = run([record(1000.0, 1002.0), record(1008.0, 1010.0)]).stdout
        self.assertIn("of which 6.0 s (60%) with no request in flight", out)
        self.assertIn("cost of the window 0.0100 EUR, of which 0.0060", out)

    def test_a_failed_request_takes_time_not_tokens(self):
        out = run([record(1000.0, 1005.0), record(1005.0, 1010.0, ok=False)]).stdout
        self.assertIn("served 1 of 2 requests: 10 prompt tokens, 5 generated", out)
        self.assertIn("EUR per million generated tokens: 2000.00", out)

    def test_no_generated_token_has_no_cost_per_token(self):
        out = run([record(1000.0, 1010.0, ok=False)]).stdout
        self.assertIn("EUR per million generated tokens: none", out)

    def test_the_rate_is_required(self):
        p = run([record(1000.0, 1010.0)], rate=())
        self.assertEqual(p.returncode, 2)
        self.assertIn("--eur-per-hour", p.stderr)

    def test_the_header_must_be_a_run_config(self):
        p = run([record(1000.0, 1010.0)], head={"type": "run", "ts_start": 1000.0})
        self.assertNotEqual(p.returncode, 0)
        self.assertIn("not a run_config", p.stderr)


if __name__ == "__main__":
    unittest.main()
