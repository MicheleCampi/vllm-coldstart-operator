#!/usr/bin/env python3
"""Analyze one item-4 run directory (rehearsal on kind or GPU session).

Inputs (produced by run.sh): load.jsonl, markers.json, operator.log,
events.txt. Outputs: summary.json and timeline.png in the same directory,
plus a human-readable table on stdout.

Metrics:
- per target, per window (before / during / after the preemption notice):
  request count, errors, throughput, p50/p99 latency;
- T_decision: operator log line for the reschedule decision minus t_notice;
- T_scheduled / T_kill: k8s events for the moved child (surge pod scheduled,
  old pod killed) relative to t_notice — the make-before-break bracket;
- max success-gap on the moved target within 60s after the notice: the
  longest wall-clock stretch with no successful completion, i.e. the
  worst-case continuity hole a client observed.
"""

import json
import re
import sys
from datetime import datetime, timezone
from pathlib import Path

import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt

DURING_S = 30.0  # fixed during-window for cross-run comparability
ANSI = re.compile(r"\x1b\[[0-9;]*m")  # tracing colors survive file redirect
BUCKET_S = 5.0


def iso_to_epoch(ts: str) -> float:
    return datetime.fromisoformat(ts.replace("Z", "+00:00")).timestamp()


def pctl(sorted_vals, q):
    if not sorted_vals:
        return None
    return sorted_vals[min(int(len(sorted_vals) * q), len(sorted_vals) - 1)]


def main(run_dir: str) -> None:
    run = Path(run_dir)
    markers = json.load(open(run / "markers.json"))
    t0 = markers["t_notice"]
    lines = [json.loads(l) for l in open(run / "load.jsonl")]
    config, recs = lines[0], lines[1:]

    def window(r):
        dt = r["ts_start"] - t0
        if dt < 0:
            return "before"
        return "during" if dt < DURING_S else "after"

    targets = sorted({r["target"] for r in recs})
    summary = {"run_dir": str(run), "t_notice": t0, "config": config,
               "during_window_s": DURING_S, "targets": {}}

    print(f"{'target':7s} {'window':7s} {'n':>5s} {'ok':>5s} {'err':>4s} "
          f"{'req/s':>6s} {'p50ms':>8s} {'p99ms':>8s}")
    for tgt in targets:
        summary["targets"][tgt] = {}
        for w in ("before", "during", "after"):
            rs = [r for r in recs if r["target"] == tgt and window(r) == w]
            ok = [r for r in rs if r.get("ok")]
            lat = sorted(r["latency_ms"] for r in ok)
            run_end = max(r["ts_end"] for r in recs)
            span = {"before": t0 - config["ts_start"],
                    "during": DURING_S,
                    "after": max(run_end - (t0 + DURING_S), 0)}[w]
            stats = {
                "n": len(rs), "ok": len(ok), "err": len(rs) - len(ok),
                "rps": round(len(rs) / span, 2) if span > 0 else None,
                "p50_ms": pctl(lat, 0.50), "p99_ms": pctl(lat, 0.99),
            }
            summary["targets"][tgt][w] = stats
            print(f"{tgt:7s} {w:7s} {stats['n']:5d} {stats['ok']:5d} "
                  f"{stats['err']:4d} {str(stats['rps']):>6s} "
                  f"{str(stats['p50_ms']):>8s} {str(stats['p99_ms']):>8s}")

    # T_decision from the operator log (finding-9 line).
    dec = re.compile(
        r"^(\S+)\s+INFO.*rescheduling slot (\d+) to '([^']+)'")
    t_decision = moved_child = target_node = None
    fleet_re = re.compile(r"FleetService '([^']+)':")
    for line in open(run / "operator.log", errors="replace"):
        line = ANSI.sub("", line)
        m = dec.search(line)
        if m and iso_to_epoch(m.group(1)) >= t0:
            t_decision = iso_to_epoch(m.group(1)) - t0
            fm = fleet_re.search(line)
            moved_child = f"{fm.group(1)}-{m.group(2)}" if fm else None
            target_node = m.group(3)
            break
    summary["t_decision_s"] = round(t_decision, 3) if t_decision is not None else None
    summary["moved_child"] = moved_child
    summary["replacement_node"] = target_node

    # Make-before-break bracket from k8s events (1s resolution).
    t_sched = t_kill = None
    if moved_child:
        for line in open(run / "events.txt", errors="replace"):
            parts = line.split(None, 5)
            if len(parts) < 5 or moved_child not in parts[3]:
                continue
            # scheduler events have lastTimestamp <nil>; fall back to the
            # eventTime column (present when events.txt carries ETIME).
            ts = None
            for cand in (parts[0], parts[4] if len(parts) > 5 else ""):
                try:
                    ts = iso_to_epoch(cand)
                    break
                except ValueError:
                    continue
            if ts is None:
                continue
            if ts < t0 - 1:
                continue
            if parts[2] == "Scheduled" and t_sched is None:
                t_sched = ts - t0
            if parts[2] == "Killing" and t_kill is None:
                t_kill = ts - t0
    # k8s event timestamps are truncated to whole seconds; values can land up
    # to 1s before the sub-second t_notice. Causality is preserved by
    # T_decision (operator log, microsecond resolution).
    summary["event_ts_resolution_s"] = 1
    summary["t_surge_scheduled_s"] = round(t_sched, 1) if t_sched is not None else None
    summary["t_old_pod_kill_s"] = round(t_kill, 1) if t_kill is not None else None

    # Max success-gap on the moved target within 60s of the notice.
    moved_target = None
    if moved_child and config.get("targets"):
        # convention: run.sh maps target 'b' to the preempted node's service
        moved_target = "b" if "b" in config["targets"] else targets[-1]
    if moved_target:
        ends = sorted(r["ts_end"] for r in recs
                      if r["target"] == moved_target and r.get("ok")
                      and t0 - 5 <= r["ts_end"] <= t0 + 60)
        gaps = [(ends[i + 1] - ends[i], ends[i] - t0) for i in range(len(ends) - 1)]
        if gaps:
            g, at = max(gaps)
            summary["max_success_gap_s"] = {"gap_s": round(g, 2),
                                            "starts_at_T_plus_s": round(at, 1),
                                            "target": moved_target}

    # Timeline plot: throughput and p50 per bucket, decision markers.
    fig, (ax1, ax2) = plt.subplots(2, 1, figsize=(11, 7), sharex=True)
    for tgt in targets:
        rs = sorted((r for r in recs if r["target"] == tgt), key=lambda r: r["ts_end"])
        buckets = {}
        for r in rs:
            b = (r["ts_end"] - t0) // BUCKET_S * BUCKET_S
            buckets.setdefault(b, []).append(r)
        xs = sorted(buckets)
        ax1.plot(xs, [len([r for r in buckets[x] if r.get("ok")]) / BUCKET_S for x in xs],
                 label=f"{tgt} ok req/s", marker=".")
        ax1.plot(xs, [len([r for r in buckets[x] if not r.get("ok")]) / BUCKET_S for x in xs],
                 label=f"{tgt} err/s", linestyle=":", marker="x")
        ax2.plot(xs, [
            (lambda v: pctl(v, 0.5))(sorted(r["latency_ms"] for r in buckets[x] if r.get("ok")))
            for x in xs], label=f"{tgt} p50 ms", marker=".")
    for ax in (ax1, ax2):
        ax.axvline(0, color="red", linestyle="--", label="notice")
        if t_decision is not None:
            ax.axvline(t_decision, color="orange", linestyle="--", label="decision")
        if t_sched is not None:
            ax.axvline(t_sched, color="green", linestyle="--", label="surge scheduled")
        if t_kill is not None:
            ax.axvline(t_kill, color="purple", linestyle="--", label="old pod kill")
        ax.legend(fontsize=8)
        ax.grid(alpha=0.3)
    ax1.set_ylabel("req/s (5s buckets)")
    ax2.set_ylabel("p50 latency ms")
    ax2.set_xlabel("seconds relative to preemption notice")
    fig.suptitle(f"fleet preemption run: {run.name}")
    fig.tight_layout()
    fig.savefig(run / "timeline.png", dpi=120)

    json.dump(summary, open(run / "summary.json", "w"), indent=2)
    print(f"\nT_decision={summary['t_decision_s']}s  "
          f"surge_scheduled=T+{summary['t_surge_scheduled_s']}s  "
          f"old_pod_kill=T+{summary['t_old_pod_kill_s']}s")
    if "max_success_gap_s" in summary:
        print(f"max success-gap: {summary['max_success_gap_s']}")
    print(f"written: {run}/summary.json, {run}/timeline.png")


if __name__ == "__main__":
    main(sys.argv[1])
