# Observability

In-cluster metrics pipeline for the operator: Grafana Alloy scrapes the
operator's `/metrics` endpoint and `remote_write`s to Grafana Cloud (Mimir).

```
operator :8080/metrics  ->  Alloy (scrape)  ->  remote_write  ->  Mimir (Grafana Cloud)
```

## Files

- `alloy-values.yaml` — Helm values for the `grafana/alloy` chart (tested with
  1.9.0). Defines the scrape config (alloy-self + operator) and the
  `remote_write` target.

## Prerequisites

### 1. Remote-write secret

Alloy reads Grafana Cloud credentials from a Kubernetes secret named
`gc-remote-write` in the install namespace. The values file references it by
key (`username`, `password`) — credentials are never inlined.

Create it (password entered interactively, never echoed to the shell):

```bash
read -rs GC_PROM_PASS && \
  kubectl create secret generic gc-remote-write \
    -n monitoring \
    --from-literal=username='<GC_PROM_USERNAME>' \
    --from-literal=password="$GC_PROM_PASS" && \
  unset GC_PROM_PASS
```

The token must have **metrics push** (write) scope. A write-only token cannot
be used to query the dashboard — use the Grafana Cloud UI for reads.

## Install

```bash
helm repo add grafana https://grafana.github.io/helm-charts
helm repo update
helm install alloy grafana/alloy \
  -n monitoring --create-namespace \
  -f alloy-values.yaml
```

## Cluster-specific assumptions

These are hardcoded in `alloy-values.yaml` and must be edited for reuse
elsewhere:

- **Scrape namespace** — `discovery.kubernetes "operator"` is scoped to
  `vllm-operator-system`. Change `namespaces.names` if the operator runs
  elsewhere.
- **External label** — `cluster = "kind-operator-test"`. Set this per cluster;
  dashboard queries filter on it (`{cluster="..."}`).
- **Remote-write endpoint** — the Mimir URL is region-specific
  (`prod-eu-west-2`). Match it to your Grafana Cloud stack.
