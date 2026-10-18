# ADR-0013: Service level objectives for the simulated service, and one replica's capacity

Status: Accepted, measured on the simulator 2026-10-18
Date: 2026-10-18

## Context

ADR-0012 made the `sim-load` CI job replay a public trace open-loop and
assert that every request is served correctly. It recorded latency and
asserted none: at the load it replays, 10% of the busiest five minutes,
one replica of the simulator has up to 13 requests in flight for 5 slots,
and the time to first token reaches seconds. A recorded number with no
objective does not say whether the service is good enough, and the load
one replica can carry, the input any scaling target needs, was never
measured.

This ADR declares service level objectives for the simulated chat
service, measures the load one replica carries while meeting them, and
makes CI assert them where they should hold. It also records what the
simulator measures and what it only computes, because that decides which
of its numbers can check the load generator's.

## Facts that constrain the design

Source lines are from llm-d-inference-sim v0.8.2, the base image of
`hack/rehearsal/Dockerfile.sim`, under its `pkg/` directory.

1. **The simulator reports the latencies it computes, not ones it
   measures.** `simulateTTFT` computes a time to first token, sleeps for
   it and sends that same value to the histogram
   (`llm-d-inference-sim/context.go:186-189`); the time per output token
   is handled the same way (`context.go:194-199`). Comparing the load
   generator's TTFT or TPOT with these would compare a measurement with
   the model that produced it.
2. **It does measure four durations**: queue time, from when a request
   is queued (`simulator.go:336` and `:347`); prefill, from the start of
   `simulateTTFT` (`context.go:190`); inference (`worker.go:93`); and
   end-to-end, from `startProcessingTime()` (`worker.go:92`). On Docker
   at 10% of the profile, mean end-to-end minus mean inference was
   1.425 s, and so was the mean queue time over all requests: end-to-end
   starts when the request arrives.
3. **A labelled histogram exists only after its first observation.** At
   2.5% of the profile no request queued, and `/metrics` had no
   queue-time series at all: a missing series is a zero.
4. **The load factor reads a lagging counter.** TTFT and inter-token
   latency are scaled by `1 + (f - 1)(running - 1)/(slots - 1)`
   (`latencies.go:59-64`), with `f` = 2.0 and 5 slots in our image, and
   `running` is a metrics counter updated through a channel
   (`metrics.go:691-693`). Sent one at a time, 21 requests averaged
   187 ms, 0.75 x the 250 ms configured, the factor for `running` = 0:
   a request on its own is not yet counted at its first token. No
   standard deviation is set (`common/parser.go:129-130`), and with
   none the draw returns the mean (`common/utils.go:115-117`).
5. **Waiting does not enter the computed latencies.** Queued requests
   are not counted by the load factor; they wait for a slot. At 10% of
   the profile the client's mean TTFT was 1.857 s: 1.425 s of queue and
   0.429 s of prefill.
6. **The simulator's TTFT buckets are coarse above 100 ms**: 0.25, 0.5,
   0.75, 1, 2.5, 5 s and up. A p95 estimated from them can be off by a
   bucket; the share of requests under a bucket edge is exact. The TPOT
   buckets have an edge at 0.05 s.
7. **The fleet scales only to and from zero.** The FleetService scale
   subresource has no `labelSelectorPath` (`deploy/crd.yaml:489-493`),
   so the HPA cannot take it from one replica to more (ADR-0009,
   postscript of 2026-08-17). Scaling from zero under a burst would also
   need something to hold requests until a replica is Ready.

## Decisions

**D1. Objectives, in `hack/loadgen/slo.json`.** For 95% of requests, a
time to first token of at most 1 s and a time per output token of at
most 50 ms, taken as the request's mean from its first token to its
last; and 99.5% of requests served, that is answered with 200 and with
the profile's token counts. One second is about where a chat starts to
feel stalled; 50 ms a token is 20 tokens a second, faster than people
read. Both thresholds are bucket edges of the simulator's histograms
(Fact 6). They were chosen for the user before any load was measured
against them, and they are written as shares rather than percentiles,
so a run's error budget is a count, (1 - share) x requests, and each
run reports how much of it was consumed.

**D2. The client is the source of the indicators; the simulator checks
it where it measures.** The load generator's mean TTFT must equal the
simulator's mean queue time plus mean prefill, and its mean end-to-end
latency the simulator's, within 25 ms; and the simulator must have
counted exactly the requests the client sent. Measured on Docker, the
means differ by 3.2 to 3.4 ms for TTFT and 1.5 to 1.7 ms end-to-end;
25 ms is under 3% of the TTFT objective, so a larger disagreement could
change a verdict. The simulator's own TTFT and TPOT are compared with
nothing (Fact 1). The check reads `/metrics` before and after each run
and uses the differences, so runs need no restart, and a missing series
counts as zero (Fact 3). Health probes do not enter the count: five
calls to `/health` left it unchanged.

**D3. One replica carries about 0.4 requests per second of this
profile.** That is the highest load tested at which every objective
held on every seed tried: 5% of the profile, seeds 42, 1 and 2, from
0.345 to 0.408 req/s. At 0.538 req/s (7.5%, seed 42) the TTFT objective
was missed. The rule, the highest load meeting every objective on every
seed tried, was written down before seeds 1 and 2 ran.

**D4. CI replays the profile twice, both at seed 42 with their sequence
hashes pinned.** At 2.5%, where every objective was met with no error
budget consumed, it asserts the objectives with `--assert-slo`; the
margin is for a shared runner on kind, whose timing is not the timing
measured here. At 10%, which overloads the replica on purpose, it
reports the objectives and asserts correctness and D2 only.
`hack/loadgen/slo_check.py` replaces the check ADR-0012 put inline in
the workflow, with the same correctness assertions.

**D5. Scaling under a burst is out of scope.** It needs the fleet to
scale past one replica and something to hold requests at zero (Fact 7):
operator features, to be designed in their own record, not changes to
the load test.

## Measured, 2026-10-18

On Docker on one machine, with the image of
`hack/rehearsal/Dockerfile.sim`, one replica, the profile of ADR-0012
thinned at run time, and a simulator restarted before each run. Every
run served every request with 200 and the profile's token counts, and
met the TPOT objective.

| Keep | Seed | Requests | Offered, req/s | TTFT ≤ 1 s | TTFT p95, ms | TTFT error budget consumed | Client minus simulator, TTFT / e2e, ms |
|---|---|---|---|---|---|---|---|
| 0.025 | 42 | 68 | 0.229 | 100% | 440 | 0% | 3.4 / 1.7 |
| 0.05 | 42 | 121 | 0.407 | 96.7% | 502 | 66% | 3.2 / 1.6 |
| 0.05 | 1 | 103 | 0.345 | 100% | 446 | 0% | 3.4 / 1.6 |
| 0.05 | 2 | 122 | 0.408 | 96.7% | 705 | 66% | 3.4 / 1.6 |
| 0.075 | 42 | 160 | 0.538 | 93.1% | 1638 | 138% | 3.3 / 1.6 |
| 0.1 | 42 | 222 | 0.746 | 67.6% | 8537 | 649% | 3.3 / 1.5 |

## Consequences

- `sim-load` replays the profile twice, about five minutes each, inside
  the job's existing timeout of 20 minutes.
- The objectives live in one file. Changing a threshold is a reviewed
  diff to `slo.json`, and the capacity of D3 has to be measured again.
- The capacity is the number a scaling target needs: a replica should be
  added before its load passes about 0.4 requests per second of this
  profile.
- Every CI log shows what missing the objectives looks like: in the 10%
  run the TTFT error budget is consumed several times over while every
  request is still served.

## What this ADR does not prove

- **Engine performance.** The latencies are those configured in the
  simulator's image, scaled by its load factor, and its TTFT and TPOT
  are computed (Fact 1). The objectives are the ones a GPU replica would
  have to meet; whether one does, and at what load, needs vLLM on a GPU.
- **An SLO in the operational sense.** The objectives are evaluated per
  run, over a few minutes. A running service is held to them over a
  rolling window, with alerts on how fast its error budget burns;
  nothing here collects metrics continuously or alerts.
- **The capacity beyond what was run.** One trace, one window, the
  simulator's queue and 5 slots, Docker on one machine; three seeds at
  5% and one seed at each other load. The capacity on kind is not
  measured.
- **Scaling under a burst** (D5), or anything with more than one
  replica.
