# Cold-start benchmarks

Measured cold-start cost of a vLLM server, broken down by phase, and the
effect of the `warmupStrategy` trade-off this operator exposes.

## Setup

- Model: Qwen/Qwen2.5-7B-Instruct (bfloat16)
- GPU: NVIDIA A10 (24 GB), Lambda Cloud
- vLLM: 0.22.0, `--max-model-len 4096`
- Measured with vLLM run directly (Docker, `--gpus all`); the figures come
  from one run per strategy, on 2026-06-01. Validation through the operator's
  Warming->Ready transition on a GPU cluster is on the roadmap; until then,
  the numbers below are what that transition is expected to wait on, not a
  measurement of it.
- The vLLM logs of both runs are in [`docs/benchmarks/`](benchmarks/).

## Method

Phase durations are taken from vLLM's own startup log lines:
`Time spent downloading weights`, `Model loading took` and
`init engine ... took`. Weight *download* time is reported but excluded from
the strategy comparison: it is one-time where the model cache persists (here
each container downloaded it again) and varies with the network, so it is
noise for the cold-start trade-off.

In vLLM 0.22, `Model loading took` includes the download: in both logs it
ends about 3 s after the download ends and lasts about 3 s longer (Eager
17.15 s against 14.20 s, Graph 17.65 s against 14.52 s). The table below
subtracts it.

## Results

Phase breakdown, Qwen2.5-7B on A10:

| Phase | Graph (vLLM's default) | Eager (`--enforce-eager`, the operator's default) |
|---|---|---|
| Weight download (one-time) | 14.5s | 14.2s |
| Model loading, download excluded (14.29 GiB) | 3.1s | 3.0s |
| Init engine (compile + KV cache + warmup) | 30.4s | 8.3s |

From the first log line to the server registering `/health`: 77 s (Graph)
and 54 s (Eager), download included; the `/health` 200 line itself has no
timestamp. The weights were written to the container's filesystem seconds
before loading, with over 200 GiB of RAM free, so the 14.19 GiB were most
likely read from the page cache (2.3 s); that is inferred, not measured.

The first two phases are essentially identical across strategies. The
difference is concentrated in init engine, which is exactly what
`warmupStrategy` controls.

## The warmupStrategy trade-off

`Graph` (CUDA graphs on) spends 30.4s in init engine: ~14.7s of
`torch.compile` plus ~6s capturing CUDA graphs. `Eager` disables both and
spends 8.3s — a **3.7x faster** init engine, saving ~22s of cold start.

- `warmupStrategy: Eager` — fast cold start. Best for scale-to-zero, where a
  pod is recreated on demand and time-to-serve dominates.
- `warmupStrategy: Graph` — slower cold start, in exchange for faster
  decode. Whether that becomes more end-to-end throughput depends on load: in
  [cuda-graphs-experiment](https://github.com/MicheleCampi/cuda-graphs-experiment)
  (H100, vLLM 0.23, n = 5 per cell) decode was faster in every case, and
  throughput rose on the 7B (+4.8% to +8.5%) and on the 32B with sporadic
  traffic (+3.2%), but fell 5.2% on the 32B under sustained overload. Not
  measured on the A10. Best for long-lived replicas whose load has been
  checked to benefit.

This is the trade-off the operator makes a first-class, declarative choice
rather than a hidden flag.

## A note on measurement

Wall-clock time-to-ready can mislead: it includes the weight download, which
both runs repeated and which varies with the network. Isolating the phase that
the strategy actually changes — init engine — is what reveals the 3.7x
difference. Compare phases, not wall-clock.

## Relation to vllm-coldstart-probe

[vllm-coldstart-probe](https://github.com/MicheleCampi/vllm-coldstart-probe)
timed the same model on an A10, in a different way: FP16 rather than bfloat16,
an offline `LLM(...)` construction rather than `vllm serve`, one figure for the
whole construction, and a vLLM version it did not record per run. Its
[analysis/CAPTURES.md](https://github.com/MicheleCampi/vllm-coldstart-probe/blob/main/analysis/CAPTURES.md)
gives, for the `qwen2.5-7b-fp16` rows: 15.05 s eager with a cold page cache,
11.70 s eager with a warm one, 47.72 s with CUDA graphs and a cold cache. The
Eager phases here, without the download, add up to 11.22 s (2.95 + 8.27), close
to the probe's warm-cache figure. With CUDA graphs the two do not line up
(33.55 s here, 3.13 + 30.42), and neither repository records enough to explain
the gap. The setups differ, so none of this is a cross-check.
