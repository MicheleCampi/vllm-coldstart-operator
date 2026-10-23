# ADR-0018: A fleet spreads across nodes before warmth

Status: Accepted, rehearsed on kind 2026-10-23
Date: 2026-10-23
Amends: ADR-0007, ADR-0011 D1

## Context

ADR-0017 left one gap in "What this ADR does not prove": the replicas of
one fleet were not spread across nodes. Both orderings opened with
warmth, so with one Warm node and one Cold node every slot of a fleet
landed on the Warm one, and one node failure took the whole fleet.
`Spread` and `BinPack` were accepted by the CRD and ordered like
`WarmthFirst`, without saying so to the user.

## Facts that constrain the design

1. `select_node_for_placement` and `select_node_efficiency_aware`
   (`src/fleet_placement.rs`) both ranked `warmth_rank` first;
   `select_replacement_node` goes through `select_node_with_strategy`, so
   a rule in the two orderings also covers replacements.
2. The controller already counted this fleet's pins per node, Ready or
   not (`own_placements_per_node`), but summed them into
   `active_service_count` with the other fleets' services, below warmth.
3. Warmth comes from the VllmServices of the same model in other fleets
   (ADR-0011 D1): a node can be Warm because another fleet runs there.
4. The fleet controller watches every FleetService with one typed list
   (`src/fleet_controller.rs`, `Controller::new`). Rehearsed on kind: one
   stored FleetService whose strategy the new type does not know fails
   the initial list (`InitialListFailed(SerdeError("unknown variant
   `Spread`"))`), and no fleet is reconciled, a healthy one included.

## Decisions

- A new field on each candidate, `own_replicas`: the replicas of the
  fleet being placed that are already pinned to the node. It is the
  first key of every ordering, lower wins; the rest of each ordering is
  unchanged. A fleet shares a node only when no other eligible node is
  left. The planner adds one to it for each slot it fills in a batch.
- `Spread` and `BinPack` are removed from the CRD: a new write with
  either is refused by the API server. Accepting them and ordering like
  `WarmthFirst` was a silent degrade.
- A stored object with a value the type does not know reads as a hidden
  variant, `Unsupported` (`#[serde(other)]`, not in the schema; the
  generated CRD is byte for byte the same with or without it). The
  controller plans and moves nothing for that fleet, leaves its children
  running, writes why in `status.message` the way ADR-0011 D4 does, and
  requeues; every other fleet reconciles.
- Rejected: the spread rule only under `Spread` (the default fleet would
  stay exposed to one node failure); hard anti-affinity (a fleet with
  more replicas than nodes would not place); implementing `Spread` and
  `BinPack` as separate orderings (no user asked for them, and BinPack
  is the risk this ADR removes); removing the values with no
  `Unsupported` (one old object stops every fleet, fact 4); a note in
  the changelog only (whoever skips it gets the same outage).

## Rehearsed, 2026-10-23

- Unit: `a_fleet_spreads_across_nodes_before_warmth`, one Warm and one
  Cold node, two slots. With `own_replicas` taken out of both orderings
  it fails with `["warm", "warm"]`; with it, `["warm", "cold"]`.
- Unit: `a_stored_removed_strategy_reads_as_unsupported`.
- kind, `preemption` job, new step after the cordon: the fleet goes to
  two replicas with its first worker Warm and the second Cold. Without
  the change slot 1 lands on the first worker, next to slot 0, and the
  step fails; with it slot 1 is on the second worker, both Ready, one
  pod per node.
- kind, by hand on optim-dev, not in CI: a fleet stored with `Spread`
  under the 0.5.0 CRD, then this CRD and this binary, next to a healthy
  fleet. Before `Unsupported` the healthy fleet's child was never
  created; with it the healthy fleet's child was created and reached Ready, the other kept its
  two children and its message. Set back to `WarmthFirst`, it reported
  Ready, two ready, an empty message, and nothing moved.

## Consequences

- One node failure no longer takes every replica of a fleet while
  another eligible node exists.
- Cost: the second replica can start on a colder node. On a real GPU
  node that is a longer cold start for that replica; it has not been
  measured on a GPU, the simulator has no cold start to measure.
- A fleet that still says `Spread` or `BinPack` stops being managed
  until it is changed; its replicas keep serving, its status says why.

## What this ADR does not prove

- Placements already on one node are not moved apart: pins are kept
  (ADR-0017), so a fleet placed before this change stays where it is
  until a reclaim, a cordon or a scale places it again. Seen on kind:
  after the fix the rehearsal fleet kept both replicas on one worker.
  Rebalancing would move replicas with no fault, which ADR-0005 keeps
  out; a separate decision.
- Whether a node with one free GPU admits a second replica is the
  ADR-0008 D3 filter, not read again for this change.
- The kind rehearsals have no load. Faults under load are R6 item 3.
