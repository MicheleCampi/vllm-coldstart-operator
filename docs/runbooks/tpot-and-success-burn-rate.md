# Runbook: time per output token and request success burn their budgets

Alerts from `deploy/monitoring/slo-rules.yaml` (ADR-0026). Objectives:
95% of requests with a mean time per output token within 100 ms (5%
budget), and 99.5% of requests not aborted (0.5% budget). Labels
`namespace` and `service` name the VllmService. Budget policy:
`docs/slo-policy.md`.

## VllmTpotBudgetBurnFast (severity page)

More than 72% of requests with a mean TPOT over 100 ms in the last hour
and the last 5 minutes.

1. Is the batch too large? `vllm:num_requests_running{service="..."}`
   near the replica's max-num-seqs means decoding is shared by many
   streams.
2. Is the KV cache full? `vllm:gpu_cache_usage_perc` near 1 and
   `vllm:num_preemptions_total` growing mean requests are preempted and
   recomputed.
3. Did the model, image or GPU change? Compare the VllmService spec and
   the node type with the last good run.

Act: lower max-num-seqs or add replicas (cost estimate first); roll back
a change of model or image with the VllmService spec.

## VllmTpotBudgetBurnSlow (severity ticket)

More than 30% over 100 ms in the last 6 hours and the last 30 minutes.
Same checks; usually a sustained load or long prompts.

## VllmSuccessBudgetBurnFast (severity page)

More than 7.2% of finished requests aborted in the last hour and the
last 5 minutes.

1. Client or server? Abort also counts client disconnects: check the
   client's timeouts and the gateway's log for cancelled requests.
2. Is the replica restarting? `kubectl get pods -n vllm-system` for
   restarts and OOMKilled; the operator's log.
3. Was a node preempted or drained? `kubectl get nodestate -A`
   (ADR-0005, ADR-0017).

Act: if clients time out on slow requests, the TPOT or TTFT runbook is
the cause; if the replica restarts, fix memory or roll back.

## VllmSuccessBudgetBurnSlow (severity ticket)

More than 3% aborted in the last 6 hours and the last 30 minutes. Same
checks.

## What these alerts do not see

Requests lost before the replica, and a replica whose `/metrics` is
down (VllmTargetDown). The TPOT metric is absent from the simulator
v0.8.2: on a simulator these TPOT alerts never fire.
