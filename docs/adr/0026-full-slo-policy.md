# ADR-0026: A full SLO policy: time per output token and request success

Status: Accepted
Date: 2026-11-10
Follows: ADR-0021 (TTFT burn-rate alerts), ADR-0013 (TTFT objective)

## Context

ADR-0021 alerts on one objective, time to first token. A replica can
answer its first token in time and then stream slowly, or abort the
request; neither burns the TTFT budget. Traguardo 2 asks for a full
policy: latency of the stream and success of the request, with the same
multiwindow, multi-burn-rate alerts and one error budget policy.

## Facts that constrain the design

1. vLLM v0.11.0 exports `vllm:request_time_per_output_token_seconds`,
   one observation per request (its mean TPOT), with a bucket edge at
   exactly 0.1 s.
2. The metric is absent from the simulator v0.8.2: the TPOT rules are
   tested with promtool, not yet on a live replica.
3. `vllm:request_success_total` has `finished_reason` in stop, length,
   abort only. Abort also counts a client that disconnects.
4. A request lost before it reaches a replica (gateway, network) is not
   seen by the server: no server metric can count it.

## Decisions

- D1. TPOT objective: 95% of requests with a mean TPOT within 100 ms
  over 30 days (budget 5%), from the `le="0.1"` bucket.
- D2. Success objective: 99.5% of finished requests not aborted over 30
  days (budget 0.5%). Stop and length are successes: length is a limit
  the client asked for.
- D3. Same alert shape as ADR-0021: fast 14.4x on 1h and 5m (page),
  slow 6x on 6h and 30m (ticket), grouped by namespace and service.
- D4. One runbook for both objectives,
  `docs/runbooks/tpot-and-success-burn-rate.md`, and one budget policy,
  `docs/slo-policy.md`, for all three objectives.

## Consequences

- Client disconnects burn the success budget; the runbook says how to
  tell them apart from server aborts.
- Fact 4 stays open: an availability SLO measured at the gateway is
  future work.
- The TPOT rules need a live proof on a GPU run, with a BUDGET.md
  estimate first.
