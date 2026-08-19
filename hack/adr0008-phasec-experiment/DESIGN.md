# ADR-0008 phase C — EfficiencyAware placement, with traffic that reaches the placement

Design written 2026-08-18, before any node is booked. Frozen parameters are
marked as such; measured results go in a separate section after the run, and
anything learned that changes the design goes in a dated amendment rather than
back into the body.

## Why this design exists

The level-3 experiment (`hack/gpu-session/adr0007-ea-experiment/DESIGN.md`) ran
8/8 reps cleanly and could not test its own hypothesis. Its amendment of
2026-07-23 states the defect exactly: the replay drives one fixed vLLM endpoint,
deliberately, because that is how the design manufactures prefix-locality
asymmetry — but *"no component routes requests to the service the strategy has
just placed. The placed replica receives no traffic in either arm, so both arms
measure the same loaded node."*

The recorded deltas (+1.1pp hit-rate, −0.4% tokens/joule, both under threshold)
are on the record explicitly **not** as a negative result about EfficiencyAware.
The measurement path could not carry the effect regardless of strategy quality.

This design closes that loop. It is not a re-run with more repetitions: more reps
would tighten a confidence interval around a quantity that carries no signal.

## Hypothesis (falsifiable)

With traffic delivered to the placed service, at equal warmth, `EfficiencyAware`
places replicas on nodes where they run measurably more efficiently than
`WarmthFirst` does, beyond run-to-run noise.

A delta within noise is a negative result about EA under this workload, and is
published as one. Unlike the level-3 outcome, this design is entitled to that
conclusion, because the measurement path can carry the effect.

## What changed since the level-3 attempt

Three things, and each is verified rather than assumed:

**Traffic reaches the placement (D1).** `FleetService.status` publishes the
current placement — which node each slot sits on and the name of the owned child.
The dispatcher reads it and sends the replayed workload to the placed replica. It
lives in `hack/` as measurement instrumentation, not in the operator: the operator
reports where it placed and does not choose endpoints.

**The asymmetry mechanism is confirmed.** Gate 3 was flagged unverified until
2026-08-18 and now is not. On a Lambda A10: `sudo` available, persistence mode
already enabled, enforced power limit movable across 100W–150W, every write
confirmed by reading the value back. Under load the cap bites hard — generating
time 26.06s at 150W against 39.35s at 100W on an identical trajectory, +51%, with
tool wall identical by construction (evidence: `hack/gate3-evidence/`).

**Observations carry when they were measured (D2).** `NodeState.status` grows a
per-signal `observedAt`, the reporter retains last valid values, and the planner
applies a horizon. An expired observation is treated as never observed, not as
zero.

## Topology

Three A10 nodes on Lambda, k3s, one vLLM per node — the same shape the level-3
session used, because constraint P forces it: on single-GPU nodes a node that
differs by being loaded is a node where placement is impossible, so the
discriminating difference has to exist between *idle* nodes.

- **node-A, node-B** — the two placement candidates, idle, equal warmth,
  differing only by power cap
- **node-C** — carries the pre-load traffic that gives the reporter something to
  measure, and is never a placement target

## Experimental design

Per rep, in order:

1. **Cap assignment.** One of node-A/node-B is set to 100W, the other to 150W.
   Which physical node holds the low cap alternates across reps (see
   counterbalancing).
2. **Pre-load.** Drive both candidates under their caps until the reporter has
   written `tokensPerJoule` for each, with `observedAt` inside the horizon.
   This is what makes the signals exist; without it EA has nothing to order on.
3. **Drain.** Stop the pre-load, wait for the candidates to return to idle. The
   observations persist — that is what D2's retention is for.
4. **Decide.** Apply a `FleetService` with one slot and the strategy under test.
   The controller places within the declared horizon.
5. **Dispatch.** The harness reads `status.placements[0]` and sends the replayed
   workload to that child's Service.
6. **Measure.** Over a declared window on the placed replica.

Two arms — `EfficiencyAware` and `WarmthFirst` — in ABBA+BAAB order, 8 reps
total, the same ordering discipline the level-3 run used.

## Design parameters (inputs, frozen before the run)

| parameter | value | why |
|---|---|---|
| power caps | 100W / 150W | the widest the A10 allows; measured to separate generating time by 51% (gate 3, 2026-08-18) |
| model | Qwen2.5-7B-Instruct | continuity with every campaign in this repo |
| engine | vLLM 0.23.0, `--enforce-eager` | matrix invariant: CUDA graphs are a second lever with their own sign inversion |
| workload | the cost campaign's replay trajectory | deterministic; 1.8% spread between replicas of one cell |
| reps | 8, ABBA+BAAB | same as level 3 |
| horizon | to be fixed at the pre-flight gate | must exceed drain duration or step 4 sees no signals |

The cap asymmetry is a **design parameter**, exactly as prefix-locality was at
level 3. It manufactures the discriminating condition; it is not a finding.

## Decision criterion (fixed now, before any run)

**Primary metric:** tokens/joule of the **placed replica**, over the declared
window. Fleet aggregates are explicitly not primary — that is the mistake the
level-3 amendment recorded, and repeating it would reproduce a number dominated
by a term identical across arms.

**Relevance threshold:** +3% on the primary, consistent with the level-3 design.

**Guardrails, non-regression:** TTFT and TPOT p50/p99. A strategy that buys
efficiency by making the service slower has not won.

Verdict, decided by the numbers rather than by reading them:

- delta ≥ +3% with CI clear of zero → **hypothesis supported**
- CI straddling zero → **negative result about EA under this workload**, published
- delta ≤ −3% → EA places worse than WarmthFirst; also published

**Counterbalancing, and why it is not optional.** WarmthFirst breaks a tie between
two idle equal-warmth nodes on `activeServiceCount` then `gpuUtilization`, and
where those tie it resolves by list order — `max_by` returns the last maximal
element and the candidate set preserves the `NodeState` list verbatim. That order
is not a documented API guarantee, which is the point: the design must not depend
on knowing it either way. If the high-cap node were always the same physical
machine, WarmthFirst would systematically win or lose by construction, and the
measured delta would be an artefact of that alignment. Which node holds which cap
therefore alternates across reps.

## Gates, in order

**1. Property tests (zero cost).** Expired-equals-never-observed; warmth
dominance under every combination of signals, ages and absences; the full
lexicographic chain in its D4 order.

**2. Kind rehearsal (zero cost).** The whole harness end to end with metrics
fixtures: capacity filter, published placement and dispatcher, retention and
horizon, the pre-load/drain/decide sequence, evidence layout, verdict logic.
The level-3 lesson applies — exercise every flag the GPU run will use, including
the ones only the GPU path reaches.

**3. Pre-flight on the node, before the first rep.** Three checks that each have
an abort criterion, because discovering any of them mid-run wastes the session:

- the cap can be set on **all three** nodes, not just the one gate 3 tested
- the horizon exceeds the drain duration — if a signal expires during the drain,
  step 4 decides on nothing and both arms degrade to the same fallback
- the dispatcher resolves `status.placements[0]` to a reachable Service, verified
  with one request before any measured rep

## Cost envelope

Three A10 at $1.29/h — the rate actually paid on the cost campaign, not a list
price. The level-3 session ran 8 reps on this topology; its cost was never
recorded in the repo, so there is no figure to extrapolate from and the envelope
here is derived from the rate and the expected wall-clock instead. Each rep adds
a pre-load and a drain to what level 3 did. Budget **one hour of three nodes,
about $4**, and abort if the pre-flight is not clean within the first half
hour. The measured traffic itself is minutes; the cost is getting there.

## What this design does not settle

**It does not test routing policy.** The dispatcher sends traffic to the placed
replica because that is what makes placement observable. Which endpoint serves a
given request, when there are several, stays where ADR-0007 D5 put it — the
router layer, as configuration rather than code.

**It does not close constraint P.** Signal source and placement target remain
mutually exclusive on single-GPU nodes; this design works around P with a
pre-load and a drain rather than removing it.

**It says nothing about multi-tenant nodes** (ADR-0008 fact 4: the scrape is
node-pooled and not attributable when more than one engine runs on a node), nor
about engines other than vLLM.

**And the asymmetry is manufactured.** A 100W-vs-150W cap is not a condition a
production fleet encounters; it is the cheapest way to make two idle nodes differ
measurably before a decision. If EA shows an effect here, what transfers is that
the strategy acts on the signal it claims to act on — not a prediction of how
much it would buy on hardware that differs for other reasons.

## Measured results (outputs)

Run 2026-08-19, 3x A10 on Lambda, k3s v1.36.3, vLLM 0.23.0,
Qwen2.5-0.5B-Instruct, --enforce-eager. Evidence: `runs/20260819-session/`.

**Verdict against the criterion fixed before the run: negative result.**

| | EA | WF |
|---|---|---|
| placed-node tokens/joule | 10.384 (sd 0.094) | 10.175 (sd 0.277) |
| n | 4 | 4 |

Delta +2.05%, below the +3% relevance threshold, with a 95% CI of
[-2.5%, +6.6%] straddling zero. Under the decision criterion that is a
negative result about EfficiencyAware under this workload, and it is
published as one.

**Unlike the level-3 outcome, this design was entitled to conclude it.**
The measurement path carried the effect and the two strategies diverged
deterministically: EA placed on the node with the better tokens/joule in
4 of 4 reps, alternating across both physical machines, while WarmthFirst
took the same physical node every time — by list order, exactly as the
counterbalancing section anticipated. `decidedOn` recorded a live signal
on all eight reps, so the planner demonstrably ranked on real inputs.

**The asymmetry came out with the opposite sign to the one assumed.** The
design treats the cap as something that makes a node worse. Measured, the
capped node is *more* efficient — 10.41 tokens/joule at 100W against 9.79
at 150W — which is the well-known result that reducing power improves
energy efficiency per token while lowering throughput. The experiment
remains valid, because the hypothesis is about EA ranking on the signal
rather than about the direction of the cap, but it explains the size of
the delta: the two candidates differ by about 6%, so a strategy that
always picks the better one can move the mean by at most half of that.
A +3% threshold against a 6% available margin is a demanding test, and it
was fixed before any of this was known.

**What this does not license.** It says nothing about EA under a larger
node asymmetry, on heterogeneous hardware, or with a workload where the
placed replica runs long enough for compounding to matter. Each rep
measured a short burst on a freshly placed replica; the design chose that
for cost, and a longer window is the obvious next variant.

## Session notes

Total cost ~$6, against the $4 envelope — the overrun is entirely setup,
recorded above, not measurement.

The pre-flight earned the session. It found that the reporter published
`Some(0.0)` for an idle GPU, which would have left both candidates with
identical inputs after every drain and produced a null delta determined
by the instrument rather than the strategy — the level-3 failure
reproduced, and harder to spot the second time because all the machinery
built to prevent it was in place and working. Fixed before any arm was
spent (`9df920d`).

## Cluster setup, as it actually took (2026-08-19)

Recorded because three of these cost real minutes on a metered node and none
of them is guessable from the design.

**The NVIDIA device plugin needs `runtimeClassName: nvidia` on k3s.** Without
it the DaemonSet runs, reports "Incompatible strategy detected auto", and every
node advertises no allocatable GPU. The D3 capacity filter then excludes all
three nodes and nothing places — a pre-flight abort caused entirely by setup.

**The reporter image needs three build args, not one.** `CARGO_FEATURES=gpu-nvidia`
alone yields "built without feature" gone but "Dynamic loading not supported":
nvml-wrapper dlopens libnvidia-ml.so and a static musl binary cannot. The
Dockerfile documents the full recipe — `RUST_TARGET=x86_64-unknown-linux-gnu`,
`CARGO_FEATURES=gpu-nvidia`, `RUNTIME_IMAGE=gcr.io/distroless/cc-debian12:nonroot`.

**And `runtimeClassName` alone does not inject the driver libraries.** The
container must also ask for them: `NVIDIA_VISIBLE_DEVICES=all` plus
`NVIDIA_DRIVER_CAPABILITIES=utility`. Utility rather than compute on purpose —
the reporter reads NVML and never launches a kernel, and requesting a GPU
resource would consume the one the experiment is trying to measure.

Each failure was visible in the reporter log with a distinct message, which is
why they took minutes rather than the session: "built without feature", then
"Dynamic loading not supported", then "libnvidia-ml.so.1: cannot open shared
object file". A fail-open that stayed silent would have produced eight clean
reps with every efficiency signal absent, and both arms falling back to
warmth-first — the level-3 outcome, reproduced exactly.
