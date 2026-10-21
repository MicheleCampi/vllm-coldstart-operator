"""Report how long a scale-from-zero fleet stays up after its load ends,
and check that KEDA takes it back to zero within the bounds ADR-0015
derives from KEDA's code: not before the cooldown, and not after the
Prometheus trigger's one-minute average, two polling intervals and the
cooldown, with a margin for scrape, query and reconcile delays.

Reads the loadgen records (OUT.jsonl) and the samples the job takes once
the load is over (OUT.idle, every 5 s: epoch spec.replicas readyReplicas
pods). Times are seconds from the end of the last response.
"""
import json
import sys

# deploy/examples/keda/scale-from-zero.yaml
COOLDOWN = 300  # cooldownPeriod
POLL = 30  # pollingInterval
WINDOW = 60  # the Prometheus trigger's avg_over_time range
MARGIN = 60  # scrape, query and reconcile delays: a margin, not derived


def main(jsonl, idle):
    with open(jsonl) as f:
        reqs = [r for r in map(json.loads, f) if r["type"] == "request"]
    end = max(r["ts_start"] + r["latency_ms"] / 1000 for r in reqs if r.get("ok"))
    with open(idle) as f:
        samples = [[float(x) for x in line.split()] for line in f if line.strip()]
    zero = next((s for s in samples if s[1] == 0), None)
    gone = next((s for s in samples if s[1] == 0 and s[3] == 0), None)
    # Replica-seconds from the last response to spec.replicas 0, each
    # sample's count held until the next sample.
    up = samples[0][1] * (samples[0][0] - end)
    steps = [(samples[0][0] - end, samples[0][1])]
    for prev, s in zip(samples, samples[1:]):
        if prev[1] == 0:
            break
        up += prev[1] * (s[0] - prev[0])
        if s[1] == prev[1]:
            continue
        steps.append((s[0] - end, s[1]))
    print("spec.replicas after the last response: " + ", ".join(f"{n:.0f} at {t:.0f} s" for t, n in steps))
    if zero:
        print(f"spec.replicas 0 at {zero[0] - end:.0f} s (5 s samples)")
    if gone:
        print(f"no fleet pod left at {gone[0] - end:.0f} s")
    print(f"{up:.0f} replica-seconds up with no request to serve")

    lo, hi = COOLDOWN, WINDOW + 2 * POLL + COOLDOWN + MARGIN
    errors = []
    if zero is None:
        errors.append("the fleet did not return to zero replicas")
    elif not lo <= zero[0] - end <= hi:
        errors.append(f"spec.replicas 0 at {zero[0] - end:.0f} s after the last response, want {lo} to {hi} s")
    if gone is None:
        errors.append("fleet pods were still running at the last sample")
    for e in errors:
        print(f"::error::{e}")
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main(*sys.argv[1:]))
