#!/usr/bin/env python3
"""Build the trace-derived load profile that loadgen.py --profile replays.

Source: Azure LLM inference trace 2023, conversation service, published by
Microsoft under CC-BY-4.0 with Patel et al., "Splitwise: Efficient
generative LLM inference using phase splitting", ISCA 2024. The trace is
pinned to one commit of Azure/AzurePublicDataset and checked by SHA-256.

The profile is the trace's busiest five minutes: the 60 s sliding window
with the most arrivals, from two minutes before it, 300 s long. Every row
in it is kept as published; loadgen.py thins it at run time with a seed.
"""
import argparse
import csv
import datetime as dt
import hashlib
import io
import urllib.request

COMMIT = "790921015d50dd6aae7f7e47f39ba0e235ad6b08"
TRACE = "data/AzureLLMInferenceTrace_conv.csv"
URL = f"https://raw.githubusercontent.com/Azure/AzurePublicDataset/{COMMIT}/{TRACE}"
SHA256 = "2f1e5b666d4e3055fdbba98598ce2ec307767b9064e03e2fa46676dbcc7d0bf8"
HEADER = ["TIMESTAMP", "ContextTokens", "GeneratedTokens"]
TICK = 10**7  # the trace has seven fractional digits: 100 ns ticks, exact in integers
PEAK, BEFORE, WINDOW = 60 * TICK, 120 * TICK, 300 * TICK
EPOCH = dt.datetime(1970, 1, 1)


def ticks(stamp):
    base, _, frac = stamp.partition(".")
    whole = dt.datetime.strptime(base, "%Y-%m-%d %H:%M:%S") - EPOCH
    return (whole.days * 86400 + whole.seconds) * TICK + int((frac + "0000000")[:7])


def seconds(t):
    return f"{t // TICK}.{t % TICK:07d}"


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--trace", help="local copy of the trace; fetched from the pinned commit if omitted")
    ap.add_argument("--out", required=True, help="profile CSV to write")
    args = ap.parse_args()
    if args.trace:
        with open(args.trace, "rb") as f:
            raw = f.read()
    else:
        with urllib.request.urlopen(URL, timeout=60) as r:
            raw = r.read()
    digest = hashlib.sha256(raw).hexdigest()
    if digest != SHA256:
        raise SystemExit(f"trace sha256 {digest} does not match the pinned {SHA256}")
    rows = list(csv.reader(io.StringIO(raw.decode("utf-8"))))
    if rows[0] != HEADER:
        raise SystemExit(f"unexpected trace header {rows[0]}")
    rows = rows[1:]
    t = [ticks(r[0]) for r in rows]
    if any(a > b for a, b in zip(t, t[1:])):
        raise SystemExit("trace is not sorted by TIMESTAMP")
    # Busiest PEAK sliding window by arrivals; the earliest wins a tie.
    best, start, j = 0, 0, 0
    for i in range(len(t)):
        while t[i] - t[j] >= PEAK:
            j += 1
        if i - j + 1 > best:
            best, start = i - j + 1, j
    ws = min(max(t[0], t[start] - BEFORE), t[-1] - WINDOW)
    keep = [k for k in range(len(t)) if ws <= t[k] < ws + WINDOW]
    with open(args.out, "w", newline="") as f:
        f.write(f"# source: https://github.com/Azure/AzurePublicDataset/blob/{COMMIT}/{TRACE}\n")
        f.write(f"# source_sha256: {SHA256}\n")
        f.write("# license: CC-BY-4.0 (Microsoft, Azure Public Dataset)\n")
        f.write('# citation: Patel et al., "Splitwise: Efficient generative LLM inference using phase splitting", ISCA 2024\n')
        f.write(f"# peak: {best} arrivals in {PEAK // TICK} s from {rows[start][0]}\n")
        f.write(f"# window: {WINDOW // TICK} s from {BEFORE // TICK} s before the peak; offset_s is from the window start\n")
        f.write(f"# rows: {len(keep)}, trace lines {keep[0] + 2}..{keep[-1] + 2}\n")
        w = csv.writer(f, lineterminator="\n")
        w.writerow(["trace_line", "offset_s", "context_tokens", "generated_tokens"])
        for k in keep:
            w.writerow([k + 2, seconds(t[k] - ws), int(rows[k][1]), int(rows[k][2])])
    print(f"{args.out}: {len(keep)} rows; peak {best} arrivals in {PEAK // TICK} s from {rows[start][0]}")


if __name__ == "__main__":
    main()
