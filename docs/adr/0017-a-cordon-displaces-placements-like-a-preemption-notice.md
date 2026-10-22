# ADR-0017: A cordon displaces placements like a preemption notice

Status: Accepted, rehearsed on kind 2026-10-22
Date: 2026-10-22
Amends: ADR-0005, decision 1

## Context

ADR-0005 made a spot preemption notice the only trigger that moves a
fleet's placements, under a concurrency cap and with drain-and-hold
when no survivor is left. Node maintenance takes a node out another
way: `kubectl cordon` marks it unschedulable and `kubectl drain`
evicts its pods. Each child is pinned to its node by a nodeSelector
(ADR-0004), so the Deployment's new pod can only go back to the
cordoned node, and stays Pending.

## Facts that constrain the design

1. The fleet controller preserves an existing pin and plans a node only
   for slots that have no child yet; the preemption pass is the only
   path that moves a placed slot (`src/fleet_controller.rs`,
   `reconcile_inner`, steps 3 and the preemption pass).
2. ADR-0005 decision 1 excludes warmth changes and node disappearance
   because they oscillate; the cap and drain-and-hold are sound because
   a preemption notice is "a rare, one-way, urgent event".
3. Before this change the Nodes were read only for a fleet that
   requests GPUs or has a node pool selector, so without either a
   cordon was never seen; with a selector a cordoned node was dropped
   from the candidates, but a slot already on it stayed.
4. The chart's ClusterRole already grants `get` and `list` on `nodes`
   (`chart/templates/rbac.yaml:48-50`).

## Decisions

- A cordon (`spec.unschedulable: true`) is a second trigger. It has the
  properties decision 1 asks of one: an operator sets it on purpose,
  it is one-way, and it says what a notice says, that the node is about
  to stop serving. The controller builds `displaced` = preempted
  nodes plus cordoned nodes and uses it wherever it used the preempted
  set: moves in flight, the replacement candidates, the preemption
  pass with its cap, and drain-and-hold.
- The Nodes are read on every reconcile. If the read fails, no node
  counts as cordoned: an API hiccup must not start moves.
- Without a node pool selector, cordoned nodes are also dropped from
  the candidates for fresh slots.
- Rejected: leaving decision 1 as it was and documenting that a drain
  needs a manual scale or uncordon (the platform would not survive
  routine maintenance); also moving on node disappearance or NotReady
  (signals that can flap, the case decision 1 keeps out, which would
  need the hysteresis ADR-0005 deferred).
- Placements do not move back after an uncordon: pins are preserved,
  so there is nothing to oscillate.

## Rehearsed, 2026-10-22

The `preemption` job gains a step after the notice case: it clears the
notice on the first worker, runs `kubectl cordon` and `kubectl drain`
on the second, where the replica is, and asserts that slot 0 moves to
the first worker, Ready, with one child pod there, the child pinned to
it, a reschedule line in the operator's log and no drain-and-hold.

Run with `job-run.py` on kind on optim-dev, same two-worker topology:

- without this change the step fails: after the drain the fleet is
  `Placing` with no pod, slot 0 still on the cordoned worker;
- with it the step passes: the replica is on the first worker, Ready,
  one pod.

## Consequences

- A drained node no longer strands its replicas; they move under the
  same cap as a reclaim, and a fleet with no survivor drains and holds.
- One Node list per reconcile for every fleet.

## What this ADR does not prove

- The replicas of one fleet are not spread across nodes: placement
  orders on warmth before active service count, so with one Warm and
  one Cold node every slot lands on the Warm one, and draining it
  moves the whole fleet at once, within the cap. `Spread` and
  `BinPack` order like `WarmthFirst` today. A separate decision.
- The rehearsal has one replica and no load. Faults under load are R6
  item 3, which this change makes possible.
- A cordon with no survivor holds the replica in Draining on a node the
  drain has already emptied; the fleet is then down until a node is
  uncordoned or added (deduced from the code, not rehearsed).
