# Changelog

All notable changes to vllm-coldstart-operator are recorded here.
The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Printer columns on all three CRDs.** `kubectl get` showed only NAME and
  AGE. VllmService now shows PHASE, MODEL and AGE (MESSAGE with `-o wide`),
  FleetService PHASE, DESIRED, READY, WARMING and AGE, and NodeState WARMTH,
  GPU, SERVICES and AGE. CI asserts the values in the chart-install and e2e
  jobs, since a wrong jsonPath leaves a column empty without an error.
- **Chart install in CI.** A `chart-install` job builds the operator image,
  loads it into kind, installs the chart with its defaults, waits for the
  example VllmService to be Ready, and checks that the operator pod runs the
  image built in that job. Ready alone is not enough: without the load, the
  node pulls the published image with the same tag and the example still
  becomes Ready.
- **A FleetService with no candidate node says so in its status (ADR-0011
  D4).** It returned before writing any status, so its PHASE stayed empty
  and only the operator log explained why. It now writes a `message`, shown
  by `kubectl get -o wide`, plus a first phase for a fleet that has none; the
  next full status write clears the message. The status fields are now
  optional in the schema, with defaults, so that partial write can be read
  back.

### Changed

- **`rust-version` is 1.95, the toolchain CI and the Dockerfile use.** It
  said 1.83 and the README said MSRV 1.85; neither builds this lockfile.
  Cargo 1.83 stops on a dependency that needs the 2024 edition, and with
  1.85 cargo refuses `home@0.5.12`, which requires rustc 1.88. The
  toolchain check in CI now covers `rust-version` too.
- **The Helm release used by CI is declared once**, with its SHA-256, in the
  workflow `env`.
- **A preemption replacement may go to a node that is Cold for the fleet's
  model (ADR-0011 D2).** `select_replacement_node` dropped Cold survivors,
  so with no Warm or Warming node left the replica drained and held. Warmth
  now only orders the survivors; eligibility is the caller's filters
  (preemption notice, GPU capacity, node pool selector), and drain-and-hold
  remains for a replica they leave no target for. A Cold node starts slower:
  without a weights cache, minutes rather than 57 s. The kind rehearsal seeds
  its control plane Cold, which kept it out of replacement; it is now a
  possible target, ranked last.
- **Warmth is derived from the VllmServices the cluster runs (ADR-0011
  D1).** For a fleet of model M, a node reads Warm when a Ready VllmService
  of M is pinned to it, Warming when one is Warming, and Cold otherwise. The
  fleet's own children do not count: they already count as load, and as
  warmth they would pull every new replica onto the node the fleet already
  uses. The planner ranks on the warmer of that and NodeState warmth, so
  runs that seed warmth by hand rank as before, and `decidedOn.warmth`
  records the combined value. If the VllmServices cannot be listed, that
  reconcile ranks on NodeState warmth alone.

### Fixed

- **`nodePool.selector` restricts where a fleet places.** The CRD accepted
  it and described it as the node pool the fleet may place onto, but the
  controller never read it, so fleets placed regardless of it. New
  placements and replacements now go only to nodes carrying every listed
  label, with a Pod's nodeSelector semantics; existing placements are not
  moved. While a selector is set, a node whose labels cannot be read is not
  admitted: the opposite of the GPU capacity filter, which keeps such a
  node, because here the error would place a replica where it was
  excluded. A fleet that no node admits stays Placing, and the operator logs
  the selector. A fleet whose selector no node satisfies, which placed
  until now, stops placing new replicas. `spotPolicy` remains
  unimplemented, and the schema now says so.

## [0.4.0] — 2026-10-08

The first fleet-layer release whose chart installs the binary it
describes. The v0.3.0 chart shipped the FleetService and NodeState CRDs
but defaulted to the
0.2.1 image, which has neither the fleet controller nor the reporter. CI
now fails on that mismatch, and the release workflow will not publish an
image whose tag disagrees with the crate and the chart. Between v0.3.0 and
this tag the fleet layer gained its autoscaling surface (ADR-0009) and a
placement record that says what each decision was based on (ADR-0008).

### Upgrade notes

- **Apply the CRDs before `helm upgrade`.** Helm installs `crds/` on the
  first install and never updates it, so an upgrade from 0.3.0 keeps the
  0.3.0 schema. Apply `chart/crds/crd.yaml` from this tag first, for
  example with `kubectl apply -f chart/crds/crd.yaml`, the command the CI
  end-to-end job runs on the identical `deploy/crd.yaml`. The 0.4.0 schema
  only adds fields.
- **The default image now follows the chart's `appVersion`** (see Changed).
  Installs that set `image.tag` or `image.spec` are unaffected.

### Added

- **Scale subresource on FleetService** (`.spec.replicas` to
  `.status.replicas`), so an external autoscaler can drive the fleet, and
  `status.warmingReplicas` for placements that exist but are not serving
  yet (ADR-0009 D1, D3). KEDA can move a fleet between 0 and 1 replicas;
  scaling from 1 to N through the HPA is not supported (see
  `deploy/examples/keda`).
- **Demand signals on NodeState**: `requestsWaiting` and `requestsRunning`,
  summed by the reporter from vLLM's `num_requests_waiting` and
  `num_requests_running` (ADR-0009 D2).
- **`decidedOn` on each placement**: the strategy and the signal values the
  planner ranked on when it chose the node (ADR-0008 D1).
- **Signal age.** `kvCacheHitRateObservedAt` and `tokensPerJouleObservedAt`
  record when a value was measured, and `placement.signalMaxAgeSeconds`
  makes an older signal rank as never observed (ADR-0008 D2).
- **Release-consistency checks.** CI fails when `Cargo.toml`, the chart
  `version` and the chart `appVersion` disagree, when the chart rendered
  with its defaults and the reporter enabled produces any image other than
  `repository:appVersion` for the operator and the reporter, or when the
  workflow toolchain refs or the Dockerfile builder drift from
  `rust-toolchain.toml`. The release workflow refuses to publish an image
  under a tag that does not match `Cargo.toml` and the chart.

### Changed

- **EfficiencyAware ranks tokens/joule above KV-cache hit rate** (ADR-0008
  D4; ADR-0007 D3 had cache first).
- **Scale-down waits, scale-up does not.** Placements beyond `spec.replicas`
  are removed after `stableReconcilesRequired` consecutive reconciles
  (ADR-0009 D4).
- **Allocatable GPUs are a placement precondition.** A node without enough
  `nvidia.com/gpu` is not a candidate; a node whose capacity cannot be read
  is kept (ADR-0008 D3).
- **The chart's default image now follows `appVersion`.** `image.tag` is
  empty by default and the image helper falls back to the chart's
  `appVersion`; a `tag` or `spec` that you set still takes precedence. At
  v0.3.0, and on main until this change, the default was `0.2.1`: a chart
  installed at either ref runs an operator without the fleet controller or
  the reporter, against CRDs that declare FleetService and NodeState.
- **The Rust toolchain is pinned to 1.95** in `rust-toolchain.toml`, the
  version the Dockerfile builder already used. CI had installed `stable`,
  so its verdict depended on the day it ran: the same source passes clippy
  on 1.95.0 and fails on 1.99.0 (`double_must_use` on code generated by
  `async_trait`).

### Fixed

- **No tokens/joule for a GPU that did no work.** A reporting round with
  energy but no generated tokens published 0.0, which ranked an idle node
  last; the signal is now absent instead.
- **The committed CRDs match the types again.** Both copies were three
  fields behind (`signalMaxAgeSeconds` and the two `ObservedAt` fields) and
  the API server rejected them; CI now diffs both against the generator.
- **The agentic-kv efficiency figure in ADR-0006, ADR-0007 and the GIE
  study note.** It was cited as -69.2% tokens/joule; the experiment reports
  +69.2% for H2 over H0, which is -40.9% read from H2 toward H0. Each
  passage now gives both values and the direction.

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
