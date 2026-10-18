# ADR-0012: A trace-derived load profile, replayed open-loop

Status: Accepted, measured on the simulator 2026-10-18
Date: 2026-10-18

## Context

The `sim-load` CI job drove the operator's Service with 30 seconds of
closed-loop load: four workers, each sending its next request when the
previous one finished, every prompt the same length and every response
capped at 64 tokens. That proves the serving path and describes no
workload. A closed loop slows down when the server does, so the time a
request would have waited before being sent is never recorded
(coordinated omission), and fixed sizes hide the length mix that decides
how long a request holds a slot.

ADR-0010's last postscript reached the same point from the GPU side: what
a replica does depends on how arrivals are phased, and a fleet sized on
arrivals that come together "is not how a production fleet receives
them". This ADR gives the platform a production arrival stream to replay.
It replays it on the simulator, so what it adds is a workload, not a
performance figure.

## Facts that constrain the design

1. **The trace.** The Azure LLM inference trace 2023, conversation service
   (`data/AzureLLMInferenceTrace_conv.csv` in Azure/AzurePublicDataset),
   published by Microsoft under CC-BY-4.0 with Patel et al., "Splitwise:
   Efficient generative LLM inference using phase splitting", ISCA 2024.
   One row per request: arrival timestamp, context tokens, generated
   tokens; no prompt text. 19,366 requests over 3,502 s, 5.53 requests/s
   on average, inter-arrival coefficient of variation 1.094. The commit
   and the SHA-256 it is read at are pinned in
   `hack/loadgen/build_profile.py`.
2. **The simulator counts one word as one token.** llm-d-inference-sim
   v0.8.2, the base of `hack/rehearsal/Dockerfile.sim`, uses the Hugging
   Face tokenizer only when a request to huggingface.co for the model
   returns 200, and its own tokenizer otherwise
   (`pkg/tokenizer/tokenizer.go:86-109`); that tokenizer counts one word
   or one punctuation mark per token (`tokenizer.go:44-46`). Built from
   this repository and run with network access on 2026-10-18, the image
   logged "Model is not a real HF model, using simulated tokenizer" for
   `facebook/opt-125m` and reported 4 prompt tokens for 4 words. Why the
   request did not return 200 was not read, so this is a measurement of
   this image, and D3 repeats it on every run.
3. **Prompt length costs nothing in this image's latency model.** With a
   time to first token set, as `Dockerfile.sim` sets it, the simulator
   derives it from that value and the load factor, not from the prompt
   (`pkg/llm-d-inference-sim/latencies.go:122-124`); only with it unset
   does prefill depend on prompt tokens (`latencies.go:114-120`).
   `--max-model-len` rejects with 400 any request whose prompt plus
   `max_tokens` exceeds it (`pkg/llm-d-inference-sim/request.go:113-121`).
   It also sizes metric histograms and a metrics channel of
   `(max-num-seqs + max-waiting-queue-length) × max-model-len` float64
   values (`metrics.go:268-271`), reserved rather than resident: idle,
   the container used 10.3 MiB at 4,096 and 9.6 MiB at 16,384.
4. **The simulator serves at most five sequences at once** by default
   (`pkg/common/config.go:307`); requests beyond that wait in a queue of
   up to 1,000 (`config.go:308`, `simulator.go:156-165`). What happens
   past the queue was not read; the runs here stay far below it.
5. **aiohttp caps a session at 100 connections by default**
   (`TCPConnector(limit=100)`), which would quietly turn an open loop
   into a closed one at 100 requests in flight.

## Decisions

**D1 — The profile is the trace's busiest five minutes, kept whole.** The
window starts 120 s before the 60 s sliding window with the most arrivals
(522, from 2023-11-16 18:46:29.56) and lasts 300 s: trace lines 9515 to
11794, 2,280 requests, 7.6 requests/s, 1.37 times the trace's mean. Per
minute it carries 455, 446, 522, 454 and 403 requests, so it is a plateau
rather than an isolated burst, and it is declared as such. Its length mix
differs from the whole trace: median context 426 tokens against 1,020,
90th percentile 4,084 against 2,735, and 431 requests (18.9%) above 4,096
tokens of context plus response. `hack/loadgen/build_profile.py` rebuilds
`hack/loadgen/profiles/azure-conv-2023-peak.csv` from the pinned commit
and stops if the trace's SHA-256 differs; built from a local copy and
from a fresh download, the two files were byte-identical. The file's
header carries source, SHA-256, license, citation, peak and window.

**D2 — Thinning happens at run time, per trace line.** The profile mode
of loadgen (`--profile FILE --keep P --seed S`) keeps each row with
probability P from an RNG seeded by `S:trace_line`, and the same RNG
draws a prompt of exactly `context_tokens` words. A row's draw depends
only on the seed and its line, so thinnings are nested: every request
kept at 0.05 is kept at 0.1. With seed 42 the profile keeps 121
requests at 0.05, 222 at 0.1, 332 at 0.15 and 445 at 0.2.

**D3 — Open loop, and every run proves what it replayed.** Each kept
request is sent at its scheduled offset whatever is in flight, on a
connector without a connection limit (fact 5), with `ignore_eos` so the
response is exactly `generated_tokens` long, and with
`stream_options.include_usage` so the server reports the tokens it
counted. Each record carries `ts_scheduled` and `send_delay_ms`, so a
generator that falls behind shows up instead of hiding queueing. The
run's header carries the SHA-256 of the profile file and of the kept
sequence (line, offset, sizes and prompt hash of every request): the same
profile, keep and seed give the same hash. The closed loop stays the
default, and `hack/rehearsal/run.sh` still uses it.

**D4 — The simulator admits 16,384 tokens.** `--max-model-len` in
`hack/rehearsal/Dockerfile.sim` goes from 4,096 to 16,384, which covers
the trace's largest context, 14,050 tokens, plus its largest response,
1,000. It is an admission limit (fact 3), so no latency changes and no
request of the profile is refused. At 4,096 the simulator would refuse
requests already at keep 0.05, whose largest kept request is 4,623
tokens of context plus response.

**D5 — CI replays the profile at keep 0.1, seed 42, and asserts
correctness only.** `sim-load` fails if the run's sequence hash is not
the one pinned in the workflow, if fewer or more requests are recorded
than scheduled, if any request is not answered with 200, if any served
prompt or completion token count differs from the profile, or if any
send leaves more than 250 ms late. Latency is printed, not asserted:
thresholds belong to a service level objective, which this ADR does not
set. The check was run on a real run and on a copy with five injected
faults; it passed the first and reported all five on the second.

## Measured, 2026-10-18

The image built from `hack/rehearsal/Dockerfile.sim` with 16,384, run as
one container on a development host without Kubernetes, keep 0.1, seed 42:

| | |
|---|---|
| requests | 222 sent over 297.4 s, all answered with 200 |
| served tokens equal to the profile | 222 of 222 |
| send delay p99 / max | 3.0 / 3.5 ms |
| time to first token p50 / p90 / p99 / max | 446 / 7,333 / 10,309 / 11,579 ms |
| time per output token p50 / p99 / max | 36.9 / 40.9 / 41.0 ms |
| requests in flight, max | 13 |

The generator kept its schedule, so the latencies are the server's. The
tail is queueing: 13 requests in flight against five slots (fact 4).
Time per output token sits near 40 ms, the image's 20 ms inter-token
latency times its 2.0 load factor. At 0.75 requests/s one simulated
replica holds the median of this peak and not its tail. Whether more
replicas absorb it is not tested here.

## Consequences

**sim-load runs about five minutes longer.** The replay lasts
297 s plus the tail of the last requests, against 30 s before; the job
took 2 min 21 s on the last run before this change, inside a 20-minute
timeout.

**Anyone can repeat the run.** Rebuild the profile from the public trace,
run loadgen with the same keep and seed, and compare the sequence hash.

**A change to the profile, the thinning or the prompt generation turns
CI red** until the pinned hash is updated in the same change, so none of
them can move the workload without saying so.

**The repository carries 58,150 bytes of third-party data** under
CC-BY-4.0, attributed in the file's header and here.

## What this ADR does not prove

- Nothing about engine or GPU performance. The latencies are the
  simulator's model (fact 3), the envelope the README already states for
  `sim-load`.
- Not the trace's minute-scale shape at keep 0.1. Below the minute the
  window is already close to Poisson arrivals (inter-arrival coefficient
  of variation 1.043), and thinning moves it little: 1.003 at 0.2, 0.967
  at 0.1. Across minutes it loses the shape: at 0.1 the five minutes carry
  44, 37, 45, 46 and 50 requests, and the window's peak minute, about 15%
  above its neighbours, is inside sampling noise. Keep 0.1 replays the
  window's rate level, scaled, and its exact per-request length mix.
- Not a service level objective. No latency here is a target.
- One trace, one service, one window, one replica. The dataset's other
  services and the trace's other windows are not read.
- Prompt text is random words, which share no prefix. The simulator
  ignores it; replayed against an engine with prefix caching, it would
  measure a cache that never hits.
