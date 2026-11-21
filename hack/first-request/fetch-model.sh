#!/usr/bin/env bash
# Download MODEL at MODEL_REVISION into HF_CACHE (a host directory), with
# the HuggingFace client of the vLLM image itself, so that the servers of
# the first-request job then run offline (HF_HUB_OFFLINE=1). Bounded: each
# attempt has a 10 min limit and the client's own timeouts; one line per
# attempt. With MODEL_REVISION empty it resolves main and prints the commit
# to pin. Usage: fetch-model.sh IMAGE  (env: MODEL, MODEL_REVISION, HF_CACHE)
set -euo pipefail
image=$1
mkdir -p "$HF_CACHE"
py='
import os, sys
from huggingface_hub import HfApi, snapshot_download
m, r = os.environ["MODEL"], os.environ.get("MODEL_REVISION") or None
if r is None:
    r = HfApi().model_info(m).sha
    print(f"MODEL_REVISION not pinned: {m} main is {r}", file=sys.stderr)
p = snapshot_download(m, revision=r, allow_patterns=["*.json", "*.safetensors", "*.txt", "tokenizer*"])
print(r)
print(p, file=sys.stderr)
'
for i in 1 2 3; do
  if rev=$(timeout 600 docker run --rm -v "$HF_CACHE:/root/.cache/huggingface" \
      -e MODEL -e MODEL_REVISION -e HF_HUB_DOWNLOAD_TIMEOUT=30 -e HF_HUB_ETAG_TIMEOUT=15 \
      --entrypoint python3 "$image" -c "$py"); then
    echo "model $MODEL at $rev in $HF_CACHE (attempt $i)"
    echo "MODEL_REVISION=$rev" >> "${GITHUB_ENV:-/dev/null}"
    exit 0
  fi
  echo "model download attempt $i failed, retrying in $((30 * i)) s"
  sleep $((30 * i))
done
exit 1
