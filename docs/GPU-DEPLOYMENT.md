# GPU deployment status

This operator's control plane is validated; running it against a real GPU
cluster is documented here honestly, including what works and the open issue.

## What is validated

The control plane is fully exercised end-to-end on a `kind` cluster in CI:
reconcile loop, the Pending -> Warming -> Ready state machine, owned-resource
creation, owner references, and garbage collection. The GPU-specific code
paths (image, `nvidia.com/gpu` limit, `runtimeClassName: nvidia`, and the
readiness probe gating) are unit-tested and confirmed to render correctly,
with `gpu=0` leaving them absent so CI stays GPU-free.

The cold-start cost the Warming->Ready transition observes is measured
separately on real A10 hardware; see BENCHMARKS.md.

## Open issue: GPU scheduling on single-node K3s

Bringing up GPU scheduling on a single-node K3s cluster (Lambda Cloud A10)
hit a reproducible problem at the cluster layer, not in the operator:

- K3s 1.35 does not auto-detect the NVIDIA container runtime even with the
  toolkit present; a hand-written `config.toml.tmpl` either failed to
  register the runtime or broke K3s startup (version-sensitive template).
- The NVIDIA GPU Operator (Helm) installs cleanly and the CUDA validator
  confirms the GPU is usable, but with `CONTAINERD_SET_AS_DEFAULT=true` the
  containerd restart leaves K3s's flannel CNI uninitialized: the node goes
  `NotReady` and pods (including the operator's own validator) cannot schedule.

The GPU itself works; the failure is the interaction between the GPU
Operator's containerd reconfiguration and K3s's embedded CNI on a single node.
This is a known sharp edge of K3s + GPU, not a defect in this operator.

## Next step

The robust path is a managed or kubeadm cluster, where the GPU Operator is
heavily tested and does not contend with K3s's embedded CNI. The operator
code is ready for it: it already emits `runtimeClassName: nvidia` and the GPU
resource limit. Validating the full Warming->Ready transition against vLLM on
such a cluster is the remaining work.
