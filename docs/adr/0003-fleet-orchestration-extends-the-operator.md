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
