# T_decision anomaly: 1-second quantization — root cause

## Observation

Across all 3 GPU-session reps, the operator's reschedule decision landed at
a near-identical wall-clock fraction (12:44:50.457950, 13:04:27.458418,
13:19:33.457758 — spread under 1ms across 35 minutes), and the intervals
between decisions had a GCD of exactly 1.000s. Measured T_decision read
~1.458s in every rep, which is implausibly repeatable for a reactive chain.

By contrast, the kind rehearsal (run 20260703T211728) measured
T_decision = 113ms from a nanosecond-precision marker, with no grid.

## Root cause

k3s does not run etcd: it runs kine over SQLite. kine implements the etcd
watch API by polling the SQL event log, and the poll loop uses a hardcoded
1-second ticker (pkg/logstructured/sqllog/sql.go, `poll()`:
`wait := time.NewTicker(time.Second)`; verified on kine master 2026-07-04,
behavior long-standing). The in-process `notify` fast path only covers
writes that flow through the same kine instance's write path in a way that
triggers it; an external `kubectl patch` from optim-dev was delivered to
watchers on the next 1s tick.

Causal chain per rep:

1. `kubectl patch nodestate --subresource=status` writes a new row in
   kine's SQLite log.
2. The watch stream (API server -> operator's `.watches(NodeState)`)
   receives the event on the next tick of kine's 1s poller. The grid's
   phase is anchored to kine/k3s process start, which explains the
   constant wall-clock fraction.
3. The operator decides ~30ms after the watch event arrives (the stable
   +0.028s offset) — this is the operator's own latency.
4. Our t_notice marker was truncated to whole seconds (`date +%S`), so
   measured T_decision = decision_time - floor(patch_time) ≈ 1.458s.

## Corrected statement for writeups

Measured T_decision (~1.46s, all reps) is dominated by k3s/kine watch
delivery latency (1s SQLite poll ticker), not by the operator. Operator
latency from watch event to logged decision is ~30ms, consistent with the
113ms end-to-end measured on kind/etcd where watches are push-based.
On an etcd-backed cluster, notice-to-decision would be sub-200ms.

## Evidence

- Modulo-1s arithmetic on the three decision timestamps vs operator start
  (rep1-3 operator.log in this directory).
- kine source: pkg/logstructured/sqllog/sql.go, poll() ticker.
- Counter-example: hack/rehearsal/runs/20260703T211728 (kind/etcd, 113ms).
