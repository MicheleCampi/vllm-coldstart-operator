#!/usr/bin/env bash
# Study, not a gate (EKS live A-021, 2026-11-22, finding 3): on the same
# m7i-flex.large nodes, pod request 1 CPU, eager mode and the same probe,
# vLLM CPU v0.34.0 answered at steady state in p50 5.86 s against 3.81 s on
# v0.11.2. Turning the OpenMP binding off did not change it. This runs one
# fresh server per job, warms it like operator v0.17.0 (16-token completion),
# then times the game day probe request, so the CPU budget and the mode can
# be told apart from the release.
# Usage: study.sh IMAGE MODE CPUS OUT
#   MODE  eager | default
#   CPUS  thread (1 logical CPU) | core (cpu0 and its SMT siblings: what a
#         2-vCPU m7i-flex.large is) | all (every runner CPU)
# Env: MODEL, MODEL_REVISION, HF_CACHE (host directory holding the model).
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
image=$1 mode=$2 cpus=$3 out=$4
mkdir -p "$out"

case "$cpus" in
  thread) set_=0 ;;
  core)   set_=$(cat /sys/devices/system/cpu/cpu0/topology/thread_siblings_list) ;;
  all)    set_=0-$(($(nproc) - 1)) ;;
  *) echo "unknown CPUS $cpus"; exit 2 ;;
esac
args=(--max-model-len 2048 --dtype bfloat16)
[ "$mode" = eager ] && args+=(--enforce-eager)

cpu=$(sed -n 's/^model name[[:space:]]*: //p' /proc/cpuinfo | head -1)
isa=$(grep -o -w -E 'amx_bf16|avx512f' /proc/cpuinfo | sort -u | paste -sd, - || true)
echo "cpu: $cpu (${isa:-avx2 only}), runner $(nproc) cpus, cpuset $set_ ($cpus), smt siblings of cpu0: $(cat /sys/devices/system/cpu/cpu0/topology/thread_siblings_list)" | tee "$out/cpu.txt"
lscpu -e > "$out/lscpu.txt" || true

docker rm -f vllm > /dev/null 2>&1 || true
docker run -d --name vllm --cpuset-cpus "$set_" --memory 6g --shm-size 1g \
  -e VLLM_CPU_KVCACHE_SPACE=1 -e HF_HUB_OFFLINE=1 -p 8000:8000 \
  -v "$HF_CACHE:/root/.cache/huggingface" \
  --entrypoint vllm "$image" serve "$MODEL" --revision "$MODEL_REVISION" \
  --tokenizer-revision "$MODEL_REVISION" --host 0.0.0.0 --port 8000 "${args[@]}" > /dev/null
t0=$SECONDS
ready=
for i in $(seq 1 540); do
  if curl -sf http://127.0.0.1:8000/health > /dev/null; then ready=$((SECONDS - t0)); break; fi
  docker inspect -f '{{.State.Running}}' vllm | grep -q true || break
  [ $((i % 6)) = 0 ] && echo "waiting for /health ($((SECONDS - t0)) s)"
  sleep 5
done
docker logs vllm > "$out/vllm.log" 2>&1 || true
if [ -z "$ready" ]; then tail -100 "$out/vllm.log"; exit 1; fi
echo "{\"ready_s\":$ready}" > "$out/ready.json"

# The warmup of operator v0.17.0, then the probe request of tests/gameday/probe.yaml.
specs=("warmup:16:Write one sentence about the sea.")
for i in $(seq 1 40); do specs+=("probe:32:Kubernetes is"); done
python3 "$here/../first-request/measure.py" http://127.0.0.1:8000 "$MODEL" "${specs[@]}" > "$out/requests.jsonl"
docker logs vllm > "$out/vllm.log" 2>&1 || true
docker rm -f vllm > /dev/null
python3 "$here/summarize.py" "$out"
