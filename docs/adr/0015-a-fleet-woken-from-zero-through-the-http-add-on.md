# ADR-0015: A fleet woken from zero through the KEDA HTTP add-on

Status: Accepted, rehearsed on kind and measured in CI 2026-10-21
Date: 2026-10-21

## Context

ADR-0014 made a fleet a target the HPA scales from 1 to N, and kept its
ScaledObject at `minReplicaCount: 1`
(`deploy/examples/keda/scaledobject-scale-out.yaml:26`): a fleet at zero
has no pod to answer its first request. To and from zero stayed with
`scaledobject-scale-to-zero.yaml`, capped at one replica (line 24). On
the platform's GPU node an idle replica is the larger cost (see "What a
wake costs" below), so a fleet that cannot go to zero is paid for while
it serves nothing.

This ADR puts the KEDA HTTP add-on 0.16.0 in front of the fleet. A
request that arrives at zero waits in the add-on's interceptor, the
add-on's count of held and in-flight requests wakes the fleet, and
ADR-0014's trigger takes it from 1 to N. It measures what one wake costs
in objectives and in idle replica time, and prices both for the
platform's own GPU node.

## Facts that constrain the design

1. **At zero, a request can only wait in front of the fleet.** The
   InterceptorRoute holds a request until its target has a Ready
   endpoint, for at most its readiness timeout (the `InterceptorRoute`
   v1beta1 schema in the add-on's 0.16.0 chart). Past it, with no
   fallback, the interceptor answers 504, "backend not ready"
   (`interceptor/middleware/endpoint_resolver.go:92-95`); 503 is only
   the overflow past the cold-start bound (D4).

2. **The cold-start queue limit is per interceptor replica.** The same
   schema: "The limit applies per interceptor replica: the effective
   cluster-wide capacity is maxPendingRequests multiplied by the number
   of interceptor replicas". The chart runs at least three interceptor
   replicas (`interceptor.replicas.min: 3` in its `values.yaml`).

3. **The add-on wakes the fleet; KEDA's polling puts it to sleep.** The
   add-on's scaler reads the interceptors every 500 ms
   (`KEDA_HTTP_QUEUE_TICK_DURATION`, `scaler/config.go:33`) and pushes
   to KEDA only a change to active, every 200 ms
   (`scaler/config.go:37`, `scaler/handlers.go:95-100`); active means
   any metric above zero (`handlers.go:72-74`), and "standard metric
   polling handles inactivation and scale-down" (`handlers.go:80-81`).
   The signal reaches KEDA within about 0.7 s of the first held
   request. The add-on's one-minute window applies to its request rate,
   not to concurrency (`scaler/queue_pinger.go:41`).

4. **KEDA takes a fleet to zero a cooldown after the last moment any
   trigger was active, and straight to zero.** In KEDA 2.21.0 the
   defaults are 300 s for `cooldownPeriod`
   (`pkg/scaling/executor/scale_executor.go:38`) and 30 s for
   `pollingInterval` (`apis/keda/v1alpha1/withtriggers_types.go:51`,
   applied at `:107`); the check, and the scale to the idle or minimum
   count, are at `pkg/scaling/executor/scale_scaledobjects.go:222-225`.
   A Prometheus trigger is active for any value above its activation
   threshold, zero by default (`pkg/scalers/prometheus_scaler.go:56`
   and `:338`). ADR-0014's query is a one-minute average, so it stays
   active for up to a minute after the last request. An object that was
   never active may go to zero at once: `initialCooldownPeriod`
   defaults to 0 (`scale_executor.go:37`).

5. **The HPA follows the largest of its metrics and scales down on the
   highest recent recommendation.** With several metrics the HPA
   computes a replica count for each "and then the largest of the
   desired" counts is used; scale-down uses "the highest recommendation
   from within that window" (Kubernetes documentation, Horizontal Pod
   Autoscaling).

6. **The platform's GPU is one L4, and at most one.** The gke platform
   (gke-llm-inference-platform at 6884060) creates a node pool of
   `g2-standard-4` with one `nvidia-l4` in `europe-west4-a`, from 0 to 1
   node, with no spot or preemptible setting
   (`terraform/variables.tf:9,27,33,39,45,51`,
   `terraform/modules/gke-gpu/main.tf:75-77,90-97`, and `gpu_count`
   defaulting to 1 at `terraform/modules/gke-gpu/variables.tf:47`, which
   the root module does not set). The eks platform
   (eks-llm-inference-platform at f9420b3) has one node group,
   `m7i-flex.large`, and no GPU (`main.tf:17`).

## Decisions

**D1. The add-on is installed the way KEDA is: chart by SHA-256, images
by digest.** The workflow pins the chart's URL, its SHA-256 and the
three images (`.github/workflows/ci.yml:28-32`). The chart composes one
tag for all three images, so `hack/keda-http-install.sh` installs the
chart's own tag, sets each deployment to its pinned digest, and fails
unless every add-on pod runs exactly its pinned image (lines 8-12,
19-20 and 25-42).

**D2. Three interceptor replicas, held there during the measurement.**
The script sets the interceptor's minimum and maximum to three, the
chart's minimum (`hack/keda-http-install.sh:21-23`), and fails unless
three are Ready (`:37-42`). A replica count that moved under load would
move the queue limit (fact 2) under the measurement. In production the
interceptor is autoscaled from at least three, and the limit is read as
a per-replica number. One interceptor was rejected: it would make the
gateway a single point of failure to simplify a measurement.

**D3. Two triggers, two jobs.** The add-on's trigger is a wake signal,
not a control law: `targetValue: 100` requests per replica
(`deploy/examples/keda/scale-from-zero.yaml:25-30`) is far above what a
replica serves (ADR-0013), so it asks for one replica while any request
is held or in flight. ADR-0014's Prometheus trigger, unchanged, takes
the fleet from 1 to N (`:75-82`). A low target, about 3, would wake the
fleet straight to two or three replicas. It is not taken here: the HPA
would follow whichever trigger asks for more (fact 5), and the two
signals would contend for 1 to N, which this ADR has not measured. It
is the lever for the one-replica window measured below.

**D4. Held requests are bounded, and past the bound they are
rejected.** `maxPendingRequests: 16` per interceptor with `overflow:
Reject` (`scale-from-zero.yaml:35-41`): twice the 8 requests held at
once in the unbounded run, even if all land on one interceptor, and 48
across three; Little's law predicts 5.9 (0.746 requests per second for
7.9 s). Past the bound a client gets 503 at once instead of queueing
behind a fleet that may not wake. `readiness: 120s` (`:34`) is the
longest a held request waits; past it the interceptor answers 504
(fact 1).

**D5. KEDA's timing is written down.** `pollingInterval: 30` and
`cooldownPeriod: 300` (`scale-from-zero.yaml:55-61`) are KEDA's defaults
(fact 4), stated because they set what a wake costs, as
`scaledobject-scale-to-zero.yaml:29` states its own. A shorter cooldown
would cut idle time, but the right value depends on the gaps in a
service's traffic, which a five-minute peak trace does not contain.

**D6. One job asserts the wake, the scale-out and the return to zero.**
The `scale-from-zero` job repeats the `scale-out` job's bench rather
than sharing it, so that the `scale-out` check keeps its name and
history (`.github/workflows/ci.yml:1269-1271`). It applies the route and
the ScaledObject to a Ready fleet, checks that a request crosses the
interceptor (`:1351-1356`), and patches the fleet to zero rather than
waiting for the cooldown (`:1380-1392`). The load enters through the
interceptor's NodePort (`hack/scale-from-zero/interceptor-nodeport.yaml`).

- `measure.sh` asserts ADR-0014's scale-out, with `OUT`, `HPA` and
  `FROM_ZERO` set by the job (`hack/scale-out/measure.sh:48`). With
  `--from-zero`, `slo_check.py` subtracts from the client's sums the
  wait of the requests sent before the first token, which the simulator
  did not see, and says that this overstates the wait
  (`hack/loadgen/slo_check.py:15-19` and `:100-107`). Raising the 25 ms
  tolerance was rejected, because it would hide a per-request cost of
  the interceptor; dropping the comparison too, because it is the check
  that the client measures what the simulator serves.
- `coldstart.py` asserts that the fleet started at zero, became Ready,
  rejected nothing and served at least 99.5%
  (`hack/scale-from-zero/coldstart.py:38-47`).
- `idle.py` asserts that the fleet reaches zero between 300 and 480 s
  after the last response, that is between the cooldown and the
  one-minute average plus two polling intervals, the cooldown and a
  60 s margin, and that no fleet pod is left; it counts the
  replica-seconds up with no request to serve
  (`hack/scale-from-zero/idle.py:31-39` and `:47-54`).

The objectives are reported, not asserted, as in ADR-0014 D5.

## Measured, 2026-10-20 and 2026-10-21

On optim-dev, on kind, with the job's steps run locally and the
simulator image of `hack/rehearsal/Dockerfile.sim`, replaying ADR-0012's
profile at 10%, seed 42 (sequence `b170da22`, 222 requests at 0.746 per
second). The pull request's CI run adds its own numbers at the end of
this section.

| Keep 0.1, seed 42 | From zero, no bound | From zero, bound 16 | From zero, bound 16 | From one replica, `scale-out` |
|---|---|---|---|---|
| Date | 2026-10-20 | 2026-10-20 | 2026-10-21 | 2026-10-20 |
| First token, s after the first request | 7.904 | 7.925 | 8.348 | - |
| Requests held until then | 8 | 8 | 8 | - |
| Their first tokens, s after the first request | 7.904 to 12.697 | 7.925 to 12.161 | 8.348 to 13.692 | - |
| Longest TTFT of a held request, ms | 9918 | 8500 | 8348 | - |
| TTFT at most 1 s | 91.9% | 89.6% | 90.5% | 98.6% |
| TTFT misses, error budget consumed | 18, 162% | 23, 207% | 21, 189% | 3, 27% |
| Requests served by each pod | 79 / 76 / 67 | 95 / 60 / 67 | 98 / 65 / 59 | 99 / 70 / 53 |

With the bound in place every request was answered with 200 and none
with 503. The first run predates `--from-zero` and failed only on the
client-simulator comparison, by 127.2 and 125.3 ms; the held requests'
wait up to the first token, 28729 ms, against a difference of 28238 ms,
leaves -2.2 ms per request: the interceptor adds no latency this bench
can measure.

Where the second run's misses came from: 7 while held; 12 while one
replica served, because the HPA asked for three replicas 11 s after the
first was Ready and the new ones took 8 s more; and 4 with three
replicas Ready, all queued on one pod. `sim-fleet-0` counted 19 queued
requests against 15 in the first run, and the other two pods queued
none in either. Each request had its own connection (ADR-0014 D4), so
the concentration ADR-0014 saw at +270 s happens without connection
reuse; why it happens is still not investigated.

The wake: the signal reaches KEDA within about 0.7 s (fact 3), and the
first replica was Ready at 8 s in all three runs (2 s samples). The
other seven seconds belong to the fleet: the HPA, scheduling, the
simulator's start and its readiness.

The eight held requests do not get their first token together. In
each of the four runs (three on kind and the CI run below) five get it
within 0.5 s of the first token and three later, the last 4.2 to 5.3 s
after it; each of those three gets it 500 to 509 ms after another
request ends (`scale-from-zero.jsonl`). The simulator serves at most
five requests at a time: `max-num-seqs` defaults to 5
(llm-d-inference-sim at the v0.8.2 tag, `pkg/common/config.go:307`)
and `hack/rehearsal/Dockerfile.sim:9-15` does not set it. With five
running, its time to first token is 250 ms times a load factor of 2.0
(`pkg/llm-d-inference-sim/latencies.go:59-64,123`,
`Dockerfile.sim:12,14`), the 500 ms measured. So the longest wait from
zero is the wake plus a queue on the one Ready replica: a held
request's TTFT reached 8.3 to 10.9 s, against a first token at 7.9 to
8.3 s. The five slots are the bench's; on vLLM the bound is its own
`--max-num-seqs`, which this ADR does not read.

The return to zero, in the 2026-10-21 run (`scale-from-zero.idle`, 5 s
samples):

| After the last response | spec.replicas |
|---|---|
| 0 s | 3 |
| 316 s | 2 |
| 332 s | 0, and no fleet pod left |

That is 981 replica-seconds up with no request to serve, and zero
332 s after the last response, inside the 300 to 420 s that fact 4
predicts. The step from three to two at 316 s is consistent with the
HPA's scale-down window (fact 5); the job does not record the HPA's
recommendations, so it does not show it.

### In CI, 2026-10-21

The pull request's run (GitHub Actions run 39575073713, commit
cb94e12) ran the `scale-from-zero` job on a runner of image
ubuntu-24.04, version 20261018.339.1, with llm-d-inference-sim v0.8.2
(`sha256:c3ba4350`). It replayed the same 222 requests at keep 0.1,
seed 42 (sequence `b170da22`), at 0.746 requests per second, from
zero replicas (first sample: spec.replicas 0, ready 0); every one was
answered with 200.

| Keep 0.1, seed 42 | From zero, bound 16, CI |
|---|---|
| First Ready sample, s after the first request | 9 (2 s samples) |
| First token, s after the first request | 8.164 |
| Requests held until then | 8 |
| Their first tokens, s after the first request | 8.164 to 13.392 |
| Longest TTFT of a held request, ms | 10866 |
| TTFT at most 1 s | 90.1% |
| TTFT misses, error budget consumed | 22, 198% |
| TTFT p50 / p95 / p99, ms | 314.1 / 4437.3 / 6298.9 |
| TPOT at most 50 ms | 100% |
| Most requests in flight | 11 |
| Client minus simulator, TTFT / e2e, ms | -4.0 / -5.1 |
| Requests served by each pod | 80 / 71 / 71 |
| spec.replicas after the last response | 3 at 0 s, 2 at 318 s, 0 at 334 s |
| Replica-seconds up with no request to serve | 986 |

The first Ready sample at 9 s, after a first token at 8.164 s, is the
2 s sampling: the replica was Ready before the first token, after the
previous sample. The HPA wanted two or more replicas 23 s after the
start of the load and the second was Ready at 29 s; the fleet peaked
at its cap of three, and no fleet pod was left at 334 s. Each step
down came 2 s later than in the kind run of 2026-10-21.

### What a wake costs on the platform's GPU node

Prices from Google's Cloud Billing Catalog API (Compute Engine,
`europe-west4`, read 2026-10-21, effective from 2026-10-20, in EUR as
the API returns them), and the shape of `g2-standard-4` from the Compute
API: 4 vCPUs, 16 GiB, one `nvidia-l4`.

| Per hour, EUR | On demand | Spot |
|---|---|---|
| G2 vCPU | 0.023112 | 0.013869 |
| G2 RAM, per GiB | 0.002708 | 0.001624 |
| L4 GPU | 0.517995 | 0.310728 |
| One `g2-standard-4` | 0.6538 | 0.3922 |

The L4 is 79.2% of the node's hour. One node kept on costs 15.69 EUR a
day on demand, 471 EUR per 30 days, and 9.41 EUR a day spot; the
project's whole GPU budget, 100 EUR, would last 6.4 days.

On gke a fleet at zero leaves its GPU node unneeded, and the cluster
autoscaler removes a node unneeded for more than 10 minutes (cluster
autoscaler FAQ, kubernetes/autoscaler at 292f5d594cb6, lines 841 and
1130; GKE's page refers to it). With one replica, all the gke platform
holds (fact 6), a wake leaves 332 s of pod and 600 s of node up with
nothing to serve: 0.169 EUR. With three replicas, if they fitted, 981
replica-seconds and three nodes: 0.505 EUR. A pause in traffic saves
GPU time only if it lasts more than about 15.5 minutes; a shorter one
never reaches zero and costs what a replica kept on costs. Scale to
zero therefore never pays for more GPU time than a replica kept on,
except the node's boot at each wake, which was not measured. Its price
is in the objectives: 18, 23 and 21 misses in the three wakes, 20.7 on
average or 9.3% of this replay, against 3 from one replica.

## Consequences

- A fleet can wait at zero and be woken by its first request. With the
  bound in place no request was rejected; the first ones waited for the
  cold start instead.
- The cost decision is explicit. On the gke platform scale to zero is
  the default, because one node kept on would exhaust the project's GPU
  budget in under a week; its price is about 21 requests over the TTFT
  objective per wake on this replay, and more where a wake creates a
  node. A service whose objectives cannot absorb a wake keeps a replica
  on with ADR-0014's ScaledObject (`minReplicaCount: 1`), at 15.69 EUR a
  day on demand or 9.41 spot.
- Three levers are named and not taken: waking to two or three replicas
  (D3), against the 12 misses of the one-replica window; a shorter HPA
  scale-down window, against the three replicas held through the
  cooldown, at the risk of flapping under bursts and of changing
  ADR-0014's measured behaviour; spot GPUs, at 60% of the on-demand
  price, against preemptions, which ADR-0005 handles.
- The gateway is not where a wake's time goes: about 0.7 s of signal
  against about 7 s of fleet on kind, and far more on a GPU node.
  Shortening a wake is the engine's and the operator's work.
- More replicas still help only where requests go: the concentration on
  one pod returned with one connection per request, and a load-aware
  router (ADR-0007 D5) remains the remedy ADR-0014 named.
- The gke platform as declared holds one GPU replica
  (`max_gpu_nodes = 1`, one zone; fact 6), fewer than this ADR's cap of
  three and than ADR-0014's scale-out assumes. Matching them is the
  platform's change, not this repository's.

## What this ADR does not prove

- **A wake on a GPU.** On gke a wake from zero also creates a node:
  provisioning, the driver install the node pool asks for
  (`gpu_driver_installation_config`), the image pull and the model's
  weights. ADR-0014 cites 11.7 to 19.0 s to construct a vLLM engine
  alone. `readiness: 120s` is set for the simulator and is likely too
  short there; on a GPU it must come from a measured cold start, or
  every request held through a slower wake ends in a 504 (fact 1).
  No run here came near it: Ready came at 8 s, so the 504 is read in
  the code, not observed.
- **GKE's own scale-down timing.** The 10 minutes are the open-source
  default; GKE's page does not give its value, and neither a node's
  deletion nor its billing while it boots was measured.
- **The queue behind the first replica on vLLM.** The stagger of the
  held requests is the simulator's five slots (Measured); on vLLM it
  depends on its own `--max-num-seqs` and on the prompts, not measured
  here. That the simulator image's digest was built from the v0.8.2
  tag read here is not verified.
- **The Reject path.** At most 8 requests were held, against a bound of
  16 per interceptor; no run reached it.
- **The interceptor under autoscaling.** It was held at three replicas;
  autoscaled, the cluster-wide bound moves with its replica count
  (fact 2).
- **The HPA's step from three to two.** Observed, not recorded.
- **Why requests concentrate**, as in ADR-0014.
- **Production traffic.** One wake per run, three runs, five minutes of
  a public trace at 10%, a simulator on kind; the misses per wake varied
  from 18 to 23.
- **eks.** It has no GPU node group, so nothing here is priced for it.
