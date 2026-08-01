# ADR-0009 D5 — warming-aware vs naive autoscaling (kind experiment design)

Status: design frozen before execution (2026-08-01), amended by two dated
postscripts before any rep was run (see the end of this file). Everything in
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
- The consumer is an instrumentation-only loop in this directory. It
  does not exist in the operator by design: D1 decided the operator
  exposes the scale subresource and does not own the scaling loop.

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
| per-replica capacity | declared constant | instrumentation only; not a capacity model |
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

The consumer is instrumentation, not a product. It is not an HPA, not a
KEDA scaler, and its capacity model is a constant. The claim available
from this experiment is about the *information* the operator publishes,
not about any particular autoscaler's quality.

The warm-up delay is synthetic and uniform. Real cold starts vary
widely — the 27s/96s spread measured on H100 is the counter-example —
so this run bounds the effect at one warm-up time, not across the
distribution.

kind schedules pods in milliseconds and pulls nothing. Everything that
makes real capacity slow to arrive except the warm-up itself is absent.

## Postscript 2026-08-01 — two constants the table did not name

Written before any rep was run, while implementing the consumer. The body
above is unchanged.

The parameter table above declares "per-replica capacity: declared
constant" without giving the constant, and does not mention a replica
ceiling at all. Both exist in `consumer.py` and both had to be chosen, so
they belong here rather than being discovered in the source later:

| Parameter | Value | Rationale |
|---|---|---|
| per-replica capacity | 2.0 RPS | with the x4 step this makes `needed` = 4 |
| demand window | 10 s | trailing window over trace arrivals |
| MAX_REPLICAS (fuse) | 40 | non-binding by measurement, see below |

The scale-up rule also needed a form the table did not fix. It is
incremental:

    desired = spec.replicas + max(0, needed - available)

not `desired = needed`. This is not a detail: a consumer that writes
`needed` outright is insensitive to `warmingReplicas` by construction and
the experiment could only ever return zero. The naive arm over-requests
precisely because it re-asks on every tick for capacity that is already on
its way.

**The fuse was binding at its first value, and that is a finding about the
design, not about the system.** Simulated against the rep-1 trace, with
the arms' pure functions imported from `consumer.py` and only the cluster
modelled, the naive arm hit a ceiling of 12 at t=150 and held it for 440
of the 600 seconds. Its primary metric would then have been the ceiling —
the same number in every rep, variance zero, and a bootstrap CI over the
per-rep deltas computed on a constant chosen by the author. Uncapped on
that trace the naive arm peaks at 28 against the warming-aware arm's 4.

Raising the fuse was checked against the platform rather than assumed. The
simulator holds 41.8 MB RSS idle and 42.7 MB under 30 concurrent requests
(read from `/proc/<pid>/status` in the container, +0.9 MB: it simulates
latency and does not allocate per token), host load was 0.24 before and
0.20 after that burst on 4 cores, the child Deployment carries no resource
requests at `gpu: 0`, and kind allows 110 pods per node. 28 replicas cost
about 1.2 GB against 5.9 GB free. The fuse is 40 and the experiment cannot
reach it.

One consequence for the writeup: the naive arm's peak is a function of how
many ticks fit inside a warm-up window, since it re-requests once per tick.
Simulated peaks are 28 at a 5 s tick, 22 at 10 s, 16 at 15 s, 13 at 20 s,
10 at 30 s, against a flat 4 for the warming-aware arm. The effect size is
therefore reported at a stated tick, not as a constant of the hypothesis.
The 5 s tick is kept as frozen: lengthening it would have shrunk the
effect to fit a ceiling, which is adapting the measurement to the
instrument.

## Postscript 2026-08-01 — how the load reaches a set of replicas that changes

Written before any rep was run, while building the dispatcher. The body
above is unchanged.

The design says the load is replayed from a frozen trace but never says
against what, and by this point the answer is not a single URL. Children are
`d5probe-0..N`, each with its own ClusterIP Service, and the set of them
varies during the run because varying it is the entire experiment.
`replay.py` from the ADR-0007 experiment takes one `--url`, so it is left
byte-identical — its output is committed evidence there — and
`dispatch_d5.py` is derived from it: same pacing loop, same per-request
accounting, multi-target discovery added.

Discovery is the consumer side of ADR-0008 D1. The operator publishes
placements in `FleetService.status`; the dispatcher reads them and sends to
the ones in phase `Ready`. Endpoints are derived rather than guessed:
`build_service()` in `src/main.rs` names the Service after the child
VllmService — the string carried in `placement.vllmServiceRef` — and exposes
port 8000, so `<vllmServiceRef>:8000` follows from the operator's own naming.
Selection is round-robin; the policy is identical in both arms and only the
set differs, which is the point.

Reachability is a single `kubectl proxy` on localhost, with targets varying
only in the request path. This keeps the dispatcher stdlib-only and on the
host beside `consumer.py`, with no image to build, no RBAC to grant, and no
per-target port-forward to die mid-run and be indistinguishable from a
failure of the system under test. The latency the proxy adds does not reach
the primary metric, which is peak replicas.

With zero Ready targets a request fails immediately and is recorded as
`no_ready_target` rather than waiting for capacity. Waiting would make the
generator's timing a function of the arm, which is the open-loop property the
design depends on. Each record also carries `target` and `n_ready_at_send`,
which gives the requests-per-replica distribution as secondary evidence at no
extra cost.

**A rep must outlast the step by more than the warm-up, or the variation it
exists to create is not observable.** The child readiness delay was measured
rather than taken from the image tag: pods created at 09:55:08 reported Ready
at 09:56:10, 62 s, against the 60 s the `llmd-sim-d5:warm60` tag names
nominally. In a 150 s smoke with the fleet verified at one replica
beforehand and a scale to three at t=20, the dispatcher sent 165 requests
while one target was Ready and 135 while three were, the last single-target
send at t=82.0 and the first three-target send at t=82.5 — the step plus the
measured warm-up, with no intermediate value. Traffic split 210/45/45 overall
and 45/45/45 after the transition. An earlier smoke of the same shape
returned a perfectly uniform 100/100/100 and looked like a working
round-robin; it was a run that had begun with three replicas already up, and
the uniformity was the evidence that the transition had never been exercised
at all.

During that check the operator was sampled alongside the pods, which settles
something the arms depend on: placements stayed `Pending` with
`warmingReplicas` at 2 for the whole warm-up and turned `Ready` in the same
tick the pods did. `readyReplicas` therefore does not count replicas that
cannot serve, and the naive arm's arithmetic measures what this document says
it measures. The new children sat in `Pending` and never in `Warming`, which
is the D3 postscript's case observed rather than reasoned about.
