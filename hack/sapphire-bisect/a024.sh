#!/usr/bin/env bash
# Study, not a gate. A-023 (2026-11-22) found the vLLM CPU steady-state step
# on Xeon 8488C (Sapphire Rapids) between v0.19.1 (p50 3.785 s) and v0.20.2
# (5.725 s). v0.20.0 is where torch goes 2.10 -> 2.11 and oneDNN 3.7.1 ->
# 3.10.2. This separates torch from vLLM, on the CPU where the step shows:
#   1. matmul.py, a torch-only bf16 linear timing at the decode shapes, with
#      the torch of the v0.19.1 and of the v0.20.0 image (1 and 2 threads,
#      on cpu0 and its SMT sibling), plus the oneDNN kernel each one picks;
#   2. the A-023 steady-state measure (steady-study/study.sh) of v0.20.0, with
#      v0.19.1 in the same session as control; v0.20.1 too only if v0.20.0 is
#      fast. Every result line starts with "A024 ".
# Usage: a024.sh  (env: REPS=2, OUT=out, MODEL, MODEL_REVISION, HF_CACHE;
# SKIP_STUDY=1 runs only the matmul part).
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
reps=${REPS:-2} out=${OUT:-out}
mkdir -p "$out"
v0191=docker.io/vllm/vllm-openai-cpu:v0.19.1-x86_64@sha256:4c697ae650ebeb3a41f3c9c7020913d4c84d2729dc428ce39d60ca353975a4ce
v0200=docker.io/vllm/vllm-openai-cpu:v0.20.0-x86_64@sha256:dc28ea3382c4cfe71be754fc1bbf66dc6c8e487afaae0b55d938c54bc158f363
v0201=docker.io/vllm/vllm-openai-cpu:v0.20.1-x86_64@sha256:3e882ce09ec9146958940f2e313f8fdb8b058e12e05555f6c70991454f9a0b84
core=$(cat /sys/devices/system/cpu/cpu0/topology/thread_siblings_list)

say() { echo "A024 $*" >&2; }

matmul() {  # matmul NAME IMAGE
  local t
  for t in 1 2; do
    docker run --rm --cpuset-cpus "$core" --memory 6g -v "$here:/bench:ro" --entrypoint python3 "$2" /bench/matmul.py "$t" \
      > "$out/matmul-$1-t$t.jsonl" 2> "$out/matmul-$1-t$t.err" || say "matmul $1 t$t FAILED: $(tail -1 "$out/matmul-$1-t$t.err")"
    { grep -E '"info"|per_token' "$out/matmul-$1-t$t.jsonl" || true; } | while read -r l; do say "matmul $1 t$t $l"; done
  done
  # Which oneDNN kernel a decode-shape bf16 linear runs (gate_up, M=1).
  docker run --rm --cpuset-cpus "$core" -e ONEDNN_VERBOSE=1 --entrypoint python3 "$2" -c \
    'import torch; l=torch.nn.Linear(896,9728,bias=False).to(torch.bfloat16); x=torch.randn(1,896,dtype=torch.bfloat16)
with torch.no_grad():
  [l(x) for _ in range(3)]' 2>&1 | grep -E '^onednn_verbose.*,exec,' | cut -d, -f4-7 | sort | uniq -c | head -5 \
    | while read -r l; do say "onednn $1: $l"; done || true
}

study() {  # study NAME IMAGE -> median p50 on stdout
  local r d v vals=()
  for r in $(seq 1 "$reps"); do
    d="$out/$1-$r"
    if bash "$here/../steady-study/study.sh" "$2" eager core "$d" > "$d.log" 2>&1; then
      v=$(python3 "$here/../steady-study/summarize.py" "$d" | python3 -c 'import json,sys; r=json.load(sys.stdin); print(r["p50_s"], r["p95_s"], r["first_s"], r["ready_s"], r["n"], r["failed"])')
      say "run $1 rep $r: p50 p95 first ready n failed = $v"
      vals+=("${v%% *}")
    else
      say "run $1 rep $r: FAILED"; tail -5 "$d.log" | sed 's/^/A024   /' >&2
    fi
  done
  [ ${#vals[@]} -gt 0 ] || { echo none; return; }
  printf '%s\n' "${vals[@]}" | sort -n | awk '{a[NR]=$1} END{print (NR%2 ? a[(NR+1)/2] : (a[NR/2]+a[NR/2+1])/2)}'
}

say "cpu: $(sed -n 's/^model name[[:space:]]*: //p' /proc/cpuinfo | head -1), $(nproc) vcpus, cpuset $core, $(grep -o -w -E 'amx_bf16|avx512f' /proc/cpuinfo | sort -u | paste -sd, - || true)"
for n in v0.19.1 v0.20.0; do
  img=$v0191; [ $n = v0.20.0 ] && img=$v0200
  docker pull -q "$img" > /dev/null
  matmul $n "$img"
  if [ -z "${SKIP_STUDY:-}" ]; then
    p=$(study $n "$img"); say "$n p50 $p"; eval "p_${n//./_}=$p"
  fi
  docker rmi -f "$img" > /dev/null 2>&1 || true
done
if [ -z "${SKIP_STUDY:-}" ]; then
  # A-023 threshold: midpoint of v0.19.1 and v0.20.2 (4.755 s). v0.20.0 slow means the step is in v0.20.0.
  if [ "$p_v0_20_0" != none ] && awk -v p="$p_v0_20_0" 'BEGIN{exit !(p < 4.755)}'; then
    say "v0.20.0 is fast: measuring v0.20.1"
    p=$(study v0.20.1 "$v0201"); say "v0.20.1 p50 $p"
    docker rmi -f "$v0201" > /dev/null 2>&1 || true
  fi
fi
say "DONE"
