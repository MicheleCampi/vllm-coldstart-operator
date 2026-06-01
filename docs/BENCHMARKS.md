# Cold-start benchmarks

Measured cold-start cost of a vLLM server, broken down by phase, and the
effect of the `warmupStrategy` trade-off this operator exposes.

## Setup

- Model: Qwen/Qwen2.5-7B-Instruct (bfloat16)
- GPU: NVIDIA A10 (24 GB), Lambda Cloud
- vLLM: 0.22.0, `--max-model-len 4096`
- Measured with vLLM run directly (Docker, `--gpus all`). Validation through
  the operator's Warming->Ready transition on a GPU cluster is on the roadmap;
  the numbers below are the cost that transition observes.

## Method

Cold start is timed from process start to the `/health` endpoint returning
200 (the moment the server can actually serve). Phase durations are taken
from vLLM's own startup logs. Weight *download* time is reported but excluded
from the strategy comparison: it is one-time (cached afterwards) and varies
with network, so it is noise for the cold-start trade-off.

## Results

Phase breakdown, Qwen2.5-7B on A10:

| Phase | Graph (default) | Eager (`--enforce-eager`) |
|---|---|---|
| Weight download (one-time) | 14.5s | 14.2s |
| Model loading (14.29 GiB) | 17.6s | 17.1s |
| Init engine (compile + KV cache + warmup) | 30.4s | 8.3s |

The first two phases are essentially identical across strategies. The
difference is concentrated in init engine, which is exactly what
`warmupStrategy` controls.

## The warmupStrategy trade-off

`Graph` (CUDA graphs on) spends 30.4s in init engine: ~14.7s of
`torch.compile` plus ~6s capturing CUDA graphs. `Eager` disables both and
spends 8.3s — a **3.7x faster** init engine, saving ~22s of cold start.

- `warmupStrategy: Eager` — fast cold start. Best for scale-to-zero, where a
  pod is recreated on demand and time-to-serve dominates.
- `warmupStrategy: Graph` — slower cold start, but the compiled graphs pay off
  in steady-state throughput. Best for long-lived, high-traffic replicas.

This is the trade-off the operator makes a first-class, declarative choice
rather than a hidden flag.

## A note on measurement

Wall-clock time-to-ready can mislead: an `Eager` run measured a *longer*
total than a `Graph` run because both re-downloaded weights and download time
varied. Isolating the phase that the strategy actually changes — init engine —
is what reveals the real 3.7x difference. Compare phases, not wall-clock.
