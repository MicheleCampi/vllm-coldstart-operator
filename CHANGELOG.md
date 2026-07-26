# Changelog

All notable changes to vllm-coldstart-operator are recorded here.
The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

_Nothing yet._

## [0.3.0] — 2026-07-26

The fleet layer. v0.2.1 tagged a single-service operator on managed GPU
clusters and nothing more: the FleetService CRD, the placement strategies,
the per-node reporter, and every GPU number the README quotes landed on
main afterwards and were never carried by a tag. This release exists so
that the claims in the README are verifiable at a released ref rather than
at a moving branch. The crate version also rejoins reality here — it had
stayed at 0.1.0 across both 0.2.x tags.

### Added

- **FleetService CRD and fleet-level orchestration.** Placement of vLLM
  services across GPU nodes, with warmth as the first placement signal: a
  node with the model already cached locally recovers in about a minute, a
  cold one in several.
- **Spot-preemption handling, make-before-break (ADR-0005).** On a
  preemption notice the operator surges a replacement onto the warmest
  surviving node, waits for Ready, and only then drains the doomed pod,
  with a hysteresis cap against thundering herds. Validated on a real
  3-node A10 fleet under closed-loop load, 3 repetitions: zero errors on
  the unaffected service in every window of every rep, replacement Ready
  in 57 s, maximum service gap 2.3 s. Notice injection is disclosed as
  the simulation boundary. Evidence in `hack/gpu-session/runs/2026-07-04`.
- **EfficiencyAware placement strategy (ADR-0007).** Nodes ranked on
  energy and cache signals rather than warmth alone, threaded through both
  decision points (initial placement and replacement selection).
- **Per-node reporter DaemonSet.** Samples NVML energy and utilization,
  scrapes vLLM prefix-cache counters, and joins the two into
  tokens-per-joule on the same reporting round, publishing to a NodeState
  status the planner reads. Disjoint field ownership via merge-patch.
- **Apache-2.0 LICENSE**, which the repository had been published without.

### Changed

- **Placement signals are `Option`, not zero-defaulted.** Absence and
  idleness are different states; a serde default of 0.0 silently biased
  the comparators toward "idle node". The type system now prevents a real
  source from fabricating zeros.
- **Chart:** reporter DaemonSet (opt-in), namespaced least-privilege Role,
  operator ClusterRole realigned to the fleet, `image.spec` wired into the
  template helper instead of being documented but never read.
- Single-reconciler rollout pinned (`strategy: Recreate`).

### Known limits

- The level-3 GPU session (8 reps, ABBA+BAAB, 3×A10) validated the
  **mechanism**: signals populated from real hardware on every node, the
  two strategies diverging deterministically at both decision points.
  It did **not** answer whether efficiency-aware placement improves fleet
  hit-rate or tokens-per-joule — the load generator drives a fixed
  endpoint and nothing routes traffic to a service just placed, so both
  arms measure the same node. The near-zero deltas are recorded in the
  experiment design as a topology limit, not published as a verdict.
- CRDs are `v1alpha1`. No compatibility guarantee across minor versions.

## [0.2.1] — 2026-06-14

`LD_LIBRARY_PATH` fix for GPU pods on managed clusters. Chart bumped to
0.2.1; the crate version was left at 0.1.0.

## [0.2.0] — 2026-06-13

Real vLLM serving on managed GPU clusters. Single-service scope: no fleet
CRD, no placement strategies, no energy or cache signals.

## [0.1.0] — 2026-06-12

Helm chart and ArgoCD GitOps with Image Updater on semver release tags.
Cold start as a first-class lifecycle signal for a single vLLM service.
