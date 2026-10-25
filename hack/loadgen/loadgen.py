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

Open-loop with --profile: requests follow the arrival schedule of a
trace-derived profile (hack/loadgen/profiles, built by build_profile.py)
instead of a fixed concurrency. Each profile row is kept with probability
--keep from an RNG seeded per (seed, trace line), and that RNG also draws a
prompt of exactly context_tokens vocabulary words, one token each for
Qwen2.5 and for the simulator; ignore_eos forces the
response to generated_tokens. Each record carries its scheduled time and
send delay, so a late send shows up instead of hiding queueing (coordinated
omission), and the token usage the server reports. The run config carries
the SHA-256 of the profile file and of the kept request sequence: same
profile, keep and seed give the same sequence and the same hash.
"""

import argparse
import asyncio
import csv
import hashlib
import json
import random
import time

import aiohttp

# One token each for Qwen2.5, first or after a space, and one \w+ match for
# the simulator's SimpleTokenizer: profile prompts are context_tokens long on
# both (test_loadgen_tokens.py).
VOCAB = (
    "the and for are but not you all any can had her was one our out day "
    "get has him his how man new now old see two way who boy did its let "
    "put say she too use top red big end run sun sky"
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


def profile_requests(path: str, keep: float, seed: int):
    """Requests kept from a profile, the profile's SHA-256 and the SHA-256 of
    the kept sequence (schedule, sizes and prompt text)."""
    with open(path, "rb") as f:
        raw = f.read()
    lines = [l for l in raw.decode("utf-8").splitlines() if not l.startswith("#")]
    seq = hashlib.sha256()
    reqs = []
    last = 0.0
    for r in csv.DictReader(lines):
        line, offset = int(r["trace_line"]), float(r["offset_s"])
        if offset < last:
            raise SystemExit(f"{path}: offset_s decreases at trace line {line}")
        last = offset
        rng = random.Random(f"{seed}:{line}")
        if rng.random() >= keep:
            continue
        ctx, gen = int(r["context_tokens"]), int(r["generated_tokens"])
        prompt = " ".join(rng.choice(VOCAB) for _ in range(ctx))
        digest = hashlib.sha256(prompt.encode()).hexdigest()
        seq.update(f"{line},{r['offset_s']},{ctx},{gen},{digest}\n".encode())
        reqs.append(
            {
                "trace_line": line,
                "offset_s": offset,
                "context_tokens": ctx,
                "generated_tokens": gen,
                "prompt": prompt,
            }
        )
    return reqs, hashlib.sha256(raw).hexdigest(), seq.hexdigest()


async def send(
    session: aiohttp.ClientSession,
    target_name: str,
    base_url: str,
    r: dict,
    args: argparse.Namespace,
    out_q: asyncio.Queue,
    clock: tuple,
) -> None:
    t_start, wall_start = clock
    payload = {
        "model": args.model,
        "prompt": r["prompt"],
        "max_tokens": r["generated_tokens"],
        "ignore_eos": True,
        "stream": True,
        "stream_options": {"include_usage": True},
    }
    rec = {
        "type": "request",
        "target": target_name,
        "trace_line": r["trace_line"],
        "context_tokens": r["context_tokens"],
        "generated_tokens": r["generated_tokens"],
        "ts_scheduled": wall_start + r["offset_s"],
        "ts_start": time.time(),
    }
    t0 = time.monotonic()
    rec["send_delay_ms"] = round((t0 - t_start - r["offset_s"]) * 1000, 1)
    chunks = 0
    try:
        async with session.post(f"{base_url}/v1/completions", json=payload) as resp:
            rec["status"] = resp.status
            if resp.status == 200:
                async for raw in resp.content:
                    line = raw.strip()
                    if not line.startswith(b"data: ") or line == b"data: [DONE]":
                        continue
                    data = json.loads(line[6:])
                    usage = data.get("usage") or {}
                    if usage:
                        rec["prompt_tokens"] = usage.get("prompt_tokens")
                        rec["completion_tokens"] = usage.get("completion_tokens")
                    if not data.get("choices"):
                        continue
                    elapsed_ms = round((time.monotonic() - t0) * 1000, 1)
                    if chunks == 0:
                        rec["ts_first_token"] = time.time()
                        rec["ttft_ms"] = elapsed_ms
                    rec["last_token_ms"] = elapsed_ms
                    chunks += 1
                rec["ok"] = True
            else:
                body = await resp.text()
                rec["ok"] = False
                rec["error"] = body[:200]
    except (aiohttp.ClientError, asyncio.TimeoutError, ValueError) as e:
        rec["ok"] = False
        rec.setdefault("status", 0)
        rec["error"] = f"{type(e).__name__}: {e}"[:200]
    rec["ts_end"] = time.time()
    rec["latency_ms"] = round((time.monotonic() - t0) * 1000, 1)
    rec["chunks"] = chunks
    await out_q.put(rec)


async def replay(
    target_name: str,
    base_url: str,
    reqs: list,
    args: argparse.Namespace,
    out_q: asyncio.Queue,
    clock: tuple,
) -> None:
    # Open loop: each request goes out at its scheduled time whatever is in
    # flight, so the connection pool must not cap concurrency (limit=0).
    # --force-close opens one connection per request: a Kubernetes Service
    # balances per connection, so this spreads requests across its pods.
    t_start = clock[0]
    timeout = aiohttp.ClientTimeout(total=args.request_timeout)
    connector = aiohttp.TCPConnector(limit=0, force_close=args.force_close)
    async with aiohttp.ClientSession(timeout=timeout, connector=connector) as session:
        sends = []
        for r in reqs:
            delay = t_start + r["offset_s"] - time.monotonic()
            if delay > 0:
                await asyncio.sleep(delay)
            sends.append(
                asyncio.create_task(
                    send(session, target_name, base_url, r, args, out_q, clock)
                )
            )
        await asyncio.gather(*sends)


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
    ap.add_argument("--duration", type=float, help="seconds; closed loop only")
    ap.add_argument("--max-tokens", type=int, default=128)
    ap.add_argument("--prompt-words", type=int, default=100)
    ap.add_argument("--seed", type=int, default=42)
    ap.add_argument("--request-timeout", type=float, default=180.0)
    ap.add_argument("--out", required=True, help="JSONL output path")
    ap.add_argument("--profile", help="open loop: replay this profile CSV instead")
    ap.add_argument("--keep", type=float, default=1.0, help="fraction of rows kept")
    ap.add_argument(
        "--force-close",
        action="store_true",
        help="open loop: close each connection after its response",
    )
    args = ap.parse_args()
    if args.profile is None and args.duration is None:
        ap.error("--duration is required without --profile")
    if args.force_close and args.profile is None:
        ap.error("--force-close applies to the open loop only (--profile)")
    if not 0.0 < args.keep <= 1.0:
        ap.error("--keep must be in (0, 1]")

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
    if args.profile:
        reqs, profile_sha, seq_sha = profile_requests(args.profile, args.keep, args.seed)
        run_config.update(
            {
                "mode": "profile",
                "profile": args.profile,
                "profile_sha256": profile_sha,
                "keep": args.keep,
                "force_close": args.force_close,
                "requests_per_target": len(reqs),
                "sequence_sha256": seq_sha,
            }
        )
    await out_q.put(run_config)

    w = asyncio.create_task(writer(args.out, out_q))
    if args.profile:
        clock = (time.monotonic(), time.time())
        workers = [
            asyncio.create_task(replay(name, url, reqs, args, out_q, clock))
            for name, url in targets
        ]
        span = reqs[-1]["offset_s"] if reqs else 0.0
    else:
        deadline = time.monotonic() + args.duration
        workers = [
            asyncio.create_task(worker(name, url, i, args, out_q, deadline))
            for name, url in targets
            for i in range(args.concurrency)
        ]
        span = args.duration
    # Let in-flight requests finish past the deadline, bounded by the
    # request timeout, then cut whatever is left.
    done, pending = await asyncio.wait(
        workers, timeout=span + args.request_timeout + 5
    )
    for p in pending:
        p.cancel()
    await out_q.put(None)
    await w
    n = len(done)
    print(f"run complete: {n}/{len(workers)} workers exited cleanly -> {args.out}")
    if args.profile:
        print(f"profile: {len(reqs)} requests per target, sequence sha256 {seq_sha}")


if __name__ == "__main__":
    asyncio.run(main())
