# ADR-0025: The cost of a run per million generated tokens

Status: Accepted
Date: 2026-10-29
Follows: inferscope ADR-015 (cost attribution per trajectory step)

## Context

R7 asks for a cost model: the cost per million tokens per scenario, its
assumptions written, calibrated on the GPU runs of R6. inferscope
ADR-015 already defines how this platform derives a cost, over
inferscope's own reports; the load records of this repository are
another input, and need the same rules rather than new ones.

## Facts that constrain the design

1. The first line of a loadgen run is a `run_config` with `ts_start`; in
   profile mode it has no end (`duration_s` is null). Every request
   record has `ts_start` and `ts_end`.
2. B-005 ran one L4 node, estimated at 0.793 EUR/h with its GPU; the
   rate is an estimate from the provider's catalogue, not a billed rate.
3. On the profile, a request sends about ten prompt tokens for each
   token it generates (B-005, every run).

## Decisions

- D1. Cost is not in the load records or in `slo_check.py`: a separate
  `hack/loadgen/cost.py` derives it from an archived run, so a run can
  be priced again at another rate without repeating it (ADR-015 D1).
- D2. One basis, occupancy: the node is rented by the hour and its
  energy is in the price (ADR-015 D2).
- D3. The rate and its source are required on the command line and
  printed with every figure; no price list ships here (ADR-015 D3).
- D4. The window is the run's: from the `run_config`'s `ts_start` to the
  end of the last request (fact 1; ADR-015 D4).
- D5. Time with no request in flight is paid and printed apart; requests
  that overlap count once (ADR-015 D5).
- D6. The cost per million tokens is over the generated tokens of the
  requests `slo_check.py` counts as served; a run with none has no such
  figure (ADR-015 D6). Prompt tokens are printed beside it (fact 3).

## Measured, 2026-10-29

On the records of B-005 at 0.793 EUR/h, windows of 317 to 369 s; the
objectives are those of `slo-l4-qwen7b-bf16.json` judged on each run:

| run | idle | EUR per million generated tokens | objectives |
|---|---|---|---|
| keep 0.025, replay 1 | 18% | 8.07 | met |
| keep 0.05, seed 42 | 2% | 4.38 | met |
| keep 0.05, seed 1 | 0% | 4.97 | met |
| keep 0.05, seed 2 | 2% | 4.12 | TPOT missed |
| keep 0.075, seed 42 | 0% | 3.38 | missed |
| keep 0.1, seed 42 | 0% | 2.58 | missed |
| keep 0.25, seed 42 | 0% | 1.16 | missed |

Within its objectives one L4 costs 4.38 to 8.07 EUR per million
generated tokens on this profile. The price falls with the load only
past the capacity of ADR-0022, where the objectives no longer hold.

Six tests in `hack/loadgen/test_cost.py` cover D3 to D6; replacing the
union of request intervals with their sum makes the overlap test fail.

## Consequences

- A cost figure for this platform is a range tied to a load and its
  objectives, never one number.
- The figures move with the rate: the same records give another price
  at another rate, with no new run.

## What this ADR does not prove

- The window is not the invoice: provisioning, model load and idle time
  outside it are billed and not here (ADR-015 D7).
- Prompt tokens are not priced apart: a figure per generated token
  carries the prefill, and does not compare one to one with a price
  list that charges input and output separately.
- One profile, one model, one GPU class, one engine version, and an
  estimated rate; the keep 0.025 row is one replay of three.
