"""Workload replay client. Stdlib only (GPU-session rule: no dependency
surprises on paid nodes).

Replays a gen_workload.py JSONL honouring t_offset_s. Two endpoint modes:
- vllm: POST /v1/completions (OpenAI-compat), real run.
- fixture: GET the fixture /metrics endpoint instead — same pacing loop,
  same result accounting, zero-cost kind rehearsal of the harness.

Per-request record: t_send, ttfb_ms (TTFT proxy at HTTP level), total_ms,
status. Output JSONL, one line per request, plus a summary line to stderr.
"""
import argparse
import concurrent.futures
import json
import sys
import time
import urllib.error
import urllib.request


def do_request(url, mode, item, timeout):
    t0 = time.monotonic()
    try:
        if mode == "vllm":
            body = json.dumps({
                "model": item.get("model", "default"),
                "prompt": item["prompt"],
                "max_tokens": item["max_tokens"],
                "temperature": 0.0,
            }).encode()
            req = urllib.request.Request(
                url, data=body, headers={"Content-Type": "application/json"}
            )
        else:
            req = urllib.request.Request(url)
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            ttfb = (time.monotonic() - t0) * 1000
            resp.read()
            total = (time.monotonic() - t0) * 1000
            return {"status": resp.status, "ttfb_ms": round(ttfb, 1),
                    "total_ms": round(total, 1)}
    except urllib.error.HTTPError as e:
        return {"status": e.code, "ttfb_ms": None,
                "total_ms": round((time.monotonic() - t0) * 1000, 1)}
    except Exception as e:
        return {"status": -1, "error": type(e).__name__, "ttfb_ms": None,
                "total_ms": round((time.monotonic() - t0) * 1000, 1)}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--workload", required=True)
    ap.add_argument("--url", required=True)
    ap.add_argument("--endpoint-mode", choices=["vllm", "fixture"], default="vllm")
    ap.add_argument("--timeout-s", type=float, default=60.0)
    ap.add_argument("--out", required=True)
    ap.add_argument("--max-workers", type=int, default=64)
    args = ap.parse_args()

    items = [json.loads(l) for l in open(args.workload)]
    results = [None] * len(items)
    t_start = time.monotonic()
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.max_workers) as ex:
        futures = {}
        for i, item in enumerate(items):
            delay = item["t_offset_s"] - (time.monotonic() - t_start)
            if delay > 0:
                time.sleep(delay)
            t_send = time.monotonic() - t_start
            fut = ex.submit(do_request, args.url, args.endpoint_mode, item,
                            args.timeout_s)
            futures[fut] = (i, t_send, item.get("shared", False))
        for fut in concurrent.futures.as_completed(futures):
            i, t_send, shared = futures[fut]
            r = fut.result()
            r.update({"i": i, "t_send_s": round(t_send, 3), "shared": shared})
            results[i] = r

    ok = sum(1 for r in results if r and r["status"] == 200)
    with open(args.out, "w") as f:
        for r in results:
            f.write(json.dumps(r) + "\n")
    wall = time.monotonic() - t_start
    print(f"replayed={len(items)} ok={ok} fail={len(items)-ok} "
          f"wall_s={wall:.1f}", file=sys.stderr)
    return 0 if ok == len(items) else 1


if __name__ == "__main__":
    sys.exit(main())
