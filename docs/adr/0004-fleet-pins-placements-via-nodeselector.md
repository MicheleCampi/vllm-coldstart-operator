# ADR-0004: Fleet pins placements via nodeSelector, not spec.nodeName

Status: Accepted

## Context

ADR-0003 established that the operator owns fleet placement *policy*: the
warmth signal is domain-specific, lives in the `NodeState` CRD, and the
default scheduler has no knowledge of it. `plan_initial_placements` picks a
target node per slot and returns node names.

The owned `VllmService` child must then land on the chosen node. This ADR
fixes *how* that landing is expressed, because the choice determines whether
the default scheduler stays in the loop or is bypassed.

## Decision

The fleet controller pins each owned `VllmService` to its chosen node by
writing a `nodeSelector` on the pod template, keyed on the well-known
`kubernetes.io/hostname` label. A new optional field `node_name:
Option<String>` on `VllmServiceSpec` carries the choice; `build_deployment`
translates it into the `nodeSelector` when set, and emits no node constraint
when unset (preserving current single-service and CI behaviour).

The controller decides the node. The default scheduler executes the binding.

## Rationale

`nodeSelector` keeps the default scheduler in the loop. The scheduler still
performs resource fit (GPU availability via the device plugin), admission,
and taint/toleration evaluation before binding. The operator contributes the
one thing vanilla Kubernetes lacks — the warmth-based decision — and delegates
the mechanism it is not equipped to perform: the controller does not track
non-fleet pods or real-time device-plugin state, so it cannot itself
guarantee resource fit.

This is the clean reading of ADR-0003's "placement in the controller, not a
scheduler extender": controller = policy, default scheduler = mechanism.

## Alternatives considered

### spec.nodeName (rejected)

Setting `.spec.nodeName` binds the pod to the node directly, bypassing the
scheduler entirely. Consequences:

- No resource-fit check. If the controller's view of `NodeState` is stale
  relative to real device-plugin capacity (a race between the reporter's
  write and actual GPU availability), a force-bound pod is rejected at
  kubelet admission rather than staying visibly Pending. Under concurrent
  saturation — precisely the regime this project sets out to measure — this
  is the worst failure mode: the signal we want to capture cleanly is
  corrupted by a self-inflicted binding failure.
- Taints and tolerations are not evaluated, because no scheduler runs. This
  breaks spot-node handling (ADR-0005, forthcoming), where spot nodes are
  expected to carry taints the pod must tolerate.

Taking ownership of resource fit means reimplementing the scheduler, poorly,
inside a controller that lacks the state to do it. Rejected.

### Soft nodeAffinity, preferredDuringScheduling (rejected for v1)

Translating warmth into a weighted `preferredDuringScheduling` affinity and
letting the scheduler make the final choice is the most orthodox Kubernetes
division of labour. It is rejected for v1 because it dissolves the property
this project is built to demonstrate: if the final placement decision is the
scheduler's soft preference, the operator is no longer decisive, and the
emergent fleet behaviours under load (preemption mid-warmup, contention,
autoscaler oscillation) stop being deterministic and seed-reproducible. That
is a weaker, different project. This is the natural evolution point for the
spot block (preferred placement with fallback), where non-determinism in the
fallback path is acceptable and desired.

### Required nodeAffinity for a single hostname (rejected)

`requiredDuringScheduling` nodeAffinity matching one hostname is functionally
equivalent to `nodeSelector` for an exact single-node match, but more verbose.
`nodeSelector` is the honest minimal expression of "this exact node". Rejected
as visible over-engineering.

## Consequences

- `VllmServiceSpec` gains `node_name: Option<String>`. Additive, non-breaking:
  the field defaults to unset and `crdgen` regenerates the schema. Existing
  single-service and CI (`gpu=0`, placeholder) paths emit no node constraint.
- `build_deployment` gains a `nodeSelector` branch gated on the field.
- Downstream (spot block): real spot nodes will likely carry taints; the pod
  will need matching tolerations to land. Not exercised on kind (no spot
  taints). The evolution to `preferredDuringScheduling` + fallback is additive
  — `node_name` remains, only its translation in `build_deployment` changes.
  No API break.
