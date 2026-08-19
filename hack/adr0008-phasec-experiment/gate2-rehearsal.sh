#!/usr/bin/env bash
# ADR-0008 phase C, gate 2: the whole harness on kind, zero GPU minutes.
#
# The level-3 lesson is the reason this exists: exercise every flag the GPU
# session will use, including the ones only the GPU path reaches. What that
# session cannot afford to discover is a decision that never fires because a
# horizon was shorter than a drain, or a dispatcher that cannot resolve what
# the operator published.
#
# Four scenarios, each with explicit asserts and a PASS/FAIL verdict:
#   A) D2 horizon: a signal inside it decides; the same value outside it
#      ranks as never observed and the fallback decides instead.
#   B) D4 order: at equal warmth, tokensPerJoule outranks kvCacheHitRate.
#   C) D1 record: the placement publishes what the planner ranked on, and
#      an expired signal is absent from that record.
#   D) D1 dispatch: dispatch.py resolves the placement to the child's
#      Service, and refuses with its own exit code before Ready.
#
# Operator runs IN-cluster via the chart, so the RBAC that D3 extended with
# nodes get/list is exercised for real rather than assumed.
set -euo pipefail

cd "$(dirname "$0")/../.."
CLUSTER="${CLUSTER:-adr0008c}"
CTX="kind-${CLUSTER}"
IMG="vllm-coldstart-operator:adr0008c"
TS="$(date -u +%Y%m%dT%H%M%S)"
RUN_DIR="hack/adr0008-phasec-experiment/runs/${TS}-gate2"
mkdir -p "$RUN_DIR"
PASS=0; FAIL=0

ok()   { echo "  PASS: $*"; PASS=$((PASS+1)); }
bad()  { echo "  FAIL: $*"; FAIL=$((FAIL+1)); }
check(){ # check <description> <expected> <actual>
  if [ "$2" = "$3" ]; then ok "$1 ($3)"; else bad "$1: expected '$2', got '$3'"; fi
}

echo "==> cluster ${CLUSTER}"
kind get clusters 2>/dev/null | grep -qx "$CLUSTER" || kind create cluster --name "$CLUSTER" --wait 120s
kubectl config use-context "$CTX" >/dev/null

echo "==> build and load the operator image"
docker build -q -t "$IMG" . >/dev/null
kind load docker-image "$IMG" --name "$CLUSTER" >/dev/null

# Helm installs anything under crds/ once and never updates it on upgrade —
# documented behaviour, and the reason the first run of this rehearsal failed
# against a schema three fields out of date. Applying the generated CRDs here
# makes the run reproducible on a cluster that already exists.
echo "==> apply CRDs from the generator (Helm will not update them)"
cargo run --quiet --bin crdgen 2>/dev/null > "${RUN_DIR}/crd.yaml"
kubectl --context "$CTX" apply -f "${RUN_DIR}/crd.yaml" >/dev/null
for c in fleetservices nodestates vllmservices; do
  kubectl --context "$CTX" wait --for=condition=Established \
    "crd/${c}.inference.michelecampi.dev" --timeout=30s >/dev/null
done

echo "==> install the chart (exercises RBAC, including D3's nodes get/list)"
helm upgrade --install op chart/ --kube-context "$CTX" \
  --set image.repository="${IMG%%:*}" --set image.tag="${IMG##*:}" \
  --set image.pullPolicy=Never --wait --timeout 3m >/dev/null

NODE="$(kubectl --context "$CTX" get nodes -o jsonpath='{.items[0].metadata.name}')"
echo "==> node under test: $NODE"

# Helper: seed a NodeState with signals and an observedAt offset in seconds.
seed_node() {  # seed_node <node> <tpj> <hit> <age_secs>
  local n="$1" tpj="$2" hit="$3" age="$4"
  local at; at="$(date -u -d "${age} seconds ago" +%Y-%m-%dT%H:%M:%SZ)"
  kubectl --context "$CTX" get nodestate "$n" >/dev/null 2>&1 || \
    kubectl --context "$CTX" create -f - >/dev/null <<NS
apiVersion: inference.michelecampi.dev/v1alpha1
kind: NodeState
metadata: {name: $n}
spec: {}
NS
  kubectl --context "$CTX" patch nodestate "$n" --subresource=status --type=merge \
    -p "{\"status\":{\"warmth\":\"Warm\",\"gpuUtilization\":50.0,\"activeServiceCount\":1,
         \"tokensPerJoule\":${tpj},\"tokensPerJouleObservedAt\":\"${at}\",
         \"kvCacheHitRate\":${hit},\"kvCacheHitRateObservedAt\":\"${at}\"}}" >/dev/null
}

apply_fleet() {  # apply_fleet <name> <strategy> <horizon|"">
  local name="$1" strat="$2" horizon="$3"
  local hz=""
  [ -n "$horizon" ] && hz="    signalMaxAgeSeconds: ${horizon}"
  kubectl --context "$CTX" apply -f - >/dev/null <<FLEET
apiVersion: inference.michelecampi.dev/v1alpha1
kind: FleetService
metadata: {name: ${name}, namespace: default}
spec:
  model: facebook/opt-125m
  replicas: 1
  template: {image: registry.k8s.io/pause:3.10, gpu: 0, healthPath: ""}
  placement:
    strategy: ${strat}
${hz}
FLEET
}

wait_placed() {  # wait_placed <fleet> -> prints nodeRef or empty
  local f="$1" n=""
  for _ in $(seq 1 30); do
    n="$(kubectl --context "$CTX" get fleetservice "$f" \
         -o jsonpath='{.status.placements[0].nodeRef}' 2>/dev/null || true)"
    [ -n "$n" ] && break
    sleep 2
  done
  echo "$n"
}

echo
echo "=== A) D2 horizon decides whether a signal counts ==="
# One node with an excellent but old reading, one with a poor fresh one. The
# horizon is the only thing that changes between the two fleets.
seed_node "$NODE" 90.0 0.9 7200
apply_fleet "gate2-a-nohorizon" "EfficiencyAware" ""
apply_fleet "gate2-a-horizon"   "EfficiencyAware" "600"
sleep 8
A_NO="$(kubectl --context "$CTX" get fleetservice gate2-a-nohorizon \
        -o jsonpath='{.status.placements[0].decidedOn.tokensPerJoule}' 2>/dev/null || true)"
A_HZ="$(kubectl --context "$CTX" get fleetservice gate2-a-horizon \
        -o jsonpath='{.status.placements[0].decidedOn.tokensPerJoule}' 2>/dev/null || true)"
check "no horizon: the 2h-old signal still counts" "90" "$A_NO"
check "600s horizon: the same signal is absent from the record" "" "$A_HZ"

echo
echo "=== B) D4: energy outranks cache at equal warmth ==="
# Fresh signals, deliberately opposed: the node has a poor hit-rate and a good
# tokensPerJoule. Under ADR-0007 D3's order the cache would have decided; under
# D4 the energy does. With one node the winner is fixed, so what is asserted is
# what the record says the planner ranked on — the ordering itself is covered by
# the unit and property tests, which can build fleets this rehearsal cannot.
seed_node "$NODE" 42.0 0.1 5
apply_fleet "gate2-b" "EfficiencyAware" "600"
sleep 8
B_TPJ="$(kubectl --context "$CTX" get fleetservice gate2-b \
         -o jsonpath='{.status.placements[0].decidedOn.tokensPerJoule}' 2>/dev/null || true)"
B_HIT="$(kubectl --context "$CTX" get fleetservice gate2-b \
         -o jsonpath='{.status.placements[0].decidedOn.kvCacheHitRate}' 2>/dev/null || true)"
check "both efficiency signals reach the record: tokensPerJoule" "42" "$B_TPJ"
# kvCacheHitRate is an f32: 0.1 round-trips through JSON as
# 0.10000000149011612, which is the correct value and not the string I first
# asserted. Compare numerically, with a tolerance, rather than as text.
if python3 -c "import sys; sys.exit(0 if abs(float('${B_HIT:-nan}') - 0.1) < 1e-6 else 1)" 2>/dev/null; then
  ok "kvCacheHitRate reaches the record (${B_HIT})"
else
  bad "kvCacheHitRate: expected ~0.1, got '${B_HIT}'"
fi

echo
echo "=== C) D1: the record names the strategy and the fresh inputs ==="
C_STRAT="$(kubectl --context "$CTX" get fleetservice gate2-b \
           -o jsonpath='{.status.placements[0].decidedOn.strategy}' 2>/dev/null || true)"
C_UTIL="$(kubectl --context "$CTX" get fleetservice gate2-b \
          -o jsonpath='{.status.placements[0].decidedOn.gpuUtilization}' 2>/dev/null || true)"
check "strategy recorded" "EfficiencyAware" "$C_STRAT"
check "NVML signal recorded (no horizon applies to it)" "50" "$C_UTIL"

echo
echo "=== D) D1: the dispatcher resolves what the operator published ==="
NODE_B="$(wait_placed gate2-b)"
[ -n "$NODE_B" ] && ok "placement published on '$NODE_B'" || bad "no placement published"
# Before Ready the dispatcher must refuse with its own exit code, so a caller
# can tell "not yet" from "the cluster is unreachable".
# Asserting the refusal on gate2-b was unsound: by the time the dispatcher ran
# the placement was already Ready, so the check passed without exercising
# anything. A fleet created for this purpose is interrogated immediately, while
# it still has no placement at all.
apply_fleet "gate2-notready" "WarmthFirst" ""
set +e
python3 hack/adr0008-phasec-experiment/dispatch.py --fleet gate2-notready --slot 0 >/dev/null 2>&1
RC_EARLY=$?
set -e
check "dispatch refuses before a placement is Ready" "2" "$RC_EARLY"
for _ in $(seq 1 30); do
  PH="$(kubectl --context "$CTX" get fleetservice gate2-b \
        -o jsonpath='{.status.placements[0].phase}' 2>/dev/null || true)"
  [ "$PH" = "Ready" ] && break
  sleep 2
done
set +e
URL="$(python3 hack/adr0008-phasec-experiment/dispatch.py --fleet gate2-b --slot 0 --url-only 2>/dev/null)"
RC_READY=$?
set -e
check "dispatch succeeds once Ready" "0" "$RC_READY"
check "resolved URL points at the placed child" "http://gate2-b-0.default.svc.cluster.local:8000" "$URL"

echo
echo "==> evidence"
for f in gate2-a-nohorizon gate2-a-horizon gate2-b; do
  kubectl --context "$CTX" get fleetservice "$f" -o json > "${RUN_DIR}/${f}.json" 2>/dev/null || true
done
kubectl --context "$CTX" get nodestate "$NODE" -o json > "${RUN_DIR}/nodestate.json" 2>/dev/null || true
kubectl --context "$CTX" logs deploy/op-vllm-coldstart-operator --tail=200 \
  > "${RUN_DIR}/operator.log" 2>/dev/null || true
echo "  written to ${RUN_DIR}"

echo
echo "===================================="
echo "  PASS: $PASS   FAIL: $FAIL"
if [ "$FAIL" -eq 0 ]; then
  echo "  GATE 2: PASS"
  echo "===================================="
  exit 0
fi
echo "  GATE 2: FAIL"
echo "===================================="
exit 1
