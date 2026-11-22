"""Torch-only bf16 linear timing at the decode shapes of Qwen2.5-0.5B.

Study, not a gate (A-024). Runs with the python3 and torch of a vLLM CPU
image, so v0.19.1 gives torch 2.10 / oneDNN 3.7.1 and v0.20.0 gives torch
2.11 / oneDNN 3.10.2, with no vLLM code involved. If the 0.20 steady-state
step on Sapphire Rapids is in torch, the per-token sum below moves with it.

Usage: python3 matmul.py THREADS   Prints one JSON line per shape and a total.
"""
import json
import re
import statistics
import sys
import time

import torch

threads = int(sys.argv[1])
torch.set_num_threads(threads)
torch.manual_seed(0)

# Qwen2.5-0.5B: hidden 896, intermediate 4864, 14 query heads and 2 KV heads
# of 64, 24 layers, vocab 151936. (name, in, out, times per token)
SHAPES = [
    ("qkv", 896, 896 + 2 * 128, 24),
    ("o", 896, 896, 24),
    ("gate_up", 896, 2 * 4864, 24),
    ("down", 4864, 896, 24),
    ("lm_head", 896, 151936, 1),
]


def bench(k, n, m, budget_s=1.0):
    lin = torch.nn.Linear(k, n, bias=False).to(torch.bfloat16)
    x = torch.randn(m, k, dtype=torch.bfloat16)
    with torch.no_grad():
        for _ in range(20):
            lin(x)
        times = []
        end = time.perf_counter() + budget_s
        while time.perf_counter() < end or len(times) < 20:
            t = time.perf_counter()
            lin(x)
            times.append(time.perf_counter() - t)
    return statistics.median(times) * 1e6, len(times)


def bf16_supported():
    try:
        return bool(torch.ops.mkldnn._is_mkldnn_bf16_supported())
    except Exception:  # noqa: BLE001 - informational only
        return None


onednn = re.search(r"MKL-DNN v[0-9.]+", torch.__config__.show())
info = {
    "torch": torch.__version__,
    "onednn": onednn.group(0) if onednn else "?",
    "mkldnn_available": torch.backends.mkldnn.is_available(),
    "mkldnn_bf16": bf16_supported(),
    "threads": torch.get_num_threads(),
}
print(json.dumps({"info": info}), flush=True)
for m in (1, 4):
    total = 0.0
    for name, k, n, per_tok in SHAPES:
        us, iters = bench(k, n, m)
        total += us * per_tok
        print(json.dumps({"m": m, "shape": name, "k": k, "n": n, "median_us": round(us, 1), "iters": iters}), flush=True)
    print(json.dumps({"m": m, "per_token_linear_ms": round(total / 1000, 2), "torch": torch.__version__, "threads": threads}), flush=True)
