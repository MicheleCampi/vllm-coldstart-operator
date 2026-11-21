"""Summarize study.sh outputs: one table row per job and run, plus what the
server logged while the first request was in flight.

Usage: summarize.py DIR...   (each DIR holds env.json, cold.jsonl,
warm-1.jsonl, cold.server.log, warm-1.server.log)
Prints a Markdown table, then for each cold run the server log lines
written between Ready and the end of the first request (compilation
messages show up there if any happen at request time).
"""
import json
import re
import sys
from datetime import datetime, timedelta

KEYWORDS = re.compile(r"compil|inductor|dynamo|graph|warm|jit|triton|kernel", re.I)


def ts(s):
    # docker logs -t: 2026-11-21T21:40:01.123456789Z
    s = s.rstrip("Z")
    head, _, frac = s.partition(".")
    return datetime.fromisoformat(head) + timedelta(microseconds=int((frac + "000000")[:6] or 0))


rows, notes = [], []
for d in sys.argv[1:]:
    env = json.load(open(f"{d}/env.json"))
    for run in ("cold", "warm-1"):
        lines = [json.loads(l) for l in open(f"{d}/{run}.jsonl") if l.startswith("{")]
        if not lines:
            continue
        first = lines[0]
        client = next(l for l in lines if l["label"] == "client")
        rest = [l["s"] for l in lines if l["label"] in ("again", "other-prompt")]
        rows.append((env["vllm"], env["mode"], d.rsplit("/", 1)[-1], run, first["ready_s"],
                     first["label"], first["s"], client["s"], max(rest) if rest else "",
                     all(l["code"] == 200 for l in lines)))
        if run == "cold":
            start = ts(first["ready_at"])
            end = start + timedelta(seconds=first["s"] + 1)
            window = []
            for raw in open(f"{d}/{run}.server.log", errors="replace"):
                stamp, _, msg = raw.partition(" ")
                try:
                    t = ts(stamp)
                except ValueError:
                    continue
                if start <= t <= end:
                    window.append(msg.rstrip())
            hits = [m for m in window if KEYWORDS.search(m)]
            notes.append((d, env, len(window), hits[:15]))

print("| vLLM | mode | job | run | Ready (s) | first request | first (s) | client 32 tok (s) | later 32 tok, max (s) | all 200 |")
print("|---|---|---|---|---|---|---|---|---|---|")
for r in rows:
    print("| " + " | ".join(str(x) for x in r) + " |")
print()
for d, env, n, hits in notes:
    print(f"**{d}** ({env['vllm']}, {env['mode']}, {env['cpu_model']}, {env['cpus']} CPUs, AVX-512: {bool(env['avx512'])}, AMX: {bool(env['amx'])}): "
          f"{n} server log lines during the first request; matching compile/JIT keywords: {len(hits)}")
    for h in hits:
        print(f"    {h[:240]}")
