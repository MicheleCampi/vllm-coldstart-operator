# ADR-0007 level 3 — EfficiencyAware vs WarmthFirst (GPU experiment design)

Status: design frozen before execution (2026-07-19). Everything in
"Design parameters" is an input choice; everything in "Measured results"
is an output. The two must never be conflated in the writeup.

## Hypothesis (falsifiable)

At equal warmth, EfficiencyAware placement selects nodes with higher
observed prefix-cache hit-rate, and under a workload with prefix
locality this yields a measurable advantage over WarmthFirst in
fleet-aggregate hit-rate and tokens/joule. If the measured delta is
~0 (below the relevance threshold), EA does not pay for its complexity
under this workload — a negative result, reported as such.

## Topology

- 3× A10 (Lambda), k3s, same layout as the item-4 rehearsal.
- One vLLM per node (digest-pinned, same model as item-4:
  Qwen2.5-7B), `modelCacheHostPath` warm on all nodes.
- Reporter DaemonSet in Real mode: `REPORTER_SCRAPE_TARGETS_NODE_<n>`
  pointing at the node-local vLLM `/metrics` — the exact chain
  validated by the kind real-source rehearsal (6/6, commit 62f5d69).
- inferscope attached per-node for tokens/joule (NVML clock base,
  ADR-010) on the same window as the hit-rate scrape (ADR-011).

## Experimental design

Within-subject A/B: the same recorded workload is replayed against a
FleetService with `strategy: EfficiencyAware` and one with
`strategy: WarmthFirst`. Repetition order ABBA (then mirrored: BAAB on
the second pair) to neutralise thermal and cache drift. Between reps:
vLLM restart on every node (prefix cache reset), fixed cool-down.

The workload manufactures prefix-locality asymmetry: a configured
fraction of requests share long common prefixes, routed so that one
node accumulates high observed hit-rate before the placement decision
under test is triggered. This asymmetry is a *design parameter* — it
creates the discriminating condition; it is not a finding.

Placement decisions under test: (a) initial placement of a new
FleetService replica during the loaded phase; (b) replacement after a
simulated preemption (same two code paths as level-2 falsification).

## Design parameters (inputs, frozen before the run)

| Parameter | Value | Rationale |
|---|---|---|
| shared-prefix fraction | 0.6 | strong but not degenerate locality |
| shared prefix length | ~1500 tok | well past vLLM block size, realistic RAG/system-prompt scale |
| unique tail length | 100-300 tok (uniform) | avoids identical requests |
| output length | 128 tok fixed | TPOT window comparable across reps |
| request rate | 8 RPS steady | below saturation on A10/7B (from cuda-graphs data) |
| warm-up window | 120 s, excluded | reporter needs >=2 scrape rounds + rate stabilisation |
| measurement window | 300 s per rep | >= 10 scrape intervals |
| reps | 4 per strategy, ABBA+BAAB | bootstrap needs >=4; budget-bounded |
| seed | fixed per rep index | same workload replay across strategies |

## Measured results (outputs)

Primary:
- fleet-aggregate prefix-cache hit-rate (scrape deltas, per window)
- tokens/joule per node and fleet-aggregate (inferscope, same window)

Secondary / mechanism:
- placement decision taken by each strategy in (a) and (b)
- per-node request distribution over the window

Guardrails (non-regression, not optimisation targets):
- TTFT p50/p99, TPOT p50/p99 — EA must not degrade latency beyond
  noise while chasing cache locality.

## Decision criterion (fixed now)

Per-rep deltas (EA − WarmthFirst) on the primary metrics, mean with
bootstrap 95% CI over reps. Relevance thresholds, declared before any
data: +5pp fleet hit-rate, +3% fleet tokens/joule. Outcomes:
- CI above threshold on either primary -> EA advantage claimed, scoped
  to this workload class.
- CI straddling zero or below threshold -> negative result: EA does
  not pay for its complexity here. Published with the same rigour.
- Guardrail regression beyond noise -> reported as a cost regardless
  of primary outcome.

## Cost envelope

3× A10 at Lambda rates, target < $12: ~8 reps × (120s+300s) + setup
and teardown, rehearsed at zero cost on kind first (harness below).

## Kind rehearsal scope (zero cost, before any GPU minute)

The full harness must run end-to-end on kind with the metrics
fixtures standing in for vLLM: scenario orchestration, scrape-window
accounting, evidence layout, verdict logic. Only the numbers are fake;
every moving part of the harness is real. GPU session then pays for
execution only.

## Amendment 2026-07-22 (pre-run, during RPS verify gate)

Request rate 8 -> 2 RPS. The 8 RPS figure came from H100 cuda-graphs
data and was explicitly flagged for re-verification on A10. Measured on
the session hardware (A10, Qwen2.5-7B, this exact workload shape):
- 8 RPS: p50 12.1s, wall 95.8s on a 60s workload -> saturated, pacing lost
- 4 RPS: p50 10.4s, wall 65.0s -> still saturated
- 2 RPS: p50 5.9s, p95 6.1s, wall 64.3s, per-quartile p50 flat
  (5.8-6.0s) -> steady state, no queue growth
Amended BEFORE any measured rep; all reps run at 2 RPS. Measurement
window unchanged (300s -> 600 requests/rep, still >= 10 scrape rounds).

## Amendment 2026-07-23 (post-run, design limitation found in analysis)

Recorded AFTER the full 8-rep run (evidence: runs/20260723T175747).
It changes no protocol parameter — the run stands as executed. It
scopes what the measured numbers can and cannot support.

The primary metrics as specified — fleet-aggregate prefix-cache
hit-rate and fleet-aggregate tokens/joule — cannot discriminate
EfficiencyAware from WarmthFirst on this topology. The replay drives
one fixed vLLM endpoint, which is deliberate: it is how the design
manufactures the prefix-locality asymmetry that makes the placement
decision discriminating. But no component routes requests to the
service the strategy has just placed. The placed replica receives no
traffic in either arm, so both arms measure the same loaded node, and
the fleet aggregate is dominated by a term that is identical by
construction across strategies.

Measured deltas (EA - WF), 4 reps per arm, for the record:
- hit-rate: 0.8179 (sd 0.0124) vs 0.8068 (sd 0.0290) -> +1.1pp,
  threshold +5pp
- tokens/joule: 1.657 (sd 0.088) vs 1.663 (sd 0.077) -> -0.4%,
  threshold +3%

Both fall in the "CI straddling zero" branch of the decision criterion.
They are NOT reported as a negative result about EA. A negative result
means the strategy was given a fair chance to show an effect and did
not; here the measurement path could not carry the effect regardless of
strategy quality, so the near-zero delta was determined before any GPU
minute was spent. Publishing it as evidence that EA does not pay for
its complexity would defend a number that does not measure what it
claims to measure.

What the run does establish, on real hardware:
- the in-vivo signal chain: NVML sampler + vLLM scrape feeding
  NodeState status on all three nodes, cross-source tokens/joule join
  live under load
- the comparators consume those signals and diverge deterministically
  at both decision points, reproducibly across reps
- harness, evidence layout, and verdict logic at full scale, 8/8 reps

Testing the primary hypothesis requires closing the loop: a routing
layer that sends traffic to the placed service, so that a placement
decision changes which node accumulates cache. That is a distinct
piece of work (ADR-0007 phase C or an ADR of its own), not a re-run of
this design — repeating these reps with more repetitions or longer
windows would tighten a confidence interval around a quantity that
carries no signal.
