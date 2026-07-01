# ADR-0005: Spot preemption triggers bounded reschedule, degrading gracefully

Status: Accepted

## Context

ADR-0003 established the fleet controller owns placement policy; ADR-0004
fixed how a placement lands (nodeSelector, scheduler in the loop). This ADR
covers the fleet's response to spot preemption: a node signalling it is about
to be reclaimed, surfaced as `status.spot.preemptionNoticeDetected` on its
`NodeState`.

The pure phase logic already exists: `placement_phase_for` transitions a
placement to `Draining` on a preemption notice even when the node is Warm
(test `preemption_notice_forces_draining_even_if_warm`). This ADR fixes the
*wiring and the defensive bounds* around that transition — the part that
decides whether a reclaimed spot pool drains gracefully or cascades into an
overload of the surviving nodes.

"Without cascade" (project done-definition item 3) is the whole point: the
failure mode to avoid is a whole spot pool getting reclaimed at once, the
fleet stampeding every displaced replica onto the few healthy nodes, and
those nodes falling over under the cramming.

## Decision

### 1. Trigger: preemption notice only

A reschedule is triggered *only* by `preemptionNoticeDetected` on a node
carrying placements. Warmth transitions (Warm→Cold) and node disappearance do
*not* trigger a reschedule in v1.

Rationale: warmth is a steady-state signal that oscillates; rescheduling on
warmth would cause the fleet to churn placements continuously — the opposite
of anti-cascade. The preemption notice is the only signal that is both urgent
(a hard deadline before the node vanishes) and unambiguous.

### 2. Bounded reschedule: two brakes, both active

- **Concurrency cap** — `spec.hysteresis` backs `max_concurrent_reschedules`;
  the controller tracks in-flight reschedules in `status.activeReschedules`
  and refuses to start a new one past the cap. When a whole spot pool is
  reclaimed at once, displaced replicas are drained and replaced in bounded
  waves, not all at once. This is the primary anti-cascade brake.
- **Hysteresis** — a placement that has just moved (its `stableSince` is
  recent) is not moved again within the hysteresis window. This stops the
  ping-pong where a replace immediately re-triggers on transient state.

Both are active. The cap bounds the *burst*; hysteresis bounds the
*frequency*. They address different cascade shapes and neither subsumes the
other.

### 3. No healthy target: drain and hold, do not force

When a displaced replica has no healthy node to move to — every candidate is
Cold, or capacity is exhausted — the replica stays in `Draining` and the
fleet does *not* force it onto an unsuitable node. The fleet runs with fewer
live replicas until a healthy node appears.

Rationale: graceful degradation *is* the anti-cascade property. Forcing the
placement onto a Cold or full node is exactly the cascade — it overloads a
node that was not chosen because it could not take the load. Fewer live
replicas is the correct, honest failure mode; the status reflects the
degradation rather than hiding it behind a bad placement.

### 4. Watch mapper: namespace-wide, not reverse-indexed

The controller gains `.watches(NodeState, ...)` so a node's preemption notice
wakes the fleet without waiting for the requeue interval. The mapper wakes
*all* FleetServices in the node's namespace, not only those with a placement
on the changed node.

Rationale: the reconcile is idempotent and cheap (list nodes, plan, apply —
no work when nothing changed). A reverse-index from node to affected fleets is
a premature optimisation for the fleet sizes this targets. Trade-off: a node
event wakes fleets that do not care, doing a no-op reconcile each. Acceptable;
revisit if fleet count per namespace grows large.

## Consequences

- New pure function to select a replacement node for a displaced placement,
  excluding the preempted node and respecting the concurrency cap. Unit-
  testable in isolation, same pattern as `select_node_for_placement`.
- The fleet reconcile gains a preemption pass: detect placements on preempted
  nodes, transition to Draining via the existing pure logic, and — within the
  cap and hysteresis window — plan a replacement, degrading gracefully when
  none is available.
- `.watches(NodeState)` added to the fleet Controller with a namespace-wide
  mapper. This is the first reactive (non-poll) path in the fleet controller.
- `status.activeReschedules` becomes load-bearing: written up as reschedules
  start, down as they complete, and read to enforce the cap.
- Validation on kind: simulate preemption by patching
  `status.spot.preemptionNoticeDetected=true` on a node carrying placements,
  observe bounded drain + replace, and the graceful-hold case by preempting
  with no healthy target available.
