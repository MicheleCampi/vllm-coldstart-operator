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

**Real and measured:** the control plane end-to-end (reconcile, server-side apply, GC, status machines), the fleet orchestration under preemption (measured on 3x A10 above), real vLLM serving with GPU scheduling (`runtimeClassName`, digest-pinned image, model cache on hostPath) — validated both on this fleet and on GKE with GPU node pools in a separate GitOps deployment.

**Simulated:** the preemption *notice* (status patch, as disclosed above) and the `NodeState` warmth/utilization reporting, which in these runs was seeded rather than fed by a node agent. Both are input boundaries: everything downstream of them is real.

## Testing & CI

- **Unit tests** (~33) on the pure decision logic: lifecycle derivation, warmth-first placement, planning, per-node phase machine, hysteresis behavior.
- **End-to-end CI** ([`.github/workflows/ci.yml`](.github/workflows/ci.yml)) on every push: `fmt --check` + `clippy -D warnings` + tests + release build, plus an ephemeral kind cluster that installs the CRDs, runs the operator, applies a `VllmService`, and asserts the full lifecycle with bounded polling (convergence, not timing luck).

## Roadmap

- Disaggregation-aware orchestration on the Gateway API Inference Extension contract: single InferencePool v1 + `llm-d.ai/role` labels, per-role warmth semantics (decode `CacheWarm` from `vllm:prefix_cache_*`), role-differentiated placement and recovery. Design accepted in [ADR-0006](docs/adr/0006-disaggregation-aware-orchestration.md); implementation scheduled next.
- Real preemption-notice detection (cloud metadata endpoint) behind the same `NodeState` interface.
- Efficiency-aware closed-loop placement: the per-node reporter publishes window-based tokens/joule and KV-cache hit-rate from inferscope into `NodeState.status`; placement ranks them as strict lexicographic tie-breakers inside warmth classes (`EfficiencyAware` strategy). Design accepted in [ADR-0007](docs/adr/0007-efficiency-aware-closed-loop-placement.md). Phase A done: `EfficiencyAware` comparator as a pure function with property-tested warmth dominance and fail-open absence semantics, status fields and CRDs in place. Phase B done: per-node reporter (same image, DaemonSet entrypoint, disjoint-ownership status merge-patches), strategy threaded through both natural decision points (initial planning and post-preemption replacement), opt-in Helm wiring with least-privilege RBAC. Falsification level 2 passed 5/5 on kind (`hack/rehearsal/adr0007-run.sh`): EA discriminates on kvCacheHitRate at equal warmth on both decision paths, and degrades to warmth-first when signals are absent. The first in-cluster run also surfaced (and fixed) a real schema/design mismatch — required status fields are incompatible with multi-writer merge-patch ownership. Next: real signal source (NVML via inferscope + vLLM prefix-cache scrape) and GPU validation (level 3), post-2026-08-11.
- Scale-down orphan handling and Helm chart alignment (RBAC/CRDs) for the fleet path.
- Capstone write-up linking probe, operator, and the measured preemption data.

## License

Apache-2.0
