#!/usr/bin/env bash
# One load run from inside the cluster (PLAN.md, "Where the load comes from").
# Usage: run-load.sh NAME KEEP SEED MODEL OUTDIR [TARGET_URL]
# Runs hack/loadgen/loadgen.py as a Job on the system pool against the
# Service the operator applies, with aiohttp pinned as in CI, and writes
# the per-request JSONL to OUTDIR/NAME.jsonl. Needs KUBECONFIG.
set -euo pipefail
[ $# -ge 5 ] || { echo "usage: $0 NAME KEEP SEED MODEL OUTDIR [TARGET_URL]" >&2; exit 2; }
NAME=$1 KEEP=$2 SEED=$3 MODEL=$4 OUTDIR=$5 TARGET=${6:-http://qwen-7b:8000}
NS=${NS:-vllm-system}
IMAGE=python:3.12-slim@sha256:a0f57064cbdcd6285fa45f51237ab0b35ea8ebaed06a1f5973e89e19dfb86704
POOL=${POOL:-system-pool}
ROOT=$(cd "$(dirname "$0")/../../.." && pwd)
JOB=load-$NAME
mkdir -p "$OUTDIR"
[ ! -e "$OUTDIR/$NAME.jsonl" ] || { echo "$OUTDIR/$NAME.jsonl exists" >&2; exit 1; }
kubectl -n "$NS" create configmap loadgen \
  --from-file=loadgen.py="$ROOT/hack/loadgen/loadgen.py" \
  --from-file=profile.csv="$ROOT/hack/loadgen/profiles/azure-conv-2023-peak.csv" \
  --dry-run=client -o yaml | kubectl apply -f - >/dev/null
kubectl -n "$NS" label configmap loadgen gpu-session-infra=true --overwrite >/dev/null
kubectl -n "$NS" apply -f - >/dev/null <<JOBEOF
apiVersion: batch/v1
kind: Job
metadata:
  name: $JOB
  labels: {gpu-session-infra: "true"}
spec:
  backoffLimit: 0
  activeDeadlineSeconds: 1500
  template:
    metadata:
      labels: {gpu-session-infra: "true", job-name: $JOB}
    spec:
      restartPolicy: Never
      nodeSelector: {cloud.google.com/gke-nodepool: $POOL}
      containers:
      - name: loadgen
        image: $IMAGE
        command: ["sh", "-c"]
        args:
        - >-
          pip install --quiet --no-cache-dir --root-user-action=ignore aiohttp==3.14.1 &&
          python -I /lg/loadgen.py --target svc=$TARGET --model '$MODEL'
          --profile /lg/profile.csv --keep $KEEP --seed $SEED --out /tmp/out.jsonl &&
          echo ===JSONL-BEGIN && cat /tmp/out.jsonl && echo ===JSONL-END
        volumeMounts: [{name: lg, mountPath: /lg}]
      volumes: [{name: lg, configMap: {name: loadgen}}]
JOBEOF
echo "job $JOB started $(date -u +%FT%TZ)"
kubectl -n "$NS" wait --for=condition=complete "job/$JOB" --timeout=1500s || {
  kubectl -n "$NS" logs "job/$JOB" --tail=30 || true
  echo "job $JOB did not complete" >&2; exit 1; }
kubectl -n "$NS" logs "job/$JOB" | sed -n '/^===JSONL-BEGIN$/,/^===JSONL-END$/p' | sed '1d;$d' > "$OUTDIR/$NAME.jsonl"
echo "job $JOB done $(date -u +%FT%TZ): $(wc -l < "$OUTDIR/$NAME.jsonl") lines in $OUTDIR/$NAME.jsonl"
