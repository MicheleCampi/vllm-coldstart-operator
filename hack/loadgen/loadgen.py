#!/usr/bin/env python3
"""Deterministic closed-loop load generator for fleet preemption validation.

Drives one or more OpenAI-compatible completion endpoints (llm-d inference
sim on kind, real vLLM on GPU nodes) with fixed per-target concurrency and
writes one JSONL line per request: wall-clock start / first-token / end
timestamps (for correlation with operator logs and Kubernetes events), TTFT
and total latency from the monotonic clock, HTTP status and error string.
The first line of the output is the full run config.

Determinism: prompts are generated from an RNG seeded per
(seed, target, worker, request-index) and max_tokens is fixed, so two runs
with the same arguments produce the same request stream and the
before/during/after windows are comparable across runs.

Closed-loop by design: each worker issues the next request only when the
previous one completes, so `--concurrency` bounds in-flight requests per
target and saturation is reached by setting it above the server's capacity.
"""

import argparse
import asyncio
import json
import random
import time

import aiohttp

VOCAB = (
    "alpha bravo charlie delta echo foxtrot golf hotel india juliett kilo "
    "lima mike november oscar papa quebec romeo sierra tango uniform victor "
    "whiskey xray yankee zulu"
).split()


def build_prompt(rng: random.Random, n_words: int) -> str:
    words = [rng.choice(VOCAB) for _ in range(n_words)]
    return "Repeat the following list of words: " + " ".join(words)


async def worker(
    target_name: str,
    base_url: str,
    wid: int,
    args: argparse.Namespace,
    out_q: asyncio.Queue,
    deadline: float,
) -> None:
    timeout = aiohttp.ClientTimeout(total=args.request_timeout)
    req_idx = 0
    async with aiohttp.ClientSession(timeout=timeout) as session:
        while time.monotonic() < deadline:
            rng = random.Random(f"{args.seed}:{target_name}:{wid}:{req_idx}")
            payload = {
                "model": args.model,
                "prompt": build_prompt(rng, args.prompt_words),
                "max_tokens": args.max_tokens,
                "stream": True,
            }
            rec = {
                "type": "request",
                "target": target_name,
                "worker": wid,
                "req_idx": req_idx,
                "ts_start": time.time(),
            }
            t0 = time.monotonic()
            chunks = 0
            try:
                async with session.post(
                    f"{base_url}/v1/completions", json=payload
                ) as resp:
                    rec["status"] = resp.status
                    if resp.status == 200:
                        async for raw in resp.content:
                            if not raw.strip():
                                continue
                            if chunks == 0:
                                rec["ts_first_token"] = time.time()
                                rec["ttft_ms"] = round(
                                    (time.monotonic() - t0) * 1000, 1
                                )
                            chunks += 1
                        rec["ok"] = True
                    else:
                        body = await resp.text()
                        rec["ok"] = False
                        rec["error"] = body[:200]
            except (aiohttp.ClientError, asyncio.TimeoutError) as e:
                rec["ok"] = False
                rec.setdefault("status", 0)
                rec["error"] = f"{type(e).__name__}: {e}"[:200]
            rec["ts_end"] = time.time()
            rec["latency_ms"] = round((time.monotonic() - t0) * 1000, 1)
            rec["chunks"] = chunks
            await out_q.put(rec)
            req_idx += 1
            if not rec["ok"]:
                # Back off briefly so a dead endpoint yields a readable error
                # timeline instead of a tight-loop flood.
                await asyncio.sleep(0.5)


async def writer(path: str, out_q: asyncio.Queue) -> None:
    with open(path, "w", encoding="utf-8") as f:
        while True:
            rec = await out_q.get()
            if rec is None:
                return
            f.write(json.dumps(rec) + "\n")
            f.flush()


async def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument(
        "--target",
        action="append",
        required=True,
        metavar="NAME=URL",
        help="repeatable; e.g. svc-a=http://127.0.0.1:8001",
    )
    ap.add_argument("--model", required=True)
    ap.add_argument("--concurrency", type=int, default=8, help="per target")
    ap.add_argument("--duration", type=float, required=True, help="seconds")
    ap.add_argument("--max-tokens", type=int, default=128)
    ap.add_argument("--prompt-words", type=int, default=100)
    ap.add_argument("--seed", type=int, default=42)
    ap.add_argument("--request-timeout", type=float, default=180.0)
    ap.add_argument("--out", required=True, help="JSONL output path")
    args = ap.parse_args()

    targets = []
    for t in args.target:
        name, _, url = t.partition("=")
        if not name or not url:
            ap.error(f"bad --target {t!r}, expected NAME=URL")
        targets.append((name, url.rstrip("/")))

    out_q: asyncio.Queue = asyncio.Queue()
    run_config = {
        "type": "run_config",
        "ts_start": time.time(),
        "targets": dict(targets),
        "model": args.model,
        "concurrency": args.concurrency,
        "duration_s": args.duration,
        "max_tokens": args.max_tokens,
        "prompt_words": args.prompt_words,
        "seed": args.seed,
        "request_timeout_s": args.request_timeout,
    }
    await out_q.put(run_config)

    w = asyncio.create_task(writer(args.out, out_q))
    deadline = time.monotonic() + args.duration
    workers = [
        asyncio.create_task(worker(name, url, i, args, out_q, deadline))
        for name, url in targets
        for i in range(args.concurrency)
    ]
    # Let in-flight requests finish past the deadline, bounded by the
    # request timeout, then cut whatever is left.
    done, pending = await asyncio.wait(
        workers, timeout=args.duration + args.request_timeout + 5
    )
    for p in pending:
        p.cancel()
    await out_q.put(None)
    await w
    n = len(done)
    print(f"run complete: {n}/{len(workers)} workers exited cleanly -> {args.out}")


if __name__ == "__main__":
    asyncio.run(main())
