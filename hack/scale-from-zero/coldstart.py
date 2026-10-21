"""Report the cold start of a scale-from-zero run and check what ADR-0015
asserts: the fleet starts with no replica, the requests that arrive while
it has none wait instead of being rejected, and every request is served
(the success objective of hack/loadgen/slo.json, 99.5%).

Reads what hack/scale-out/measure.sh writes: the loadgen records
(OUT.jsonl) and the replica samples (OUT.replicas, every 2 s: epoch
spec.replicas readyReplicas desiredReplicas). Times are seconds from the
first request sent; the first Ready replica is known to the sampler's 2 s,
the first token to the loadgen's millisecond.
"""
import collections
import json
import sys


def main(jsonl, replicas):
    with open(jsonl) as f:
        reqs = [r for r in map(json.loads, f) if r["type"] == "request"]
    with open(replicas) as f:
        samples = [line.split() for line in f if line.strip()]
    ok = [r for r in reqs if r.get("ok")]
    t0 = min(r["ts_start"] for r in reqs)
    first_token = min(r["ts_first_token"] for r in ok)
    ready = [float(s[0]) for s in samples if int(s[2]) >= 1]
    held = [r for r in reqs if r["ts_start"] < first_token]
    ttft = sorted(r["ttft_ms"] for r in held if r.get("ok"))
    statuses = collections.Counter(r.get("status") for r in reqs)
    print(f"requests {len(reqs)}, served {len(ok)}, by status {dict(statuses)}")
    print(f"first sample: spec.replicas {samples[0][1]}, ready {samples[0][2]}")
    if ready:
        print(f"first replica Ready at {ready[0] - t0:.0f} s (2 s samples)")
    print(f"first token at {first_token - t0:.3f} s: the cold start a user sees")
    print(f"{len(held)} requests sent before it, all waiting together just before it;")
    print(f"  their TTFT: min {ttft[0]:.0f} ms, max {ttft[-1]:.0f} ms")

    errors = []
    if not samples[0][1:3] == ["0", "0"]:
        errors.append("the fleet did not start from zero replicas")
    if not ready:
        errors.append("no fleet replica became Ready")
    if statuses.get(503):
        errors.append(f"{statuses[503]} requests rejected with 503")
    if statuses.get(504):
        errors.append(f"{statuses[504]} requests held past readiness, 504")
    if len(ok) < 0.995 * len(reqs):
        errors.append(f"{len(ok)} of {len(reqs)} requests served, want 99.5%")
    for e in errors:
        print(f"::error::{e}")
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main(*sys.argv[1:]))
