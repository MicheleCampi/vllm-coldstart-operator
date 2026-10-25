# GPU calibration on one L4 on GKE: plan, written before the run

Status: plan. Budget: BUDGET B-001 in the private notes (one L4 on demand,
cap 5 EUR, destroy within 4 h of apply); decision D-0041.

## Why

The simulator in `hack/rehearsal/Dockerfile.sim` stands in for vLLM in
the CI jobs sim-load, faults-under-load, scale-out and scale-from-zero,
with three hand-chosen numbers:
`--time-to-first-token 250ms` (constant, whatever the prompt length),
`--inter-token-latency 20ms` and `--time-factor-under-load 2.0`.
ADR-0013 D3 measured one replica's capacity on that simulator: every
objective of `hack/loadgen/slo.json` held up to 0.408 req/s (keep 0.05)
and TTFT was missed at 0.538 req/s (keep 0.075). This run measures the
same quantities on the stack the platform declares, so the simulator's
numbers and D3 can be replaced or bounded by measured ones.

## The system measured

The deployment of gke-llm-inference-platform at main bc368ad: operator
v0.6.0 through ArgoCD, one VllmService `qwen-7b`, Qwen/Qwen2.5-7B-Instruct
on vllm/vllm-openai:v0.11.0 with `--max-model-len 8192` and
`--gpu-memory-utilization 0.90`, one g2-standard-4 node with one L4 in
europe-west4. Nothing in that deployment is changed for the run; the load
client Job is measurement infrastructure, labelled as such.

## Where the load comes from

From inside the cluster: `hack/loadgen/loadgen.py` runs as a Job on the
system node and targets `qwen-7b:8000`, the Service the operator
applies for the VllmService (`build_service` in src/main.rs). In ADR-0013 the
client and the simulator shared one machine; a client on optim-dev
would add the internet and the API-server tunnel to every TTFT and make
the comparison with D3 meaningless.

## Measurements, in order

1. Cold start: from `kubectl apply` of the root app to the VllmService
   Ready, split from Kubernetes events and pod conditions into node
   ready, image pulled, container started, and model serving (first
   200 on /health). Event timestamps have a 1 s resolution, which bounds
   every interval. Repeated once by deleting the serving pod, with the
   image already on the node.
2. The fixed replay, three times: keep 0.025, seed 42, 68 requests,
   sequence 6c10349917a6. Checked with
   `slo_check.py --client-only`: its client-minus-simulator check
   compares against times the simulator computes (slo_check.py:10-13).
3. The D3 ladder, one run per row of ADR-0013's table: keep 0.05 at
   seeds 42, 1 and 2, then 0.075 and 0.1 at seed 42.

From the per-request records (`ttft_ms`, `last_token_ms`, `latency_ms`):
TTFT against prompt tokens, time per output token, and the highest load
meeting every objective on every seed tried, the rule of D3.

## What is concluded, and what is not

- The simulator's three parameters are replaced or bounded in an ADR,
  with this run's envelope: one L4, this vLLM version, this model, this
  engine configuration, this profile and these seeds.
- Nothing is concluded about other GPUs, other models, other engine
  arguments, or more than one replica.

## Stop rules

- Destroy within 4 h of apply. At B-001's estimate, about 0.85 EUR/h,
  4 h is 3.41 EUR, under the 5 EUR cap. The limit is time, set from
  that estimate.
- Before apply, check that the cluster version is not in extended
  support, which B-001 prices at 0.44 EUR/h more.
- A step that fails is not debugged on the paid cluster: evidence is
  copied off, the cluster is destroyed, the failure is analysed on
  optim-dev.
- Evidence (load records, events, operator log, nvidia-smi, versions)
  is copied off before `terraform destroy`, and the destroy is verified
  with gcloud.
