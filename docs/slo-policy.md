# SLO and error budget policy

Three objectives, each over a rolling 30 days, grouped by namespace and
service (ADR-0013, ADR-0021, ADR-0026).

| Objective | Target | Budget | Alerts |
|---|---|---|---|
| Time to first token within 1 s | 95% | 5% | VllmTtftBudgetBurnFast, VllmTtftBudgetBurnSlow |
| Mean time per output token within 100 ms | 95% | 5% | VllmTpotBudgetBurnFast, VllmTpotBudgetBurnSlow |
| Requests not aborted | 99.5% | 0.5% | VllmSuccessBudgetBurnFast, VllmSuccessBudgetBurnSlow |

## Alerts

Fast burn (14.4x, 1h and 5m windows) pages: a 30-day budget is gone in
about two days. Slow burn (6x, 6h and 30m) opens a ticket: about five
days. Runbooks: `docs/runbooks/ttft-burn-rate.md`,
`docs/runbooks/tpot-and-success-burn-rate.md`.

## When the budget is spent

1. Changes of model, image or operator stop, except fixes for the
   burn and security fixes, until the 30-day budget is positive again.
2. The incident gets a written cause in DECISIONS.md and, if it is a new
   failure mode, a row in `docs/fmea.md`.
3. If the cause is capacity, raise the fleet's cap only with a cost
   estimate in BUDGET.md; otherwise accept the burn and say so.

## What the objectives do not see

Requests lost before a replica, and replicas whose `/metrics` is down
(VllmTargetDown covers the second). A missing series is not a met
objective.
