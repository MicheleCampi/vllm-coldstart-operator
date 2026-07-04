#!/usr/bin/env bash
# Pre-pull the vLLM image and pre-download the model on all 3 nodes, in
# parallel. Removes CDN bandwidth from the measured recovery path
# (modelCacheHostPath mounts HF_CACHE_DIR into the serving pods).
set -euo pipefail
: "$VLLM_IMAGE" "$MODEL" "$HF_CACHE_DIR"

prep() { # $1 = ssh fn name, $2 = label
  # huggingface_hub 1.x: the CLI entrypoint is `hf` (ships with the base
  # package; the old [cli] extra and huggingface-cli module are gone).
  # Verified against optim-dev install + docs, 2026-07-04.
  "$1" "sudo ctr -n k8s.io image pull 'docker.io/$VLLM_IMAGE' \
    && sudo mkdir -p '$HF_CACHE_DIR' \
    && sudo python3 -m pip install -q --break-system-packages huggingface_hub \
    && sudo -- sh -c 'command -v hf' \
    && sudo HF_HOME='$HF_CACHE_DIR' hf download '$MODEL' \
    && echo '=== $2 DONE ==='" &
}
prep ssh_a A; prep ssh_b B; prep ssh_c C
wait
echo "prepull complete on A/B/C. Next: 04-deploy.sh"
