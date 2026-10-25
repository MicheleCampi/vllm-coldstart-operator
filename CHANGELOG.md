# Changelog

All notable changes to vllm-coldstart-operator are recorded here.
The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **The time per output token is an objective per GPU class (ADR-0020).**
  For one L4 running Qwen2.5-7B-Instruct in bf16 on vLLM v0.11.0 it is at
  most 100 ms, in `hack/loadgen/slo-l4-qwen7b-bf16.json`: under the 137 ms a
  token of a reader at 320 words a minute, the requirement of ADR-0013,
  whose 50 ms no load meets on that GPU (floor about 64 ms).
  `test_slo_files.py` keeps every other objective equal to `slo.json`.

### Changed

- CI runs kind v0.33.0 with kindest/node:v1.36.4, the Kubernetes patch the GKE REGULAR channel defaulted to on 2026-10-25. The image checks of the chart job resolve the pod's imageID on the node to its config digest (hack/ci/pod-image-config.sh): containerd 2 reports the repo digest it made on import.
- CI runs on ubuntu-24.04 instead of ubuntu-latest, and every action is pinned by commit sha with its release in a comment (checkout v7.0.1 as in capstone-iac, cache v6.1.0, upload-artifact v7.0.2, kind-action v1.15.1, the Docker actions at their latest majors), all on the node24 runtime. The toolchain pin check now asks for one rust-toolchain sha commented with the channel.
- **Profile prompts are `context_tokens` long in real model tokens.** The
  NATO vocabulary was 1.72 Qwen2.5 tokens a word, so a GPU served prompts
  1.5 to 2.1 times the profile; 46 English words are one token each for
  Qwen2.5 and for the simulator, checked by `test_loadgen_tokens.py`
  against a pinned tokenizer. The pinned sequences change: keep 0.025
  seed 42 is `6c10349917a6`, keep 0.1 seed 42 `b5b7fff2f0fe`.

## [0.6.0] — 2026-10-23

The release in which the operator runs under a realistic load and through
faults. A fleet scales from one replica to N through the HPA and KEDA and
wakes from zero through the KEDA HTTP add-on (ADR-0014, ADR-0015); every
reconcile has a 30 s limit (ADR-0016); a cordoned node moves its replicas
like a preemption notice (ADR-0017); a fleet spreads across nodes before
warmth, and the Spread and BinPack strategies are removed (ADR-0018); a pod
leaves the Service 30 s before it stops serving, so a drain or a notice no
longer cuts a response in flight (ADR-0019). The load profile, the SLOs and
the faults that measure this run in CI on kind with the llm-d simulator
(ADR-0012, ADR-0013); none of it has run on a GPU.

### Added

- **Faults under load run in CI (ADR-0019).** A `faults-under-load` job
  serves the within-capacity replay through one NodePort over a fleet of
  two replicas on two workers, with a probe of four long streams across
  each fault, and runs a cordon and drain and a preemption notice on the
  node of one replica. It fails if a probe stream fails, if none is open
  at the fault, or if the replay is not the pinned one.
  `hack/loadgen/slo_check.py` gains `--client-only`, and its first tests.

- **The preemption path runs in CI.** A `preemption` job creates a kind
  cluster with two workers (`.github/kind/two-workers.yaml`), places a
  replica on the worker whose NodeState is Warm, signals a preemption
  notice there, and asserts that the replica moves to the Cold one: the
  placement and the child's pin change, the fleet is Ready again, and one
  child pod runs on the new node. It also asserts that the child's
  Deployment carries `maxSurge: 1` and `maxUnavailable: 0`, the strategy
  that keeps the old pod until the new one is available. The `e2e` job runs
  on one node and could not reach this path.

- **The reporter runs in CI.** The `e2e` job no longer writes the
  NodeState by hand: it starts the reporter with its synthetic source
  (`hack/e2e-reporter.sh`), which creates the NodeState and writes its
  status, and the job asserts that the reporter's next report leaves the
  warmth it writes in place. The `chart-install` job turns on the reporter
  DaemonSet with `reporter.synthetic` and a signal through
  `reporter.extraEnv`, and asserts that it reports from the image built in
  the job, through the chart's own ServiceAccount and Role.

- **KEDA scale-to-zero runs in CI.** A `keda` job installs KEDA 2.21.0
  from its chart, pinned by SHA-256, with its three images pinned by
  digest, applies `deploy/examples/keda`, and asserts that an idle queue
  takes `ci-fleet` to `spec.replicas: 0` with no child and that a waiting
  request brings it back to one Ready replica. The queue stub's nginx
  now comes from mirror.gcr.io by digest.

- **A fleet scales from 1 to N (ADR-0014).** The FleetService `scale`
  subresource now has a selector, `.status.selector`, set to
  `inference.michelecampi.dev/fleet=<name>`, so an HPA, and KEDA through
  it, can drive a fleet above one replica; until now the HPA refused a
  fleet with `InvalidSelector`. A `scale-out` job applies
  `deploy/examples/keda/scaledobject-scale-out.yaml`, a KEDA Prometheus
  trigger on the fleet's running plus waiting requests averaged over a
  minute, with a threshold of 1.3 per replica and a cap of 3; replays the
  10% trace profile through the fleet's Service; and asserts that the
  fleet reaches two replicas, never exceeds three, and serves from at
  least two pods. The objectives are reported, not asserted. The load
  generator's new `--force-close` opens one connection per request, so
  that the Service spreads requests across pods, and the run record says
  so in its `run_config`.

- **A fleet wakes from zero through the KEDA HTTP Add-on (ADR-0015).** A
  `scale-from-zero` job installs the add-on 0.16.0, its chart pinned by
  SHA-256 and its images by digest, with the interceptor fixed at three
  replicas (`hack/keda-http-install.sh`), and applies
  `deploy/examples/keda/scale-from-zero.yaml`: an `InterceptorRoute`
  that holds a request until a fleet replica is Ready, for at most 120 s
  and 16 requests per interceptor replica, and a ScaledObject from 0 to
  3 whose add-on trigger wakes the fleet and whose Prometheus trigger,
  ADR-0014's, scales it from 1 to N. The job starts the fleet at zero,
  replays the 10% trace profile through the interceptor, and asserts
  that a replica became Ready, that no request was rejected (503) or
  held past readiness (504), and that at least 99.5% were served
  (`hack/scale-from-zero/coldstart.py`); then, with no load, that the
  fleet returns to zero 300 to 480 s after the last response with no
  pod left, and it reports the replica-seconds the fleet stayed up with
  nothing to serve (`hack/scale-from-zero/idle.py`). The objectives are
  reported, not asserted: from zero they miss, and ADR-0015 breaks the
  misses down. `slo_check.py --from-zero` subtracts the wait of the
  requests held before the first token from the client's sums before
  comparing them with the simulator's, and `hack/scale-out/measure.sh`
  takes `OUT`, `HPA` and `FROM_ZERO` from the job.
- **The sim-load job records the cluster and the operator when it
  fails.** On the first attempt of 546fe53 the Ready wait timed out
  after 180 s, and `operator.log` held one reconcile that never
  finished; the client's read timeout is 295 s. On failure the job
  now prints the API server's `/readyz?verbose`, the VllmService,
  Deployment, Service and pods with their `describe`, the events in
  time order, and the operator's `vcso_` metrics. Every call is
  bounded, since the API server may be what does not answer.

### Changed

- **A fleet spreads across nodes before warmth (ADR-0018).** Every
  placement ordering now ranks first the replicas of the same fleet
  already on a node, fewer first, so a fleet shares a node only when no
  other eligible node is left. Before, warmth came first, and with one
  Warm node every replica landed there. The second replica can start on
  a colder node.

- **`Spread` and `BinPack` are removed from `placement.strategy`.** They
  were accepted and ordered like `WarmthFirst`. The CRD now refuses them.
  A fleet stored with either is not placed or moved, keeps its replicas
  running, and says why in `status.message`; the other fleets reconcile.

- **Build images come from mirror.gcr.io.** A Docker Hub outage could stop the
  release build before the Dockerfile ran: the BuildKit builder, the
  Dockerfile frontend and the Rust base image all came from Docker Hub. They
  now come from mirror.gcr.io, Google's cache of Docker Hub, which served the
  same digests for all three when this was checked. The frontend and the Rust
  base, which decide what the image contains, are pinned by digest, so a
  stale mirror fails the build instead of changing it. A CI step fails if
  any of the three stops coming from the mirror, or if the frontend or the
  base loses its digest.

- **The simulator load test replays a production trace.** The `sim-load`
  job drove 30 seconds of closed-loop load at a fixed concurrency of four,
  with prompts of one length and responses capped at 64 tokens. It now
  replays, open-loop, a seeded 10% sample of the busiest five minutes of
  the Azure LLM inference trace 2023 (conversation service, CC-BY-4.0):
  each request leaves at its time in the trace with the trace's prompt and
  response lengths (`hack/loadgen/profiles/azure-conv-2023-peak.csv`,
  rebuilt by `hack/loadgen/build_profile.py`). The job fails if the
  replayed sequence is not the one pinned in the workflow, if any request
  is not answered with 200 or is served a token count other than the
  profile's, or if a request leaves more than 250 ms after its time.
  Latency is recorded and not asserted. The simulator now admits 16,384
  tokens per request. See ADR-0012.

- **The simulator load test asserts service level objectives (ADR-0013).**
  `sim-load` replays the trace profile twice. At 2.5% it asserts, from
  `hack/loadgen/slo.json`, a time to first token of at most 1 s and a time
  per output token of at most 50 ms for 95% of requests, and 99.5% of
  requests served; one replica of the simulator met them up to about 0.4
  requests per second. At 10% it overloads the replica and reports the
  objectives, missed, with the error budget consumed. Both runs check the
  load generator's mean latencies against the ones the simulator measures,
  within 25 ms. The checks run in `hack/loadgen/slo_check.py`.

- **A fleet's pods carry the fleet's label.** A fleet's children already
  carried `inference.michelecampi.dev/fleet`; their pods now carry it
  too, in the pod template only, so the Deployment's selector, which is
  immutable, does not change. A standalone `VllmService` keeps its pod
  template as it was.

- **One KEDA install for all three jobs.** The `keda`, `scale-out` and
  `scale-from-zero` jobs install KEDA through the same script,
  `hack/keda-install.sh`.

### Fixed

- **A fault no longer cuts the responses in flight (ADR-0019).** A drain
  or a move sent SIGTERM to a pod still serving streams, and the
  simulator stops within 0.3 s. Every child pod now sleeps 30 s in a
  preStop hook first, out of the Service, and its grace is 120 s with a
  GPU and 40 s without one. Measured on kind: 3 and 2 cut streams before,
  0 after.

- **A drained node no longer strands its replicas.** A cordon now
  moves a fleet's placements off the node as a preemption notice does,
  under the same cap and drain-and-hold; the Nodes are read on every
  reconcile, and a cordoned node is never a candidate. Before, after a
  `kubectl drain` the child pod stayed Pending, pinned to the cordoned
  node. The `preemption` job now cordons and drains a worker
  (ADR-0017, amending ADR-0005 decision 1).
- **A reconcile no longer stalls for the client's read timeout.**
  Every reconcile of both controllers now fails after 30 s, is
  counted under `timeout` in `vcso_reconcile_failures_total` and
  is requeued. On the first attempt of 546fe53 one reconcile
  waited on the API server past the job's 180 s, while kube-client
  2.0.1 would have given up only at 295 s. In CI the slowest
  reconcile took 204 ms (ADR-0016).
- **The scale-from-zero job no longer races KEDA before the
  replay.** It waited for the HPA's `ScalingActive`, which is `False`
  once KEDA has taken the idle fleet to zero, and KEDA may do that as
  soon as the ScaledObject exists (ADR-0015, fact 4). On the push of
  546fe53 to main it did, in the second the HPA was created, and the
  job failed after 120 s. The job now waits for `AbleToScale`, which
  holds at zero.

### Upgrading from 0.5.0

Apply the CRDs before `helm upgrade`, with the commands given for 0.5.0
below and this release's tag. With the 0.5.0 CRDs left in place, the
FleetService `scale` subresource has no `labelSelectorPath`, and the HPA
keeps refusing a fleet with `InvalidSelector`.

Every fleet child rolls out once, because its pod template gains the
fleet label. The rollout surges first (`maxSurge: 1`,
`maxUnavailable: 0`) and the new pod is pinned to the old one's node, so
on a node with no spare GPU the new pod stays Pending while the old one
keeps serving, and the rollout completes only once a GPU is free there.
This follows from the strategy and the pin; no GPU node has run it.

Before the upgrade, list the fleets that set a removed strategy:

```
kubectl get fleetservice -A -o jsonpath='{range .items[*]}{.metadata.namespace}/{.metadata.name} {.spec.placement.strategy}{"\n"}{end}' | grep -E ' (Spread|BinPack)$'
```

Set them to `WarmthFirst`, which is what they got, or `EfficiencyAware`.
One left behind is not managed until it is changed, and recovers when
it is (ADR-0018, rehearsed on kind). Replicas already sharing a node
stay there: the new ordering applies to the next placement.

The same rollout gives every child pod the preStop sleep and the grace
(ADR-0019): from 0.5.0 both changes reach the pod template together. From then on a pod that leaves,
for a drain, a move, a scale-in or a scale to zero, takes 30 s longer to
go; a fleet scaled to zero frees its node 30 s later.

Rehearsed on kind, 2026-10-23: a v0.5.0 install made with that tag's
chart and its public 0.5.0 image, running `ci-placeholder` and the
`ci-fleet` example given a `nodePool.selector` on the kind node's
hostname (with the chart's defaults no reporter runs, so a fleet needs a
selector, ADR-0011); then this release's CRDs applied and `helm upgrade`
with its chart and the 0.6.0 image built locally and loaded into kind.
The operator pod ran that image (imageID equal to its config digest).
Both pods came back Ready with the 30 s preStop sleep and a 40 s grace,
the child's pod with the fleet label it lacked under 0.5.0; the fleet's
scale subresource returned the selector
`inference.michelecampi.dev/fleet=ci-fleet`; the operator logged no
ERROR line and no failed reconcile. The HPA refusing a fleet under the
0.5.0 CRDs is what ADR-0014 observed, not part of this rehearsal; the
0.5.0 CRD's scale subresource has no `labelSelectorPath`.

## [0.5.0] — 2026-10-10

The release in which a FleetService that sets `nodePool.selector` works on
a chart installed with its defaults. Its candidates were the NodeStates,
which only the reporter creates and the chart leaves off, and the warmth it
ranked on was either set by hand or the Cold a NodeState defaults to.
ADR-0011 makes `nodePool.selector`, when set, define the
candidates from the cluster's Nodes, derives warmth from the VllmServices
the cluster runs, keeps warmth out of the eligibility of a replacement after
a preemption notice, and has a fleet with no candidate say why in its
status. `nodePool.selector` itself, in the schema since the fleet CRDs
shipped in 0.3.0, was never read before this release.

### Upgrading from 0.4.0

Apply the CRDs before `helm upgrade`. Helm installs the CRDs in a chart's
`crds/` directory but does not upgrade them, and 0.5.0 changes the
FleetService schema: a `status.message` field, a Message column, and status
fields that are no longer required. With the 0.4.0 schema left in place, a
fleet with no candidate cannot write its status: the API server answers 422,
`[status.activeReschedules: Required value, status.readyReplicas: Required
value]`, and that fleet's reconcile fails.

```bash
git clone --branch v0.5.0 --depth 1 https://github.com/MicheleCampi/vllm-coldstart-operator
kubectl apply -f ./vllm-coldstart-operator/chart/crds/crd.yaml
helm upgrade vcso ./vllm-coldstart-operator/chart --namespace vllm-system --wait
```

`kubectl apply` warns that each CRD lacks the
`kubectl.kubernetes.io/last-applied-configuration` annotation, because Helm
created them, and adds it; the CRDs are configured all the same.

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
  (preemption notice, GPU capacity, node pool selector and, with a
  selector, a Node not marked unschedulable), and drain-and-hold remains
  for a replica they leave no target for. A Cold node starts slower:
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
- **With `nodePool.selector` set, the candidates are the Nodes it admits
  (ADR-0011 D3).** They were the NodeStates with a status, which only the
  reporter creates, so a fleet on a chart with its defaults placed nothing.
  A selected Node without a NodeState is now a candidate with every measured
  signal absent; Nodes marked unschedulable (`kubectl cordon`) are not, and
  taints are left to the scheduler. A fleet whose selector admits no
  schedulable Node places nothing and says so in its status. Without a
  selector the candidates are the NodeStates, as before.

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
- **A node with two NodeStates is one candidate.** NodeState is namespaced,
  so the reporter's and a hand-made one can share a name, and both became
  candidates: the planner counted that node twice. The one in the
  lexicographically first namespace that has a status now lends its
  signals, whatever order they are listed in; a preemption notice on either
  still counts.

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
