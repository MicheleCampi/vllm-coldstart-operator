# vllm-coldstart-operator

A Kubernetes operator, written in Rust with [kube-rs](https://kube.rs/), that manages vLLM inference workloads and treats **cold start as a first-class signal** — first for single-service lifecycle, now as the basis for **GPU fleet orchestration under spot preemption**, validated with measured data on real GPUs.

Kubernetes decides a pod is ready when its process is up. For an LLM inference server that is the wrong moment: the process is alive, but it still has to load weights and warm up before it can serve a token. This operator models that gap explicitly, and uses it to make placement and rescheduling decisions: a warm node with the model in cache is a fundamentally better reschedule target than a cold one, and the difference is measured in minutes of service degradation.

The operator is the operational half of a pair of tools:

- [vllm-coldstart-probe](https://github.com/MicheleCampi/vllm-coldstart-probe) — an eBPF profiler that **measures** where vLLM cold start goes (syscalls, libcuda, GPU warmup).
- **vllm-coldstart-operator** (this repo) — **acts** on that knowledge inside a cluster.

## Measured results: preemption without cascade

The headline claim — a spot preemption on one node does not cascade to the rest of the fleet, and recovery is bounded — was validated on a real 3-node GPU fleet (3x NVIDIA A10, Lambda, k3s v1.36.2+k3s1, vLLM v0.23.0 digest-pinned, Qwen2.5-7B-Instruct) under closed-loop load, 3 repetitions:

| Metric | rep1 | rep2 | rep3 |
|---|---|---|---|
| Errors on unaffected service (all windows) | **0** | **0** | **0** |
| Replacement pod Ready (warm spare, model cached) | 57s | 57s | 57s |
| Old pod killed (after replacement Ready) | T+58s | T+59s | T+59s |
| Max service gap on moved service | 2.3s | 2.27s | 2.28s |
| Failed requests on moved service (per event) | 4 | 4 | 6 |

The recovery sequence is **make-before-break**: on a preemption notice the operator surges a replacement onto the warmest surviving node, waits for it to become Ready (weights load from a local `modelCacheHostPath` cache, not the HF CDN — that is the difference between 57s and several minutes), and only then drains the preempted node. Decision latency from notice to reschedule action is sub-2s.

Full evidence — per-request JSONL, operator logs, Kubernetes events, resource snapshots, provenance (driver, digests, model snapshot hash) — is committed under [`hack/gpu-session/runs/2026-07-04/`](hack/gpu-session/runs/2026-07-04/), including per-rep `timeline.png` plots.

**Disclosed boundary:** the preemption *notice* was injected via a `NodeState` status patch; detecting the cloud provider's real notice (e.g. the metadata endpoint) is out of scope for this validation. What is measured is everything downstream of the notice: decision, surge, drain, and service continuity.

Method note: the entire preemption mechanic was first validated on a zero-cost kind rehearsal harness ([`hack/rehearsal/`](hack/rehearsal/)); the GPU session then reproduced it on real hardware in ~1.5h for $5.58.

## Measured results: autoscaling that counts warming replicas

A replica that is warming is capacity that has been paid for and has not
arrived yet. An autoscaler that reads only ready replicas cannot see it, so it
re-requests the same capacity on every tick for the whole warm-up.

`FleetService` publishes `status.warmingReplicas` alongside `status.replicas`
and exposes the `scale` subresource, so the consumer of that number does the
subtracting ([ADR-0009](docs/adr/0009-replicas-follow-demand.md)). To
find out whether publishing it changes anything, two arms of one instrumented
consumer were run against the same workload: one counting ready replicas
only, one counting ready plus warming.

Four pairs on kind, 3 workers, a 60 s synthetic warm-up, demand stepping x4
at t=120 s, each pair replaying one frozen trace whose hash was checked
identical in both arms, order alternated NW/WN
([evidence](hack/adr0009-d5-experiment/runs), [design and
results](hack/adr0009-d5-experiment/DESIGN.md)):

| | ready-only | ready + warming |
|---|---|---|
| peak replicas | 27, 28, 28, 28 | 4, 4, 4, 4 |
| replica-seconds held | 12,364 | 2,113 |
| requests served per replica-second | 0.330 | 1.931 |
| requests served / failed | 4080 / 0 | 4080 / 0 |

Peak allocation falls 85.6% (bootstrap 95% CI [85.3%, 85.7%]) and held
allocation falls 5.85x, with 10,252 replica-seconds per run that no request
used. The replica-second ratio is the smaller figure and the one to quote:
peak flatters the result, since the ready-only arm also spends the pre-step
part of each run at low counts. Both arms served every request with no
failures, so the excess buys nothing.

Two limits stated with the number. The confidence interval is narrow because
the warming-aware arm sits at exactly 4 in every rep, so it describes the
reproducibility of the mechanism more than the uncertainty of the effect. And
the effect is quoted at the consumer's 5 s tick: the ready-only peak is a
function of how many ticks fit inside a warm-up window, falling to 22, 16 and
13 at 10, 15 and 20 s ticks. The mechanism is the claim; the percentage is
the claim at one tick and one warm-up time.

The consumer is instrumentation, not a product — not an HPA, not a KEDA
scaler, and its capacity model is a constant. What is measured is the value
of the information the operator publishes, not the quality of any particular
autoscaler.

## What it does

The operator manages two custom resources.

### `VllmService` — cold-start-aware single service

```yaml
apiVersion: inference.michelecampi.dev/v1alpha1
kind: VllmService
metadata:
  name: qwen-7b
spec:
  model: "Qwen/Qwen2.5-7B-Instruct"
  replicas: 1
  warmupStrategy: Eager   # Eager | Graph
```

For each `VllmService`, the controller reconciles an owned Deployment via idempotent server-side apply (owner references make deletion garbage-collect everything), maps `warmupStrategy` to real vLLM configuration (`Eager` = faster cold start for scale-to-zero; `Graph` = CUDA graphs on, faster steady state — the operational lever behind the probe's finding that CUDA graphs make cold start ~3x slower), and derives a cold-start-aware phase written to `/status`: `Pending` -> `Warming` (process up, not yet able to serve) -> `Ready` (warm and serving).

### `FleetService` — multi-node orchestration under preemption

```yaml
apiVersion: inference.michelecampi.dev/v1alpha1
kind: FleetService
metadata:
  name: gpufleet
spec:
  model: Qwen/Qwen2.5-7B-Instruct
  replicas: 2
  hysteresis:
    maxConcurrentReschedules: 1
  template:
    image: vllm/vllm-openai@sha256:...   # digest-pinned
    gpu: 1
    healthPath: /health
    runtimeClassName: nvidia
    modelCacheHostPath: /opt/hf-cache    # weights from local disk on reschedule
    extraArgs: ["--max-model-len", "4096", "--gpu-memory-utilization", "0.90"]
```

The fleet controller places child `VllmService`s across nodes using **warmth-first placement** (per-node `NodeState` resources report warmth, GPU utilization and spot status), reacts to preemption notices with the surge-first sequence measured above, caps concurrent reschedules (`hysteresis`) to prevent thundering herds, and falls back to `drain-and-hold` when no healthy target exists. Reconciliation reads its own child resources as the source of truth, which eliminates oscillation from stale external state.

## Architecture

```
src/lib.rs                 VllmService CRD, phase_for() lifecycle logic
src/main.rs                VllmService controller: reconcile, Deployment apply, status
src/fleet_types.rs         FleetService / NodeState CRDs, per-node phase machine
src/fleet_controller.rs    fleet reconcile: placement, preemption pass, surge/drain
src/fleet_placement.rs     warmth-first node selection (pure, unit-tested)
src/fleet_planning.rs      greedy initial placement planner (pure, unit-tested)
src/metrics.rs             Prometheus metrics
src/bin/crdgen.rs          emits CRD YAML
deploy/                    generated CRDs, examples
hack/rehearsal/            zero-cost kind harness: loadgen, analyze.py, run scripts
hack/gpu-session/          Lambda GPU session: bootstrap scripts, manifest, runs/
```

Built on kube-rs 2.x, k8s-openapi 0.26, Rust edition 2021, MSRV 1.85. The fleet controller `.owns()` its children and `.watches()` `NodeState` resources namespace-wide with a reactive mapper, so both spec changes and node condition changes retrigger reconciliation. Design decisions are documented as ADRs in the repo.

## Try it locally

Requires Docker, [kind](https://kind.sigs.k8s.io/), `kubectl`, and a Rust toolchain.

```bash
# 1. Create a local cluster
kind create cluster --name operator-dev

# 2. Install the CRDs
cargo run --bin crdgen > deploy/crd.yaml
kubectl apply -f deploy/crd.yaml

# 3. Run the operator (talks to the cluster via your kubeconfig)
cargo run --bin vllm-coldstart-operator

# 4. In another shell, create a VllmService and watch the lifecycle
kubectl apply -f deploy/examples/qwen-7b.yaml
kubectl get vllmservice qwen-7b -o jsonpath='{.status.phase}: {.status.message}'
# Pending -> ... -> Ready

# 5. Delete it and watch the Deployment garbage-collect
kubectl delete vllmservice qwen-7b
kubectl get deployment qwen-7b   # NotFound
```

The full preemption rehearsal (A/B/C topology, load, notice injection, analysis) runs on kind at zero cost: see [`hack/rehearsal/`](hack/rehearsal/).

## What is real and what is simulated

This section stays honest about boundaries, because the value is in what is actually exercised.

**Real and measured:** the control plane end-to-end (reconcile, server-side apply, GC, status machines), the fleet orchestration under preemption (measured on 3x A10 above), real vLLM serving with GPU scheduling (`runtimeClassName`, digest-pinned image, model cache on hostPath) — validated both on this fleet and on GKE with GPU node pools in a separate GitOps deployment. It also covers the per-node signal chain feeding `NodeState.status`: NVML energy and utilization plus vLLM prefix-cache scrape, joined into `tokensPerJoule` on the same reporting round, exercised in vivo on 3x A10 across 8 reps ([evidence](hack/gpu-session/adr0007-ea-experiment/runs/20260723T175747)).

**A measured bound, tested rather than published:** the cost campaign on this workload derived a packing bound — how many trajectories a replica could host if generating segments never contended. That is an upper bound under declared non-interference, so ADR-0010 tested it against a running engine instead of exporting it as a capacity figure, with the falsification criterion fixed nine days before the node was booted. Six arms on 1xA10: observed running count 1.2434 against a predicted 1.2639 at N=2, `BOUND SUPPORTED` unanimously ([evidence](hack/adr0010-evidence)). The raw series then narrowed the claim — the count is almost never 1, so the trajectories march in step and share their idle time rather than interleaving it. The prediction holds; the mechanism behind it is not the one the design assumed, and the interleaving case remains unmeasured.

**Covered in CI, not just in a session:** the `e2e` job on kind exercises both CRDs — `VllmService` end to end, and `FleetService` through placement, child ownership, the hostname pin the planner chose, the scale subresource ADR-0009 introduces, and garbage collection. It also restarts the operator over a live child and asserts the child survives with the same UID *and* the same metadata generation: recreation would cycle every replica on every upgrade, and a generation bump on an unchanged spec would mean each restart is a rollout. Nineteen steps, about ninety seconds, no GPU.

**Simulated:** the preemption *notice* (status patch, as disclosed above). It is an input boundary: everything downstream of it is real. Earlier runs also seeded `NodeState` warmth and utilization by hand; the level-3 session replaced that with the reporter DaemonSet reading real hardware, so the seeding survives only in the kind rehearsals, where it is deterministic by design.

## Testing & CI

- **Unit tests** (57: 44 lib, 8 reporter, 5 main, plus 3 property tests) on the pure decision logic: lifecycle derivation, warmth-first placement, planning, per-node phase machine, hysteresis behavior.
- **End-to-end CI** ([`.github/workflows/ci.yml`](.github/workflows/ci.yml)) on every push: `fmt --check` + `clippy -D warnings` + tests + release build, plus an ephemeral kind cluster that installs the CRDs, runs the operator, applies a `VllmService`, and asserts the full lifecycle with bounded polling (convergence, not timing luck).

## Roadmap

- Disaggregation-aware orchestration on the Gateway API Inference Extension contract: single InferencePool v1 + `llm-d.ai/role` labels, per-role warmth semantics (decode `CacheWarm` from `vllm:prefix_cache_*`), role-differentiated placement and recovery. Design accepted in [ADR-0006](docs/adr/0006-disaggregation-aware-orchestration.md); implementation scheduled next.
- Real preemption-notice detection (cloud metadata endpoint) behind the same `NodeState` interface.
- Efficiency-aware closed-loop placement: the per-node reporter publishes window-based tokens/joule and KV-cache hit-rate from inferscope into `NodeState.status`; placement ranks them as strict lexicographic tie-breakers inside warmth classes (`EfficiencyAware` strategy). Design accepted in [ADR-0007](docs/adr/0007-efficiency-aware-closed-loop-placement.md). Phase A done: `EfficiencyAware` comparator as a pure function with property-tested warmth dominance and fail-open absence semantics, status fields and CRDs in place. Phase B done: per-node reporter (same image, DaemonSet entrypoint, disjoint-ownership status merge-patches), strategy threaded through both natural decision points (initial planning and post-preemption replacement), opt-in Helm wiring with least-privilege RBAC. Falsification level 2 passed 5/5 on kind (`hack/rehearsal/adr0007-run.sh`): EA discriminates on kvCacheHitRate at equal warmth on both decision paths, and degrades to warmth-first when signals are absent. The first in-cluster run also surfaced (and fixed) a real schema/design mismatch — required status fields are incompatible with multi-writer merge-patch ownership. Phase B extended with the real signal source: `VllmScrape` (vLLM prefix-cache counters, ADR-011 schema) and an NVML sampler behind a build-feature plus runtime double gate, joined cross-source into `tokensPerJoule` on the same reporting round. Level-3 mechanism validated on 3x A10 (Lambda, k3s, one vLLM per node, [evidence](hack/gpu-session/adr0007-ea-experiment/runs/20260723T175747)): NVML and scrape signals populate `NodeState.status` in vivo on all three nodes, `EfficiencyAware` and `WarmthFirst` diverge deterministically at both decision points, 8/8 reps clean with the operator log free of anything but two named benign classes. The primary hypothesis — that EA yields higher fleet hit-rate and tokens/joule — remains open: the replay drives one fixed endpoint by design and nothing routes traffic to the service just placed, so the fleet aggregate cannot carry a strategy signal (measured deltas and the reasoning are recorded as a post-run amendment in the [experiment design](hack/gpu-session/adr0007-ea-experiment/DESIGN.md)). Testing it needs a placement-following routing layer — scoped as separate work, not a longer re-run.
- Scale-down orphan handling and Helm chart alignment (RBAC/CRDs) for the fleet path.
- Capstone write-up linking probe, operator, and the measured preemption data.

## License

Apache-2.0
