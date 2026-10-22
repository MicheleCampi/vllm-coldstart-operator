# ADR-0016: A time limit on every reconcile

Status: Accepted, unit-tested 2026-10-22
Date: 2026-10-22

## Context

On the first attempt of 546fe53 (run 39666087210) the `sim-load` job's
Ready wait timed out after 180 s. The operator's log held one line,
`reconciling VllmService 'sim-load'`, and nothing after it: no status,
no `reconciled`, no `reconcile failed`. On a green attempt the same
reconcile logs all three lines within about 70 ms.

## Facts that constrain the design

1. Between those log lines the reconcile awaits three API calls: the
   Deployment and Service applies and the status patch
   (`src/main.rs`, `reconcile_inner`).
2. A failed reconcile is logged and counted: `error_policy` logs
   `reconcile failed`, sets `vcso_reconcile_failures_total` and
   requeues after 15 s (`src/main.rs:414-418`); the fleet controller's
   logs it and requeues after 10 s (`src/fleet_controller.rs:850-855`).
3. The client is `Client::try_default()`, shared by both controllers,
   with kube-client 2.0.1's defaults: `connect_timeout` 30 s,
   `read_timeout` and `write_timeout` 295 s (`config/mod.rs:400-402`).
   A request the API server accepts and never answers holds the
   reconcile for up to 295 s, with nothing logged or counted.
4. The same client serves the controllers' watches, which are long
   requests by design.

## Decisions

- Every reconcile of both controllers runs inside
  `bounded(reconcile_inner(..), RECONCILE_TIMEOUT)`
  (`src/reconcile_timeout.rs`), with `RECONCILE_TIMEOUT` = 30 s. When
  the limit passes, the reconcile returns `Timeout`, labelled
  `timeout` in `vcso_reconcile_failures_total`, and `error_policy`
  requeues it as it does any failure.
- The client's own timeouts are left as they are (fact 4): a shorter
  `read_timeout` would also cut the watches.

## Measured, 2026-10-22

On run 39738666453 (bd35177, main, kind), the `scale-out` and
`scale-from-zero` jobs' `operator.log` held 182 reconciles, none still
open at the end:

| job | VllmService | FleetService |
|---|---|---|
| scale-from-zero | 36, median 16 ms, max 72 ms | 88, median 22 ms, max 134 ms |
| scale-out | 21, median 13 ms, max 74 ms | 37, median 19 ms, max 204 ms |

The limit is about 147 times the slowest reconcile seen.

## Consequences

- A stalled reconcile is logged, counted under `timeout` and retried
  after 30 s plus the requeue (15 s; 10 s for a fleet), instead of
  holding for up to 295 s
  with nothing logged.
- A reconcile cut at the limit may have applied some of its objects.
  Each step is an idempotent server-side apply or a status patch, so
  the retry converges.

## What this ADR does not prove

- The stall cannot be forced on kind, so CI does not show the limit
  firing; two unit tests on paused time show `bounded` returning
  `None` at the limit and the output before it.
- The durations are from kind, with a local and idle API server. On a
  real cluster they are longer, and the margin is smaller.
- Why the API server did not answer on 546fe53 is not known. The
  `sim-load` job now records the cluster when it fails (PR #43).
