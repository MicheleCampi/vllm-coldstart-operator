# ADR-0023: A replica the operator asks for and no target answers for

Status: Accepted, rules and metrics tested; synced to Grafana Cloud on 2026-10-28, no live series evaluated yet
Date: 2026-10-28
Extends: ADR-0021 (VllmTargetDown)

## Context

VllmTargetDown (ADR-0021) fires on `up == 0`: a target that exists and
does not answer. When a vLLM pod disappears, the discovery drops its
target and its `up` series stops, so nothing fires; the burn-rate alerts
lose their input too, and Grafana resolves them as NoData. The runbook
already said that a missing series is not a met objective, and no rule
enforced it. On 2026-10-27 (GPU session B-005) the fast burn alert
resolved for NoData when the destroy removed the pod.

## Facts that constrain the design

1. **Absence alone is not a fault.** The platform scales to zero on
   purpose (ADR-0014), so `absent(up)` would fire on every scale to zero.
2. **The phase cannot tell the two cases apart.** `phase_for(0, 0)`
   returns Pending (src/lib.rs), the phase of a replica that is wanted and
   not ready.
3. **Per-object gauges outlived their object.** `vcso_vllmservice_phase`
   was a gauge set in the reconcile, with no finalizer and no cleanup: a
   deleted VllmService kept its series until the operator restarted.
4. **The operator's series carry the object's namespace as
   exported_namespace.** In the Grafana Cloud stack on 2026-10-28,
   `vcso_vllmservice_phase` had `exported_namespace` (the VllmService) and
   `namespace` (the operator pod): the scrape writes its own namespace
   label.
5. **A cold start has no target for minutes.** A pod waiting for its GPU
   node has no IP, so no target and no `up`. On 2026-10-27 the phase stayed
   Pending for about 14 min and the cold start took 20 min 40 s (B-005).
6. **The CI Prometheus does not scrape the operator:**
   hack/scale-out/prometheus.yaml has the job fleet-pods only.

## Decisions

D1. **The per-VllmService series are read from the controller's cache on
each scrape.** A collector walks `Controller::store()` and writes
`vcso_vllmservice_phase` and the new
`vcso_vllmservice_desired_replicas{name, namespace}` (`spec.replicas`). A
deleted object leaves the cache and its series leave the scrape. No
finalizer: it would write to every object and block deletion while the
operator is down. The counters `reconcile_failures` by instance stay as
they are: counters, read by no alert.

D2. **VllmServiceReplicasMissing, severity ticket:** desired replicas above
0 and no `up` series for that service, for 30 min. The join renames
`namespace` and `service` of `up` to `exported_namespace` and `name`. 30
min is longer than the cold start measured on one L4, and is revised if
that changes.

D3. **kube-state-metrics was not chosen:** it adds a component and series
on the free plan, and measures the Deployment, not what the operator asks
for.

## Consequences

- A pod that vanishes while its VllmService asks for it opens a ticket
  after 30 min; VllmTargetDown keeps the target that does not answer.
- A service scaled to zero asks for 0 and stays silent.
- The rule depends on how the scrape names the namespace label. Alloy in
  gke writes `namespace`, so the object's lands in `exported_namespace`; a
  scrape with honor_labels, or without its own namespace label, needs the
  join changed.
- The burn-rate alerts still resolve as NoData when their input stops; this
  alert is the one that says the input stopped.

## Validity envelope

- promtool, on the labels read in the stack: a vanished pod fires at 40 min
  and not at 25 min; a service scaled to zero and a live pod do not fire;
  the tests fail without the rule. Rust: a deleted VllmService leaves no
  series.
- Not evaluated on live series, since the CI Prometheus does not scrape
  the operator: the next GPU session checks it
  (hack/gpu-session/gke-l4/CHECKLIST.md).
- Synced to Grafana Cloud on 2026-10-28 at 20:50Z with the command of
  ADR-0021 (0 groups created, 2 updated): the folder lists 8 rules, the
  four alerts Normal with no cluster running.

## Rollback

Revert the commit and run the sync of ADR-0021 again, or delete the rule
from the folder vllm-coldstart-operator.
