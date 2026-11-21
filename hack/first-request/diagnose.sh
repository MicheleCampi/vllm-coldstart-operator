#!/usr/bin/env bash
# Where does the first request to a fresh vLLM CPU replica spend 17 to 19 s?
# (EKS game day #1, 2026-11-20, finding 4; kind experiment of 2026-11-19.)
# Three fresh servers, same image, model and args as the operator gives
# tests/gameday/model.yaml, each Ready on /health, then timed requests:
#   cold     no warmup: what a client gets today
#   warm-1   one 1-token completion first, then the client's request
#   warm-32  one 32-token completion first, then the client's request
# The HF cache volume is shared, so only the first server downloads.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
docker volume create hfcache > /dev/null

serve() {
  docker rm -f vllm > /dev/null 2>&1 || true
  docker run -d --name vllm --cpus 2 --memory 6g --shm-size 1g \
    -e VLLM_CPU_KVCACHE_SPACE=1 -p 8000:8000 -v hfcache:/root/.cache/huggingface \
    --entrypoint vllm "$VLLM_CPU_IMAGE" serve "$MODEL" --host 0.0.0.0 --port 8000 \
    --enforce-eager --max-model-len 2048 --dtype bfloat16 > /dev/null
  local t0=$SECONDS
  for i in $(seq 1 120); do
    if curl -sf http://127.0.0.1:8000/health > /dev/null; then
      echo "{\"run\":\"$1\",\"ready_s\":$((SECONDS - t0))}"
      return 0
    fi
    echo "$1 waiting for /health ($((SECONDS - t0)) s)"
    sleep 5
  done
  docker logs --tail 50 vllm
  return 1
}

m() { python3 "$here/measure.py" http://127.0.0.1:8000 "$MODEL" "$@" | sed "s/^{/{\"run\":\"$run\",/"; }

run=cold;    serve $run; m "client:32:Kubernetes is" "again:32:Kubernetes is" \
  "other-prompt:32:Explain in one sentence what a Kubernetes operator does for a team" "one-token:1:Hi"
docker logs vllm > cold.log 2>&1
run=warm-1;  serve $run; m "warmup:1:Hi" "client:32:Kubernetes is" \
  "other-prompt:32:Explain in one sentence what a Kubernetes operator does for a team"
docker logs vllm > warm-1.log 2>&1
run=warm-32; serve $run; m "warmup:32:Hi" "client:32:Kubernetes is" \
  "other-prompt:32:Explain in one sentence what a Kubernetes operator does for a team"
docker logs vllm > warm-32.log 2>&1
docker rm -f vllm > /dev/null
