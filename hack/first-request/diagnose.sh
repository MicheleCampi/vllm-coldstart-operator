#!/usr/bin/env bash
# Where does the first request to a fresh vLLM CPU replica spend 17 to 19 s?
# (EKS game day #1, 2026-11-20, finding 4; kind experiment of 2026-11-19.)
# Three fresh servers, same image, model and args as the operator gives
# tests/gameday/model.yaml, each Ready on /health, then timed requests:
#   cold     no warmup: what a client gets today
#   warm-1   one 1-token completion first, then the client's request
#   warm-32  one 32-token completion first, then the client's request
# The HF cache volume is shared, so only the first server downloads.
# Usage: diagnose.sh [RUN...]  (default: cold warm-1 warm-32)
# With HF_CACHE (a host directory already holding the model, see
# fetch-model.sh) and MODEL_REVISION, the server runs offline at that
# revision: no HuggingFace call at runtime. Without them it downloads into
# a Docker volume, as before.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
if [ -n "${HF_CACHE:-}" ]; then
  cache=(-v "$HF_CACHE:/root/.cache/huggingface" -e HF_HUB_OFFLINE=1)
  rev=(--revision "$MODEL_REVISION" --tokenizer-revision "$MODEL_REVISION")
else
  docker volume create hfcache > /dev/null
  cache=(-v hfcache:/root/.cache/huggingface)
  rev=()
fi

serve() {
  docker rm -f vllm > /dev/null 2>&1 || true
  docker run -d --name vllm --cpus 2 --memory 6g --shm-size 1g \
    -e VLLM_CPU_KVCACHE_SPACE=1 -p 8000:8000 "${cache[@]}" \
    --entrypoint vllm "$VLLM_CPU_IMAGE" serve "$MODEL" "${rev[@]}" --host 0.0.0.0 --port 8000 \
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
  docker logs vllm 2>&1 | tail -200
  return 1
}

m() { python3 "$here/measure.py" http://127.0.0.1:8000 "$MODEL" "$@" | sed "s/^{/{\"run\":\"$run\",/"; }

cold()    { m "client:32:Kubernetes is" "again:32:Kubernetes is" \
  "other-prompt:32:Explain in one sentence what a Kubernetes operator does for a team" "one-token:1:Hi"; }
warm-1()  { m "warmup:1:Hi" "client:32:Kubernetes is" \
  "other-prompt:32:Explain in one sentence what a Kubernetes operator does for a team"; }
warm-32() { m "warmup:32:Hi" "client:32:Kubernetes is" \
  "other-prompt:32:Explain in one sentence what a Kubernetes operator does for a team"; }

for run in "${@:-cold warm-1 warm-32}"; do
  for r in $run; do
    run=$r
    serve "$run"
    "$run"
    docker logs vllm > "$run.log" 2>&1
  done
done
docker rm -f vllm > /dev/null
