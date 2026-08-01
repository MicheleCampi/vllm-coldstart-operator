# ADR-0009 D5 — warming-aware vs naive autoscaling (kind experiment design)

Status: design frozen before execution (2026-08-01). Everything in
"Design parameters" is an input choice; everything in "Measured results"
is an output. The two must never be conflated in the writeup.

This experiment runs entirely on kind at zero cost. It needs no GPU:
what is under test is a counter and a control loop, not inference
throughput.

## Hypothesis (falsifiable)

An autoscaler that subtracts warming replicas from available capacity
(D3) allocates fewer peak replicas against the same offered load than
one that sees only ready replicas, because the naive loop re-requests
capacity that is already on its way. If the measured reduction is below
the relevance threshold, publishing `warmingReplicas` does not pay for
itself at realistic warm-up times — a negative result, reported as such.

## Topology

- kind, 1 control-plane + 3 workers (same layout as the ADR-0007
  rehearsal).
- Operator in-cluster via the chart, reporter disabled: D5 measures the
  replica counter, not placement signals. NodeStates are seeded
  directly, all `Warm` at utilisation 0, so placement never becomes the
  discriminating variable.
- Serving pods are `llmd-sim-d5`, the rehearsal simulator behind an
  artificial warm-up delay (`hack/rehearsal/Dockerfile.d5`).
- The consumer is a toy loop in this directory. It does not exist in
  the operator by design: D1 decided the operator exposes the scale
  subresource and does not own the scaling loop.

## Experimental design

Within-subject A/B: the same recorded arrival trace is replayed against
two consumers reading the same `FleetService`.

- **Arm N (naive):** desired = f(demand, `status.readyReplicas`).
- **Arm W (warming-aware):** desired = f(demand,
  `readyReplicas + warmingReplicas`), i.e. capacity already on its way
  is not requested twice.

Both arms write through `kubectl scale`. Only arm W reads
`warmingReplicas`, which the `autoscaling/v1` Scale object does not
carry, so both arms read the CR and write through the subresource —
identical plumbing, one line of arithmetic different. Keeping the two
loops otherwise byte-identical is what makes the comparison mean
anything.

The trace carries a step increase in arrival rate. The effect only
exists when demand rises faster than a replica warms: at a flat rate
both arms converge to the same steady state and measure nothing.

Order alternates N/W then W/N across pairs. There is no thermal or
cache drift on kind, but the cluster accumulates state (deleted pods,
event backlog), so the cluster is recreated between reps rather than
reused.

## Design parameters (inputs, frozen before the run)

| Parameter | Value | Rationale |
|---|---|---|
| warm-up delay | 60 s | measured window t+0..t+61 unready, serving by t+66 |
| consumer tick | 5 s | several ticks inside one warm-up window |
| scale-up rule | ceil(demand / per-replica capacity) | same in both arms |
| per-replica capacity | declared constant | a toy consumer; not a capacity model |
| initial replicas | 1 | the step must find the fleet under-provisioned |
| step | at t=120 s, rate x4 | after both arms reach steady state |
| run length | 600 s per rep | >= 8 warm-up windows past the step |
| reps | 4 per arm, NW+WN | matches the ADR-0007 harness |
| seed | fixed per rep index | same trace replayed across arms |
| `stableReconcilesRequired` | 3 (chart default) | not tuned for this run |

## Measured results (outputs)

Primary:
- peak replicas allocated over the run.

Secondary:
- requests served per replica-second (allocation that was actually used).

Mechanism / assertions (pass-fail, not effect sizes):
- surplus children are deleted after `stableReconcilesRequired`, in
  both arms, and `status.replicas` follows the live children down.
- `warmingReplicas` is non-zero during warm-up in both arms; only arm W
  acts on it.

Guardrail (non-regression):
- requests failed or refused. Arm W must not win peak replicas by
  under-provisioning into errors.

## Decision criterion (fixed now)

Per-rep deltas (N − W) on peak replicas, mean with bootstrap 95% CI
over reps. Relevance threshold, declared before any data: a peak-replica
reduction below 15% is not a result. Outcomes:
- CI above threshold -> the subtraction pays, scoped to this warm-up
  regime.
- CI straddling zero or below threshold -> negative result: publishing
  warming replicas does not change allocation at these warm-up times.
  Published with the same rigour.
- Guardrail regression -> reported as a cost regardless of the primary
  outcome.

## Why the scale-down assertion is here

D4's scale-down pass is covered by unit tests only through its pure
function `surplus_hysteresis`. The delete itself, the in-window count
and the 404-as-success path have no test, and the e2e job in CI
exercises `VllmService` only, never `FleetService`. This run is where
that code meets an API server repeatedly, so it is made to report
whether it works. A failed assertion invalidates the rep, not the
hypothesis.

## Established before the design was written

Four mechanisms were exercised on kind on 2026-08-01 and are recorded
in the dated postscripts on ADR-0009. They are preconditions, not
findings of this experiment:

- `kubectl scale` writes `spec.replicas` in both directions.
- The wrapper yields an unready window of t+0..t+61 at
  `WARMUP_DELAY_S=60`.
- `warmingReplicas` reads 2 across that window; placements go
  `Pending -> Ready` and never enter `Warming`, so the D3 amendment is
  what makes the counter non-zero at all.
- Scaling 2 -> 1 removes the surplus child by t+11; the hysteresis
  counter is event-driven, so its window is not a duration and must not
  be treated as one when reading the traces.

## Known limitations, stated before the run

The consumer is a toy. It is not an HPA, not a KEDA scaler, and its
capacity model is a constant. The claim available from this experiment
is about the *information* the operator publishes, not about any
particular autoscaler's quality.

The warm-up delay is synthetic and uniform. Real cold starts vary
widely — the 27s/96s spread measured on H100 is the counter-example —
so this run bounds the effect at one warm-up time, not across the
distribution.

kind schedules pods in milliseconds and pulls nothing. Everything that
makes real capacity slow to arrive except the warm-up itself is absent.
