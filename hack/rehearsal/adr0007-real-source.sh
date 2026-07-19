#!/usr/bin/env bash
# ADR-0007 real-source rehearsal: the reporter's Real source (vLLM-schema
# Prometheus scrape) validated on kind against fake /metrics fixtures with
# advancing counters. Asserts, PASS/FAIL verdict, evidence directory:
#   1) worker  (fixture-a, ratio 0.90): kvCacheHitRate in [0.88, 0.92]
#   2) worker2 (fixture-b, ratio 0.30): kvCacheHitRate in [0.28, 0.32]
#   3) worker  activeServiceCount == 1
#   4) worker3 (unreachable target): kvCacheHitRate ABSENT, reporter alive
#   5) gpuUtilization/gpuMemoryUsedBytes ABSENT on every worker (NVML not
#      wired = key deleted by merge-patch null, not zero)
#   6) reporter logs free of panics
set -euo pipefail
cd "$(dirname "$0")/../.."
CLUSTER="${CLUSTER:-adr0007rs}"
CTX="kind-${CLUSTER}"
IMG="vllm-coldstart-operator:adr0007rs"
TS="$(date -u +%Y%m%dT%H%M%S)"
RUN_DIR="hack/rehearsal/runs/${TS}-adr0007-real-source"
mkdir -p "$RUN_DIR"
PASS=0; FAIL=0
verdict() {
  if [ "$2" -eq 0 ]; then echo "PASS: $1" | tee -a "$RUN_DIR/verdict.txt"; PASS=$((PASS+1));
  else echo "FAIL: $1" | tee -a "$RUN_DIR/verdict.txt"; FAIL=$((FAIL+1)); fi
}

echo "==> kind cluster ${CLUSTER} (1 cp + 3 workers)"
kind delete cluster --name "$CLUSTER" >/dev/null 2>&1 || true
cat << 'KINDCFG' | kind create cluster --name "$CLUSTER" --config -
kind: Cluster
apiVersion: kind.x-k8s.io/v1alpha4
nodes:
- role: control-plane
- role: worker
- role: worker
- role: worker
KINDCFG

echo "==> build + load operator image"
docker build -t "$IMG" .
kind load docker-image "$IMG" --name "$CLUSTER"

echo "==> fixtures: fake vLLM /metrics with advancing counters"
kubectl --context "$CTX" create configmap vllm-fixture-src \
  --from-file=server.py=hack/rehearsal/vllm-metrics-fixture.py
for fx in a:0.90 b:0.30; do
  NAME="fixture-${fx%%:*}"; RATIO="${fx##*:}"
  cat << FIXTURE | kubectl --context "$CTX" apply -f -
apiVersion: apps/v1
kind: Deployment
metadata:
  name: ${NAME}
spec:
  replicas: 1
  selector: { matchLabels: { app: ${NAME} } }
  template:
    metadata: { labels: { app: ${NAME} } }
    spec:
      containers:
      - name: metrics
        image: python:3.12-alpine
        command: ["python3", "/src/server.py"]
        env: [{ name: HIT_RATIO, value: "${RATIO}" }]
        ports: [{ containerPort: 9090 }]
        volumeMounts: [{ name: src, mountPath: /src }]
      volumes:
      - name: src
        configMap: { name: vllm-fixture-src }
---
apiVersion: v1
kind: Service
metadata:
  name: ${NAME}
spec:
  selector: { app: ${NAME} }
  ports: [{ port: 9090, targetPort: 9090 }]
FIXTURE
done
kubectl --context "$CTX" rollout status deployment/fixture-a --timeout=180s
kubectl --context "$CTX" rollout status deployment/fixture-b --timeout=180s

echo "==> CRDs + chart install (reporter enabled, REAL source, per-node targets)"
kubectl --context "$CTX" apply --server-side -f deploy/crd.yaml
helm --kube-context "$CTX" install adr7rs chart/ \
  --set image.repository=vllm-coldstart-operator \
  --set image.tag=adr0007rs \
  --set image.pullPolicy=Never \
  --set reporter.enabled=true \
  --set-string 'reporter.extraEnv[0].name=REPORTER_SCRAPE_TARGETS_NODE_ADR0007RS_WORKER' \
  --set-string 'reporter.extraEnv[0].value=http://fixture-a.default.svc.cluster.local:9090/metrics' \
  --set-string 'reporter.extraEnv[1].name=REPORTER_SCRAPE_TARGETS_NODE_ADR0007RS_WORKER2' \
  --set-string 'reporter.extraEnv[1].value=http://fixture-b.default.svc.cluster.local:9090/metrics' \
  --set-string 'reporter.extraEnv[2].name=REPORTER_SCRAPE_TARGETS_NODE_ADR0007RS_WORKER3' \
  --set-string 'reporter.extraEnv[2].value=http://no-such-service.default.svc.cluster.local:9090/metrics'

kubectl --context "$CTX" rollout status deployment/adr7rs-vllm-coldstart-operator --timeout=120s
kubectl --context "$CTX" rollout status daemonset/adr7rs-vllm-coldstart-operator-reporter --timeout=120s

echo "==> wait: delta-derived rates on worker + worker2 (needs >=2 scrape rounds)"
for i in $(seq 1 45); do
  N=$(kubectl --context "$CTX" get nodestates -o json 2>/dev/null \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); print(sum(1 for i in d["items"] if (i.get("status") or {}).get("kvCacheHitRate") is not None))' || echo 0)
  [ "$N" -ge 2 ] && break
  sleep 2
done
kubectl --context "$CTX" get nodestates -o yaml > "$RUN_DIR/nodestates.yaml"
kubectl --context "$CTX" get nodestates -o json > "$RUN_DIR/nodestates.json"

python3 - "$RUN_DIR/nodestates.json" "$CLUSTER" << 'PYASSERT' > "$RUN_DIR/asserts.txt"
import json, sys
data = json.load(open(sys.argv[1])); cluster = sys.argv[2]
st = {i["metadata"]["name"]: (i.get("status") or {}) for i in data["items"]}
def ck(name, ok): print(("PASS" if ok else "FAIL") + f": {name}")
w1, w2, w3 = (st.get(f"{cluster}-{n}", {}) for n in ("worker", "worker2", "worker3"))
r1, r2 = w1.get("kvCacheHitRate"), w2.get("kvCacheHitRate")
ck("worker kvCacheHitRate ~0.90", r1 is not None and 0.88 <= r1 <= 0.92)
ck("worker2 kvCacheHitRate ~0.30", r2 is not None and 0.28 <= r2 <= 0.32)
ck("worker activeServiceCount == 1", w1.get("activeServiceCount") == 1)
ck("worker3 kvCacheHitRate absent + reporter alive",
   "kvCacheHitRate" not in w3 and w3.get("lastReportTime") not in (None, ""))
ck("gpu keys absent on all workers",
   all("gpuUtilization" not in w and "gpuMemoryUsedBytes" not in w for w in (w1, w2, w3)))
PYASSERT
cat "$RUN_DIR/asserts.txt"
while read -r line; do
  ok=0; [ "${line%%:*}" = "PASS" ] || ok=1; verdict "${line#*: }" $ok
done < "$RUN_DIR/asserts.txt"

echo "==> reporter logs"
PANICS=0
for p in $(kubectl --context "$CTX" get pods -l app.kubernetes.io/component=reporter -o jsonpath='{.items[*].metadata.name}'); do
  kubectl --context "$CTX" logs "$p" > "$RUN_DIR/reporter-${p}.log"
  C=$(grep -ci "panicked" "$RUN_DIR/reporter-${p}.log" || true)
  PANICS=$((PANICS + C))
done
ok=0; [ "$PANICS" -eq 0 ] || ok=1; verdict "reporter logs free of panics" $ok

echo "==> verdict: ${PASS} pass / ${FAIL} fail (evidence: ${RUN_DIR})"
cat "$RUN_DIR/verdict.txt"
[ "$FAIL" -eq 0 ]
