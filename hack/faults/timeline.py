"""Summarise one fault run: requests against the fault time, readiness.

Usage: timeline.py RECORDS.jsonl FAULT READY
RECORDS is loadgen's JSON lines; FAULT one line "<epoch> <fault> <node>";
READY lines "<epoch> <readyReplicas>". All three use the same host clock.
"""
import json
import sys


def q(values, p):
    if not values:
        return None
    s = sorted(values)
    return s[min(len(s) - 1, int(p * len(s)))]


def main():
    recs = [json.loads(l) for l in open(sys.argv[1]).read().splitlines()[1:]]
    t_fault, fault, node = open(sys.argv[2]).read().split()
    t_fault = float(t_fault)
    ready = []
    for line in open(sys.argv[3]):
        t, _, n = line.strip().partition(" ")
        ready.append((float(t), int(n) if n.isdigit() else None))
    print(f"fault {fault} on {node}, {len(recs)} requests")
    bad = [r for r in recs if not (r.get("ok") and r.get("status") == 200)]
    for r in bad:
        print(f"failed: trace line {r['trace_line']} started {r['ts_start'] - t_fault:+.1f} s from the fault, "
              f"ended {r['ts_end'] - t_fault:+.1f} s, status {r.get('status')}, {r.get('error')}")
    print(f"failed requests: {len(bad)}")
    before = [r["ttft_ms"] for r in recs if r not in bad and r["ts_start"] < t_fault]
    after = [r["ttft_ms"] for r in recs if r not in bad and r["ts_start"] >= t_fault]
    print(f"ttft p95 ms: before the fault {q(before, 0.95)} ({len(before)} requests), "
          f"after {q(after, 0.95)} ({len(after)} requests)")
    if len(sys.argv) > 4:
        probe = [json.loads(l) for l in open(sys.argv[4]).read().splitlines()[1:]]
        open_at = [r for r in probe if r["ts_start"] <= t_fault <= r["ts_end"]]
        cut = [r for r in probe if not (r.get("ok") and r.get("status") == 200)]
        print(f"probe: {len(probe)} streams, {len(open_at)} open at the fault, {len(cut)} failed")
        for r in cut:
            print(f"probe failed: started {r['ts_start'] - t_fault:+.1f} s, ended "
                  f"{r['ts_end'] - t_fault:+.1f} s, status {r.get('status')}, {r.get('error')}")
        if not open_at:
            print("ERROR: no probe stream was open at the fault, so the run did not expose it")
            sys.exit(1)
        if cut:
            # ADR-0019: the pre-stop sleep lets a stream in flight finish;
            # a probe that cannot connect fails the run too.
            print(f"ERROR: {len(cut)} probe streams failed across the fault")
            sys.exit(1)
    post = [(t, n) for t, n in ready if t >= t_fault]
    low = [n for _, n in post if n is not None]
    print(f"readyReplicas after the fault: minimum {min(low) if low else None}")
    dropped = next((t for t, n in post if n is not None and n < 2), None)
    if dropped is None:
        print("readyReplicas never dropped below 2")
        return
    back = next((t for t, n in post if t > dropped and n == 2), None)
    print(f"below 2 at {dropped - t_fault:+.1f} s, back to 2 at "
          + (f"{back - t_fault:+.1f} s" if back else "never, within the record"))


if __name__ == "__main__":
    main()
