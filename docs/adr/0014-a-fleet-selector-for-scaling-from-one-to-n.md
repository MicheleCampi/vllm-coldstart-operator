# ADR-0014: A fleet selector for scaling from one to N

Status: Accepted, rehearsed on kind and measured in CI 2026-10-19
Date: 2026-10-19

## Context

ADR-0009 D1 gave the FleetService a `scale` subresource so that an
external autoscaler could drive a fleet. Its 2026-08-17 postscript found
that KEDA could drive it only to and from zero: above one replica the
work belongs to the HPA, and the HPA refused the fleet. ADR-0013 kept
that limit as its fact 7 and left scaling under a burst out of scope
(D5), and its own table shows the cost: one replica replaying the
ADR-0012 profile at 10% met the time-to-first-token objective for 67.6%
of requests, with 649% of the error budget consumed.

This ADR makes the fleet a target the HPA accepts, gives it a trigger,
and measures one scale-out under that same replay.

## Facts that constrain the design

1. **The HPA wants a selector even where it does not use one.** It
   refused the fleet with `ScalingActive=False InvalidSelector`, the
   scale "missing a selector", although the metric KEDA registers is
   `external` and is not aggregated over pods (ADR-0009, 2026-08-17
   postscript).

2. **A Deployment's selector is immutable; its pod template is not.** The
   label map that names a child's pods also feeds its Deployment's
   `LabelSelector` (`src/main.rs`, `build_deployment`), and the `e2e`
   job asserts that changing it is rejected
   (`.github/workflows/ci.yml:619-645`). Labels added to the template
   alone change the pods, through a rollout, and leave the selector as
   it was.

3. **A child is pinned to its node and surges first.** Its pod selects
   its node by `kubernetes.io/hostname` (`src/main.rs:240-254`), requests
   `nvidia.com/gpu` (`src/main.rs:101`), and rolls out with
   `maxSurge: 1` and `maxUnavailable: 0` (`src/main.rs:273-274`).

4. **The simulator reports its load per pod and admits five requests at
   a time.** `vllm:num_requests_running` and `vllm:num_requests_waiting`
   are its gauges (llm-d-inference-sim v0.8.2,
   `pkg/llm-d-inference-sim/metrics.go:53-54`), and `MaxNumSeqs`
   defaults to 5 (`pkg/common/config.go:307`); neither
   `hack/rehearsal/Dockerfile.sim` nor `hack/scale-out/sim-fleet.yaml`
   changes it.

5. **A Service balances per connection.** kube-proxy picks a pod when a
   connection opens, and a client that keeps its connection alive sends
   every request on it to that pod (`hack/scale-out/sim-fleet.yaml:17-20`).

6. **Prometheus rejects one selector over both gauges inside
   `avg_over_time`.** The function drops the metric name, which leaves
   two series per pod with one labelset; the first rehearsal's query
   failed with 422, "vector cannot contain metrics with the same
   labelset".

## Decisions

**D1. The fleet label goes on the pods, not on the selector.** A fleet's
child adds `inference.michelecampi.dev/fleet=<name>` to its pod template
only (`src/main.rs:80-86`). Its Deployment's selector stays what it was,
byte for byte (test `a_fleet_child_labels_its_pods_and_keeps_the_selector`),
and a standalone service's template does not change
(`a_standalone_service_keeps_its_pod_template`). The fleet publishes the
matching query as `.status.selector` (`src/fleet_types.rs:216-219`),
which the scale subresource reads through `labelSelectorPath`
(`src/fleet_types.rs:25`). The key is defined once (`FLEET_LABEL`,
`src/fleet_types.rs:191`) and the selector is derived from it
(`fleet_selector`, `src/fleet_types.rs:195-197`); a test pins the key and
the path (`scale_subresource_reads_the_fleet_selector`). The controller
writes the selector into the status of a fleet with no candidate as well
as into the full status (`src/fleet_controller.rs:390-392` and `:816`),
so the subresource has a selector from the fleet's first status. Unlike
the field ADR-0009 declined, this one selects exactly the fleet's pods.

**D2. The signal is requests in flight, averaged over a minute.** KEDA's
Prometheus trigger reads the fleet's running and waiting requests, each
averaged over one minute and summed over pods, so that a burst shorter
than a minute does not buy a replica
(`deploy/examples/keda/scaledobject-scale-out.yaml:39-47`). It is two
sums rather than one selector over both gauges (fact 6); the samples
share a scrape, so the sum of the averages is the average of the sum.

**D3. Threshold 1.3 requests per replica, cap 3.** Little's law on
measured numbers: one replica serves about 0.4 requests per second
(ADR-0013 D3), and with nothing queued a request took 3.36 s on average
(3.363 s, the mean end-to-end latency of the 68 requests of ADR-0013's
2.5% run), so a replica at capacity holds 0.4 x 3.36 = 1.34 requests.
Rounding down adds a replica slightly early. The cap of three bounds what
a wrong signal can cost. `minReplicaCount` is 1
(`scaledobject-scale-out.yaml:26-27`): this object does not take the
fleet to zero, which stays the job of `scaledobject-scale-to-zero.yaml`.

**D4. The test owns the Service, and each request opens its own
connection.** Routing belongs to the router layer (ADR-0007 D5, ADR-0008
D1), not to the operator, so the `scale-out` job applies its own Service
over the fleet label (`hack/scale-out/sim-fleet.yaml:17-20`). The load
generator's `--force-close` closes each connection after its response,
so each request is balanced on its own (fact 5). It is refused outside
the open loop, and the run record carries it as `force_close` in its
`run_config` line (`hack/loadgen/loadgen.py`).

**D5. CI asserts the control loop and reports the objectives.** The job
replays the ADR-0012 profile at 10%, seed 42, through that Service. It
fails if the fleet does not reach two replicas, exceeds three, or serves
from fewer than two pods (`hack/scale-out/measure.sh:48-49` and `:59`),
and when `slo_check.py` finds a sequence other than the pinned one, a
record count other than the profile's, a response other than 200 or
with other token counts, a request that left late, a simulator count
other than the client's, or mean latencies that disagree with the
simulator's by more than 25 ms (`hack/loadgen/slo_check.py:51-96`).
It runs without `--assert-slo` (`hack/scale-out/measure.sh:43-44`), so
the objectives of ADR-0013 are reported, not asserted: the second
replica arrives while the load is already on, and the spread of fact 5
is not the operator's
to fix.

**D6. One KEDA install, pinned images.** The `keda` and `scale-out` jobs
install KEDA through one script (`hack/keda-install.sh`). Prometheus runs
from `mirror.gcr.io` by digest, inline in its manifest
(`hack/scale-out/prometheus.yaml:91`), and scrapes every 5 s (`:53`).

## Measured, 2026-10-19

On optim-dev, on kind, with the `scale-out` job's steps run locally and
the simulator image of `hack/rehearsal/Dockerfile.sim`. The pull
request's CI run adds its own numbers at the end of this section.

The first rehearsal failed at the HPA step. Its query used one selector
over both gauges, Prometheus answered 422 (fact 6), and the HPA reported
`ScalingActive=False FailedGetExternalMetric`, while `AbleToScale` was
True: the selector itself had been accepted. The second rehearsal, with
the two sums, passed every step.

| From the start of the load | spec.replicas | Ready | HPA desired |
|---|---|---|---|
| 0 s | 1 | 1 | 1 |
| 28 s | 2 | 1 | 2 |
| 35 s | 2 | 2 | 2 |
| 41 s | 2 | 2 | 3 |
| 44 s | 3 | 2 | 3 |
| 50 s | 3 | 3 | 3 |

The replica counts were sampled about every 2 s. All 222 requests were
answered with 200, offered at 0.746 requests per second, with a largest
send delay of 3.6 ms. The three pods served 106, 63 and 53 requests; the
first was alone for the first 35 s. Time to first token was at most 1 s
for 95.5% of requests (objective 95%; 10 misses of 11.1 budgeted, 90% of
the budget) and time per output token at most 50 ms for all of them.
TTFT p50, p95 and p99 were 314.7, 848.4 and 3838.7 ms, and at most 9
requests were in flight.

ADR-0013 replayed the same sequence (keep 0.1, seed 42, 222 requests)
against one replica: 67.6% within 1 s, 649% of the budget, p95 8537 ms.
That run was on Docker and this one on kind, so the difference points
the same way as the scaling but is not a measured effect of it (see the
last section).

The ten slow requests came in two groups: three in the first 17 s, with
one replica, and seven between +263 and +271 s, with three replicas
Ready. At +270 s Prometheus showed 5 requests running and 3 waiting on
one pod, 1 running on the second and none on the third: 8 of the 9
requests in flight were on one replica while 9 slots on the other two
were free (fact 4). If each request picked a pod uniformly and
independently, that concentration would be rare, roughly 1 in 350 by an
estimate that ignores how long requests last.

### In CI, 2026-10-19

The pull request's run (GitHub Actions run 39334011664, commit
ef074fc) ran the `scale-out` and `sim-load` jobs side by side, on two
runners of the same image (ubuntu-24.04, version 20261012.334.1) with
the same simulator image. Both replayed the same 222 requests at keep
0.1, seed 42 (sequence `b170da22`, checked by `slo_check.py`), at 0.746
requests per second; every one was answered with 200 and the profile's
token counts. `sim-load` sent them to one replica, `scale-out` to the
fleet, one connection per request (`"force_close": true` in its record).

| Keep 0.1, seed 42 | One replica, `sim-load` | Fleet, `scale-out` |
|---|---|---|
| TTFT at most 1 s | 68.9% | 96.4% |
| TTFT error budget consumed | 622% (69 misses) | 72% (8 misses) |
| TTFT p50 / p95 / p99, ms | 442.0 / 8565.1 / 10221.4 | 314.2 / 703.9 / 3743.9 |
| TPOT at most 50 ms | 100% | 100% |
| Most requests in flight | 13 | 9 |
| Client minus simulator, TTFT / e2e, ms | 2.2 / 1.4 | 2.2 / 1.2 |

The fleet's replicas, sampled about every 2 s from 1.3 s before the first
request (`scale-out.replicas` in the job's artifact):

| From the start of sampling | spec.replicas | Ready | HPA desired |
|---|---|---|---|
| 0 s | 1 | 1 | 1 |
| 25 s | 2 | 1 | 2 |
| 32 s | 2 | 2 | 2 |
| 280 s | 3 | 2 | 3 |
| 288 s | 3 | 3 | 3 |

Unlike the rehearsal, the fleet served most of the replay on two
replicas: the third was Ready at 288 s, near the end of the 300 s
replay, and its pod served 5 requests, against 129 and 88. The HPA
asks for a third replica only once the minute's average of requests
in flight is above twice the threshold of D3, 2 x 1.3 = 2.6; the job
does not record the metric itself, so this run cannot say how close
it came before 280 s.

The eight slow requests came in the same two groups as in the
rehearsal: three between 8.8 and 16.1 s after the first request, with
one replica Ready, and five between 262.7 and 270.8 s, with two. In
both runs the second group starts in the heaviest ten seconds of the
sequence: from 260 to 270 s it sends 13 requests and 1871 output
tokens, against a mean of 927 tokens per ten seconds. That window is a
condition, not a sufficient cause: the one from 200 s sends 1836
tokens and the one from 150 s sends 14 requests, and neither produced
a slow request. With the per-connection spread of fact 5, which the
rehearsal saw at +270 s, it is the likely explanation; this run did
not record the load per pod.

## Consequences

- A fleet is a target the HPA accepts, so KEDA, or any HPA, can scale it
  from 1 to N. To and from zero still goes through KEDA's scale
  executor, as before.
- The CRDs have to be applied before the operator is upgraded: without
  `labelSelectorPath` the HPA keeps refusing the fleet.
- Upgrading the operator rolls out every fleet child once, because its
  pod template gains the label. With surge first and the node pin
  (fact 3), on a node with no spare GPU the new pod stays Pending and
  the old one keeps serving until a GPU frees there. The CHANGELOG says
  so under "Upgrading from 0.5.0".
- More replicas help only where the Service sends requests. Fact 5 makes
  the spread a property of connections, not of load; a load-aware
  router (ADR-0007 D5) is the remedy, not a lower threshold.
- ADR-0013's fact 7 and D5 no longer describe the fleet. ADR-0013 stays
  as written; this ADR supersedes those two points.

## What this ADR does not prove

- **A GPU replica's time to Ready.** The 6 to 7 s from decision to Ready
  above are the simulator's, on kind. Constructing a vLLM engine for a
  7B model on an A10 took 11.7 to 19.0 s before it served a request
  (vllm-coldstart-probe, cited in the README).
- **Scale-down.** The run ends with the load, and no replica was seen to
  leave.
- **Why the requests concentrated.** Chance, or the way kube-proxy and
  connection tracking assign connections (reused source ports, for
  example), was not investigated.
- **The upgrade on GPU nodes.** The Pending rollout above follows from
  the strategy and the pin; no GPU node has run it.
- **An effect of scaling on the objectives.** ADR-0013's one-replica
  numbers come from Docker and these from kind. Until the pull request's
  CI runs `sim-load` and `scale-out` on one runner, the difference is
  not a measured effect.
- **Production traffic.** This is five minutes of a public trace at 10%,
  served by a simulator.
