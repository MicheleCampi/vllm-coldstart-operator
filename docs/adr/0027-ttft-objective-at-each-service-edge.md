# ADR-0027: Each service is judged at its own time-to-first-token edge

Status: Accepted, rules tested with promtool and two negative cases; no live series evaluated yet
Date: 2026-11-13
Follows: ADR-0021 (TTFT burn-rate alerts), ADR-0013 (TTFT objective)

## Context

ADR-0021 measures every service at the 1 s bucket of
`vllm:time_to_first_token_seconds`. A team may declare
`spec.objective.ttftSeconds`, but any value other than 1 only raised
`VllmTtftObjectiveUnsupported`: the error budget was per service, the
threshold was not (break 2 of the golden path).

## Facts that constrain the design

1. The histogram has 22 bucket edges in vLLM v0.11.0
   (`vllm/v1/metrics/loggers.py`, lines 379-381): 0.001, 0.005, 0.01,
   0.02, 0.04, 0.06, 0.08, 0.1, 0.25, 0.5, 0.75, 1.0, 2.5, 5.0, 7.5, 10.0,
   20.0, 40.0, 80.0, 160.0, 640.0, 2560.0. The simulator v0.8.2 has the
   same lists (ADR-0021, fact 1).
2. The share of requests under an edge is exact; a quantile between edges
   is interpolated (ADR-0013, fact 6).
3. vLLM writes `le="1.0"`, the simulator `le="1"`.
4. PromQL cannot use a sample value to select a label: the declared
   threshold has to be a label to be joined with `le`.

## Decisions

1. The operator exports `vcso_vllmservice_objective_ttft_seconds` with a
   `le` label, Rust's Display of the f64 (`1.0` -> `"1"`, `0.5` ->
   `"0.5"`), tested on the encoded `/metrics` text.
2. `slo:ttft_over_le:ratio_rate{5m,30m,1h,6h}` keep one ratio per edge,
   with `le` normalized by stripping a trailing `.0`.
   `slo:ttft_over_objective:ratio_rate*` take the ratio at the declared
   edge, joined `on (namespace, service, le)`, else at `le="1"`
   (ADR-0013). The fast and slow alerts read these.
3. `VllmTtftObjectiveUnsupported` fires when the declared `le` is not one
   of the 22 edges, listed in the rule; it does not depend on traffic. Such
   a service is measured at 1 s.
4. CI keeps the ADR-0021 rule that tests must fail on a broken rule: it
   breaks the normalization, then the join, four rules each, and promtool
   must fail on both.

Rejected: `histogram_quantile` for any threshold (interpolated, not a
verifiable count); an enum of edges in the CRD (rejects existing objects,
and a new edge would need a release; the alert keeps the warning visible).

## Consequences

- A team can declare any of the 22 edges and is paged or ticketed on its
  own threshold; 0.25 to 10 s are the useful ones for an LLM.
- The recording rules are renamed from `slo:ttft_over_1s:*`; the CI query
  and the GKE checklist follow. The Grafana-managed rules in Grafana Cloud
  must be synced again after the release.
- Rules and operator ship together: with an operator older than this
  release the objective gauge has no `le`, and every declared objective
  would be reported unsupported.
- Not yet seen on live series: the next GPU session should read
  `slo:ttft_over_objective:ratio_rate5m` for a service with a declared edge.
