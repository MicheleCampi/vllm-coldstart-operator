# ADR-0009: Replicas follow demand, and a warming replica is not capacity

Status: Accepted, superseded in part (see postscripts)
Date: 2026-07-31

## Context

ADR-0003 made the fleet own placement: `FleetService` decides *where* a
model runs. It does not decide *how many* replicas run. `spec.replicas`
is a required `i32` written by a human (`src/fleet_types.rs`), and the
fleet controller fans it out as one `VllmService` of `replicas: 1` per
placement (`src/fleet_controller.rs`), so fleet-level replicas means
number of placements. Nothing changes that number in response to load.

That is the gap this ADR closes, and the interesting part is not the
arithmetic of scaling but the fact that an LLM replica is useless for
the first several seconds of its life. ADR-0002 established cold start
as a first-class state for one service; at fleet level the same fact
becomes a scheduling problem, because a controller that counts a
warming replica as capacity under-provisions, and one that counts it as
absent over-provisions.

Source review before writing established six facts, each cited to the
file that establishes it, because each closes off an option that would
otherwise look reasonable.

1. **No status field reports how many replicas exist.**
   `ready_replicas` counts placements in phase `Ready`, and
   `desired_replicas` is a copy of `spec.replicas`
   (`src/fleet_controller.rs`). The Kubernetes convention for the scale
   subresource is that the status path reports *observed* replicas,
   ready or not; neither field carries that number, so one has to be
   added before the subresource can be exposed honestly.
2. **kube-derive supports the scale subresource with typed fields.**
   `#[kube(scale(...))]` accepts `spec_replicas_path` and
   `status_replicas_path` as *mandatory* keys and `label_selector_path`
   as optional (kube-derive 2.0.1, `src/custom_resource.rs`). The doc
   example in that crate spells the second key `status_replica_path`,
   which the parser rejects; only the plural form parses.
3. **The reporter already speaks the vLLM Prometheus dialect.**
   `VllmScrape::sample` reads `vllm:prefix_cache_hits`,
   `vllm:prefix_cache_queries` and `vllm:generation_tokens_total`, with
   `_total`-suffix tolerance and per-target fail-open
   (`src/bin/reporter.rs`). Adding series is an extension of an existing
   parser, not a new component.
4. **The signals published today cannot drive scaling.**
   `NodeStateStatus` carries `gpu_utilization`, `active_service_count`,
   `kv_cache_hit_rate` and `tokens_per_joule` (`src/fleet_types.rs`).
   None of these is a demand signal: a GPU at 90% with an empty queue is
   saturated and healthy, while a GPU at 40% with fifty requests waiting
   is starved by something that adding replicas may not fix.
5. **Anti-oscillation machinery exists and is reusable.**
   `HysteresisSpec` provides `stable_reconciles_required` and
   `max_concurrent_reschedules` (`src/fleet_types.rs`), and
   `PlacementStatus.stable_since` records how long a phase has held.
6. **Every placement signal is `Option`, and absence is not zero.**
   ADR-0007 established this for placement; a demand signal that
   defaults to 0.0 when a scrape fails would read as "no load" and
   scale the fleet to its floor exactly when the engine is unreachable.

## Non-goals

This ADR does not build an autoscaler. The replica count is computed
outside the operator, by HPA, KEDA, or a human with `kubectl scale`.
Vertical sizing (GPU count per replica) stays in the template.
Arbitration between competing fleets on one pool is out of scope. No
KEDA `ScaledObject` is vendored: that is configuration, and shipping it
as code would bind the operator to one autoscaler's schema.

## Decision

### D1 — The scale subresource is exposed; the operator does not compute the number

`FleetService` gains `#[kube(scale(spec_replicas_path = ".spec.replicas",
status_replicas_path = ".status.replicas"))]`. `kubectl scale
fleet/<name> --replicas=N` works, and any external autoscaler can target
the CRD the way it targets a Deployment.

The status path points at total live placements, not at
`readyReplicas`, and the distinction is the whole reason D3 exists. An
autoscaler reads that field as "how many replicas are there now"; if it
reported only the ready ones, a replica still loading weights would be
invisible, the loop would see unmet demand next to a low count, and it
would scale again — creating exactly the over-provisioning this ADR is
written to prevent. Reporting the total is also what the convention in
fact 1 requires.

`label_selector_path` is deliberately omitted. It exists so that
`/scale` can report which pods belong to the workload, which HPA needs
for per-pod resource metrics. This fleet is meant to be scaled on
*external* metrics — queue depth, in-flight requests — and fact 3 of
ADR-0008 records why a fleet-wide pod selector does not exist today: pod
labels and the immutable Deployment selector share one map. Omitting the
path is therefore a scope decision, not an oversight, and it means
per-pod-resource HPA is not supported. That limit is stated in the CRD
docs rather than discovered.

The alternative — an autoscaling loop inside the fleet controller —
was rejected. It reimplements a decision Kubernetes has standardised,
and it would put the replica count in two places: the spec a human
wrote and the number the controller believed.

### D2 — Demand is queue depth and in-flight requests, never GPU utilization

The reporter scrapes `vllm:num_requests_waiting` and
`vllm:num_requests_running` alongside the ADR-011 series, and publishes
them to `NodeState.status` as `Option<f32>`, summed per target with the
same per-target fail-open as the existing scrape.

Utilization is rejected as the scaling signal for the reason in fact 4:
it answers "is the GPU busy", and scaling answers "is work waiting".
The two diverge in both directions, and the divergence is the normal
case for LLM serving, not an edge case.

Absent stays absent (fact 6). A fleet whose demand signal is `None` does
not scale in either direction; it holds and says so in status. An
autoscaler reading a missing metric is a condition its own configuration
must handle, and the operator refuses to fabricate a zero for it.

### D3 — A warming replica is neither capacity nor absent

`FleetServiceStatus` gains two fields: `replicas: i32`, the count of
live placements whatever their phase, and `warming_replicas: i32`, the
subset not yet able to serve. `ready_replicas` and `desired_replicas`
keep their current meaning. The three counts satisfy
`ready + warming <= replicas`, with the remainder being placements in
neither state (Placing, Draining).

This is the decision the whole ADR exists for. A generic autoscaler
observes demand and current ready replicas; if a replica takes ~18s to
become useful on a 7B and considerably longer on a 32B (vllm-coldstart-probe,
Phase A-D; cuda-graphs-experiment measured +7.0s and +15.9s of additional
cold start with graphs enabled), then during that window demand is still
unmet and ready count is still low, so a naive loop scales again. The
fleet already knows the replica is coming: it publishes that, so the
decision upstream can subtract it.

The operator does not enforce the subtraction — it cannot, since it does
not own the loop (D1). It publishes the fact that makes the correct
decision possible, which is the same shape as ADR-0008 D1: the operator
publishes, the consumer decides.

### D4 — Scale-down is not the mirror of scale-up

Scale-up is immediate: demand that exists now is real. Scale-down must
persist for `stable_reconciles_required` before a placement is removed,
reusing the existing hysteresis rather than adding a second mechanism,
and `max_concurrent_reschedules` continues to cap blast radius.

The asymmetry is not caution for its own sake. Removing a replica is
cheap and instant; recreating it costs a cold start. A symmetric policy
pays that cost on every dip in a signal that is bursty by nature.

### D5 — Falsification

Hypothesis: publishing warming replicas (D3) reduces over-provisioning
against the same offered load, compared with an autoscaler that sees
only ready replicas.

Primary metric: peak replicas allocated over the run. Secondary:
requests served per replica-second. Both arms replay one deterministic
trace with a step increase in arrival rate, since the effect only
appears when demand rises faster than a replica warms.

Runs at zero cost on kind against `ghcr.io/llm-d/llm-d-inference-sim`
(requires `POD_IP`, `--enable-kvcache`, `--enable-prefix-caching`),
with an artificial warm-up delay standing in for GPU load time. The
simulator makes the cold-start window a parameter, which is what makes
this testable without a GPU.

Declared threshold: a peak-replica reduction below 15% is not a result.
A negative outcome is publishable and says the subtraction does not
matter at realistic warm-up times, which would itself be worth knowing.

## Consequences

The scale subresource requires the status subresource, which is already
enabled. Existing `FleetService` objects are unaffected: `spec.replicas`
remains required and human-written until something scales it.

`replicas` and `warming_replicas` are new status fields. ADR-0007
recorded that status
fields without `#[serde(default)]` become required in the CRD schema and
break multi-writer merge-patch with a 422; this field carries the
default like the rest.

The fleet becomes scalable by machinery it does not contain, which is
the point: the operator's claim moves from "it decides where" to "it
decides where, and it tells you honestly what is not yet serving".

## Postscript, 2026-07-31 — D3 amended: which phases count as warming

D3 as written says a placement in `Warming` is counted in
`warming_replicas`. Source review during implementation showed that
`Warming` at fleet level does not mean what ADR-0002 means by it.

`placement_phase_for` (`src/fleet_types.rs`) reaches `Warming` only from
`Ready`, when the node stops being ready; the next reconcile without
readiness sends it to `Pending`. A newly placed replica therefore goes
`Pending -> Ready` and never passes through `Warming`. The cold-start
sense of the word lives on the child `VllmService`
(`Pending -> Warming -> Ready`, `src/main.rs`), not on the placement.

Counting only `Warming` would have inverted the signal: it would report
capacity that is *falling out* of service as capacity that is *arriving*,
and an autoscaler subtracting that number would scale down exactly when
demand is unmet.

Amended: `warming_replicas` counts placements in `Pending` **or**
`Warming` — slots the fleet holds that are not serving yet but that it is
actively trying to bring up, whether that is a first cold start or a
recovery after lost readiness. `Draining` and `Rescheduling` are
deliberately excluded: that is capacity on its way out, and an autoscaler
must not count it as incoming.

The phase name is left alone. Renaming `Warming` to something like
`Degrading` would describe the state machine better, but it is a public
status string on the CRD and breaks any consumer reading it. That is a
separate decision, not a side effect of this one.

## Postscript, 2026-08-01 — D4 implemented: there was no scale-down to gate

D4 was written as if removal existed and only needed a delay in front of
it. It did not. The apply loop runs `for i in 0..desired`, so a slot whose
index falls beyond `spec.replicas` was never patched and never deleted:
the child `VllmService` simply outlived the scale-down. Owner references
do not help, because they garbage-collect when the `FleetService` is
deleted, not when it shrinks. `README.md` had carried "scale-down orphan
handling" as backlog since 2026-07-04, so the gap was known — what was not
noticed is that D1 made it load-bearing.

The consequence sits on D1, not on D4. `status.replicas` is the scale
subresource's status path and is documented as every live placement. It
was built from `placements`, which the same loop only ever filled for
in-range slots, so after a scale-down from five to two it reported two
while five children were still running. A consumer reading `scale` would
have scaled on top of replicas that already existed — precisely the
over-count D1 gives as the reason for not pointing that path at
`readyReplicas`.

Implemented as: `surplus_hysteresis` (pure, `src/fleet_types.rs`) plus a
scale-down pass in the reconcile that deletes surplus children, with a
per-placement `surplusReconciles` counter persisted in status. A surplus
placement still inside the hysteresis window stays in `placements` and
keeps being counted, because it is still serving; it disappears from the
count when it is actually deleted. `delete` was missing from the
`vllmservices` rule in the chart's `ClusterRole` and has been added:
without it the pass would have passed every test and returned 403 in a
cluster.

Two properties of the mechanism are worth stating because they are not
obvious from D4's wording:

The counter counts reconciles, not seconds. `REQUEUE` is 30s, but the
fleet controller also has `.watches(NodeState)` (ADR-0005 dec.4), so an
event-driven burst can advance the counter far faster than the requeue
cadence suggests. `stable_reconciles_required: 3` is therefore an upper
bound of about 90 seconds and can be much less under node churn. This is
the existing field's semantics, not a new choice, and ADR-0005 already
deferred it unused; making it a duration would be a different decision
with a different cost — it needs a clock in the reconcile, which ADR-0008
D2 argues against.

The counter resets rather than decaying. One reconcile back in range
discards the whole accumulated wait. This is the asymmetry D4 asks for,
taken to its conclusion: the expensive direction is removal, so any
evidence that the capacity is still wanted is enough to cancel it.

## Postscript, 2026-08-01 — D5 corrected: the simulator has no warm-up knob

D5 asserts that the simulator makes the cold-start window a parameter.
It does not. `ghcr.io/llm-d/llm-d-inference-sim:v0.8.2` was checked at
`--help`, whose flag list is alphabetical and complete: there is no
`--startup-delay`, no `--warmup-time`, no `--load-time`. The latency
flags that do exist — `--time-to-first-token`, `--prefill-overhead`,
`--inter-token-latency` — act per request, once the server is already
serving, and so cannot represent a replica that is not yet capacity.
`--enable-sleep-mode` is runtime sleep/wake, a different mechanism. The
sentence was written from the shape the experiment needed rather than
from the image, and it is corrected here before `DESIGN.md` inherits it.

The delay has to come from outside the simulator. `build_deployment`
hardcodes the readiness probe's `initial_delay_seconds` to 5, so it is
not reachable from spec either. What is reachable is the health
endpoint: if it does not answer for N seconds the pod stays not-Ready,
the placement stays `Pending`/`Warming`, and that is exactly the state
D3 publishes. So the warm-up is injected by a wrapper image —
`hack/rehearsal/Dockerfile.sim` already establishes the pattern of an
entrypoint that rewrites arguments — running `sleep ${WARMUP_DELAY_S:-0}`
before exec'ing the simulator. One build serves both arms and the delay
becomes an environment variable. This is a property of the harness, not
of the operator, and it does not change what D5 measures.

Two facts about the scale subresource, from exercising it on kind for
the first time (D1 declared the mechanism but never ran it):

`kubectl scale` does reach the CRD: `spec.replicas` is written through
the endpoint and read back. The toy consumer can therefore use the
standard subresource rather than patching the CR.

But the `autoscaling/v1` Scale object carries only `spec.replicas` and
`status.replicas`. There is no field in that shape for
`status.warmingReplicas`, so the warming-aware arm cannot read what it
needs from `scale` alone: it reads the CR for the subtraction and writes
through `scale`. Two reads, one write. This follows from D1's choice to
omit `labelSelectorPath` and is not a defect, but the consumer's shape
depends on it.

One further precondition for both arms. `status.replicas` carries
`default: 0` in the schema, so the endpoint serves 0 for a fleet whose
status the operator has never written — absence and zero are
indistinguishable on read. A consumer started before the first status
write sees no capacity and asks for replicas it already has. The harness
waits for the first status write before closing the loop, on both arms,
since contaminating only one would be worse than contaminating both.

Finally, D5 gains an assertion it did not have: both arms must show
surplus children actually deleted after `stableReconcilesRequired`. D4's
scale-down pass is covered only through its pure function; the delete,
the in-window count and the 404-as-success path have no test, and the
e2e job in CI exercises `VllmService` only, never `FleetService`. The
falsification run is the first place that code meets an API server, so
it should be made to say whether it works.

## Postscript, 2026-08-01 — D5 mechanisms exercised on kind

The correction above got the lever right and the plumbing wrong. It says
the delay "becomes an environment variable", which implied it could be
set per deployment. It cannot: `FleetService.spec.template` exposes only
`image`, `gpu`, `healthPath`, `extraArgs`, `modelCacheHostPath` and
`runtimeClassName`. There is no `env` field, so nothing in the fleet spec
reaches the container's environment. The value is baked at build time
with `--build-arg` instead, and since both arms share one warm-up window
this costs nothing. Adding `env` to the template to make a harness
convenient would be widening the CRD's public API for an experiment, and
was rejected on that ground.

With that settled, all four mechanisms D5 depends on were run against an
API server for the first time. Each had been decided on paper and none
had been observed.

The scale subresource writes in both directions. `kubectl scale` raised
`spec.replicas` from two to five and later lowered it to one, read back
correctly each time.

The wrapper produces the window. With `WARMUP_DELAY_S=60`, the placements
stayed unready from t+0 to t+61 and were serving by t+66, sampled every
five seconds — the delay plus the simulator's own start-up, which is the
shape wanted. During the delay the port is not listening at all, so the
probe sees a refused connection rather than an unhealthy response; a
replica that is not yet capacity is exactly that.

Warming counts, and only because D3 was amended. Throughout the window
`warmingReplicas` was 2 and `readyReplicas` 0, then both flipped. The
placements went `Pending -> Ready` and never entered `Warming` even once,
precisely as the 2026-07-31 postscript predicted for a fresh placement.
Had D3 shipped as first written, `warmingReplicas` would have read zero
for all sixty-one seconds, both arms of D5 would have subtracted nothing,
and the experiment would have reported no difference between them. That
would have looked like a clean negative result. The amendment is what
gives D5 a signal at all.

Scale-down removes, and faster than the field name suggests. Scaling from
two to one, the surplus child was still present with
`surplusReconciles: 2` at the first sample and gone by t+11, with
`status.replicas` following the live children down to one. The
over-count D1 warns about did not appear. Eleven seconds against a
`stableReconcilesRequired` of 3 and a 30s requeue is the counter's
event-driven behaviour, already stated in the D4 postscript and now
measured: the hysteresis window is not a duration and D5 must not be
tuned as though it were.

## Postscript, 2026-08-07 — the e2e now exercises FleetService

The D5 correction above notes that "the e2e job in CI exercises
`VllmService` only, never `FleetService`", and reasons from it that the
falsification run would be the first place the fleet code meets an API
server. That is no longer true, and the reasoning it supported is
weaker than it was — which is worth recording, because the conclusion it
justified (spend GPU time to find out whether the plumbing works) is
exactly the kind of conclusion that should get cheaper when a cheaper
test exists.

Five steps were added to the `e2e` job, reusing the kind cluster and the
operator process the `VllmService` scenario already brings up, so the
added CI cost is about thirty seconds. They assert four properties the
single-service scenario cannot reach: the planner places, the child is
owned by the fleet, the child is pinned to the node the planner chose,
and deleting the fleet collects its children. The scale subresource this
ADR introduces is covered too — `kubectl scale` to 2, reflected in
`status.replicas` — which nothing tested before.

Two things the fixture has to do that a manifest alone cannot, both
worth stating because both are properties of the system rather than of
the test. The per-node reporter does not run in CI, so a `NodeState` is
seeded by hand: without one, `reconcile_fleet` logs "no reported
NodeState objects, nothing to place" and places nothing, so a fleet e2e
lacking it would assert on a system that was never asked to do anything.
And `NodeState.status` is a subresource, so it goes in by
`kubectl patch --subresource=status`: an apply carrying a status block
drops it silently, which would leave `node_state_to_candidate` returning
`None` and produce the same empty-candidate path with no visible cause.

The `NodeState` name is read from the live node rather than fixed in the
manifest. Rehearsed locally on a cluster named differently from CI's, a
hardcoded name produced `FailedScheduling` for 90 seconds against a
`kubernetes.io/hostname` selector that could never match — the fixture
cannot assume the cluster's name, because ADR-0004 pins placements by
hostname and the hostname is the cluster's to choose.

First green run: `FleetService reached Ready after ~6s`, the same figure
the local rehearsal produced. What this does **not** cover is D4's
scale-down delete path, which still meets an API server for the first
time in the falsification run: the e2e scales up, not down. The
assertion the D5 correction asks for stands.
