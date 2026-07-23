# ADR-0007 level-3 GPU session — go/no-go checklist

Budget: <$12 hard cap (DESIGN.md). Topology: 3x A10 Lambda, k3s,
reuse item-4 phases 0-2 (00-env / 01-server / 02-agent / 03-prepull)
unchanged. This file covers the level-3 deltas only.

Roles, to avoid confusion in the writeup: the reporter's NVML sampler
feeds NodeState status (the EA comparator's in-vivo signal); inferscope
(ADR-010/011) is the measurement instrument for the experiment verdict.
Same counters, different consumers — never mix the two in claims.

## Phase 0b — before spending (optim-dev, zero cost)
- [ ] GPU image built and rehearsed: `GPU_VARIANT=1
      bash hack/rehearsal/adr0007-real-source.sh` -> 7/7
      (proves: gnu target + gpu-nvidia feature + distroless/cc,
      REPORTER_GPU env reaches the binary, NVML fails open w/o GPU)
- [ ] baseline rehearsal still 6/6 (same script, no GPU_VARIANT)
- [ ] image saved for transfer: `docker save
      vllm-coldstart-operator:adr0007rs-gpu | gzip >
      /tmp/vcso-gpu.tar.gz` (rsync to nodes per repo-transfer rule)

## Phase 3b — deploy reporter DaemonSet (after item-4 phase 3 steps)
- [ ] import image into k3s containerd on EVERY node:
      `sudo k3s ctr images import /tmp/vcso-gpu.tar.gz` (image is
      local-only; imagePullPolicy Never in the chart values)
- [ ] helm install with reporter.enabled=true and per-node
      REPORTER_SCRAPE_TARGETS_NODE_<n> at node-local vLLM /metrics
      (DESIGN.md line "Reporter DaemonSet in Real mode")
- [ ] reporter env MUST include, in addition to REPORTER_GPU=nvidia:
      NVIDIA_VISIBLE_DEVICES=all, NVIDIA_DRIVER_CAPABILITIES=utility
      — toolkit injects libnvidia-ml.so only on these envs; distroless/cc
      does not carry them the way CUDA images do. Do NOT request
      nvidia.com/gpu resources on the reporter (vLLM owns the GPU).

## Smoke gate (before ANY measured run — abort criteria)
- [ ] reporter logs: NO "NVML init failed" warning on any node
- [ ] NodeState status shows gpuUtilization + gpuMemoryUsedBytes
      present on all 3 nodes within 2 report rounds
- [ ] tokensPerJoule appears once vLLM serves traffic (needs both
      scrape token delta and NVML energy delta on the same round)
- ABORT if NVML init still fails after ONE targeted fix attempt
      (check toolkit envs on the pod spec first, then runtimeClass
      binding). Fallback documented, not improvised: run reporter as
      systemd unit on-host (gnu binary has direct driver access) with
      NODE_NAME=<k8s node name> and POD_NAMESPACE=default set
      explicitly — the DaemonSet gets them from the downward API,
      systemd does not. 15 min cap on the whole gate.

## Phase 4b — RPS verify + EA experiment
- [ ] RPS check per DESIGN.md before the 8 reps
- [ ] 8 reps ABBA+BAAB via run_experiment.sh (frozen protocol)

## Phase 5 — evidence off-box BEFORE teardown (item-4 rules apply)
- [ ] plus: one NodeState -o yaml snapshot per rep window showing the
      GPU signals populated (the level-3 claim needs reporter-fed
      status, not only inferscope evidence)

## Session 2026-07-22 findings — MANDATORY for the next run
The 22 Jul session aborted mid-rep-3 (SIGHUP: run was foreground in the
SSH session). rep-1-EA and rep-2-WF complete, evidence in
runs/20260722T200110/. All fixes below are already in the repo.
- vLLM topology: apply `vllm-per-node.yaml` (this dir) — 3 standalone
  Deployments pinned per node (DESIGN "one vLLM per node"), NOT
  fleet-gpu.yaml (item-4 topology, conflicts with the experiment).
  NodeSelectors carry the session node names: UPDATE THEM at launch.
  `strategy: Recreate` is required (RollingUpdate deadlocks on 1-GPU
  nodes). NodePorts 30800/30801/30802; Lambda firewall must allow all
  three inbound (30802 was missing on 22 Jul: symptom = curl empty from
  outside, fine from any node).
- Reporter: helm install with `--set reporter.runtimeClassName=nvidia`
  (now in the chart; no manual patch). Keep the three NVML envs.
- RPS: 2 (DESIGN.md amendment 2026-07-22, measured on A10). gen_workload
  default already 2; EXP_RPS env overrides.
- run_experiment.sh GPU mode env contract: KUBE_CONTEXT, EXP_NODES
  (real NodeState names, space-separated), VLLM_URL (full /v1/completions
  path of the replay target), VLLM_MODEL, EXP_BETWEEN_REPS_CMD (vLLM
  rollout-restart + status + sleep 30; kubectl reads exported KUBECONFIG).
- RUN INSIDE TMUX on optim-dev. The 22 Jul abort was a plain SIGHUP.
- Budget note: full 8-rep run from node launch ≈ 40 min setup + ~80 min
  reps + evidence/teardown; ~2h45 total, ~$6.5 at A10 rates.
