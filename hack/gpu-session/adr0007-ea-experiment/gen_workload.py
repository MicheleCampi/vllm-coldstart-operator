"""Deterministic workload generator for the ADR-0007 EA-vs-WF experiment.

Design parameters from DESIGN.md, frozen: shared-prefix fraction 0.6,
shared prefix ~1500 tok, unique tail 100-300 tok, output 128 tok, 8 RPS.
Token lengths are approximated as chars/4 here (declared approximation:
harness mechanics do not depend on exact token counts; the GPU session
records the real tokenizer counts via vLLM usage stats).

Same seed -> byte-identical JSONL (sha256 printed for evidence).
"""
import argparse
import hashlib
import json
import random
import sys

CHARS_PER_TOK = 4
SHARED_FRACTION = 0.6
SHARED_PREFIX_TOK = 1500
TAIL_TOK = (100, 300)
OUTPUT_TOK = 128
RPS = 8


def words(rng, n_tokens):
    return " ".join(
        "".join(rng.choices("abcdefghijklmnopqrstuvwxyz", k=CHARS_PER_TOK - 1))
        for _ in range(n_tokens)
    )


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--rep", type=int, required=True, help="rep index, seeds the RNG")
    ap.add_argument("--duration-s", type=int, required=True)
    ap.add_argument("--out", required=True)
    args = ap.parse_args()

    rng = random.Random(1000 + args.rep)
    shared_prefix = words(rng, SHARED_PREFIX_TOK)
    n = args.duration_s * RPS
    with open(args.out, "w") as f:
        for i in range(n):
            shared = rng.random() < SHARED_FRACTION
            tail = words(rng, rng.randint(*TAIL_TOK))
            prompt = (shared_prefix + " " + tail) if shared else tail
            f.write(json.dumps({
                "t_offset_s": round(i / RPS, 3),
                "shared": shared,
                "prompt": prompt,
                "max_tokens": OUTPUT_TOK,
            }) + "\n")
    h = hashlib.sha256(open(args.out, "rb").read()).hexdigest()
    print(f"rep={args.rep} n={n} sha256={h}")


if __name__ == "__main__":
    sys.exit(main())
