"""Instrumentation-only autoscaling consumer for the ADR-0009 D5 experiment.

Not an autoscaler. It exists because D1 decided the operator publishes the
scale subresource and does not own the scaling loop, so the loop has to
live in hack/ to be measured at all. Its capacity model is a constant and
its demand is read from the recorded trace, not from the cluster.

The two arms are one file with one branch (`available_capacity`) so that
"identical except one line of arithmetic" is a property of the source
rather than a claim about two files that drifted:

    arm N: available = readyReplicas
    arm W: available = readyReplicas + warmingReplicas

Everything else — tick, read path, write path, clamping, logging — is
shared code executed by both arms.

Scaling rule, incremental on purpose:

    desired = spec.replicas + max(0, needed - available)

The increment is what the experiment is about. A consumer writing
`desired = needed` outright would be insensitive to warmingReplicas by
construction and could only ever measure zero: the naive arm over-requests
precisely because capacity already on its way is invisible to it and it
asks again on the next tick.

Two consequences, both design inputs frozen with the rest:

- MAX_REPLICAS is a fuse against a runaway loop, deliberately set above
  anything the experiment can reach, and is NOT a design parameter of the
  scaling rule. An earlier value of 12 was binding: simulated against the
  rep-1 trace the naive arm peaked at exactly 12 and stayed there for 440s,
  so its primary metric would have reported the cap rather than a
  measurement, identically across every rep. Uncapped, the naive arm peaks
  at 28 on that trace. The fuse is 40. Platform headroom was measured, not
  assumed: the simulator holds 41.8MB RSS idle and 42.7MB under 30
  concurrent requests, so 28 replicas cost about 1.2GB against 5.9GB free
  on the host, and kind allows 110 pods per node.
- The rule never scales down, so the D5 scale-down assertion would never be
  exercised. `--final-scale-down` writes a lower value once the run ends and
  keeps sampling, identically in both arms.

Demand is open-loop: arrivals per second over a trailing window of the same
trace replay.py is replaying. Reading queue depth from the pods instead
would make the observed demand differ between arms — the arm holding fewer
replicas would see a different queue — and the within-subject comparison
depends on both arms seeing the same input series.

Reads the CR rather than the Scale object because `autoscaling/v1` Scale has
no field carrying warmingReplicas; writes through `kubectl scale` because
that is the interface D1 committed to. Ticks before the operator's first
status write are skipped: `status.replicas` defaults to 0, which is
indistinguishable from "zero children" and would have both arms request
capacity against an empty picture.

stdlib only, matching the ADR-0007 harness.
"""

import argparse
import bisect
import json
import subprocess
import sys
import time

TICK_S = 5.0
PER_REPLICA_RPS = 2.0
MAX_REPLICAS = 40
DEMAND_WINDOW_S = 10.0


def kubectl(args, timeout=30):
    return subprocess.run(
        ["kubectl", *args], capture_output=True, text=True, timeout=timeout
    )


def read_cr(namespace, name):
    """Full CR. Returns None if the operator has not written status yet."""
    r = kubectl(["get", "fleetservice", name, "-n", namespace, "-o", "json"])
    if r.returncode != 0:
        return {"error": r.stderr.strip()[:200]}
    obj = json.loads(r.stdout)
    status = obj.get("status")
    # `phase` has no serde default, so its presence is what distinguishes a
    # written status from the schema defaults.
    if not status or "phase" not in status:
        return None
    return obj


def available_capacity(arm, ready, warming):
    """The one line that differs between the arms."""
    return ready if arm == "N" else ready + warming


def demand_rps(arrivals, t_now):
    """Arrivals per second over the trailing window ending at t_now."""
    lo = t_now - DEMAND_WINDOW_S
    n = bisect.bisect_right(arrivals, t_now) - bisect.bisect_right(arrivals, lo)
    # Before a full window has elapsed the divisor is the elapsed time, so
    # the opening ticks are not biased toward zero demand.
    span = min(DEMAND_WINDOW_S, max(t_now, TICK_S))
    return n / span


def sample(arm, namespace, name, arrivals, t_now, act):
    """One observation, and the write it implies. Shared by both arms."""
    obj = read_cr(namespace, name)
    row = {"t": round(t_now, 2), "arm": arm}

    if obj is None:
        row["skipped"] = "status not written yet"
        return row
    if "error" in obj:
        row["skipped"] = obj["error"]
        return row

    status = obj["status"]
    spec_replicas = obj["spec"]["replicas"]
    ready = status.get("readyReplicas", 0)
    warming = status.get("warmingReplicas", 0)
    placements = status.get("placements", [])

    demand = demand_rps(arrivals, t_now)
    needed = -(-demand // PER_REPLICA_RPS)  # ceil, integer arithmetic
    available = available_capacity(arm, ready, warming)
    desired = min(MAX_REPLICAS, spec_replicas + max(0, int(needed) - available))

    row.update(
        {
            "demand_rps": round(demand, 3),
            "needed": int(needed),
            "ready_replicas": ready,
            "warming_replicas": warming,
            "available": available,
            "status_replicas": status.get("replicas", 0),
            "spec_replicas": spec_replicas,
            "desired": desired,
            "phase": status.get("phase"),
            "placement_phases": [p.get("phase") for p in placements],
            "surplus_reconciles": [p.get("surplusReconciles", 0) for p in placements],
            "wrote": False,
        }
    )

    if act and desired != spec_replicas:
        r = kubectl(
            ["scale", f"fleetservice/{name}", "-n", namespace, f"--replicas={desired}"]
        )
        row["wrote"] = r.returncode == 0
        if r.returncode != 0:
            row["write_error"] = r.stderr.strip()[:200]

    return row


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--arm", choices=["N", "W"], required=True)
    ap.add_argument("--namespace", default="default")
    ap.add_argument("--name", required=True)
    ap.add_argument("--workload", required=True, help="JSONL trace, for demand")
    ap.add_argument("--duration-s", type=float, required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument(
        "--start-epoch",
        type=float,
        default=None,
        help="shared t0 with replay.py; defaults to now",
    )
    ap.add_argument(
        "--final-scale-down",
        type=int,
        default=None,
        help="replicas to write once the run ends, exercising the D5 "
        "scale-down assertion",
    )
    ap.add_argument("--post-s", type=float, default=60.0)
    args = ap.parse_args()

    arrivals = sorted(json.loads(l)["t_offset_s"] for l in open(args.workload))
    t0 = args.start_epoch if args.start_epoch is not None else time.time()

    with open(args.out, "w") as f:
        while True:
            t_now = time.time() - t0
            if t_now >= args.duration_s:
                break
            if t_now >= 0:
                row = sample(args.arm, args.namespace, args.name, arrivals, t_now, True)
                f.write(json.dumps(row) + "\n")
                f.flush()
            time.sleep(TICK_S)

        if args.final_scale_down is not None:
            r = kubectl(
                [
                    "scale",
                    f"fleetservice/{args.name}",
                    "-n",
                    args.namespace,
                    f"--replicas={args.final_scale_down}",
                ]
            )
            f.write(
                json.dumps(
                    {
                        "t": round(time.time() - t0, 2),
                        "arm": args.arm,
                        "event": "final_scale_down",
                        "replicas": args.final_scale_down,
                        "ok": r.returncode == 0,
                    }
                )
                + "\n"
            )
            f.flush()

            # Observe only: the write above is the last one, so the traces
            # of the two arms stay comparable through the drain.
            end = time.time() + args.post_s
            while time.time() < end:
                row = sample(
                    args.arm,
                    args.namespace,
                    args.name,
                    arrivals,
                    time.time() - t0,
                    False,
                )
                row["post_drain"] = True
                f.write(json.dumps(row) + "\n")
                f.flush()
                time.sleep(TICK_S)


if __name__ == "__main__":
    sys.exit(main())
