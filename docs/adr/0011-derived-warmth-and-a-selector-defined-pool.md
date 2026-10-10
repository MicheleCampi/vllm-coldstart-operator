# ADR-0011: Warmth is derived from what the cluster runs, and the node pool selector defines the candidates

Status: Accepted; implemented as amended (see postscripts)
Date: 2026-10-10

## Context

FleetService ranks candidate nodes by warmth (ADR-0003, warmth-first), and
after a preemption notice it moves a replica only to a survivor that is not
Cold (ADR-0005 decision 3). Two ADRs gave warmth to the per-node reporter:
ADR-0003 decision 2 ("A per-node reporter writes warmth, GPU utilization,
and spot-preemption signal to a `NodeState` object") and ADR-0007 D2
("`Warmth` is a lifecycle state and belongs to the reporter"). The reporter
never wrote it. Every warmth value the planner has ranked on was either set
by hand, as in CI, in the kind rehearsals and in the level-3 GPU session, or
the `Cold` a NodeState defaults to.

The cost surfaced on 2026-10-09: with the chart's defaults a FleetService
places nothing. The reporter is off by default, it is the only component
that creates NodeState objects, and the planner takes its candidates only
from NodeStates.

## Facts that constrain the design

1. No shipped component writes warmth or the preemption notice. Every write
   in the tree is a `kubectl patch`: `.github/workflows/ci.yml:228`, the kind
   rehearsals under `hack/rehearsal/`, `hack/adr0009-d5-experiment/setup-cluster.sh:79`
   and `hack/gpu-session/adr0007-ea-experiment/run_experiment.sh:121-125`. The
   reporter leaves warmth and spot "to their own writers"
   (`src/bin/reporter.rs:12-13`) and has not written warmth since its first
   commit, 3cc4c26.
2. A NodeState the reporter creates reads as `Cold`: `Warmth` defaults to
   `Cold` and `NodeStateStatus.warmth` is `#[serde(default)]`
   (`src/fleet_types.rs:372-376` and `:404-405`).
3. Candidates come only from NodeStates that have a status
   (`src/fleet_controller.rs:216-225`; `node_state_to_candidate` returns None
   without one, line 129). With none, the planner logs "no reported NodeState
   objects, nothing to place" and requeues before writing any status
   (lines 241-247).
4. No ADR and no README section defines what makes a node warm. ADR-0002
   defines it for a service: `Ready` means "the model is loaded, the GPU is
   warm, and the server can actually serve a token within normal latency"
   (line 11), and the doc comment of `phase_for` says `"Ready" means warm,
   not merely alive` (`src/lib.rs:122-124`).
5. Warmth belongs to a model on a node. A node serving one model is cold for
   another, whose weights it would have to load. `NodeState.warmth` holds one
   value per node.
6. The benefit measured for a warm node is the weights cache: replacements
   load weights "from a local `modelCacheHostPath` cache, not the HF CDN —
   that is the difference between 57s and several minutes" (README, "Measured
   results: preemption without cascade"). The cache outlives the pod that
   filled it.
7. `select_replacement_node` drops Cold survivors as "not a healthy target"
   (`src/fleet_placement.rs:313-317`). ADR-0005 decision 3 gives the reason: forcing a
   placement onto "a Cold or full node is exactly the cascade — it overloads a
   node that was not chosen because it could not take the load". Cold there
   stands for a node that cannot take the load. The caller already filters
   candidates before replacement on what makes a node unable to take it: a
   preemption notice, GPU capacity (ADR-0008 D3) and the node pool selector
   (ADR-0003 postscript).
8. The operator already lists the cluster's Nodes (`chart/templates/rbac.yaml:44-50`,
   granted for ADR-0008 D3) and the VllmService objects of every namespace (a
   `ClusterRole`, "across all namespaces", lines 11-18). A VllmService names its model
   (`spec.model`, `src/lib.rs:31-32`) and, when pinned, its node
   (`spec.nodeName`, "Unset for standalone / CI use", lines 72-75). Its phase
   is `Ready` only when every desired replica is ready (`phase_for`,
   `src/lib.rs:125-141`), so a one-replica service goes from `Pending` to
   `Ready`.
9. The operator cannot know the tolerations of the Pod that finally runs.
   VllmService carries none, and mutating admission controllers "may modify
   the data for the resource being modified"; `MutatingAdmissionWebhook` and
   `MutatingAdmissionPolicy` are enabled by default (Kubernetes documentation,
   "Admission Control in Kubernetes", v1.37). Judging in the planner which
   taints exclude a node would reimplement part of the scheduler.
   ADR-0003 decision 3 rejected a scheduler extender; ADR-0004 chose
   `nodeSelector` because it "keeps the default scheduler in the loop"
   (line 29), and rejected owning resource fit because that "means
   reimplementing the scheduler, poorly" (line 58).
10. A Node marked unschedulable carries `spec.unschedulable` ("Unschedulable
    controls node schedulability of new pods", `k8s-openapi` 0.26.1, the
    version in `Cargo.lock`); `kubectl cordon` is the command that marks it.

## Decisions

### D1 — The planner derives warmth per model from VllmService phases

For a fleet serving model M, a node's derived warmth is `Warm` if a
VllmService with `spec.model` M and `spec.nodeName` set to that node reports
`Ready`, `Warming` if one reports `Warming`, and `Cold` otherwise (facts 4,
5, 8).

Every VllmService in the cluster counts, not only the fleet's own children:
an instance of M started by another fleet, or pinned by hand, warms the node
for M all the same. A VllmService without `spec.nodeName` is attributed to no
node; finding where it runs would mean reading Pods.

The ranking uses the higher of the derived value and
`NodeState.status.warmth`, where a NodeState exists. A value set by hand, or
by a later producer, can raise a node above what the planner observed and
never lower it. Runs that seeded every node `Warm` rank as they did.

`decidedOn.warmth` records the value the comparator ranked on, which ADR-0008
D1 defines as "what the planner saw"; from this ADR on, that value is the one
this ADR's D1 computes.

### D2 — Warmth is a preference; eligibility is explicit

A replacement target is any candidate that passes the caller's filters:
no preemption notice, GPU capacity, the node pool selector, and D3's
schedulability check. `select_replacement_node` no longer drops Cold
candidates; warmth still orders them.

This amends ADR-0005 decision 3, whose reason stands and is now carried by
the filters (fact 7): a node that cannot take the load is one with no
capacity, under a preemption notice, or outside the pool. A node that is
Cold for M can take the load; it starts slower. Drain-and-hold remains for a
replica that has no eligible target.

### D3 — `nodePool.selector`, when set, defines the candidates

- Selector set: the candidates are the Nodes it admits, without the Nodes
  marked unschedulable (fact 10). A NodeState of the same name, where there
  is one, contributes its signals; a Node without one is a candidate with
  every signal absent, which ADR-0007 already ranks fail-open.
- Selector empty: the candidates are the NodeStates that have a status, as
  before.

The selector is how a fleet states where it may run, with a Pod's
`nodeSelector` semantics (ADR-0003 postscript). Selecting tainted Nodes
through it has the outcome it would have for any Pod: the scheduler decides,
and the Pod waits. The planner does not filter on taints (fact 9).

The preemption notice still comes only from a NodeState
(`spot.preemptionNoticeDetected`): a Node without one never reports a notice.

### D4 — A fleet with no candidate says so in its status

`FleetServiceStatus` gains a `message`, as `VllmServiceStatus` has. A fleet
with no candidate writes its phase and a message naming the reason (no
NodeState and no selector; a selector that admits no schedulable Node),
instead of returning before it writes any status (fact 3).

## Consequences

- With a selector set, a FleetService places on a chart installed with its
  defaults: no reporter and no hand-written warmth.
- Without a selector and without the reporter, a fleet still places nothing,
  and its status now says why.
- Warmth-first ranks by the instances of the fleet's model the cluster runs.
  A node keeps M's cached weights after its instance is gone, and D1 ranks it
  Cold (fact 6). Seeing the cache needs an agent on the node that reads
  `modelCacheHostPath`; that is left to its own ADR, once the cache layout has
  been read at source.
- A replacement may go to a node that is Cold for M and start slower: without
  the cache, minutes rather than 57 s (fact 6). Under a short spot notice it
  may not reach Ready before the node is reclaimed. Before this ADR, a
  survivor set with no node that was not Cold left the replica in Draining.
- One harness behaviour changes. The kind rehearsal seeds the control-plane
  NodeState `Cold` (`hack/rehearsal/seed-nodestates.sh:12`), which kept it out
  of replacement. Under D2 it is eligible, ranked last; a rehearsal that must
  keep a node out does so with the selector.
- `spotPolicy` stays reserved; D3 does not read it.

## What this ADR does not prove

It is a design. Nothing in it has been implemented or run. In particular:

- that D1's ranking places replicas where the cached weights are: it cannot,
  when the instance that filled the cache is gone (Consequences);
- that a replacement onto a node Cold for M completes inside a real spot
  notice;
- that the kind rehearsals behave as before under D2 beyond the one change
  named above: only `hack/rehearsal/seed-nodestates.sh:12` has been read for
  this ADR.

## Postscript, 2026-10-10 — D1 does not count the fleet's own children

Implemented as written, D1 would concentrate a fleet's replicas. A node
hosting one of the fleet's own Ready children would read `Warm` for that
fleet and every node without one `Cold`, and the comparator ranks warmth
before load (ADR-0003). `plan_initial_placements` spreads a batch only by
raising `active_service_count` after each pick, and its doc names what
happens without that: "every slot in a batch would land on the same single
warmest node instead of spreading across the fleet"
(`src/fleet_planning.rs:9-11`). Warmth is not part of that bookkeeping, so
on a scale-up every new slot would go to the node the fleet already uses:
one failure domain and, with GPUs, Pods that may not fit, since the D3
capacity filter reads a node's allocatable GPUs, not its free ones
(`src/fleet_controller.rs:441`).

D1 is amended: for fleet F of model M, a node's derived warmth counts only
the VllmServices of M that are not F's own children. F's children are the
VllmServices the controller already lists as F's, by the
`inference.michelecampi.dev/fleet` label in F's namespace
(`src/fleet_controller.rs:256`). F's own placements already enter the
ranking as load, folded into `active_service_count` because "The controller
knows where it has already placed" (`src/fleet_controller.rs:144`).
Counting them as warmth too would count one fact twice, with opposite
effects.

What this costs: a node F has just moved a replica off keeps M's weights
cached and still reads Cold for F, which is the limit Consequences already
names for D1 (it sees instances, not the cache). Across fleets of the same
model, warmth draws one fleet toward the nodes another one uses, and the
capacity filter reading allocatable rather than free GPUs applies to that
co-location as it applies to any.

## Postscript, 2026-10-10 — D2 and D1 implemented

D2 is implemented in 7b563bd. D1, as amended by the postscript above, is
implemented in the same pull request as this postscript. Both are covered
by unit tests. D1 is also covered by the CI e2e step "Node warmth is
derived from what the cluster runs (ADR-0011 D1)", which fails against an
operator without derived warmth and against D1 as first written. D2's
preemption path is not in CI, which runs one node; it was run on a kind
cluster with two workers, as 7b563bd's message records.

D3 and D4 are not implemented. "What this ADR does not prove" still holds,
for D1 and D2 as for the rest: none of it has been run on GPUs.

## Postscript, 2026-10-10 — D4 implemented before D3

D4 comes first because D3 needs it. Under D3 a fleet whose selector admits
no Node has no candidate, and such a fleet used to return before writing any
status. The CI e2e step "The node pool selector decides where a fleet may
place" asserts phase `Placing` for exactly that fleet, and would fail.

The early return writes the message and, for a fleet with no phase yet, a
first phase and desired count. A full status write there would report as
gone the children a fleet has already placed. That partial write needs every
`FleetServiceStatus` field to deserialize when absent, so the four that did
not (`phase`, `readyReplicas`, `desiredReplicas`, `activeReschedules`) gained
serde defaults; without one, reading the fleet back fails with "missing field
`readyReplicas`". The schema now carries those defaults and the API server
applies them on write: a fleet with no candidate reads `readyReplicas: 0`
and `placements: []`, which is what it has.

Covered by a unit test and by the CI e2e step "A fleet with no candidate
says why in its status (ADR-0011 D4)", which fails against the operator built
from main (eec3c9e) and passes with this change.

## Postscript, 2026-10-10 — D3 implemented; the ADR is implemented

D3 is implemented in the same pull request as this postscript. With a
selector set, the candidates are the Nodes it admits that are not marked
unschedulable, each with the signals of the NodeState of its name or with
every signal absent; without one, they are the NodeStates. The Node read
moved above the construction of the candidates, and the selector filter
applied to them afterwards went with its warning: the status message of D4
now says what the warning said.

Implementing it showed one more thing D3 needed. NodeState is namespaced, so
two with one name can coexist, and both became candidates for the same node.
One per node now lends its signals, from the lexicographically first
namespace that has a status; a preemption notice counts from any of them.

Covered by unit tests and by the CI e2e step "The node pool selector defines
the candidates (ADR-0011 D3)": with no NodeState, a fleet whose selector
admits the node reaches Ready; with the node cordoned, a second fleet stays
Placing with a message. The step fails against the operator built from main
(924d67a), and the whole e2e job passes when run locally from the workflow
text.

"What this ADR does not prove" still holds: none of it has run on GPUs, and
no replacement onto a node Cold for the model has been timed against a real
spot notice.

## Postscript, 2026-10-10 — the preemption path is in CI

The postscript on D2 and D1 says that D2's preemption path is not in CI.
It now is: the CI job "preemption on multi-node kind" creates a kind
cluster with two workers, places a replica on the Warm one, signals a
preemption notice there, and asserts that the replica moves to the Cold
one, with the fleet Ready again and a single child pod on the new node.
Run from the workflow text, the job passes on main and fails against
7c27c8d, the commit before D2: the replica stays on the noticed node and
the fleet reads Degraded.
