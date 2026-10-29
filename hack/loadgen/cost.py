"""Cost of one loadgen run on a declared basis (ADR-0025, after inferscope ADR-015).

Occupancy only: the node is rented by wall-clock time and its energy is in
the hourly price. The rate is declared on the command line, not measured,
and is printed with every figure it produces. The window is the run's: from
the run_config's ts_start to the end of the last request. Time with no
request in flight is paid, and reported apart. The cost per million tokens
is over the generated tokens of requests served as the profile asked, the
requests slo_check.py counts as served; a run that generated none has no
cost per token.

Not seen here: provisioning, model load and idle time outside the window,
and the provider's billing granularity. A figure is an attribution, not an
invoice.
"""
import argparse
import json
import sys


def busy_seconds(spans):
    total, end = 0.0, None
    for s, e in sorted(spans):
        if end is None or s > end:
            total, end = total + e - s, e
        elif e > end:
            total, end = total + e - end, e
    return total


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("records", help="one loadgen run: a run_config line, then one line per request")
    ap.add_argument("--eur-per-hour", type=float, required=True, help="declared price of the whole node, per hour")
    ap.add_argument("--rate-source", required=True, help="where the rate comes from; printed with every figure")
    a = ap.parse_args()
    if a.eur_per_hour <= 0:
        ap.error("--eur-per-hour must be positive")
    lines = open(a.records).read().splitlines()
    head, recs = json.loads(lines[0]), [json.loads(l) for l in lines[1:]]
    if head.get("type") != "run_config" or "ts_start" not in head:
        sys.exit("error: the first line is not a run_config with ts_start")
    if not recs:
        sys.exit("error: no request records")
    good = [r for r in recs if r.get("ok") and r.get("status") == 200
            and (r.get("prompt_tokens"), r.get("completion_tokens")) == (r["context_tokens"], r["generated_tokens"])]
    window = max(r["ts_end"] for r in recs) - head["ts_start"]
    idle = window - busy_seconds([(r["ts_start"], r["ts_end"]) for r in recs])
    eur = window / 3600 * a.eur_per_hour
    prompt = sum(r["prompt_tokens"] for r in good)
    gen = sum(r["completion_tokens"] for r in good)
    print(f"basis: occupancy at {a.eur_per_hour} EUR/h, declared ({a.rate_source})")
    print(f"window {window:.1f} s, of which {idle:.1f} s ({100 * idle / window:.0f}%) with no request in flight")
    print(f"cost of the window {eur:.4f} EUR, of which {idle / 3600 * a.eur_per_hour:.4f} with no request in flight")
    print(f"served {len(good)} of {len(recs)} requests: {prompt} prompt tokens, {gen} generated")
    if gen:
        print(f"EUR per million generated tokens: {eur / gen * 1e6:.2f}")
    else:
        print("EUR per million generated tokens: none, no token was generated")
    print("not in the window: provisioning, model load, idle after the run; billing granularity")


if __name__ == "__main__":
    main()
