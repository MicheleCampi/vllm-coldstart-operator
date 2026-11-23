"""Requests lost around one graceful node shutdown (ADR-0028).
Usage: analyze.py MODE CLIENT_LOG T_SIGNAL T_TERMINAL TARGET_POD
The window runs from the PrepareForShutdown signal to 10 s after the target
pod is terminal. Valid only if the target pod answered requests in the 10 s
before the signal (traffic reached it).
The defect is that the target stays routed until SIGTERM; a lost request is
its consequence when one arrives at that instant, which is luck of timing
(the control lost 1 request in some runs and 0 in others). So both modes are
judged on routing, and the fixed one on losses too:
fixed: 0 failures, and the target answered nothing later than ROUTED_S after
the signal (it left the Service);
control (operator stopped, as v0.17.0 had no node watch): the target still
answered within LATE_S of its end, i.e. the job sees the defect."""
import json, sys

ROUTED_S = 10  # the node watch acts within seconds of the kubelet's report
LATE_S = 15    # routed until (almost) SIGTERM, 110 s into the drain

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
last = to_target_after[-1] if to_target_after else None
if mode == "fixed" and failed:
    sys.exit(f"FAIL: {len(failed)} requests lost with the operator's node watch")
if mode == "fixed" and last is not None and last > ROUTED_S:
    sys.exit(f"FAIL: the target still answered at +{last} s, it stayed in the Service")
if mode == "control" and (last is None or last < (t_end - t_sig) - LATE_S):
    sys.exit(f"INVALID: the control left the Service early (last answer {last}), so this job cannot see the defect")
print("PASS")
