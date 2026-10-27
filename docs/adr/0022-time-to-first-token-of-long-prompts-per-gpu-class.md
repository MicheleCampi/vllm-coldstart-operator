# ADR-0022: The time to first token of a long prompt is an objective per GPU class

Status: Accepted
Date: 2026-10-27
Amends: ADR-0020 (D3, the time to first token on one L4); measures ADR-0020 D4

## Context

ADR-0013 D1 set a time to first token (TTFT) of at most 1 s for 95% of
requests, and ADR-0020 D3 kept it for one L4. On 2026-10-27 one L4
(g2-standard-4, us-central1-a) served Qwen/Qwen2.5-7B-Instruct with vLLM
v0.11.0 in bf16 (`--enforce-eager --max-model-len 8192
--gpu-memory-utilization 0.90`), and the loadgen ran the pinned replay
three times, the D3 ladder at keep 0.05 (seeds 42, 1, 2), 0.075 and 0.1,
and keep 0.25 four times. Judged by `slo_check.py --client-only` against
`hack/loadgen/slo-l4-qwen7b-bf16.json` as ADR-0020 left it, the TTFT
objective failed in every run: 80.9% in the first replay.

## Facts that constrain the design

1. **The slow first tokens are the long prompts.** In the first replay
   12 of the 13 requests over 1 s had 4076 to 4095 prompt tokens; the
   other one, 405 tokens, waited behind other requests.
2. **A prompt of about 4080 tokens takes over 1.1 s on its own.** The
   five such requests sent with no other request in flight had a TTFT
   of 1136 to 1174 ms. Up to 2048 tokens the three requests with none in
   flight took 156 to 308 ms, and every request of that range but the
   queued one was under 1 s (807 ms the highest).
3. **Between 2048 and 3072 tokens** three requests took 734 to 812 ms;
   none was sent with nothing in flight.
4. **vLLM's TTFT histogram has no label for prompt length.** As the
   stack stored it on 2026-10-27 its labels were engine, instance, job,
   le, model_name, namespace, pod and service: the burn-rate alerts of
   ADR-0021 count every first token over 1 s.

## Decisions

**D1. On one L4, Qwen2.5-7B-Instruct, bf16, vLLM v0.11.0, a request whose
prompt has more than 2048 tokens meets the TTFT objective within 2000 ms;
every other request within 1000 ms; one share, 95%, over all requests.**
`ttft_long_above_tokens` and `ttft_long_ms` in
`hack/loadgen/slo-l4-qwen7b-bf16.json`; `slo_check.py` applies them per
request, and a file without them, such as `slo.json`, is judged as
before. 2000 ms is 1.7 times the highest floor of fact 2, about the
margin ADR-0020 D2 left over the TPOT floor (100 ms over about 64);
1500 ms would leave 1.28 times. One share keeps 15 long requests from
failing the run on one request. Chosen by Michele on 2026-10-27.

**D2. The boundary is 2048 tokens:** below it every request but a queued
one measured within 1 s (fact 2).

**D3. The capacity of one replica (ADR-0020 D4) is between 0.229 and
0.345 requests a second of this profile.** Old is the TTFT share against
1000 ms for all, as ADR-0020 judged it; new is D1.

| run | offered req/s | TTFT old | TTFT new | TPOT | success |
|---|---|---|---|---|---|
| replay 1 | 0.229 | 80.9% | 95.6% | 100.0% | 100.0% |
| replay 2 | 0.229 | 92.6% | 100.0% | 100.0% | 100.0% |
| replay 3 | 0.229 | 91.2% | 98.5% | 98.5% | 98.5% |
| keep 0.05 seed 1 | 0.345 | 78.6% | 97.1% | 100.0% | 100.0% |
| keep 0.05 seed 2 | 0.408 | 73.0% | 95.1% | 90.2% | 100.0% |
| keep 0.05 seed 42 | 0.407 | 78.5% | 96.7% | 95.0% | 100.0% |
| keep 0.075 seed 42 | 0.538 | 75.0% | 93.8% | 86.9% | 100.0% |
| keep 0.1 seed 42 | 0.746 | 71.6% | 90.1% | 77.9% | 100.0% |
| keep 0.25 seed 42, x4 | 1.912 | 15.0-15.2% | 19.2-19.8% | 1.0% | 99.5-99.7% |

At 0.229 req/s every objective holds but success in the third replay:
one request of 68 was disconnected, and 68 requests leave a budget of
0.3 of a request. At 0.408 the TPOT misses first (seed 2). A scaling
target on the L4 waits for a run between 0.229 and 0.345 req/s.

## Consequences

- GPU runs on this class are still checked with
  `--slo hack/loadgen/slo-l4-qwen7b-bf16.json`; `test_slo_files.py`
  allows it the two keys of D1 besides the TPOT of ADR-0020.
- The burn-rate alerts stay at 1 s for every request (fact 4): on this
  class they are stricter than D1, and fire on long prompts the
  objective allows.
- Another GPU, model, precision or engine version needs its own floor.

## What this ADR does not prove

- The floor of fact 2 rests on five requests of one length; prompts
  between 2048 and 4076 tokens were not sent with nothing in flight.
- Replays 2 and 3 had 5 requests over 1 s against 13 in the first, on
  the same sequence: vLLM's prefix cache likely served repeated prompts
  (deduced, not measured); only the first replay is cold.
- The sequences of the replay (`6c10349917a6`) and of keep 0.1 seed 42
  (`b5b7fff2f0fe`) are the pinned ones; the other seeds have no pinned
  sequence, so `--sequence` only checks each run against its own header.
- One session, one node, one zone; nothing about other GPUs, FP8, CUDA
  graphs or chunked prefill.
- The session's per-request records are not in this repository.
