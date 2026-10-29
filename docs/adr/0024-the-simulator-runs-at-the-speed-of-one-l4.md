# ADR-0024: The simulator runs at the speed of one L4

Status: Accepted, rehearsed on kind 2026-10-29
Date: 2026-10-29
Amends: ADR-0019 (the pre-stop sleep and the fault probe)

## Context

R6 item 4 asks that the CI simulator answer at the speed of the GPU the
platform was measured on: one L4 serving Qwen2.5-7B-Instruct in bf16 on
vLLM v0.11.0, run B-005 of 2026-10-27. Until now it ran at 250 ms to the
first token, 20 ms a token and a load factor of 2.0, and the pre-stop
sleep of ADR-0019 was sized on responses measured at that speed (14.4 s).

## Facts that constrain the design

1. On B-005, 931 requests of the profile took p50 7.4 s, p99 41.3 s and
   at most 53.7 s, 589 output tokens; lengths come from the trace.
2. The llm-d simulator v0.8.2, with no fixed time to first token, takes a
   prefill overhead plus a time per prompt token, and applies one load
   factor, linear in the requests in flight up to `--max-num-seqs`, to
   the prefill and to the inter-token latency.
3. vLLM v0.11.0 admits 256 sequences by default.
4. One request alone, 1000 prompt tokens and 100 out, took 382 ms to the
   first token and 7.0 s in all on the calibrated image.

## Decisions

- D1. `Dockerfile.sim` runs `--prefill-overhead 135ms
  --prefill-time-per-token 226us --inter-token-latency 66ms
  --max-num-seqs 256 --time-factor-under-load 7.79`, fitted on B-005.
- D2. `PRE_STOP_SLEEP_SECONDS` is 110 s, about twice the longest
  response of fact 1, the rule of ADR-0019. The grace of a pod with no
  GPU follows (120 s); that of a serving pod stays 120 s, above it.
- D3. The fault probe asks 589 tokens, the longest response of fact 1,
  and `kubectl drain` waits 240 s, so its own limit does not end a run
  the grace allows.
- D4. `sim-load` and `fault-run.sh` judge against
  `hack/loadgen/slo-l4-qwen7b-bf16.json`; `slo.json` stays for ADR-0013.

## Rehearsed, 2026-10-29

`faults-under-load` ran on kind with the calibrated image and the 589
token probe, twice:

- with the sleep at 30 s, the drain found 4 probe streams open and cut 3,
  all 30.2 s after the fault; `timeline.py` exited 1;
- with 110 s, the drain and the notice each found 4 open and cut none,
  and no replay request failed; the job took 13 min 10 s.

`sim-load` took 11 min 50 s of its 20. At keep 0.025 the simulator
recorded TTFT p50/p95 258/1126 ms and TPOT 69.3/74.0 ms; the L4, on the
same sequence over its three replays (203 requests), 217/1198 ms and
66.1/85.1 ms.

## Consequences

- A drain, a move, a scale-in or a scale to zero holds the outgoing pod
  up to 80 s longer than under ADR-0019: on one L4 at the 0.793 EUR/h
  estimated for the GPU node of B-005, about 0.02 EUR a drain.
- Every child pod rolls out once on upgrade.

## What this ADR does not prove

The simulator is not a capacity model of the L4. On the D3 ladder of
B-005, same sequences, TTFT p95 and p99 and TPOT p95 in ms, L4 / sim:

| keep, seed | TTFT p95 | TTFT p99 | TPOT p95 |
|---|---|---|---|
| 0.05, 42 | 1387 / 1204 | 2131 / 1210 | 99.5 / 77.5 |
| 0.05, 1 | 1297 / 1177 | 1706 / 1289 | 92.9 / 76.3 |
| 0.05, 2 | 1376 / 1237 | 1667 / 1298 | 109.4 / 78.2 |
| 0.075, 42 | 1779 / 1233 | 2894 / 1270 | 113.8 / 80.4 |
| 0.1, 42 | 1834 / 1260 | 2921 / 1298 | 127.4 / 87.8 |

The simulator met every objective of the L4 on all five; the L4 missed
the TPOT at keep 0.05 seed 2 and missed TTFT and TPOT at 0.075 and 0.1.
The medians agree, the tails do not: the simulator draws no dispersion
(no `-std-dev` flag), and one linear factor does not widen them. A
capacity read from it is at least twice the L4's. Fitting the tails is
open work, in a change of its own.

Not read: how vLLM ends a generation on SIGTERM; prompt tokens counted
by the simulator's tokenizer rather than Qwen's; the overload of B-005
at keep 0.25 on the simulator.
