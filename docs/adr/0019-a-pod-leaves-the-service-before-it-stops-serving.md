# ADR-0019: A pod leaves the Service before it stops serving

Status: Accepted, rehearsed on kind 2026-10-23
Date: 2026-10-23
Amends: ADR-0005 (the drain window of a move)

## Context

R6 item 3 asks what a fault costs under load: a fleet of two replicas,
one per worker (ADR-0018), the within-capacity replay (keep 0.025, seed
42, ADR-0013) through one NodePort over the fleet's pods, and one fault
per run on the node of slot 0: a cordon and drain (ADR-0017), or a
preemption notice (ADR-0005).

## Facts that constrain the design

1. The load generator does not retry (`hack/loadgen/loadgen.py`); a
   request that fails is recorded as failed. `slo_check.py` exits 1 for
   any failed request, with or without `--assert-slo`.
2. At this load (68 requests in about five minutes, a mean request of
   3 s) the first run had no request in flight at either fault, so its
   0 failures said nothing about the fault. A closed-loop probe, four
   streams of 512 tokens for 20 s from 5 s before the fault, recorded
   apart from the replay, puts streams there; a run in which none is
   open at the fault is not valid.
3. With the probe, before this change, every fault cut the streams in
   flight on the replica it hit: the drain cut 3 of 4 open streams 0.2 s
   after the fault; the notice cut 2 of 4 at +6.8 s, when the old pod was
   removed once the new one was Ready. The replay lost nothing; none of
   its requests was in flight then.
4. The llm-d simulator exits 0.28 s after SIGTERM and cuts its stream
   (92 of 300 tokens, curl exit 18; run with docker, outside Kubernetes).
   The pod had no preStop hook, and its grace was the default 30 s:
   the 120 s of `TERMINATION_GRACE_SECONDS` apply only with a GPU
   (`src/main.rs`, `serving = gpu > 0`).
5. Through one Service the simulator's metrics are scraped one pod at a
   time and an evicted pod takes its counters with it, so the
   cross-check of `slo_check.py` against them cannot hold under a fault.

## Decisions

- Every child pod gets `lifecycle.preStop.sleep` of 30 s
  (`PRE_STOP_SLEEP_SECONDS`), the native action, so the image needs no
  shell. The kubelet runs it before SIGTERM, while the endpoint leaves
  the Service: no new request reaches the pod, and a stream in flight
  has 30 s to finish whatever the server does on SIGTERM. 30 s is about
  twice the longest response measured on this profile (14.4 s); it is
  not a bound for generations on a GPU.
- The grace is always set: 120 s with a GPU, as before, and 40 s
  (`PRE_STOP_SLEEP_SECONDS` + 10) without one.
- `slo_check.py --client-only` skips the cross-check against the
  simulator's metrics; every objective is still the client's. The
  cross-check stays in `sim-load`, which runs the same generator with
  no fault.
- A `faults-under-load` job runs both faults with the probe, and fails
  if any probe stream fails, if none is open at the fault, or if the
  replay is not the pinned one.
- Rejected: leaving it to the serving engine's SIGTERM handling (not
  verified for vLLM, and the platform would ship the defect); a preStop
  `exec sleep` (needs a shell in the image); 15 s (covers the longest
  response measured with almost no margin); asserting the replay's
  objectives alone (fact 2: they could not see the defect).

## Rehearsed, 2026-10-23

Three runs of the job with `job-run.py` on kind on optim-dev:

- run 1, no probe: 0 failed, no request in flight at either fault;
- run 2, probe, before this change: drain 3 of 4 open streams cut,
  notice 2 of 4 cut; `timeline.py` exits 1 for both;
- run 3, probe, with this change: 4 open at each fault, 0 cut;
  `timeline.py` exits 0 for both.

In every run the replay met every objective (68 requests, 0 failed),
ready replicas fell to 1 for 7.6 s after the drain and never below 2
after the notice. Unit: `every_pod_sleeps_before_sigterm_within_its_grace`;
three tests of `slo_check.py` (`hack/loadgen/test_slo_check.py`), which
fail on the previous `slo_check.py`.

## Consequences

- A drain, a move, a scale-in or a scale to zero holds the outgoing pod
  30 s longer; capacity during a move is unchanged (make-before-break).
- The pod template changes, so every child rolls out once on upgrade.

## What this ADR does not prove

- The simulator is not vLLM: how vLLM ends a generation on SIGTERM is
  not read here. The sleep covers it either way for a response shorter
  than 30 s; a longer one is still cut.
- Run 2's notice also had two probe connections fail after 133.6 s
  with `Connect call failed` and no errno; it did not recur in run 3.
  A connection sent to the pod just removed is a hypothesis, not read.
- With four probe connections spread by kube-proxy, about 1 run in 16
  could have none on the pod that is hit; the job checks that streams
  are open at the fault, not on which pod.
- No load beyond keep 0.025, no GPU, and kind's networking only.
