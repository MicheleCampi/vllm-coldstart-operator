#!/usr/bin/env bash
# ADR-0009 D5: bring a kind cluster from nothing to the frozen starting state
# of one rep. Setup only — it starts no rep and generates no workload, so it
# is safe to re-run and it is the same code path for all eight reps.
#
# Reps must not differ from each other by anything the author did by hand
# between them, which is what this script exists to prevent.
#
# End state, asserted before exit:
#   - kind cluster (1 control-plane + 3 workers)
#   - operator installed by helm, reporter disabled (this experiment feeds
#     NodeState by hand; a live reporter would overwrite the seeded warmth)
#   - three NodeStates, Warm at utilisation 0, deliberately symmetric: the
#     placement comparator is not what D5 measures, so no node is given a
#     reason to win
#   - the simulator image built and loaded
#   - FleetService at replicas=1 and Ready, which is where a rep begins
#
# It does NOT start `kubectl proxy`. The proxy is per-run and dies with the
# cluster; the runbook starts it after this script and before the dispatcher.
set -euo pipefail
cd "$(dirname "$0")/../.."

CLUSTER="${CLUSTER:-adr9d5}"
CTX="kind-${CLUSTER}"
OP_IMG="vllm-coldstart-operator:${CLUSTER}"
SIM_IMG="${SIM_IMG:-llmd-sim-d5:warm60}"
# Dockerfile.d5 defaults WARMUP_DELAY_S to 0, so this must be passed
# explicitly: a build without it yields a zero-warm-up image wearing a
# warm60 tag, which would silently remove the very delay the experiment
# measures and return an effect of about zero.
WARMUP_DELAY_S="${WARMUP_DELAY_S:-60}"
RELEASE="${RELEASE:-d5}"
FLEET="${FLEET:-d5probe}"
MODEL="${MODEL:-facebook/opt-125m}"

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
docker build -t "$OP_IMG" .
kind load docker-image "$OP_IMG" --name "$CLUSTER"

echo "==> build + load simulator image (${SIM_IMG})"
docker build -f hack/rehearsal/Dockerfile.d5 \
  --build-arg "WARMUP_DELAY_S=${WARMUP_DELAY_S}" -t "$SIM_IMG" hack/rehearsal
kind load docker-image "$SIM_IMG" --name "$CLUSTER"

echo "==> install operator (reporter disabled)"
helm upgrade --install "$RELEASE" chart/ \
  --kube-context "$CTX" \
  --set image.repository=vllm-coldstart-operator \
  --set image.tag="$CLUSTER" \
  --set image.pullPolicy=Never \
  --set reporter.enabled=false \
  --wait --timeout 3m

echo "==> seed NodeStates (symmetric: Warm, utilisation 0)"
for n in "${CLUSTER}-worker" "${CLUSTER}-worker2" "${CLUSTER}-worker3"; do
  cat << NS | kubectl --context "$CTX" apply -f -
apiVersion: inference.michelecampi.dev/v1alpha1
kind: NodeState
metadata:
  name: ${n}
  namespace: default
spec:
  reportIntervalSeconds: 15
NS
  kubectl --context "$CTX" patch nodestate "$n" --subresource=status --type=merge \
    -p '{"status":{"warmth":"Warm","gpuUtilization":0.0,"activeServiceCount":0,"spot":{"preemptionNoticeDetected":false}}}'
done

echo "==> FleetService ${FLEET} at replicas=1"
cat << FLEETCR | kubectl --context "$CTX" apply -f -
apiVersion: inference.michelecampi.dev/v1alpha1
kind: FleetService
metadata:
  name: ${FLEET}
  namespace: default
spec:
  replicas: 1
  model: ${MODEL}
  placement:
    strategy: WarmthFirst
  hysteresis:
    stableReconcilesRequired: 3
    maxConcurrentReschedules: 1
  nodePool:
    selector: {}
    spotPolicy:
      enabled: false
      maxSpotFraction: 0
  template:
    image: ${SIM_IMG}
    gpu: 0
    healthPath: /health
    extraArgs: []
FLEETCR

echo "==> waiting for the fleet to report one Ready placement"
# The child carries a synthetic warm-up, so this wait is bounded by that and
# not by kind. Measured at 62s on 2026-08-01; 5m leaves room without hiding a
# hang.
deadline=$(( $(date +%s) + 300 ))
while :; do
  ready=$(kubectl --context "$CTX" get fleetservice "$FLEET" \
    -o jsonpath='{.status.readyReplicas}' 2>/dev/null || echo "")
  [ "${ready:-0}" = "1" ] && break
  if [ "$(date +%s)" -ge "$deadline" ]; then
    echo "FAIL: fleet did not reach readyReplicas=1 within 300s" >&2
    kubectl --context "$CTX" get fleetservice "$FLEET" -o yaml >&2
    exit 1
  fi
  sleep 5
done

echo "==> asserting the starting state"
rc=0
assert() {
  if [ "$2" = "$3" ]; then echo "PASS: $1 ($3)"; else echo "FAIL: $1 (want $3, got $2)"; rc=1; fi
}
st=$(kubectl --context "$CTX" get fleetservice "$FLEET" -o json)
assert "spec.replicas" "$(echo "$st" | python3 -c 'import json,sys;print(json.load(sys.stdin)["spec"]["replicas"])')" "1"
assert "status.replicas" "$(echo "$st" | python3 -c 'import json,sys;print(json.load(sys.stdin)["status"]["replicas"])')" "1"
assert "status.readyReplicas" "$(echo "$st" | python3 -c 'import json,sys;print(json.load(sys.stdin)["status"]["readyReplicas"])')" "1"
assert "status.warmingReplicas" "$(echo "$st" | python3 -c 'import json,sys;print(json.load(sys.stdin)["status"]["warmingReplicas"])')" "0"
assert "NodeState count" "$(kubectl --context "$CTX" get nodestate --no-headers | wc -l)" "3"
warm=$(kubectl --context "$CTX" get nodestate -o jsonpath='{.items[*].status.warmth}')
assert "NodeState warmth" "$warm" "Warm Warm Warm"

if [ "$rc" -ne 0 ]; then
  echo "setup FAILED — do not run a rep against this cluster" >&2
  exit 1
fi
echo
echo "cluster ${CLUSTER} ready. Next: start the proxy, then run a rep."
echo "  kubectl --context ${CTX} proxy --port=8001 &"
