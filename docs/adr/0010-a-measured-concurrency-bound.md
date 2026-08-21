# ADR-0010: A measured concurrency bound, and the experiment that can falsify it

Status: Accepted, measured 2026-08-16 (see postscripts)
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

## Postscript, 2026-08-07 — D4 amended: ceil(bound) is not enough to pick a cell

D4 says the test arm runs at `n = ceil(bound)`. Working the arithmetic
before booking a node shows that under-specifies the experiment, and at
three of the four latencies the campaign measured it makes D3
undecidable.

`ceil(bound)` is 2 at every cell of the sweep — the bound runs 1.02 to
1.59, so it always rounds to two trajectories. But D3's two criteria are
bands, and whether they separate depends on `f_nongen`, not on N:

| tool latency | f_nongen | predicted (N=2) | +15% band | −10% of N | gap |
|---|---|---|---|---|---|
| 0.2 s | 2.30% | 1.954 | 2.247 | 1.800 | **−0.447** |
| 0.5 s | 5.55% | 1.889 | 2.172 | 1.800 | **−0.372** |
| 2.0 s | 19.01% | 1.620 | 1.863 | 1.800 | **−0.063** |
| 5.0 s | 37.01% | 1.260 | 1.449 | 1.800 | +0.351 |

A negative gap means the bands overlap: an observed mean landing in the
overlap satisfies "within 15% of N*(1−f_nongen)" *and* "within 10% of N"
at once, so D3 would return two opposite verdicts on the same number.
That is not a noisy experiment, it is an undecidable one, and no amount
of replicas fixes it.

**D4 is amended: the test arm runs at 5.0 s/tool, N=2.** It is the only
cell of the campaign where the criteria separate, with 0.351 of clear
space between the bands — and it is also the cell where the effect under
test is largest, since 37% of the trajectory is non-generating there.
The two happen to coincide, which is not luck: the further `f_nongen`
sits from zero, the further the prediction sits from N.

The anchor arm stays at N=1 as D4 states, at the same 5.0 s/tool, so the
single-trajectory span can be compared against the cost campaign's cell
directly.

What this does not change: the thresholds themselves. Widening the 15%
or narrowing the 10% to make a lower-latency cell decidable would be
choosing a criterion to fit the cell, which is the thing D3 exists to
prevent. The cell moves, the criteria do not.

One consequence to state now rather than discover on the node: at 5.0
s/tool a trajectory spans roughly 40 seconds, so an arm of N=2 with
three replicas is about four minutes of node time. The session is
minutes of GPU, not hours; the cost is in getting there, not in running
it.

## Postscript, 2026-08-16 — measured: the bound holds, and the reason is not the one D2 assumed

Both arms ran on 1×A10 24GB (driver 580.105.08), vLLM 0.23.0,
Qwen2.5-7B-Instruct, `--enforce-eager`, at the 5.0 s/tool cell the
2026-08-07 postscript fixed. Three replicas each at the cost campaign's
seeds. Zero failed scrapes across 166–167 samples per arm; observed spans
40.4–40.7s against the 40.53s the campaign measured.

| arm | f_nongen | predicted | observed | delta |
|---|---|---|---|---|
| N=1 (anchor) | 36.91% | 0.6309 | 0.6148 | −2.6% |
| N=2 (test) | 36.81% | 1.2639 | 1.2434 | −1.6% |

**D3 returns `BOUND SUPPORTED` on all six replicas**, against a 15%
tolerance and an exclusion threshold of 1.80. The anchor arm cleared its
own gate first: 0.61 against 0.90, so the engine does not hold a request
in `running` through the driver's tool sleeps and D2's observable means
what this ADR assumed it meant.

**And then the sample series says something the summary statistic hides.**

Within the measurement window the running count is almost never 1. In one
N=2 replica it is only ever 0 or 2 — 62% of samples at 2, 38% at 0 — and
in the other two the intermediate value appears but stays rare. The
idle fraction is 36.8–38.0% across all six arms, which is `f_nongen` to
within a point.

That is not two trajectories filling each other's gaps. It is two
trajectories marching in step: identical structure, identical sleep
durations, launched milliseconds apart, so they generate together and
wait together. The mean of 1.2434 is `2 × 0.62`, not a smoothed overlap.

So the arithmetic prediction is confirmed and the mechanism behind it is
not the one the design had in mind. `N × (1 − f_nongen)` is right here
because the trajectories share their idle time rather than because they
stagger it. A replica hosting N synchronised trajectories is idle for the
same fraction as one hosting a single trajectory — the GPU is not being
packed, it is being multiplied.

This does not weaken the capacity claim: the observed count tracks the
prediction, and D3's criterion was declared before the run. It narrows
what the claim covers. The bound is confirmed for trajectories of this
shape driven this way, and the interesting case — trajectories whose idle
windows genuinely interleave — was unmeasured when this was written; it was
measured on 2026-08-21 and the postscript at the end of this file records
what changed. Deliberate replay
determinism is what makes the two arms comparable and it is also what
makes them synchronous; the two cannot be had at once in this design.

The natural follow-up is one line of harness: a randomised start offset
per trajectory, large enough to break the lockstep and small enough to
preserve the all-in-flight window. That is a new experiment with its own
falsification criterion, not an amendment to this one.

## Postscript, 2026-08-21 — the interleaved case, measured

The postscript above left one thing open: whether trajectories whose windows
genuinely interleave behave differently from the lockstep pair this ADR
measured. They do, and the effect is large.

Eight reps on 1×A10, same model and engine as this ADR, offset 0 against 2.5s —
half a tool call at the 5.0 s/tool cell — with everything else held
([design and evidence](https://github.com/MicheleCampi/agentic-kv-energy-experiment/tree/main/hack/adr0010-interleaving)):

| | synchronised | staggered |
|---|---|---|
| samples at `running == 1` | 0.61% | **40.69%** |
| samples at `running == 0` | 38.5% | **18.0%** |
| mean running count | 1.2477 | 1.2010 |

**Idle time halves.** A replica serving two synchronised trajectories does
nothing for 38.5% of the window; stagger the starts and that falls to 18.0%,
with a standard deviation of zero across four reps. The trajectories fill each
other's pauses, which is exactly what this ADR's design assumed and its raw
series showed was not happening.

**And the mean falls while the GPU gets busier**, 1.2477 to 1.2010. Staggering
converts time at 2 into time at 1 and time at 0 into time at 1; the first shift
costs more than the second gains. A capacity calculation built on the mean would
read staggered arrival as packing *worse*.

**What this means for the bound.** The bound is not wrong — it predicted this
ADR's lockstep measurement and was supported on six arms. But the quantity it
predicts is phase-dependent, and lockstep is the least favourable phase. A fleet
sized from these numbers is sized for trajectories that arrive together, which
is not how a production fleet receives them.

Not settled: one offset at one N gives the existence and the size of the effect,
not its shape. Whether the benefit saturates, and what happens at larger N where
more trajectories compete for the same batch, is unmeasured.
