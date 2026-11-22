"""Summarize steady-study outputs: one line per directory, or a table.

Usage: summarize.py DIR...  Each DIR holds cpu.txt, ready.json and
requests.jsonl from study.sh; its name is VARIANT-CPUS-REP.
"""
import json
import math
import os
import statistics
import sys


def row(d):
    reqs = [json.loads(line) for line in open(os.path.join(d, "requests.jsonl"))]
    probe = [r["s"] for r in reqs if r["label"] == "probe" and r["code"] == 200]
    failed = sum(1 for r in reqs if r["code"] != 200)
    s = sorted(probe[1:])  # steady state: every probe after the first
    return {
        "run": os.path.basename(d.rstrip("/")),
        "ready_s": json.load(open(os.path.join(d, "ready.json")))["ready_s"],
        "warmup_s": reqs[0]["s"],
        "first_s": probe[0] if probe else None,
        "p50_s": round(statistics.median(s), 2) if s else None,
        "p95_s": round(s[math.ceil(len(s) * 0.95) - 1], 2) if s else None,
        "n": len(probe), "failed": failed,
        "cpu": open(os.path.join(d, "cpu.txt")).read().strip(),
    }


rows = []
for d in sys.argv[1:]:
    try:
        rows.append(row(d))
    except (OSError, ValueError, KeyError) as e:
        rows.append({"run": os.path.basename(d.rstrip("/")), "error": str(e)})
if len(rows) == 1:
    print(json.dumps(rows[0]))
else:
    print("| run | Ready s | warmup s | first probe s | p50 s (rest) | p95 s (rest) | n | failed | cpu |")
    print("|---|---|---|---|---|---|---|---|---|")
    for r in sorted(rows, key=lambda r: r["run"]):
        if "error" in r:
            print(f"| {r['run']} | error: {r['error']} | | | | | | | |")
        else:
            print(f"| {r['run']} | {r['ready_s']} | {r['warmup_s']} | {r['first_s']} | {r['p50_s']} | {r['p95_s']} | {r['n']} | {r['failed']} | {r['cpu']} |")
