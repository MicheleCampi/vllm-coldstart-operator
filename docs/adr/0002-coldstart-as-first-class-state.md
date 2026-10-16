# ADR-0002: Model cold start as a first-class lifecycle state

- Status: Accepted; rationale corrected 2026-10-16 (see postscript)
- Date: 2026-05-31

## Context

Kubernetes already has a readiness model: a pod becomes Ready when its readiness probe passes. For most workloads that is enough. For an LLM inference server it is not, because there is a gap between two events that Kubernetes collapses into one:

- the process is up and accepting connections, and
- the model is loaded, the GPU is warm, and the server can actually serve a token within normal latency.

The vllm-coldstart-probe study timed the engine construction inside this gap: for a 7B model on an A10 with a cold page cache it took 11.7 to 19.0 s in eager mode, and 47.7 s with CUDA graphs in the one capture of that configuration, about three times the same model's 15.1 s in eager mode (analysis/CAPTURES.md). The probe does not show how that time divides between reading weights and GPU work. A generic operator that wraps a Deployment inherits Kubernetes' single notion of readiness and therefore cannot distinguish "running" from "warm". An operator born from the profiling work can.

## Decision

Make cold start an explicit, observable lifecycle state on the custom resource, rather than leaving it implicit in pod readiness.

The VllmService status exposes a phase derived from the owned Deployment's ready replicas:

- **Pending** — Deployment created, no replica ready.
- **Warming** — at least one replica exists but not all desired replicas are ready: the process is up, the server is warming.
- **Ready** — all desired replicas are ready: warm and able to serve, not merely running.

The warmupStrategy field (Eager / Graph) is part of the same decision: it lets the operator configure the cold-start/throughput trade-off the profiling work quantified, instead of treating it as an opaque deployment detail.

## Rationale

This is the decision that makes the operator more than a Deployment wrapper, and it is not clonable from a generic template: it exists because the cold-start cost was measured, and the measurement is what justifies treating warmth as a state worth modeling. The phase is derived from real Deployment status, not a timer, so it reflects the cluster's actual condition.

## Consequences

- The status is honest about the difference between "running" and "able to serve", which is the difference that matters for scale-to-zero and autoscaling decisions on inference workloads.
- Future work can attach the measured cost to the state — for example, surfacing the expected cold-start cost per warmupStrategy in the status, so an operator can choose a strategy on the basis of the profiling data rather than a guess.

## Note on the placeholder data plane

On a CPU-only kind cluster the managed pod is a documented placeholder image, and warmupStrategy is recorded as an environment variable rather than driving a real vLLM flag. This is a deliberate scope boundary: the control plane (the lifecycle, ownership, and status modeling described here) is real and tested; the data plane is stubbed because a GPU and a real vLLM image are out of scope for a reproducible local/CI cluster. Validating that the Warming to Ready transition tracks the measured cold start on real vLLM is the next step, recorded in the README roadmap.

## Postscript, 2026-10-16 — the throughput side was not quantified here

The Decision said the warmupStrategy field configures "the cold-start/throughput trade-off the profiling work quantified". The profiling work (vllm-coldstart-probe) quantified only the cold-start side: CUDA graphs made the cold load about 3x slower (47.72 s vs 15.05 s). The other side was measured later, in cuda-graphs-experiment on an H100 with vLLM 0.23: decode was faster in every configuration, but end-to-end throughput ranged from +8.5% on a 7B model to −5.2% on a 32B model under sustained overload. The field's documentation now says decode speed instead of throughput; see [BENCHMARKS](../BENCHMARKS.md).
