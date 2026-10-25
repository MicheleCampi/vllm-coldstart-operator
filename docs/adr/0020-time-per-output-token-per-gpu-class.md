# ADR-0020: The time per output token is an objective per GPU class

Status: Accepted, not yet measured under load
Date: 2026-10-25
Amends: ADR-0013 (D1, the time per output token of one GPU class)

## Context

ADR-0013 set the objectives on the simulator: for 95% of requests a time
to first token of at most 1 s and a time per output token (TPOT) of at
most 50 ms. The first session on a real GPU, on 2026-10-25, served
Qwen/Qwen2.5-7B-Instruct on one L4 and found that no load meets 50 ms:
the floor of that GPU and model is above it. The objective has to be kept
for the user and stated for the hardware.

## Facts that constrain the design

1. **The objective is a requirement for the user.** ADR-0013 D1: "50 ms a
   token is 20 tokens a second, faster than people read", chosen "for the
   user before any load was measured". It was also a bucket edge of the
   simulator's histograms; the GPU runs are checked client-side
   (`slo_check.py --client-only`), where buckets play no part.
2. **The L4 floor is about 64 ms a token.** One L4 (g2-standard-4,
   driver 580.178.04), vLLM v0.11.0, bf16, `--max-model-len 8192`: in
   the pinned replay the two requests with no other request in flight
   had a TPOT of 65.4 and 64.3 ms, the lowest of the 68 was 63.8 ms, the
   median 69.9 ms and the 95th percentile 101.8 ms; streamed chunks
   equalled tokens.
3. **People read 238 to 260 words a minute, most at most 320.**
   Brysbaert (2019), "How many words do we read per minute? A review and
   meta-analysis of reading rate", Journal of Memory and Language,
   doi:10.1016/j.jml.2019.104047: silent reading of English by adults is
   238 wpm for non-fiction and 260 wpm for fiction; most adults fall in
   175-300 and 200-320 wpm.
4. **English is 1.365 Qwen2.5 tokens a word.** The abstract of that
   paper, 192 words, is 262 tokens for Qwen2.5-7B-Instruct's tokenizer at
   revision a09a35458c702b33eeacc393d103063234e8bc28.
5. **The load of that session was heavier than the profile.** Its prompts
   were 1.50 to 2.10 times `context_tokens` (fixed in loadgen on
   2026-10-25), so its TTFT and its TPOT under load are not this ADR's
   evidence; only the floor of fact 2 is, and a longer prompt does not
   lower it.

## Decisions

**D1. The requirement stays, as a bound.** Faster than people read means
a TPOT under 60000 / (wpm x tokens per word) ms: 185 ms at 238 wpm,
169 ms at 260 wpm and 137 ms at 320 wpm, the fastest of the range.

**D2. One L4, Qwen2.5-7B-Instruct, bf16, vLLM v0.11.0: at most 100 ms a
token,** in `hack/loadgen/slo-l4-qwen7b-bf16.json`, for 95% of requests.
That is 1.37 times under the bound for a reader at 320 wpm and 1.85
times under it at 238 wpm; 50 ms was 2.7 times under it. 125 ms would
leave 1.1 times for the fastest readers; 80 ms would leave 1.7 times but
sits 16 ms above the floor before any concurrency. Chosen by Michele on
2026-10-25.

**D3. Every other objective is ADR-0013's.** Time to first token, shares,
success, send delay and mean tolerance are the same values;
`test_slo_files.py` fails if the file differs from `slo.json` anywhere
else. `slo.json`, and the simulator's 50 ms, do not change.

**D4. The capacity of one replica on this class is unknown until
measured.** ADR-0013 says a changed threshold means measuring D3 again;
no capacity is stated for the L4 until the replay and the D3 ladder run
with the corrected loadgen against this file.

## Consequences

- GPU runs on this class are checked with
  `--slo hack/loadgen/slo-l4-qwen7b-bf16.json`
  (hack/gpu-session/gke-l4/PLAN.md).
- Another GPU, model, precision or engine version needs its own file and
  its own measured floor; the requirement of D1 is the same for all.
- A scaling target on the L4 waits for the capacity of D4.

## What this ADR does not prove

- That the L4 meets 100 ms under load: the 95th percentile of the replay
  was 101.8 ms with prompts 1.7 times too long.
- Tokens per word beyond one academic English text of 192 words; other
  languages and other kinds of text may differ.
- Anything about other GPUs, other models, FP8, or other engine
  arguments.
- The session's per-request records are not in this repository.
