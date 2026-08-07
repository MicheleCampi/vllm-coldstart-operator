# ADR-0008: Placement is published, and observations have a horizon

Status: Accepted, deferred, amended in part (see postscript)
Date: 2026-07-27

Deferred means the design is settled and the implementation is not
scheduled next. The queue ahead of it is a market-positioning call
recorded on 2026-07-26: the corpus is vLLM-monoculture, and an
SGLang-versus-vLLM comparison on agentic prefix reuse (ADR-014, its
CPU-only prerequisite) answers a question that hiring teams are
actually asking, while efficiency-aware placement — the better problem
— answers one they are not. This ADR resumes after that block.

## Context

ADR-0007 closed the measure-actuate loop on paper and the level-3 GPU
session (2026-07-23, 3x A10, 8/8 reps ABBA+BAAB, 2 pass / 0 fail, zero
aborts) validated the mechanism in vivo: the NVML sampler and the vLLM
scrape feed `NodeState.status` on all three nodes, the cross-source
tokens/joule join is live under load (1.52-1.72), and the two
comparators diverge deterministically at both decision points, reproducibly
across reps.

The primary hypothesis did not close, and not because of a bug. The
post-run amendment to that experiment's DESIGN.md states the defect: the
replay drives one fixed vLLM endpoint — deliberately, since that is how
the design manufactures prefix-locality asymmetry — but nothing routes
traffic to the service the strategy has just placed. The placed replica
receives no traffic in either arm, so both arms measure the same loaded
node and the fleet aggregate is dominated by a term identical by
construction. The recorded deltas (+1.1pp hit-rate, -0.4% tokens/joule,
both under declared thresholds) are on the record explicitly *not* as a
negative result about EfficiencyAware: the measurement path could not
carry the effect regardless of strategy quality.

This ADR designs what makes the hypothesis testable. Source review
before writing established six facts that constrain the design; each is
cited to the file that establishes it, because each one closes off an
option that would otherwise look reasonable.

1. **The placed replica is already addressable.** `build_service`
   (`src/main.rs`) applies one Service per `VllmService`, selector
   `app: <name>`, same server-side-apply manager as the Deployment. No
   new owned resource is needed to reach a placed replica.
2. **Fleet membership does not reach the pods.** The fleet controller
   labels each owned child `inference.michelecampi.dev/fleet=<fleet>`
   in its *metadata* (`src/fleet_controller.rs`), and lists members by
   that selector. `build_deployment` builds pod labels from scratch
   (`app`, `managed-by`) and never reads the parent's metadata, so no
   Kubernetes selector can currently address a fleet as a whole.
3. **Pod labels and the Deployment selector share one map.** In
   `build_deployment` the same `labels` map feeds the pod template, the
   `LabelSelector`, and the Deployment metadata. A Deployment's selector
   is immutable after creation, so adding a key to that map would break
   apply on every existing Deployment.
4. **The scrape is node-pooled, not per-service.** `VllmScrape::sample`
   sums `vllm:prefix_cache_hits` and `_queries` across all configured
   targets and emits a single ratio; `generation_tokens_total` deltas
   are summed the same way. With more than one engine on a node, the
   value is dominated by the busiest and is not attributable.
5. **Signal source and placement target are mutually exclusive on this
   hardware.** Each vLLM requests `nvidia.com/gpu: 1` with
   `--gpu-memory-utilization 0.90`. On single-GPU A10 nodes, a node
   running an engine (and therefore producing a signal) cannot accept a
   placement, and a node that can accept a placement is idle and
   produces no efficiency signal. Call this constraint P; it is the
   central design problem of this ADR.
6. **Absence is destructive, and the existing timestamp is the wrong
   one.** The reporter patches status with RFC 7386 merge semantics, and
   `None` serializes as `null`, which *deletes* the key — the ADR-0007
   fail-open contract, working as intended. A previous value therefore
   does not survive in status. `lastReportTime` records when the reporter
   last *wrote*; the reporter is a DaemonSet and keeps writing after its
   target engine is gone, so that field stays fresh while the efficiency
   value it accompanies is stale or absent.

Five decisions follow, D1-D5.

## Non-goals and operational maturity

Stated first, because the rest of this document is easier to read
honestly against it. This operator is a rigorous experimental
instrument for one question — orchestration policy as energy policy,
measured — and not a system that has carried production traffic. The
engineering practices are production-grade; the operational envelope is
not. Known gaps at the time of writing, none of which this ADR closes
except where noted:

- No capacity awareness in the planner (D3 closes this, narrowly and
  only for the GPU-request case).
- Node-pooled scrape attribution (fact 4): multi-tenant nodes are out
  of scope for every claim made here.
- Single-reconciler rollout, pinned in the chart; no leader election.
- Operator CI runs clippy without `-D warnings`, unlike inferscope.
- Validation is kind rehearsals plus short k3s sessions at n=4 per arm.
  No soak, no upgrade path exercised, no multi-tenancy.

The claim this ADR is built to support is scoped accordingly and must
not drift upward in any downstream artefact.

## Decision

### D1 — Placement is published; the harness dispatches

`FleetService.status` gains the current placement: per slot, the node
it sits on, the name of the owned child, and the node attributes that
informed the decision. This is a read-only report of a decision the
controller has already taken, written to a resource it already owns
and already patches.

Traffic reaches the placed replica through the Service that exists
today: `build_service` applies one per `VllmService` (fact 1), so every
member is addressable by name already. The experiment's replay driver
reads the published placement and dispatches accordingly. That
dispatcher lives in `hack/` and is measurement instrumentation, not a
product component — it makes the placement decision observable and it
ships with the experiment, not with the operator.

The alternative considered and rejected: one Kubernetes Service
selecting all fleet members. It would require the fleet label
propagated onto the pod template (fact 2) and the label map in
`build_deployment` split so the immutable selector stays byte-identical
(fact 3) — product code, a pod-template change, and a rollout of every
existing Deployment on upgrade, all to serve an experimental need. The
status field costs none of that and answers the same question.

The demarcation that keeps this compatible with ADR-0007 D5: **the
operator reports where it placed; it does not choose endpoints.**
Endpoint-level scoring remains exactly where ADR-0007 D5 put it — the
router layer, where llm-d's `endpoint-attribute-scorer` plus
`customMetrics` make it configuration rather than code. Placement
decides which node hosts a service; the published placement makes that
decision observable; routing decides which endpoint serves a request,
elsewhere. Three concerns, still distinct — and the operator owns
strictly fewer of them here than the rejected alternative would have
given it.

Without D1 the experiment is unobservable, which is precisely what the
level-3 session established empirically.

### D2 — Observations carry when they were measured

The reporter retains, per ADR-0007 signal, the last valid measured
value together with an `observedAt` timestamp recording when the
measurement was taken — not when the status was written. Both are
published in `NodeState.status`. `NodeCandidate` carries the timestamps
into the pure function, and the planner applies a configurable maximum
age; a signal older than that horizon ranks exactly as a signal never
observed.

This is what constraint P (fact 5) forces. A node cannot simultaneously
produce an efficiency signal and be a placement target, so the signal
that informs a placement is necessarily an observation from before the
node was freed. Either the loop can reason about a recent past or it
cannot reason at all.

The demarcation of ADR-0007 D2 survives intact, and this is the test of
whether the decision is sound: *when a value was measured is a fact
about the node; how old is too old is fleet policy.* The reporter
therefore retains **without expiry** — any reporter-side timeout would
be a threshold, and thresholds live in the planner. The reporter still
has no opinions.

Two constraints on the implementation, both structural rather than
stylistic:

- Retention applies only to the two ADR-0007 signals. `gpuUtilization`
  and `gpuMemoryUsedBytes` come from NVML and remain available on an
  idle node; they need no horizon and must not acquire one.
- Time enters the comparator as **data, never as a call to the clock**.
  The pure function takes an evaluation instant as an argument. A
  `now()` inside the comparator would make the ordering
  non-deterministic and destroy the property tests, which are the
  strongest guarantee ADR-0007 has.

Absence semantics therefore become ternary — never observed, observed
and fresh, observed and expired — and the third case must degrade to
exactly the first in the ordering. That equivalence is a property test,
not a comment.

Prior art, acknowledged rather than reinvented: staleness horizons on
observed metrics are standard in load-aware schedulers, and a
closed-loop design that omitted one would simply have a hole. What is
specific here is the signal — energy and cache rather than CPU and
memory — and the absence contract inherited from ADR-0007, where an
unmeasured node cannot masquerade as an idle one.

### D3 — Capacity is a placement precondition, not a tie-breaker

The `NodeCandidate` contract documents that the caller filters for
selector match, non-draining status, and spot-fraction cap. Capacity is
absent from that list, and `own_placements_per_node` only counts *this
fleet's* placements. A placement onto a node whose GPU is already
allocated therefore produces a Pending pod and no error — a silent
failure in production, and in this experiment a run that dies quietly
while appearing to proceed.

The caller gains a capacity filter for the GPU-request case: a node
without an allocatable GPU is not a candidate. This is a filter, not a
signal, which is why it does not conflict with ADR-0007 D3's rejection
of efficiency *floors*: that rejection was about filters that can
shrink the candidate set until they force a cold start. A node that
cannot run the pod at all was never a candidate; excluding it removes
no viable option.

Scope is deliberately narrow: whole-GPU requests, which is what
`FleetServiceSpec.template.gpu` expresses today. Fractional GPUs,
time-slicing, and MPS are out of scope and remain out of scope for
every claim in this ADR.

### D4 — Within EfficiencyAware, energy ranks above cache

ADR-0007 D3 ordered `kvCacheHitRate` above `tokensPerJoule`, reasoning
from agentic-kv that the cache regime is the cause and tokens/joule the
effect. That reasoning holds for a *running* service and does not hold
for the decision actually under test. The prefix cache lives in the
memory of the vLLM process; `modelCacheHostPath` shares weights on
disk, not KV blocks. A newly placed replica starts with a cold cache on
any node, so a node's observed hit-rate — a property of the engine
already running there, and by fact 4 pooled across engines — carries no
causal information about what the new replica will achieve.

Tokens/joule does carry it: it reflects the physical context of the
node — power envelope, thermals, contention — which a replica placed
there inherits. The default EfficiencyAware ordering becomes:

    warmth > tokensPerJoule > kvCacheHitRate
           > gpuUtilization > activeServiceCount

`kvCacheHitRate` is demoted, not removed. It remains a published
observed signal, it remains meaningful for decisions about services
that already exist, and it would regain causal standing for placement
the day KV state is shareable across processes or nodes. Warmth stays
dominant and the invariant is unchanged: no efficiency signal in any
combination may rank a lower-warmth candidate above a higher-warmth
one.

This is a behaviour change to a strategy variant shipped in v0.3.0. It
is taken in place rather than as a new variant — EfficiencyAware is
days old, has no users, and multiplying variants to preserve an
ordering whose stated justification does not survive review would be
worse than changing it. Declared in Consequences and in the CHANGELOG.

Implementation status at the time this ADR is accepted: **not yet
implemented.** `fleet_placement.rs` still ranks `kvCacheHitRate` above
`tokensPerJoule`, as ADR-0007 D3 specified and as v0.3.0 shipped. This
ADR is design-only; the comparator, the property tests, and the CHANGELOG
entry land together in the implementation block. Anyone reading the
source before then is reading the ADR-0007 order, correctly.

### D5 — Scope and falsification

Out of scope, in addition to the exclusions ADR-0007 D1 and D5 already
carry (autoscaling, P/D rebalancing, power/frequency actuation,
endpoint-level routing policy): multi-engine nodes, fractional GPU, and
any signal-triggered migration. ADR-0007 D4 is unchanged — signals
order candidates at decision points that occur anyway; they never
generate decisions.

**The experiment.** Constraint P also removes co-tenancy as a source of
node-to-node difference: on single-GPU nodes, a node that differs by
being loaded is a node where placement is impossible. The difference
must therefore exist between *idle* nodes, be measurable before the
decision, and persist after it. An asymmetric GPU power cap satisfies
all three: identical nodes, different caps, different tokens/joule at
equal everything else. The asymmetry is a **design parameter**, exactly
as prefix-locality was in the level-3 design — it manufactures the
discriminating condition and is not a finding.

Sequence per rep: pre-load both candidate nodes under their caps until
the reporter has recorded tokens/joule for each; drain; trigger the
placement decision within the declared horizon (D2); the dispatcher
(D1) reads the published placement and sends the replayed workload to
the placed replica; measure.

Primary metric: **tokens/joule of the placed replica**, over a
declared window. Fleet aggregates are explicitly not primary — that is
the mistake the level-3 amendment recorded. Guardrails, non-regression:
TTFT and TPOT p50/p99. Relevance threshold: +3% on the primary,
consistent with the level-3 design; the cap asymmetry must be tuned at
the pre-flight gate to place the expected effect well clear of
run-to-run noise, and the tuning is recorded as an input.

**Counterbalancing, and why it is not optional.** WarmthFirst breaks a
tie between two idle equal-warmth nodes on `activeServiceCount` then
`gpuUtilization`, and where those tie it resolves arbitrarily with
respect to efficiency. The candidate set preserves the order of the
`NodeState` list verbatim (no sort is applied) and `max_by` returns the
last maximal element, so the winner at equal signals is decided by list
order. That order is not a documented API guarantee, which is the
point: the design must not depend on knowing it either way. If the
high-cap node were always the same physical machine, WarmthFirst would
systematically win or systematically lose by construction, and the
measured delta would be
an artefact of that alignment rather than of the strategy. Which
physical node holds the high cap is therefore alternated across reps,
in the same spirit as the ABBA+BAAB ordering.

**Falsifiable claim.** With traffic delivered to the placed service, at
equal warmth, EfficiencyAware places replicas on nodes where they run
measurably more efficiently than WarmthFirst does, beyond run-to-run
noise. A delta within noise is a negative result about EA under this
workload and is published as one — which, unlike the level-3 outcome,
this design is entitled to conclude, because the measurement path can
carry the effect.

**Gates, in order.**
1. Property tests: expired-equals-never-observed; warmth dominance
   under every combination of signals, ages, and absences; the full
   lexicographic chain in its new D4 order.
2. Zero-cost kind rehearsal: the whole harness end to end with metrics
   fixtures — capacity filter, published placement and dispatcher,
   retention and horizon, the pre-load/drain/decide sequence, evidence
   layout, verdict logic.
   The level-3 lesson applies: exercise every flag the GPU run will
   use, including the ones only the GPU path reaches.
3. Pre-session gate, **not yet verified and explicitly flagged**:
   whether a GPU power cap can be set on Lambda instances at all
   (privileges, persistence mode). Candidate mechanisms exist but are
   unconfirmed on that platform and must be checked before any node is
   booked, with a declared abort criterion. If no cap mechanism is
   available, the session does not run on an improvised substitute; the
   design returns here for an alternative asymmetry.

## Consequences

- `NodeState.status` grows a per-signal `observedAt` for the two
  ADR-0007 signals, and the reporter retains their last valid values
  indefinitely. Retention is reporter-side because merge-patch deletes
  on null (fact 6); the horizon is planner-side because it is a
  threshold.
- `NodeCandidate` grows the observation timestamps and the pure
  function grows an evaluation-instant argument. Property tests must
  cover the ternary absence semantics.
- `PlacementSpec` gains the maximum-age policy field. Its default is a
  policy choice recorded at implementation time, not here.
- `FleetService.status` grows the published placement (D1). Additive to
  a status the controller already writes: no new owned resource, no pod
  template change, and therefore no rollout of existing Deployments on
  upgrade. The experiment's dispatcher reads it and lives in `hack/`.
- The CRD schema grows accordingly, and the `NodeState.status` change
  above lands with it; both are additive and optional, so existing
  objects remain valid.
- The EfficiencyAware ordering changes (D4). Behaviour change to a
  v0.3.0 feature, no API break, `WarmthFirst` default untouched.
- The planner gains a capacity precondition for GPU requests (D3),
  which also removes a silent Pending-pod failure mode independent of
  this experiment.
- Until the GPU experiment runs and reports, the operator funnel claim
  stays exactly where the level-3 amendment left it: level-3 mechanism
  validated, primary hypothesis open. This ADR is design only;
  implementation and the session are post-2026-08-11 blocks.

## Postscript, 2026-08-07 — half a non-goal closed, and the deferral's premise falsified

Two things changed since this was written, and they pull in opposite
directions on the same decision.

**The upgrade path is exercised now.** The non-goals list says "no soak,
no upgrade path exercised". The soak still needs calendar time rather
than code. The upgrade path needed twenty seconds of CI: the `e2e` job
now restarts the operator over a live child and asserts the child
survives with the same UID and the same metadata generation — the first
because recreation would cycle every replica in a fleet on every
operator upgrade, the second because generation moves only on a spec
change, so a restart that rewrites the spec identically and still bumps
it means the apply is not idempotent and every restart is a rollout.
First green run in CI on 2026-08-07: uid and generation both unchanged.

Fact 3 of this ADR also stopped being only a note. It records that one
`BTreeMap` feeds the pod template, the `LabelSelector` and the Deployment
metadata, and that a Deployment's selector is immutable — so any future
version adding a key to that map breaks apply on every Deployment that
already exists, fleet membership (fact 2) being the obvious candidate.
The `e2e` job now proves the rejection rather than describing it, and
asserts the reason, because an apply failing for some other cause would
pass a naive check and leave the constraint unpinned. The API server
rejects with both `field is immutable` and `selector does not match
template labels`, the second showing that changing only one of the two
points does not escape either.

**The reason for deferring is no longer true.** The header records a
market-positioning call: efficiency-aware placement is "the better
problem" but "answers one they are not [asking]". That was a defensible
reading in July and the cost campaign of 2026-08-04 falsified it. On an
agentic trajectory, 19.01% of the cost at 2.0 s/tool and 37.01% at 5.0
s/tool is GPU allocated and not generating, while the cost of generating
holds flat within a 0.5% band — so the entire +56% in $/M token across
that sweep is placement-and-scheduling territory, not engine territory.
The question "where should this replica go, and what else can share it"
now has a dollar figure attached, which is the form in which platform
teams do ask it.

This postscript does not un-defer the ADR: scheduling is a separate
decision and the GPU session it needs is a September block. It records
that the queue ahead of it was ordered on a premise the measurements
have since removed, so the next re-ordering starts from that and not
from the July reading.

What has not changed: constraint P stands, the primary hypothesis of
ADR-0007 stays open, and the funnel claim stays where the level-3
amendment left it.
