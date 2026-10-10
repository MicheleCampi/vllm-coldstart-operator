# ADR-0003: Extend the operator into a fleet control plane, do not build a new one

- Status: Accepted
- Date: 2026-07-01

## Context

vllm-coldstart-operator manages one VllmService per reconcile: it owns a Deployment, derives a warmth-aware phase from ready replicas, and stops there. The natural next step — running a fleet of GPU nodes rather than a single node — raises a question before any code: does fleet-level reasoning belong inside this operator, or does it belong in a separate control plane that sits above it?

An operator is already a control plane: a desired-state resource plus a reconcile loop that drives observed state toward it. The correct pattern for a fleet is not a second control plane calling the first one, but richer CRDs and a reconcile loop that reasons over more than one VllmService at a time. A control plane above a control plane is two systems doing the same job at different scopes — coordination between them becomes its own failure mode, and it does not match how Kubernetes-native fleet tools (cluster-autoscaler, Karpenter) are actually built: as controllers watching cluster-wide state, not external orchestrators.

Three sub-decisions follow from that framing, each with a real alternative that was rejected.

## Decision

### 1. FleetService is a new CRD in the same operator, not a new service

A new `FleetService` CRD owns placement — which node runs which instance — by creating and reconciling owned `VllmService` objects. It does not reimplement warmup lifecycle; that stays entirely inside `VllmService`, already validated E2E on GKE. This is the Deployment → ReplicaSet → Pod layering: a higher-level controller that creates lower-level objects it does not otherwise duplicate.

Rejected alternative: a standalone fleet-orchestration service calling the existing operator's API or CRDs from outside the cluster's controller model. Rejected because it duplicates the reconcile-loop pattern the operator already provides, adds a second deployable with its own RBAC and failure surface, and gives no fleet-specific capability that a second CRD inside the same operator does not already give.

### 2. Node-level observed state is a dedicated CRD (NodeState), not Node annotations

A per-node reporter writes warmth, GPU utilization, and spot-preemption signal to a `NodeState` object, one per node, watched read-only by the fleet controller.

Rejected alternative: writing this state as annotations directly on core `Node` objects. Rejected because `Node` is already written by cluster-autoscaler and other node controllers; a second writer on the same object risks silent overwrite races that are hard to detect and harder to attribute. A dedicated CRD has its own RBAC, its own watch stream, and does not touch a resource this operator does not own.

### 3. Placement logic lives in the controller, not a scheduler extender

The fleet controller decides node placement itself and creates `VllmService` objects with the target node already resolved (`nodeSelector`/`nodeName` set), rather than deferring to a custom kube-scheduler extender or plugin.

Rejected alternative: a scheduler extender/plugin implementing the placement logic. Rejected as disproportionate infrastructure for this scope — it requires wiring into the scheduler's extension points and changes the failure mode of every pod in the cluster, not just this fleet's, for a decision (warmth-first placement across N nodes) that a controller-side decision loop makes just as correctly and is far easier to instrument, test in isolation, and explain.

## Rationale

All three decisions optimize for the same thing: the hard part of a fleet control plane is the reconcile loop's reasoning — fleet-state representation, multi-node placement, preemption handling without cascade, anti-oscillation — not the plumbing around it. Adding a second control plane, writing to Node directly, or delegating to the scheduler would each spend engineering effort on infrastructure that does not make the placement decision better, only more distributed and harder to reason about.

## Consequences

- `VllmService`'s reconciler is untouched by this work; a regression in fleet placement cannot break single-instance warmup behavior, and the existing GKE-validated code path stays as-is.
- RBAC stays clean: the fleet controller has full CRUD on `FleetService`/`VllmService`, read-only watch on `NodeState`, and no permissions on core `Node` objects at all.
- The placement strategy enum (`warmth-first` implemented, `spread`/`bin-pack` reserved) is deliberately not fully built out in v1 — persisted per-node scores for bin-packing are the likely next extension, not built now (YAGNI, tracked as future work rather than speculative code).

## Note on validation status

This ADR fixes the shape of the reconcile loop before it is written. The claims here — no cascade on mid-warmup preemption, no oscillation under load — are design intent, not yet measured. They become Accepted-with-evidence only after the multi-node GPU session validates them under real concurrent saturation; until then this ADR records the architecture, not a result.

## Postscript, 2026-10-09 — Node is read; the node pool selector is honoured

Two things this ADR recorded no longer hold as written.

**Node is read, not written.** Consequences said the fleet controller has
"no permissions on core `Node` objects at all". ADR-0008 D3 granted `get`
and `list` on nodes, cluster-scoped and read-only, so the planner can
exclude nodes without an allocatable GPU (`chart/templates/rbac.yaml`).
Decision 2 stands: the operator never writes to `Node`, and node-level
observed state stays in `NodeState`.

**The node pool selector is honoured.** `FleetServiceSpec.nodePool` entered
the schema with the first fleet types (1f25809, the day of this ADR) without
a decision here, and the controller did not read it until ed2846c. The read
of `Node` that D3 added was what it needed. The decisions taken with it:

- The selector has a Pod's `nodeSelector` semantics: a node is eligible only
  if it carries every listed label with its value, and an empty selector
  admits every node.
- While a selector is set, a node whose labels cannot be read is not
  admitted. The D3 capacity filter makes the opposite choice and keeps such
  a node, because its error is a Pending pod that runs nowhere. Here the
  error would run a replica where the selector excluded it, silently and for
  as long as the placement lives; refusing costs one placement deferred to
  the next reconcile.
- The selector governs new placements and replacements. Existing placements
  are not moved when it changes, which follows ADR-0005 decision 1: a
  reschedule is triggered only by a preemption notice.
- `spotPolicy` stays reserved. Applying `maxSpotFraction` needs to know which
  nodes are spot, and nothing this operator ships writes that.

The `NodeCandidate` contract in `fleet_placement.rs` listed a spot-fraction
filter that the caller never applied, and ADR-0008 D3 repeated it. The
contract now names the filters the caller applies.

## Postscript, 2026-10-10 — warmth is derived; the selector defines the candidates (ADR-0011)

Decision 2 had "A per-node reporter writes warmth, GPU utilization, and
spot-preemption signal to a `NodeState` object". The reporter never wrote
warmth (ADR-0011, fact 1). ADR-0011 D1 has the planner derive a node's
warmth for a fleet's model from the VllmServices the cluster runs there,
not counting the fleet's own children, and rank on the higher of that and
the NodeState's warmth. ADR-0011 D3 makes `nodePool.selector`, when set,
define the candidates: the Nodes it admits that are not marked
unschedulable, with or without a NodeState. NodeState still carries the
measured signals and the preemption notice, and decision 2's reason for
keeping them off core `Node` objects stands.

The postscript above says the `NodeCandidate` contract names the filters
the caller applies. ADR-0011 added one, the schedulability of a selected
Node, and the contract names it as well.
