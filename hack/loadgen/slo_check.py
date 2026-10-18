"""Check one loadgen run against the service level objectives in slo.json.

A run always fails on a replayed sequence other than the pinned one, a
missing record, a request not served with 200 and the profile's token
counts, a generator that fell behind its schedule, or client means that
disagree with the simulator's measured means beyond the declared tolerance.
With --assert-slo it also fails when an objective is missed; without it the
objectives are only reported, for a run that overloads one replica on purpose.

The simulator computes, rather than measures, its time to first token and
time per output token, so those are not compared. It does measure queue,
prefill and end-to-end time: the client's mean TTFT must match mean queue
plus prefill, and its mean end-to-end latency the simulator's (ADR-0013).
"""
import argparse
import json
import re
import sys


def scrape(path):
    text = open(path).read()
    return {k: float(v) for k, v in re.findall(r"^vllm:(\w+)\{[^}]*\} (\S+)$", text, re.M)}


def delta(before, after, name):
    # A labelled histogram appears only after its first observation: a run
    # in which no request queued has no queue-time series at all.
    return after.get(name, 0.0) - before.get(name, 0.0)


def q(values, p):
    s = sorted(values)
    return s[min(len(s) - 1, int(p * len(s)))] if s else None


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("records", help="loadgen JSON lines")
    ap.add_argument("metrics_before", help="the simulator's /metrics scraped before the run")
    ap.add_argument("metrics_after", help="the simulator's /metrics scraped after the run")
    ap.add_argument("--slo", required=True, help="objectives, such as hack/loadgen/slo.json")
    ap.add_argument("--sequence", required=True, help="pinned sequence sha256 for this profile, keep and seed")
    ap.add_argument("--assert-slo", action="store_true", help="fail when an objective is missed")
    a = ap.parse_args()
    slo = json.load(open(a.slo))
    lines = open(a.records).read().splitlines()
    head, recs = json.loads(lines[0]), [json.loads(l) for l in lines[1:]]
    n = len(recs)
    fails = []
    if head.get("sequence_sha256") != a.sequence:
        fails.append(f"sequence sha256 {head.get('sequence_sha256')} is not the pinned {a.sequence}")
    if n == 0 or n != head.get("requests_per_target"):
        fails.append(f"{n} records for {head.get('requests_per_target')} scheduled requests")
    if n == 0:
        print("\n".join(fails))
        sys.exit(1)
    good = [r for r in recs if r.get("ok") and r.get("status") == 200
            and (r.get("prompt_tokens"), r.get("completion_tokens")) == (r["context_tokens"], r["generated_tokens"])]
    bad = [r for r in recs if r not in good]
    fails += [f"trace line {r['trace_line']}: status {r.get('status')}, tokens {r.get('prompt_tokens')}/{r.get('completion_tokens')}"
              f" for {r['context_tokens']}/{r['generated_tokens']}, error {r.get('error')}" for r in bad[:5]]
    late = max(r["send_delay_ms"] for r in recs)
    if late > slo["max_send_delay_ms"]:
        fails.append(f"send delay {late} ms: the generator fell behind the schedule")
    ttft = [r["ttft_ms"] for r in good]
    tpot = [round((r["last_token_ms"] - r["ttft_ms"]) / (r["completion_tokens"] - 1), 1) for r in good if r["completion_tokens"] > 1]
    met = {
        "success": (len(good), slo["success_share"]),
        "ttft": (sum(1 for t in ttft if t <= slo["ttft_ms"]), slo["latency_share"]),
        "tpot": (len(good) - sum(1 for t in tpot if t > slo["tpot_ms"]), slo["latency_share"]),
    }
    ts = [r["ts_scheduled"] for r in recs]
    span = max(ts) - min(ts)
    print(f"{n} requests, offered {(n / span if span else 0):.3f} req/s, send delay max {late} ms")
    for name, (ok, share) in met.items():
        allowed = round((1 - share) * n, 9)
        print(f"slo {name}: {100 * ok / n:.1f}% (objective {100 * share:g}%), error budget {n - ok} of {allowed:.1f} = {100 * (n - ok) / allowed:.0f}% consumed")
        if a.assert_slo and ok / n < share:
            fails.append(f"objective {name} missed: {100 * ok / n:.1f}% < {100 * share:g}%")
    ev = sorted(x for r in good for x in ((r["ts_start"], 1), (r["ts_end"], -1)))
    cur = top = 0
    for _, step in ev:
        cur += step
        top = max(top, cur)
    print(f"recorded: ttft ms p50/p95/p99 {q(ttft, 0.5)}/{q(ttft, 0.95)}/{q(ttft, 0.99)}, tpot ms p50/p95 {q(tpot, 0.5)}/{q(tpot, 0.95)}, max in flight {top}")
    b, e = scrape(a.metrics_before), scrape(a.metrics_after)
    served = delta(b, e, "e2e_request_latency_seconds_count")
    if served != n:
        fails.append(f"the simulator counted {served:.0f} requests, the client {n}")
    elif not bad:
        qp = delta(b, e, "request_queue_time_seconds_sum") + delta(b, e, "request_prefill_time_seconds_sum")
        d = {"ttft": (sum(ttft) - 1000 * qp) / n,
             "e2e": (sum(r["latency_ms"] for r in good) - 1000 * delta(b, e, "e2e_request_latency_seconds_sum")) / n}
        print(f"client minus simulator means: ttft {d['ttft']:.1f} ms, e2e {d['e2e']:.1f} ms (tolerance {slo['mean_tolerance_ms']} ms)")
        fails += [f"{k} means disagree by {v:.1f} ms" for k, v in d.items() if abs(v) > slo["mean_tolerance_ms"]]
    for f in fails:
        print(f)
    sys.exit(1 if fails else 0)


if __name__ == "__main__":
    main()
