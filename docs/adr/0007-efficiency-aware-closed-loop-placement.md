# ADR-0007: Efficiency-aware placement closes the measure-actuate loop

Status: Accepted
Date: 2026-07-17

## Context

The project currently has a measurement system and an actuation system
that do not talk to each other. inferscope measures the two decisive
metrics — tokens/joule via the NVML energy counter (ADR-010) and
KV-cache hit-rate via the vLLM Prometheus scrape (ADR-011), extended to
trajectory granularity by ADR-013 — and the operator places, protects,
and recovers vLLM services on GPU fleets (ADR-0003..0006). The signals
end in Grafana dashboards; the placement decisions are made without
them. This ADR closes that loop.

Why the loop is worth closing is already quantified by our own
experiments. Cache-regime shifts move an agentic workload's energy
efficiency by up to **-69.2% tokens/joule** (18/18 matrix, Qwen2.5-32B,
H100, agentic-kv-energy-experiment), and ADR-0006 concluded from that
number that "orchestration policy is energy policy". The conclusion so
far only shaped *protection* (per-role warmth and recovery). It has not
yet shaped *selection*: when two nodes can host a service, the operator
picks by warmth, utilization, and service count
(`fleet_placement.rs::NodeCandidate`), blind to which node is running
in an efficient cache regime and which is burning joules per token.

The actuation surface is already prepared for this. Placement is a pure
function over `NodeCandidate` values, testable without a cluster
(ADR-0003). Anti-oscillation guardrails exist in the API
(`HysteresisSpec`: `stableReconcilesRequired`,
`maxConcurrentReschedules`). Observed node state already flows through
a per-node reporter into `NodeState.status` at a bounded cadence
(default 15s), which today carries `warmth`, `gpuUtilization`, and spot
status. Closing the loop is therefore an extension of three existing
contracts, not a new subsystem.

Five decisions follow, D1-D5.

## Decision

### D1 — The loop actuates placement scoring, nothing else

Efficiency signals refine the ranking of candidate nodes inside the
existing placement path. They do not drive replica autoscaling
(HPA/KEDA territory, a different claim with a much larger risk
surface) and they do not drive P/D role rebalancing (depends on the
ADR-0006 implementation phases, which have not landed). Both are
declared out of scope; the second is expected to become a follow-up
once roles exist at runtime.

The resulting claim is deliberately narrow: *energy- and cache-aware
placement, closed-loop, measured*. Placement is where the operator
already has authority, a validated cost model, and a pure function to
extend.

### D2 — Signals ride the existing NodeState reporter contract

The per-node reporter publishes two new observed values in
`NodeState.status`: `tokensPerJoule` (window-based) and
`kvCacheHitRate`, both `f32`, same writer and cadence as the existing
fields. inferscope remains the producer of the measurement — NVML
energy sampling and the `vllm:prefix_cache_*` scrape are exactly
ADR-010/011 — and the reporter consumes it locally rather than
reimplementing it. The operator stays purely watch-driven: no
Prometheus client, no query language, no coupling between loop
availability and metrics-stack availability, and the placement
function stays testable with synthetic candidates.

The demarcation line that also rejects reporter-side classification:
**status carries raw observed values (facts about the node); thresholds
and quantization live in the planner (fleet policy)**. `Warmth` is a
lifecycle state and belongs to the reporter; "what counts as
efficient" depends on model and workload and belongs to
`FleetServiceSpec` plus the pure function, where hysteresis already
lives. The reporter has no opinions; the planner's opinions are
configurable and testable.

Inherited limitation, stated explicitly: window-based attribution
under controlled concurrency (ADR-013) makes per-node tokens/joule a
reliable *trend* signal at window granularity, not per-request
accounting. For ranking placement candidates that is sufficient.

### D3 — Strict lexicographic ordering; warmth stays dominant

Signals enter as tie-breakers inside a warmth class, never across
classes. Default ordering:

    warmth > kvCacheHitRate > tokensPerJoule
           > gpuUtilization > activeServiceCount

The invariant survives intact by construction: a cold start costs
minutes, an efficiency delta costs percentage points. Quantities of
different orders of magnitude are ordered, not summed — which is also
why a weighted composite score is rejected (it breaks the warmth-first
guarantee, invites weight-tuning, and makes invariants untestable as
properties). A threshold *filter* (exclude nodes below an efficiency
floor) is rejected for v1 because a filter can shrink the candidate
set until it forces a cold start; it is noted as a possible evolution
once data exists on how hard a floor would bite.

Cache ranks **before** energy deliberately: agentic-kv shows the cache
regime is the cause and tokens/joule the effect (H0→H2 monotone; the
-69.2% is produced by regime shifts). Ordering on the effect after the
cause avoids double-counting one phenomenon and keeps the ranking
stable when tokens/joule fluctuates for reasons the cache already
explains.

The ordering is policy, so it lives in `PlacementSpec` as a new
`PlacementStrategy` variant (`EfficiencyAware`), the exact API-break
absorption that enum was created for. `WarmthFirst` remains the
default; existing FleetServices are untouched.

### D4 — The loop acts only at natural decision points

Efficiency signals *order* candidates when a placement decision must
be taken anyway — scale-up, recovery, spot reschedule. They never
*generate* decisions: no running service is drained or migrated
because a signal degraded.

The argument is quantitative, not cautious. The actuation cost of a
migration is 57s to Ready in the *best* case (warm spare +
`modelCacheHostPath`, item-4 evidence) plus 4-6 failed requests per
event, and minutes in the cold case. The signal that would justify it
is worth percentage points of a noisy window-based measure. The
payback for proactive migration exists only under extreme, persistent
degradation — a regime in which the node is likely failing for
reasons the existing recovery path already intercepts. A loop that
moves workload on small deltas of a noisy signal is the definition of
oscillation.

Signal-triggered rebalancing is declared out of scope *with its
guardrails named*, so the road is mapped rather than ignored: signal
quantization, `stableReconcilesRequired` extended to efficiency
signals, a minimum dwell time per placement, and the existing
`maxConcurrentReschedules` cap. Until those exist, "closed-loop" here
means: measure continuously, decide at the next actuation event — the
same semantics warmth already has.

### D5 — Scope and falsification

Out of scope for v1, explicitly:
- Role-differentiated efficiency (P/D): depends on ADR-0006 phases
  3-5; follow-up ADR once roles exist at runtime.
- Any actuation on GPU power caps or frequencies: different actuator,
  different ADR.
- Any interaction with InferencePool/EPP routing: endpoint-level
  energy-awareness belongs to the router layer
  (`endpoint-attribute-scorer` + `customMetrics` in llm-d make it pure
  configuration) and is complementary, not duplicated. Placement
  decides which node hosts a service; routing decides which endpoint
  serves a request. Two layers, two loops, kept distinct.

Falsification plan, three levels:
1. **Property tests on the pure function.** The dominant invariant as
   a property, not an example: no combination of efficiency signals
   may ever rank a lower-warmth candidate above a higher-warmth one.
   Plus in-class ordering tests for the full lexicographic chain.
2. **Zero-cost kind rehearsal.** A synthetic reporter writes
   `NodeState` objects with constructed signal patterns; assertions on
   the resulting placements. Nothing reaches a GPU node before this
   passes (standard session rule).
3. **GPU experiment (post 2026-08-11).** 2-3 k3s nodes on Lambda, two
   nodes at equal warmth in different cache regimes (one serving a hot
   shared-prefix workload), `EfficiencyAware` vs `WarmthFirst`
   baseline, fleet-level tokens/joule, repeated runs, mean±std, same
   measurement windows as agentic-kv. Falsifiable claim: under mixed
   workload, cache/energy-aware placement improves fleet tokens/joule
   beyond run-to-run noise. If the delta is within noise, the null
   result is published as such — "the signal at this granularity is
   insufficient" is a finding, not a failure.

## Consequences

- `NodeState.status` grows `tokensPerJoule` and `kvCacheHitRate`
  (`f32`, raw observed values). The reporter gains a dependency on
  inferscope sampling; degraded mode is defined: if either signal is
  unavailable, the fields are absent and the planner ranks exactly as
  today. The loop fails open to current behaviour.
- `PlacementSpec.strategy` gains the `EfficiencyAware` variant;
  `WarmthFirst` remains the default. No API break, no behaviour change
  for existing FleetServices.
- The planner gains the lexicographic comparator and, with it, the
  home for future thresholds; hysteresis semantics are unchanged in
  v1.
- Validation is sim-first on kind with a synthetic reporter, then one
  small GPU session (placement quality, not migration churn — a
  direct consequence of D4). Implementation and the GPU run are
  post-2026-08-11 blocks; this ADR is design only.
- This ADR is the orchestration-side counterpart of inferscope
  ADR-013: trajectory-level attribution measures the economics, this
  loop acts on them. The agentic-kv article (go-live ~2026-08-11)
  gains a forward reference: the measured signal becomes a placement
  input.
- Relation to ADR-0006: unchanged and single-role for now. When roles
  land, D1's rebalancing exclusion and D5's role exclusion are the
  two seams where a follow-up ADR plugs in.
