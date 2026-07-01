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

### 2. Bounded reschedule: concurrency cap active, hysteresis deferred

- **Concurrency cap (active in v1)** — `spec.hysteresis.max_concurrent_reschedules`
  caps simultaneous reschedules; the controller tracks in-flight reschedules in
  `status.activeReschedules` and refuses to start a new one past the cap. When a
  whole spot pool is reclaimed at once, displaced replicas are drained and
  replaced in bounded waves, not all at once. This is the primary anti-cascade
  brake and it is the one that matters for the preemption trigger.
- **Hysteresis (deferred)** — `spec.hysteresis.stable_reconciles_required` and
  the `stableSince` timestamp on `PlacementStatus` exist to damp flapping, but
  are *not* made load-bearing in v1. Hysteresis guards against ping-pong on a
  *noisy, oscillating* signal; the only v1 trigger is preemption (decision 1),
  which is a rare, one-way, urgent event that does not oscillate. Wiring a
  reconcile-counter or a timestamp window now would add per-placement state to
  guard against a failure mode v1 cannot exhibit.

The cap bounds the *burst*, which is the real risk when a spot pool is reclaimed
together. Hysteresis bounds the *frequency*, which only becomes a risk once
reschedules can trigger on an oscillating signal such as warmth — explicitly out
of scope in v1 (decision 1). Hysteresis becomes load-bearing when that trigger
is added; the spec fields are already in place for it.

Trade-off, stated: v1 protects against the burst but not the ping-pong. This is
sound precisely because preemption does not oscillate; it would not be sound if
warmth were a trigger.

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
  start, down as they complete, and read to enforce the concurrency cap. The
  hysteresis fields remain defined but inert in v1 (see decision 2).
- Validation on kind: simulate preemption by patching
  `status.spot.preemptionNoticeDetected=true` on a node carrying placements,
  observe bounded drain + replace, and the graceful-hold case by preempting
  with no healthy target available.
