"""Requests lost around one graceful node shutdown (ADR-0028).
Usage: analyze.py MODE CLIENT_LOG T_SIGNAL T_TERMINAL TARGET_POD
The window runs from the PrepareForShutdown signal to 10 s after the target
pod is terminal. Valid only if the target pod answered requests in the 10 s
before the signal (traffic reached it). fixed: exit 0 only with 0 failures.
control (pod spec of v0.17.0): exit 0 only with at least one failure, i.e.
the job sees the defect it guards against."""
import json, sys

mode, log, t_sig, t_end, target = sys.argv[1], sys.argv[2], float(sys.argv[3]), float(sys.argv[4]), sys.argv[5]
rows = [json.loads(l) for l in open(log) if l.startswith("{")]
before = [r for r in rows if t_sig - 10 <= r["t"] < t_sig]
window = [r for r in rows if t_sig <= r["t"] <= t_end + 10]
failed = [r for r in window if r["code"] != 200]
to_target_before = sum(1 for r in before if r["pod"] == target)
to_target_after = [round(r["t"] - t_sig, 1) for r in window if r["pod"] == target and r["code"] == 200]
print(json.dumps({
    "mode": mode, "target_pod": target,
    "terminal_after_s": round(t_end - t_sig, 1),
    "requests_in_window": len(window), "failed": len(failed),
    "failed_at_s": [round(r["t"] - t_sig, 1) for r in failed][:20],
    "answered_by_target_10s_before": to_target_before,
    "last_answer_by_target_s": to_target_after[-1] if to_target_after else None,
}, indent=1))
if to_target_before == 0:
    sys.exit("INVALID: no request reached the target pod before the signal")
if mode == "fixed" and failed:
    sys.exit(f"FAIL: {len(failed)} requests lost with the drain flag")
if mode == "control" and not failed:
    sys.exit("INVALID: the control lost nothing, so this job cannot see the defect")
print("PASS")
