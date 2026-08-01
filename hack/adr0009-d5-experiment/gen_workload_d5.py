"""Deterministic arrival trace for the ADR-0009 D5 experiment.

Derived from hack/gpu-session/adr0007-ea-experiment/gen_workload.py. That
generator emits a flat rate and a 1500-token shared prefix, both wrong for
D5: the effect under test only exists when demand rises faster than a
replica warms, and no prefix reuse is measured here. The original is left
untouched because its output is committed evidence with a recorded sha256.

Design parameters, frozen in DESIGN.md and hardcoded here rather than
exposed as flags: base rate 2 RPS, step at t=120s multiplying the rate by
4, run length passed in per rep. The consumer reads this same file to
derive demand open-loop, so the trace is the single source of arrivals for
both arms.

The JSONL schema is kept identical to the ADR-0007 generator (t_offset_s,
shared, prompt, max_tokens) so the existing replay.py consumes it
unchanged. `shared` is always false here: it carries no meaning for D5 and
is retained only so the schema does not fork.

Token lengths are approximated as chars/4, the same declared approximation
as the original. Same rep index -> byte-identical JSONL (sha256 printed).
"""

import argparse
import hashlib
import json
import random
import sys

CHARS_PER_TOK = 4
BASE_RPS = 2.0
STEP_AT_S = 120.0
STEP_FACTOR = 4.0
PROMPT_TOK = (20, 40)
OUTPUT_TOK = 32
SEED_BASE = 2000


def words(rng, n_tokens):
    return " ".join(
        "".join(rng.choices("abcdefghijklmnopqrstuvwxyz", k=CHARS_PER_TOK - 1))
        for _ in range(n_tokens)
    )


def rate_at(t):
    return BASE_RPS * STEP_FACTOR if t >= STEP_AT_S else BASE_RPS


def offsets(duration_s):
    """Arrival offsets as a step function of time, not a fixed cadence."""
    out = []
    t = 0.0
    while t < duration_s:
        out.append(round(t, 3))
        t += 1.0 / rate_at(t)
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--rep", type=int, required=True, help="rep index, seeds the RNG")
    ap.add_argument("--duration-s", type=int, required=True)
    ap.add_argument("--out", required=True)
    args = ap.parse_args()

    rng = random.Random(SEED_BASE + args.rep)
    ts = offsets(args.duration_s)

    with open(args.out, "w") as f:
        for t in ts:
            f.write(
                json.dumps(
                    {
                        "t_offset_s": t,
                        "shared": False,
                        "prompt": words(rng, rng.randint(*PROMPT_TOK)),
                        "max_tokens": OUTPUT_TOK,
                    }
                )
                + "\n"
            )

    h = hashlib.sha256(open(args.out, "rb").read()).hexdigest()
    before = sum(1 for t in ts if t < STEP_AT_S)
    print(
        f"rep={args.rep} n={len(ts)} before_step={before} "
        f"after_step={len(ts) - before} sha256={h}"
    )


if __name__ == "__main__":
    sys.exit(main())
