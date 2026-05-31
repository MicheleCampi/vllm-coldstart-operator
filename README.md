# vllm-coldstart-operator

A Kubernetes operator, written in Rust with [kube-rs](https://kube.rs/), that manages vLLM inference replicas and treats **cold start as a first-class signal** rather than an invisible side effect of pod startup.

Kubernetes decides a pod is ready when its process is up. For an LLM inference server that is the wrong moment: the process is alive, but it still has to load weights and warm up before it can serve a token. This operator models that gap explicitly — a `VllmService` is `Ready` only when it is *warm and able to serve*, not merely running.

The operator is the operational half of a pair of profiling tools:

- [vllm-coldstart-probe](https://github.com/MicheleCampi/vllm-coldstart-probe) — an eBPF profiler that **measures** where vLLM cold start goes (syscalls, libcuda, GPU warmup).
- **vllm-coldstart-operator** (this repo) — **acts** on that knowledge inside a cluster.

## What it does

The operator introduces a `VllmService` custom resource:

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

For each `VllmService`, the controller:

- **Reconciles an owned Deployment** via idempotent server-side apply. The Deployment carries an owner reference, so deleting the `VllmService` garbage-collects it automatically — no manual cleanup code.
- **Maps `warmupStrategy` to pod configuration.** `Eager` requests `enforce_eager` (faster cold start, slower steady state — better for scale-to-zero); `Graph` leaves CUDA graphs on (slower cold start, faster steady state). This is the operational lever behind the probe's finding that CUDA graphs make cold start ~3x slower.
- **Derives a cold-start-aware lifecycle phase** from the Deployment's ready replicas and writes it to the `VllmService` `/status` subresource:
  - `Pending` — Deployment created, no replica ready yet
  - `Warming` — some replicas ready, not all (process up, warming up)
  - `Ready` — all desired replicas ready (warm and able to serve)

Writing on the status subresource does not retrigger the spec watcher, so there is no reconcile loop.

## Architecture

```
src/lib.rs          VllmService CRD (spec + status), phase_for() lifecycle logic, unit tests
src/main.rs         controller: reconcile loop, Deployment apply, status patch
src/bin/crdgen.rs   emits the CRD YAML (cargo run --bin crdgen)
deploy/crd.yaml     generated CRD
deploy/examples/    sample VllmService
```

The controller watches `VllmService` resources and `.owns()` the Deployments it creates, so changes to either retrigger reconciliation. Built on kube-rs 2.x, k8s-openapi 0.26 (Kubernetes 1.34 API), Rust edition 2021, MSRV 1.83.

## Try it locally

Requires Docker, [kind](https://kind.sigs.k8s.io/), `kubectl`, and a Rust toolchain.

```bash
# 1. Create a local cluster
kind create cluster --name operator-dev

# 2. Install the CRD
cargo run --bin crdgen > deploy/crd.yaml
kubectl apply -f deploy/crd.yaml

# 3. Run the operator (talks to the cluster via your kubeconfig)
cargo run --bin vllm-coldstart-operator

# 4. In another shell, create a VllmService and watch the lifecycle
kubectl apply -f deploy/examples/qwen-7b.yaml
kubectl get vllmservice qwen-7b -o jsonpath='{.status.phase}: {.status.message}'
# Pending -> ... -> Ready: 1/1 replicas ready and warm

# 5. Delete it and watch the Deployment garbage-collect
kubectl delete vllmservice qwen-7b
kubectl get deployment qwen-7b   # NotFound
```

## What is real and what is a placeholder

This is honest about its boundaries, because the value is in the control plane, not a staged demo.

**Real and exercised:** the control plane. The reconcile loop, owned-Deployment management, server-side apply, owner-reference garbage collection, and the cold-start-aware status machine are real and verified end-to-end against a live cluster (see CI below).

**Placeholder:** the data plane. On a CPU-only kind cluster you cannot run real vLLM, so the managed pod uses `registry.k8s.io/pause:3.10` as a documented stand-in, and `warmupStrategy` is recorded as an environment variable rather than driving a real vLLM flag. Turning this into a real deployment is a matter of swapping the container image for a vLLM serving image and adding a GPU resource request — the lifecycle logic does not change. That substitution, and validating the `Warming -> Ready` transition against real measured cold start, is the next step on the roadmap.

## Testing & CI

- **Unit tests** on `phase_for()`, the lifecycle-derivation logic (the core cold-start decision), covering each phase transition and edge cases.
- **End-to-end CI** ([`.github/workflows/ci.yml`](.github/workflows/ci.yml)) runs on every push: one job for `fmt --check` + `clippy` + `test` + release build (with `-D warnings`), and a second job that spins up an ephemeral kind cluster, installs the CRD, runs the operator, applies a `VllmService`, and asserts the full lifecycle — Deployment created, status reaches `Ready`, owner reference set, Deployment garbage-collected on delete. Waits use bounded polling, not fixed sleeps, so the test asserts convergence without being timing-flaky.

## Roadmap

- Make the profiling-informed decision explicit in status — e.g. surface the expected cold-start cost per `warmupStrategy`, derived from the probe's measurements.
- Run against real vLLM on a GPU cluster and validate that `Warming -> Ready` tracks the measured cold start.
- Capstone write-up linking the probe and the operator: measuring cold start, then acting on it.

## License

Apache-2.0
