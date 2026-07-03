# PROVENANCE — fleet preemption reschedule

Reproducible record of the spot-preemption reschedule work on the fleet
orchestrator. Decisions are in `docs/adr/0004`, `0005`; this file records what
was built, how it was validated, and how to reproduce the validation.

## Done-definition status

1. Orchestrate N≥3 nodes as a fleet — DONE (validated on kind).
2. State-based placement (warmth/load) — DONE (validated on kind).
3. Spot preemption without cascade — DONE. Async preemption pass + reactive
   NodeState watch, validated on kind (see below).
4. Validated under saturation with a real measured signal (CV-bound) — OPEN.
   Requires a GPU session; kind validates behaviour, not the saturation-regime
   emergent signal.
5. PROVENANCE, defensible ADRs, reproducible — this file; ADRs 0004–0005.

## Commits (this block, on top of 7f8f887)

- 3ed8e92  feat: select_replacement_node pure seam (ADR-0005)
- a2ac546  feat: persist per-placement phase across reconciles
- 61cc509  feat: async preemption pass — detect, drain, bounded reschedule
- 0a699ab  feat: watch NodeState with a namespace-wide mapper (dec.4)
- 9101767  fix: exclude all preempted nodes from replacement candidates
- 8438a14  docs(adr): activeReschedules counts healthy in-flight moves

## What the preemption pass does

On each reconcile the fleet reads `preemptionNoticeDetected` off every
NodeState (the only reschedule trigger in v1, ADR-0005 dec.1). A placement
pinned to a preempted node is forced to Draining. Within
`max_concurrent_reschedules` minus the moves already in flight on healthy
nodes, it picks a warmth-first replacement (`select_replacement_node`,
excluding all preempted nodes and rejecting Cold) and repins the slot. With no
healthy target it drains and holds (dec.3). `status.activeReschedules` counts
only healthy in-flight moves, so a mass reclaim cannot deadlock the cap against
its own forced Draining. `.watches(NodeState)` fans a node event out to every
FleetService in the node's namespace (dec.4).

## Validation on kind (behavioural)

Cluster: kind `fleet-test`, 4 nodes (control-plane + worker/2/3). Operator run
locally against the kind context (`Client::try_default()` → kubeconfig).
FleetService `qwen-fleet`, replicas=5, `maxConcurrentReschedules=1`. Warmth was
seeded via `kubectl patch nodestate <n> --subresource=status`; preemption is
simulated the same way on `status.spot.preemptionNoticeDetected`.

- Drain + bounded replace: preempt a node carrying placements with warm
  survivors present. Observed: reschedule onto the warmth-first survivor, Cold
  never chosen, no replica dropped, and `activeReschedules` never exceeds the
  cap in any sample (sampled the transient at 0.3s intervals).
- Graceful hold (dec.3): preempt so the only survivors are Cold. Observed: all
  affected placements Draining, pins unchanged (no move onto Cold or onto a
  preempted node), `activeReschedules=0` — held, cap not deadlocked.
- Multi-node exclusion (9101767): preempt two nodes together; the replacement
  never targets the other preempted node.
- Reactive watch (dec.4): a NodeState metadata change triggers a FleetService
  reconcile in ~130ms (reason: "related object updated: NodeState/...") — not
  requeue-driven.

Note: on kind the vLLM pods never become Ready (no GPU/image), so placement
readiness stays Pending; this does not affect pin movement or phase transitions,
which is what this pass governs.

## Item-4 preparation block (zero GPU cost, commits c73866e..8336cbe)

Production-grade review plus a full dress rehearsal of the measurement on
kind, before spending GPU money. Findings, all fixed and validated here:

- Honest fleet status (c73866e): ready_replicas was hardcoded to 0 and the
  phase pinned to Placing; now derived (pure fleet_phase_for; Degraded
  surfaces drain-and-hold instead of hiding it).
- Declared drain mechanics (e4911da, ADR-0005 dec.5): explicit maxSurge=1 /
  maxUnavailable=0 plus terminationGracePeriodSeconds=120 on serving pods —
  make-before-break was previously inherited from k8s default rounding.
- Owned routing surface (4f76563): the operator created no Service, so
  dec.5's "leaves Service endpoints" had nothing to be true against. One
  owned ClusterIP Service per VllmService, same SSA manager.
- Reproducible child images (8aa1687): FleetServiceTemplate gained `image`;
  children were hardcoded to vllm/vllm-openai:latest.
- Self-aware replacement (d0234f3): candidates now fold in the fleet's own
  placements per node. With a stale NodeState reporter the replacement
  co-located two placements on one node while a free warm spare existed —
  run 20260703T165327 is the committed evidence. Fresh planning now also
  excludes preempted nodes.
- Decision logging in the preemption pass (d0234f3): T_decision was not
  reconstructable from the operator log (run 1's summary has it as null for
  exactly this reason).

Rehearsal result (run 20260703T211728, sim pods actually serving, load via
NodePort/kube-proxy so endpoint changes are followed): T_decision 113 ms
from notice to logged reschedule; surge-first honoured (old pod killed at
T+6.2 s, after the replacement was Ready); zero request errors on the
unaffected service in every window; max success-gap 2.57 s on the moved
service; 10 in-flight requests cut at pod kill — the sim exits on SIGTERM
without draining, real vLLM gets the 120 s grace window.

Declared rehearsal limits — exactly what the GPU session buys: no real
cold-start (sim readiness is immediate), no SIGTERM draining in the sim,
kind nodes share the host CPUs so unaffected-service isolation is
approximate. k8s event timestamps have 1 s resolution; sub-second causal
ordering comes from the operator log.

## Open

Item 4: the GPU session itself — 3 nodes (k3s, one GPU each, A/B/C =
serving / preempted / warm spare), real vLLM pods under saturating load,
3 repetitions. Measurement plan and harness (hack/loadgen, hack/rehearsal)
are locked and rehearsed. Known gaps deliberately deferred: scale-down
leaves orphan children; Helm chart RBAC/CRDs predate the fleet controller;
placement timestamps are written empty (hysteresis deferred, ADR-0005).
