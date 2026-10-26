# ADR-0021: Burn-rate alerts on the time to first token

Status: Accepted, rules tested; imported into Grafana Cloud as Grafana-managed rules on 2026-10-26, no live series evaluated yet
Date: 2026-10-25
Extends: ADR-0013 (D2, where the indicators come from)

## Context

ADR-0013 checks its objectives at the end of each run, from the client.
It named what was missing: "the objectives over a rolling window, with
alerts on how fast its error budget burns; nothing here collects metrics
continuously or alerts." A platform in production has to say when its
budget is burning, not only after a run.

## Facts that constrain the design

1. **Only the time to first token is the same on both sides.** The client
   counts a request over 1 s; `vllm:time_to_first_token_seconds` has one
   sample per request, with a bucket edge at 1.0 in vLLM v0.11.0
   (`vllm/v1/metrics/loggers.py`) and in the simulator v0.8.2
   (`pkg/common/utils.go`), the same bucket lists in both.
2. **The time per output token is not.** ADR-0013 D1 takes the request's
   mean from first token to last; `vllm:inter_token_latency_seconds` has
   one sample per token. `vllm:request_time_per_output_token_seconds`
   is per request in vLLM v0.11.0 but absent from the simulator v0.8.2.
3. **Success is not.** `vllm:request_success_total` counts what the
   server finished; a request refused or lost before it is invisible there.
4. **Production writes to Grafana Cloud.** In gke, Alloy remote-writes to
   Grafana Cloud, which evaluates Prometheus-format rule groups; since
   gke 6ebb148 Alloy scrapes the vLLM pods with labels `namespace` and
   `service`.
5. **The simulator's Go client writes `le="1"`, vLLM's Python `le="1.0"`.**

## Decisions

**D1. Alerts on the time to first token only.** The share of first tokens
over 1 s is recorded on 5 m, 30 m, 1 h and 6 h, by `namespace` and
`service`, from the 1 s bucket matched as `le=~"1|1\.0"` (Fact 5). The time
per output token and success stay client-side (Facts 2, 3).

**D2. Multiwindow, multi-burn-rate alerts on the 5% budget** (Google SRE
Workbook): page when the 1 h and 5 m ratios exceed 14.4 x 5%; ticket when
the 6 h and 30 m ratios exceed 6 x 5%. A target-down alert for a replica
whose `/metrics` stops answering, since its requests then stop counting.

**D3. One file, three readers.** `deploy/monitoring/slo-rules.yaml` is what
promtool tests, what the CI Prometheus loads and what Grafana Cloud
imports; the CI Prometheus labels `service` from the pod label
`app`, as Alloy does.

**D4. Two proofs.** promtool tests the logic on synthetic series (burnt
budget, within budget, only the short window high, the simulator's `le`,
a replica down) and must fail on a rule with the wrong bucket edge. The
scale-out job loads the rules and requires a ratio in [0, 1] on the
simulator's series after its load.

## Consequences

- `docs/runbooks/ttft-burn-rate.md` is the link of every alert.
- A request queueing behind a cold start counts against the budget, as the
  client counts it.

## Loading into Grafana Cloud

The stack's Prometheus data source is provisioned with ruler management
off, and the stack has no Mimir Alertmanager data source, so rules in the
Mimir ruler would notify nobody. The groups are imported instead as
Grafana-managed rules through Grafana's conversion API, with mimirtool
3.2.3 pinned by digest: Grafana evaluates them against the Prometheus data
source and routes them through its built-in Alertmanager, whose default
contact point sends email. The file stays the only source; mimirtool takes
the namespace from a `namespace:` key that promtool rejects, so what is
synced is this file with that one line prepended, checked byte for byte:

```
I=grafana/mimirtool@sha256:469de1cf58fdaf8e81ddafe1395c17da3c733e8a4e79ac544d4fccf11eb4f3b8
D=$(mktemp -d) && chmod 755 "$D"
{ echo 'namespace: vllm-coldstart-operator'; cat deploy/monitoring/slo-rules.yaml; } > "$D/rules.yaml"
chmod 644 "$D/rules.yaml"
tail -n +2 "$D/rules.yaml" | cmp - deploy/monitoring/slo-rules.yaml
# MIMIR_ADDRESS=https://<stack>.grafana.net/api/convert/ and MIMIR_TENANT_ID=1;
# MIMIR_AUTH_TOKEN is a service account token with no basic role and the
# fixed roles Alerting Rules Reader, Rules Writer and Set provisioning
# status, Data sources Reader, Folders Creator, Reader and Writer
docker run --rm -e MIMIR_ADDRESS -e MIMIR_TENANT_ID -e MIMIR_AUTH_TOKEN \
  -v "$D:/r:ro" "$I" rules sync /r/rules.yaml --concurrency 1 \
  --extra-headers X-Grafana-Alerting-Datasource-UID=<prometheus data source uid> \
  --extra-headers X-Grafana-Alerting-Folder-UID=vllm-coldstart-operator
```

On 2026-10-26 the sync created both groups in the folder
`vllm-coldstart-operator`: the alerting UI lists 7 rules (4 records, 3
alerts). `rules list` on the same API returned no groups, which is not
explained. The rules were removed from the Mimir ruler after the import, so
the records are evaluated once. Rollback: delete the rules of the folder.

## What this ADR does not prove

- That the alerts fire in production: the rules are in Grafana, but no gke
  session has run since, so they have evaluated no vLLM series yet.
- That a firing alert reaches someone in time: only a test email was sent,
  and it landed in a junk folder.
- Alerts on the time per output token and on success: they need a
  per-request histogram in the simulator and a component that sees every
  request (a gateway), each work of its own.
- The CI run evaluates over minutes, not the hours of the long windows;
  those are covered by promtool only.
