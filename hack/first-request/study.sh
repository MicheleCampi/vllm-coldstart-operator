#!/usr/bin/env bash
# First request after /health on a current vLLM CPU release: is it still
# slow, and is the extra time compilation at request time?
# One fresh server per run, offline on a pinned model, then timed requests.
#   cold    the first request is the client's (32 tokens)
#   warm-1  one 1-token completion first, then the client's request
# Each run writes <run>.jsonl (one line per request), <run>.server.log
# (server log with timestamps) and env.json (image, vLLM version, CPU).
# Usage: study.sh IMAGE MODE OUTDIR   MODE: default | eager
# env: MODEL, MODEL_REVISION, HF_CACHE (a host directory holding the model)
set -euo pipefail
image=$1 mode=$2 out=$3
here=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$out"
args=(serve "$MODEL" --revision "$MODEL_REVISION" --tokenizer-revision "$MODEL_REVISION"
  --host 0.0.0.0 --port 8000 --max-model-len 2048 --dtype bfloat16)
[ "$mode" = eager ] && args+=(--enforce-eager)

version=$(docker run --rm --entrypoint python3 "$image" -c 'import vllm; print(vllm.__version__)' 2>/dev/null | tail -1)
python3 - "$image" "$mode" "$version" > "$out/env.json" <<'EOF'
import json, platform, subprocess, sys
cpu = subprocess.run(["lscpu"], capture_output=True, text=True).stdout
info = dict(l.split(":", 1) for l in cpu.splitlines() if ":" in l)
flags = info.get("Flags", "").split()
print(json.dumps({
    "image": sys.argv[1], "mode": sys.argv[2], "vllm": sys.argv[3],
    "cpu_model": info.get("Model name", "").strip(), "cpus": info.get("CPU(s)", "").strip(),
    "avx512": [f for f in flags if f.startswith("avx512")], "amx": [f for f in flags if f.startswith("amx")],
    "kernel": platform.release()}))
EOF
echo "env: $(cat "$out/env.json")"

for run in cold warm-1; do
  docker rm -f vllm > /dev/null 2>&1 || true
  docker run -d --name vllm --shm-size 2g -p 8000:8000 \
    -e VLLM_CPU_KVCACHE_SPACE=2 -e HF_HUB_OFFLINE=1 \
    -v "$HF_CACHE:/root/.cache/huggingface" \
    --entrypoint vllm "$image" "${args[@]}" > /dev/null
  t0=$SECONDS
  until curl -sf http://127.0.0.1:8000/health > /dev/null; do
    if [ $((SECONDS - t0)) -gt 900 ] || [ "$(docker inspect -f '{{.State.Running}}' vllm)" != true ]; then
      docker logs vllm 2>&1 | tail -100
      echo "server not healthy after $((SECONDS - t0)) s"
      exit 1
    fi
    echo "$run waiting for /health ($((SECONDS - t0)) s)"
    sleep 5
  done
  ready=$((SECONDS - t0))
  ready_at=$(date -u +%Y-%m-%dT%H:%M:%S.%NZ)
  case $run in
    cold) reqs=("client:32:Kubernetes is" "again:32:Kubernetes is" "other-prompt:32:Explain in one sentence what a Kubernetes operator does for a team") ;;
    warm-1) reqs=("warmup:1:Hi" "client:32:Kubernetes is" "other-prompt:32:Explain in one sentence what a Kubernetes operator does for a team") ;;
  esac
  python3 "$here/measure.py" http://127.0.0.1:8000 "$MODEL" "${reqs[@]}" \
    | sed "s/^{/{\"run\":\"$run\",\"ready_s\":$ready,\"ready_at\":\"$ready_at\",/" | tee "$out/$run.jsonl"
  docker logs -t vllm > "$out/$run.server.log" 2>&1
done
docker rm -f vllm > /dev/null
