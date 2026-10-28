# Runbook: time to first token burns its error budget

Alerts from `deploy/monitoring/slo-rules.yaml` (ADR-0021). The objective
is ADR-0013 D1: 95% of requests with a first token within 1 s, a 5% error
budget. Labels `namespace` and `service` name the VllmService (for a fleet,
one child per replica).

## VllmTtftBudgetBurnFast (severity page)

More than 72% of first tokens over 1 s in the last hour and in the last
5 minutes: at this rate a 30-day budget is gone in about two days.

1. Is the replica queueing? `vllm:num_requests_waiting{service="..."}` above
   zero for minutes means more demand than the replica's slots (ADR-0013 D3:
   one simulator replica carries about 0.4 requests per second).
2. Is the fleet at its cap? `kubectl get fleetservice -A`: ready replicas
   against `spec.maxReplicas`; KEDA's HPA, `kubectl get hpa -A`.
3. Is a replica starting? A cold start holds requests for minutes on a GPU
   (about 16 minutes with the image pull, 5 min 40 s without, D-0049):
   `kubectl get pods -n vllm-system -o wide` and the VllmService phase.
4. Was a node preempted or drained? `kubectl get nodestate -A` and the
   operator's log (ADR-0005, ADR-0017).

Act: raise the fleet's cap if the cost allows it; otherwise accept the burn
and say so. Rollback a change of model or image with the VllmService spec.

## VllmTtftBudgetBurnSlow (severity ticket)

More than 30% over 1 s in the last 6 hours and in the last 30 minutes. Same
checks; usually a sustained load above capacity rather than an incident.

## VllmTargetDown (severity ticket)

`up == 0` for 5 minutes: the replica's `/metrics` does not answer, so its
requests also stop counting against the budget. Check the pod and its node
first; a missing series is not a met objective.

## VllmServiceReplicasMissing (severity ticket)

The operator asks for replicas of a VllmService
(`vcso_vllmservice_desired_replicas` above 0) and no `/metrics` target
exists for it for 30 minutes (ADR-0023). Labels `exported_namespace` and
`name` name the VllmService. Check whether its pod exists
(`kubectl get pods -n <namespace> -l app=<name>`) and whether it waits for
a node: a cold start longer than 30 minutes is itself the fault.

## What the alerts do not see

Requests lost before the replica (connection refused, no endpoint) and the
time per output token: both stay client-side checks (ADR-0013 D2, ADR-0021).
