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
