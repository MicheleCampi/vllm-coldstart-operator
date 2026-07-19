#!/usr/bin/env bash
# ADR-0007 level-3 experiment orchestrator: EA vs WarmthFirst, DESIGN.md.
# Two modes:
#   --rehearsal : kind + metrics fixtures, Real reporter, no inferscope.
#                 Validates the harness mechanics at zero cost. Verdict is
#                 on mechanics only (all reps complete, evidence complete,
#                 placements recorded on both decision paths, no operator
#                 errors) — the numbers are fake by construction.
#   (default)   : GPU run on the k3s Lambda fleet. Assumes cluster is up,
#                 vLLM per node running, inferscope attached (session
#                 checklist handles that); this script only executes reps.
set -euo pipefail
cd "$(dirname "$0")"
REHEARSAL=0
[ "${1:-}" = "--rehearsal" ] && REHEARSAL=1
EXP_DIR="$(pwd)"
REPO_ROOT="$(cd ../../.. && pwd)"
TS="$(date -u +%Y%m%dT%H%M%S)"
RUN_DIR="${EXP_DIR}/runs/${TS}$( [ $REHEARSAL -eq 1 ] && echo -rehearsal )"
mkdir -p "$RUN_DIR"
PASS=0; FAIL=0
verdict() {
  if [ "$2" -eq 0 ]; then echo "PASS: $1" | tee -a "$RUN_DIR/verdict.txt"; PASS=$((PASS+1));
  else echo "FAIL: $1" | tee -a "$RUN_DIR/verdict.txt"; FAIL=$((FAIL+1)); fi
}

# ---- design parameters (DESIGN.md; rehearsal shrinks windows only) ----
# Sequencing (rehearsal finding, run 20260719T184055): the fleet must be
# applied DURING the loaded phase, not before it — an apply at t=0 makes
# decision point (a) fire with reporters still warming up (absent
# signals -> fail-open warmth-first degeneration: rep-1-EA placed like
# WF). Replay starts first; the fleet is applied after PRELOAD_S of load.
if [ $REHEARSAL -eq 1 ]; then
  PRELOAD_S=25; WINDOW_S=30; SEQUENCE=(EA WF WF EA)   # ABBA only, mechanics
  CTX="kind-adr0007exp"; NS=default
else
  PRELOAD_S=120; WINDOW_S=300; SEQUENCE=(EA WF WF EA WF EA EA WF)  # ABBA+BAAB
  CTX="${KUBE_CONTEXT:?set KUBE_CONTEXT for the GPU run}"; NS=default
fi

strategy_of() { [ "$1" = "EA" ] && echo EfficiencyAware || echo WarmthFirst; }

# ---- rehearsal-only: bring up kind + fixtures + operator (Real) ----
if [ $REHEARSAL -eq 1 ]; then
  CLUSTER=adr0007exp
  IMG="vllm-coldstart-operator:adr0007exp"
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
  ( cd "$REPO_ROOT" && docker build -t "$IMG" . && kind load docker-image "$IMG" --name "$CLUSTER" )
  kubectl --context "$CTX" create configmap vllm-fixture-src \
    --from-file=server.py="$REPO_ROOT/hack/rehearsal/vllm-metrics-fixture.py"
  for fx in a:0.90 b:0.30 c:0.55; do
    NAME="fixture-${fx%%:*}"; RATIO="${fx##*:}"
    cat << FIXTURE | kubectl --context "$CTX" apply -f -
apiVersion: apps/v1
kind: Deployment
metadata: { name: ${NAME} }
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
      volumes: [{ name: src, configMap: { name: vllm-fixture-src } }]
---
apiVersion: v1
kind: Service
metadata: { name: ${NAME} }
spec:
  selector: { app: ${NAME} }
  ports: [{ port: 9090, targetPort: 9090 }]
FIXTURE
  done
  for d in fixture-a fixture-b fixture-c; do
    kubectl --context "$CTX" rollout status "deployment/$d" --timeout=180s
  done
  kubectl --context "$CTX" apply --server-side -f "$REPO_ROOT/deploy/crd.yaml"
  helm --kube-context "$CTX" install exp "$REPO_ROOT/chart/" \
    --set image.repository=vllm-coldstart-operator \
    --set image.tag=adr0007exp \
    --set image.pullPolicy=Never \
    --set reporter.enabled=true \
    --set-string 'reporter.extraEnv[0].name=REPORTER_SCRAPE_TARGETS_NODE_ADR0007EXP_WORKER' \
    --set-string 'reporter.extraEnv[0].value=http://fixture-a.default.svc.cluster.local:9090/metrics' \
    --set-string 'reporter.extraEnv[1].name=REPORTER_SCRAPE_TARGETS_NODE_ADR0007EXP_WORKER2' \
    --set-string 'reporter.extraEnv[1].value=http://fixture-b.default.svc.cluster.local:9090/metrics' \
    --set-string 'reporter.extraEnv[2].name=REPORTER_SCRAPE_TARGETS_NODE_ADR0007EXP_WORKER3' \
    --set-string 'reporter.extraEnv[2].value=http://fixture-c.default.svc.cluster.local:9090/metrics'
  kubectl --context "$CTX" rollout status deployment/exp-vllm-coldstart-operator --timeout=120s
  kubectl --context "$CTX" rollout status daemonset/exp-vllm-coldstart-operator-reporter --timeout=120s
  REPLAY_URL="http://127.0.0.1:19090/metrics"   # port-forward per rep, below
  NODE_PREFIX="${CLUSTER}"
fi

# ---- rep loop ----
REP=0
for S in "${SEQUENCE[@]}"; do
  REP=$((REP+1))
  STRAT="$(strategy_of "$S")"
  REP_DIR="$RUN_DIR/rep-${REP}-${S}"
  mkdir -p "$REP_DIR"
  echo "==> rep ${REP}/${#SEQUENCE[@]}: ${STRAT}"

  # warmth: all workers Warm so only the efficiency signal can decide
  for n in worker worker2 worker3; do
    kubectl --context "$CTX" patch nodestate "${NODE_PREFIX}-${n}" --subresource=status \
      --type=merge -p '{"status":{"warmth":"Warm","spot":{"preemptionNoticeDetected":false}}}'
  done

  # workload for this rep covers preload + measurement (hash in evidence)
  python3 gen_workload.py --rep "$REP" --duration-s "$((PRELOAD_S + WINDOW_S))" \
    --out "$REP_DIR/workload.jsonl" | tee "$REP_DIR/workload.meta"

  # replay starts BEFORE the fleet exists: load first, decision under load
  kubectl --context "$CTX" get nodestates -n "$NS" -o json > "$REP_DIR/nodestates-before.json"
  if [ $REHEARSAL -eq 1 ]; then
    kubectl --context "$CTX" port-forward "deployment/fixture-a" 19090:9090 >/dev/null 2>&1 &
    PF=$!; sleep 1
    python3 replay.py --workload "$REP_DIR/workload.jsonl" --url "$REPLAY_URL" \
      --endpoint-mode fixture --out "$REP_DIR/replay-results.jsonl" \
      2> "$REP_DIR/replay.summary" &
    RP=$!
  else
    python3 replay.py --workload "$REP_DIR/workload.jsonl" \
      --url "${VLLM_URL:?set VLLM_URL for the GPU run}" \
      --endpoint-mode vllm --out "$REP_DIR/replay-results.jsonl" \
      2> "$REP_DIR/replay.summary" &
    RP=$!
  fi
  echo "    pre-load ${PRELOAD_S}s (fleet applied under load)"
  sleep "$PRELOAD_S"

  # fleet under test — applied mid-load
  cat << FLEET | kubectl --context "$CTX" apply -f -
apiVersion: inference.michelecampi.dev/v1alpha1
kind: FleetService
metadata: { name: exp-fleet, namespace: ${NS} }
spec:
  model: Qwen/Qwen2.5-7B-Instruct
  replicas: 1
  placement: { strategy: ${STRAT} }
  hysteresis: { maxConcurrentReschedules: 1 }
  template:
    image: registry.k8s.io/pause:3.10
    gpu: 0
FLEET

  # decision point (a): initial placement
  P_INIT=""
  for i in $(seq 1 30); do
    P_INIT=$(kubectl --context "$CTX" get fleetservice exp-fleet -n "$NS" \
      -o jsonpath='{.status.placements[0].nodeRef}' 2>/dev/null || true)
    [ -n "$P_INIT" ] && break; sleep 2
  done
  echo "$P_INIT" > "$REP_DIR/placement-initial.txt"

  # measurement window runs out with the replay still going
  wait "$RP" || true
  [ $REHEARSAL -eq 1 ] && kill $PF 2>/dev/null || true
  kubectl --context "$CTX" get nodestates -n "$NS" -o json > "$REP_DIR/nodestates-after.json"

  # decision point (b): preemption on the placed node -> replacement
  if [ -n "$P_INIT" ]; then
    kubectl --context "$CTX" patch nodestate "$P_INIT" --subresource=status \
      --type=merge -p '{"status":{"spot":{"preemptionNoticeDetected":true}}}'
    P_REPL=""
    for i in $(seq 1 45); do
      P_REPL=$(kubectl --context "$CTX" get fleetservice exp-fleet -n "$NS" \
        -o jsonpath='{.status.placements[0].nodeRef}' 2>/dev/null || true)
      [ -n "$P_REPL" ] && [ "$P_REPL" != "$P_INIT" ] && break; sleep 2
    done
    echo "$P_REPL" > "$REP_DIR/placement-replacement.txt"
  fi

  # teardown fleet between reps (GPU run also restarts vLLM: session checklist)
  kubectl --context "$CTX" delete fleetservice exp-fleet -n "$NS" --wait=true
  sleep 3
done

# ---- mechanics verdict ----
COMPLETE=0
for d in "$RUN_DIR"/rep-*/; do
  ok=1
  for f in workload.jsonl workload.meta replay-results.jsonl replay.summary \
           nodestates-before.json nodestates-after.json \
           placement-initial.txt placement-replacement.txt; do
    [ -s "$d/$f" ] || { ok=0; echo "missing/empty: $d$f"; }
  done
  COMPLETE=$((COMPLETE + ok))
done
v=0; [ "$COMPLETE" -eq "${#SEQUENCE[@]}" ] || v=1
verdict "all ${#SEQUENCE[@]} reps have complete evidence" $v

OPERATOR_POD=$(kubectl --context "$CTX" get pods -l app.kubernetes.io/name=vllm-coldstart-operator -o jsonpath='{.items[0].metadata.name}')
kubectl --context "$CTX" logs "$OPERATOR_POD" > "$RUN_DIR/operator.log"
# Known benign classes, excluded one by one; anything else is a hard
# failure: (1) NotFound during between-rep teardown (queued events for
# just-deleted objects); (2) watch 410 "too old resource version"
# (kube-runtime relists and realigns by design, self-healing).
ERRS=$(grep -i "panic\|reconcile loop error" "$RUN_DIR/operator.log" \
  | grep -vi "ObjectNotFound\|NotFound" \
  | grep -vci "too old resource version" || true)
v=0; [ "$ERRS" -eq 0 ] || v=1
verdict "operator log clean across all reps (teardown NotFound excluded)" $v

echo "==> verdict: ${PASS} pass / ${FAIL} fail (evidence: ${RUN_DIR})"
cat "$RUN_DIR/verdict.txt"
[ "$FAIL" -eq 0 ]
