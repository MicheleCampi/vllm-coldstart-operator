#!/usr/bin/env bash
# ADR-0007 falsification level 2: kind rehearsal with the synthetic
# reporter. Two scenarios, explicit asserts, PASS/FAIL verdict:
#   A) EfficiencyAware discriminates on kvCacheHitRate at equal warmth
#      (initial placement -> best hit-rate node; replacement after
#      preemption -> second-best, same strategy on both paths).
#   B) Fail-open: EA fleet with no efficiency signals behaves warmth-first
#      and reports no errors.
# Self-contained: dedicated cluster (kind-adr0007), operator IN-cluster via
# the chart — this also exercises the realigned RBAC end-to-end for the
# first time. Evidence lands in hack/rehearsal/runs/<ts>-adr0007/.
set -euo pipefail
cd "$(dirname "$0")/../.."

CLUSTER="${CLUSTER:-adr0007}"
CTX="kind-${CLUSTER}"
IMG="vllm-coldstart-operator:adr0007"
TS="$(date -u +%Y%m%dT%H%M%S)"
RUN_DIR="hack/rehearsal/runs/${TS}-adr0007"
mkdir -p "$RUN_DIR"
PASS=0; FAIL=0
verdict() { # verdict <name> <ok:0|1>
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

echo "==> build + load operator image (multi-bin Dockerfile)"
docker build -t "$IMG" .
kind load docker-image "$IMG" --name "$CLUSTER"

echo "==> CRDs + chart install (reporter enabled, synthetic, per-node signals)"
kubectl --context "$CTX" apply --server-side -f deploy/crd.yaml
helm --kube-context "$CTX" install adr7 chart/ \
  --set image.repository=vllm-coldstart-operator \
  --set image.tag=adr0007 \
  --set image.pullPolicy=Never \
  --set reporter.enabled=true \
  --set reporter.synthetic=true \
  --set-string 'reporter.extraEnv[0].name=REPORTER_SYNTHETIC_KV_CACHE_HIT_RATE_NODE_ADR0007_WORKER' \
  --set-string 'reporter.extraEnv[0].value=0.15' \
  --set-string 'reporter.extraEnv[1].name=REPORTER_SYNTHETIC_KV_CACHE_HIT_RATE_NODE_ADR0007_WORKER2' \
  --set-string 'reporter.extraEnv[1].value=0.85' \
  --set-string 'reporter.extraEnv[2].name=REPORTER_SYNTHETIC_KV_CACHE_HIT_RATE_NODE_ADR0007_WORKER3' \
  --set-string 'reporter.extraEnv[2].value=0.50' \
  --set-string 'reporter.extraEnv[3].name=REPORTER_SYNTHETIC_GPU_UTILIZATION' \
  --set-string 'reporter.extraEnv[3].value=0.2'

echo "==> wait: operator + reporter DaemonSet ready"
kubectl --context "$CTX" rollout status deployment/adr7-vllm-coldstart-operator --timeout=120s
kubectl --context "$CTX" rollout status daemonset/adr7-vllm-coldstart-operator-reporter --timeout=120s

echo "==> wait: all three workers report kvCacheHitRate"
for i in $(seq 1 30); do
  N=$(kubectl --context "$CTX" get nodestates -o json 2>/dev/null \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); print(sum(1 for i in d["items"] if (i.get("status") or {}).get("kvCacheHitRate") is not None))' || echo 0)
  [ "$N" -ge 3 ] && break
  sleep 2
done
kubectl --context "$CTX" get nodestates -o yaml > "$RUN_DIR/nodestates-initial.yaml"
ok=0; [ "$N" -ge 3 ] || ok=1; verdict "reporter: 3 workers report kvCacheHitRate" $ok

# Warmth stays outside the reporter by design (phase B): seed it, mirroring
# the item-4 topology. All three workers Warm -> warmth cannot decide,
# only the efficiency signal can.
echo "==> seed warmth: all workers Warm, control-plane Cold"
for n in worker worker2 worker3; do
  kubectl --context "$CTX" patch nodestate "${CLUSTER}-${n}" --subresource=status \
    --type=merge -p '{"status":{"warmth":"Warm","spot":{"preemptionNoticeDetected":false}}}'
done

echo "==> scenario A: EfficiencyAware, replicas=1"
cat << FLEET | kubectl --context "$CTX" apply -f -
apiVersion: inference.michelecampi.dev/v1alpha1
kind: FleetService
metadata:
  name: adr7-ea
  namespace: default
spec:
  model: facebook/opt-125m
  replicas: 1
  placement:
    strategy: EfficiencyAware
  hysteresis:
    maxConcurrentReschedules: 1
  template:
    image: registry.k8s.io/pause:3.10
    gpu: 0
FLEET

echo "==> wait: placement decided"
NODE_A=""
for i in $(seq 1 30); do
  NODE_A=$(kubectl --context "$CTX" get fleetservice adr7-ea -o jsonpath='{.status.placements[0].nodeRef}' 2>/dev/null || true)
  [ -n "$NODE_A" ] && break
  sleep 2
done
echo "scenario A initial placement: '${NODE_A}'"
ok=0; [ "$NODE_A" = "${CLUSTER}-worker2" ] || ok=1; verdict "EA initial placement -> worker2 (hit-rate 0.85)" $ok
kubectl --context "$CTX" get fleetservice adr7-ea -o yaml > "$RUN_DIR/fleet-ea-initial.yaml"

echo "==> scenario A2: preemption on ${NODE_A} -> replacement must follow EA"
kubectl --context "$CTX" patch nodestate "${CLUSTER}-worker2" --subresource=status \
  --type=merge -p '{"status":{"spot":{"preemptionNoticeDetected":true}}}'
NODE_R=""
for i in $(seq 1 45); do
  NODE_R=$(kubectl --context "$CTX" get fleetservice adr7-ea -o jsonpath='{.status.placements[0].nodeRef}' 2>/dev/null || true)
  [ -n "$NODE_R" ] && [ "$NODE_R" != "${CLUSTER}-worker2" ] && break
  sleep 2
done
echo "scenario A2 replacement: '${NODE_R}'"
ok=0; [ "$NODE_R" = "${CLUSTER}-worker3" ] || ok=1; verdict "EA replacement -> worker3 (0.50 > 0.15)" $ok
kubectl --context "$CTX" get fleetservice adr7-ea -o yaml > "$RUN_DIR/fleet-ea-replaced.yaml"

echo "==> scenario B: fail-open — EA fleet, no efficiency signals on a fresh namespace"
# Fresh namespace: NodeStates there have no reporter (DaemonSet patches only
# its own namespace's NodeStates), so the planner sees absent signals and
# must degrade to warmth-first without erroring.
kubectl --context "$CTX" create namespace failopen
for n in worker worker2 worker3; do
  cat << NS | kubectl --context "$CTX" apply -f -
apiVersion: inference.michelecampi.dev/v1alpha1
kind: NodeState
metadata:
  name: ${CLUSTER}-${n}
  namespace: failopen
spec: {}
NS
  kubectl --context "$CTX" patch nodestate "${CLUSTER}-${n}" -n failopen --subresource=status \
    --type=merge -p '{"status":{"warmth":"Warm","gpuUtilization":0.0,"activeServiceCount":0,"spot":{"preemptionNoticeDetected":false}}}'
done
cat << FLEET | kubectl --context "$CTX" apply -f -
apiVersion: inference.michelecampi.dev/v1alpha1
kind: FleetService
metadata:
  name: adr7-failopen
  namespace: failopen
spec:
  model: facebook/opt-125m
  replicas: 1
  placement:
    strategy: EfficiencyAware
  hysteresis:
    maxConcurrentReschedules: 1
  template:
    image: registry.k8s.io/pause:3.10
    gpu: 0
FLEET
NODE_B=""
for i in $(seq 1 30); do
  NODE_B=$(kubectl --context "$CTX" get fleetservice adr7-failopen -n failopen -o jsonpath='{.status.placements[0].nodeRef}' 2>/dev/null || true)
  [ -n "$NODE_B" ] && break
  sleep 2
done
echo "scenario B placement (signals absent): '${NODE_B}'"
ok=0; [ -n "$NODE_B" ] || ok=1; verdict "fail-open: EA with absent signals still places (warmth-first degeneration)" $ok
OPERATOR_POD=$(kubectl --context "$CTX" get pods -l app.kubernetes.io/name=vllm-coldstart-operator -o jsonpath='{.items[0].metadata.name}')
ERRS=$(kubectl --context "$CTX" logs "$OPERATOR_POD" | grep -ci "panic\|reconcile loop error" || true)
ok=0; [ "$ERRS" -eq 0 ] || ok=1; verdict "operator log clean (no panics/reconcile errors)" $ok
kubectl --context "$CTX" get fleetservice adr7-failopen -n failopen -o yaml > "$RUN_DIR/fleet-failopen.yaml"

echo "==> evidence"
kubectl --context "$CTX" get nodestates -A -o yaml > "$RUN_DIR/nodestates-final.yaml"
kubectl --context "$CTX" logs "$OPERATOR_POD" > "$RUN_DIR/operator.log"
for p in $(kubectl --context "$CTX" get pods -l app.kubernetes.io/component=reporter -o jsonpath='{.items[*].metadata.name}'); do
  kubectl --context "$CTX" logs "$p" > "$RUN_DIR/reporter-${p}.log"
done
kubectl --context "$CTX" get events -A --sort-by=.lastTimestamp > "$RUN_DIR/events.txt" || true

echo "==> verdict: ${PASS} pass / ${FAIL} fail (evidence: ${RUN_DIR})"
cat "$RUN_DIR/verdict.txt"
[ "$FAIL" -eq 0 ]
