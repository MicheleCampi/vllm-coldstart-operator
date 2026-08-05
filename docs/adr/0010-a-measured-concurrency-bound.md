# ADR-0010: A measured concurrency bound, and the experiment that can falsify it

Status: Proposed
Date: 2026-08-04

## Context

The cost campaign of 2026-08-04 (`agentic-kv-energy-experiment`, phase 2,
15 cells on 1xA10) measured what fraction of an agentic trajectory's wall
time the GPU is allocated and not generating. Across the interval the
source paper documents for tool time (Fig. 7, 2-29%), the non-generating
fraction runs 2.30% -> 37.01% and the derived packing bound
`1 / (1 - f_nongen)` runs 1.02 -> 1.59. The cost of generating held flat
within a 0.5% band across every cell at identical token counts, so the
entire +56% in $/M token over that sweep is waiting rather than work.

That bound is an upper bound under *declared non-interference*: it says
how many trajectories a replica could host if the generating segments
never contended. Real batching does not work that way. The bound has
therefore never been tested against a running engine, and publishing it
as a capacity figure without testing it would be asserting a property of
a system rather than a property of a measurement.

This ADR designs the test.

It also picks up the hypothesis ADR-0007 could not close. That experiment
placed a replica by an efficiency-aware strategy and measured no
difference between arms, and the recorded reason was not strategy
quality: nothing routed traffic to the replica the strategy had just
placed, so both arms measured the same loaded node. Here traffic reaches
the replica under test by construction — the driver *is* the traffic — so
the measurement path carries the effect it is asked to carry.

## Facts that constrain the design

Established by source review before writing, each cited to the file that
establishes it, because each closes off an option that would otherwise
look reasonable.

1. **The reporter cannot compute `f_nongen`.** `Real::sample`
   (`src/bin/reporter.rs`) reads five vLLM series —
   `num_requests_waiting`, `num_requests_running`, `prefix_cache_hits`,
   `prefix_cache_queries`, `generation_tokens_total` — all instantaneous
   and node-pooled. The non-generating fraction is a property of a
   trajectory, derived offline from a driver steps-file joined onto
   sampled timelines (ADR-013 in inferscope). No engine-side series
   carries it. A `packingBound` field on `NodeState` would have no
   writer.
2. **`num_requests_running` is the observable the bound predicts.** It is
   already sampled (`src/bin/reporter.rs:332`) and already published
   (`NodeStateStatus::requests_running`, ADR-0009 D2). If N trajectories
   are driven concurrently against one replica and each spends
   `f_nongen` of its span outside generation, the time-averaged running
   count is `N * (1 - f_nongen)`, not N. The gap between the two is the
   bound, observed from the engine rather than derived from the driver.
3. **The scrape is node-pooled, not per-service** (ADR-0008 fact 4).
   Every claim here is single-engine-per-node, as on the A10 hardware
   used by both campaigns. Multi-tenant nodes are out of scope.
4. **Absence is not zero.** Every signal on `NodeStateStatus` is
   `Option<T>` with `skip_serializing_if`, and the doc comments state
   why: a serde-default `0.0` would read as "idle node" and bias
   placement toward unmeasured nodes (ADR-0007 fail-open contract). Any
   figure this ADR adds inherits that discipline.

## Decisions

**D1 — The bound is not published as a field.** No `packingBound` on
`NodeStateStatus`. Fact 1 says no writer could produce it, and a field
whose only possible value is absence is worse than no field: it invites a
consumer to branch on it. The bound stays where it was measured, in the
cost campaign's evidence, and this ADR tests it rather than exporting it.

**D2 — The observable is time-averaged `requests_running` against driven
concurrency N.** For a replica driven by N concurrent replay trajectories
at a fixed tool latency, the prediction is
`mean(num_requests_running) ≈ N * (1 - f_nongen)`, with `f_nongen` taken
from the cell of the cost campaign at that same tool latency. Both sides
are already instrumented: the left by the reporter (fact 2), the right by
`analyze_cost_decision.py`.

**D3 — The falsification criterion is stated before the run.** The bound
is *ruled out as a capacity figure* if the observed mean running count is
within 10% of N — that would mean trajectories do not overlap at all and
the idle windows are not being filled by the scheduler. It is *supported*
if the observed mean falls within 15% of `N * (1 - f_nongen)`. Anything
between the two is reported as it lands, with the interval, and closes
nothing. The thresholds are declared here so that no reading of the
result can be chosen after seeing it.

**D4 — Two arms on the same replica, N=1 and N=ceil(bound).** The N=1 arm
is the anchor: it reproduces the single-trajectory span the cost campaign
measured, on this hardware, and any divergence there invalidates the
comparison before the second arm runs. The N=ceil(bound) arm is the test.
Same model, same engine, same tool latency, same seeds — the only thing
that moves is concurrency, which is the same two-arm discipline the cost
campaign used for latency.

**D5 — Throughput is the second reading, and it is the one that matters
operationally.** Trajectories per minute at N=1 against N=ceil(bound). If
the bound holds as capacity, throughput scales by roughly the bound and
per-trajectory span is unchanged. If contention dominates, span inflates
and throughput scales sublinearly — which is a publishable result about
the bound's operational meaning, not a failed experiment. The energy
figure (tok/J from the reporter) is recorded alongside but is not a
criterion: this ADR is about capacity, and mixing the two would make
neither conclusion clean.

## Consequences

**No CRD change, no controller change.** D1 removes the only field this
ADR would have added, and D2 uses signals that already exist. What has to
be built is the harness: a driver that runs N replay trajectories
concurrently against one endpoint, and a sampler that records
`num_requests_running` over the window. Both are extensions of code that
exists — `run_replay.py` for the trajectory, the reporter's scrape loop
for the observable — not new subsystems.

**The operator gains a claim it can defend, not a feature.** If the
result supports the bound, the operator's placement decisions acquire a
capacity figure that came from measurement rather than from a tuned
constant. If the result falsifies it, the campaign's packing bound stays
a correct upper bound and loses its operational reading, which is worth
recording precisely because the temptation is to use it as capacity
without checking.

**Prerequisite that is not free.** The concurrent driver does not exist.
`run_replay.py` drives one trajectory; ADR-013 attribution wants one
trajectory in flight, which is why phase 2 was built that way. This
experiment deliberately breaks that constraint, and therefore *cannot*
use per-step attribution: with N trajectories overlapping, the wall-clock
join that ADR-013 performs has no unique owner for a sample. The
measurement here is engine-side only — running count and throughput — and
that limitation is structural, not an omission.

## What this ADR does not prove

- It does not make the operator a traffic-carrying system. ADR-0008's
  non-goals stand unchanged: this is a rigorous experimental instrument,
  not a component that has served production load.
- It does not close ADR-0007's efficiency-aware placement hypothesis. It
  removes one of the two reasons that hypothesis could not be tested —
  traffic now reaches the replica under test — and leaves the other, the
  constraint P of ADR-0008, untouched.
- It says nothing about multi-tenant nodes (fact 3) or about engines
  other than vLLM.
- A supported bound is evidence for one model, one GPU class and one
  trajectory shape. The generalisation is the design, not the number.
