#!/usr/bin/env bash
# One rehearsal/validation run: steady load on two fleet placements, inject a
# preemption notice on one node mid-run, record everything needed to build
# the timeline. Reused as-is for the GPU session (different ENTRY/params).
set -euo pipefail

CTX="${CTX:-kind-fleet-test}"
ENTRY="${ENTRY:?set ENTRY=<host:port-base entry ip>}"   # e.g. 172.18.0.4
NP_A="${NP_A:-30801}"        # baseline target (unaffected node)
NP_B="${NP_B:-30800}"        # target on the node to preempt
PREEMPT_NODE="${PREEMPT_NODE:-fleet-test-worker2}"
MODEL="${MODEL:-facebook/opt-125m}"
DURATION="${DURATION:-120}"
PREEMPT_AT="${PREEMPT_AT:-30}"
CONCURRENCY="${CONCURRENCY:-8}"
MAX_TOKENS="${MAX_TOKENS:-128}"
OPERATOR_LOG="${OPERATOR_LOG:-/tmp/operator-rehearsal.log}"

RUN_DIR="hack/rehearsal/runs/$(date +%Y%m%dT%H%M%S)"
mkdir -p "$RUN_DIR"
echo "run dir: $RUN_DIR"

python3 hack/loadgen/loadgen.py \
  --target "a=http://${ENTRY}:${NP_A}" \
  --target "b=http://${ENTRY}:${NP_B}" \
  --model "$MODEL" --concurrency "$CONCURRENCY" --duration "$DURATION" \
  --max-tokens "$MAX_TOKENS" --out "$RUN_DIR/load.jsonl" &
LOADGEN_PID=$!

sleep "$PREEMPT_AT"
T_NOTICE=$(date +%s.%N)
kubectl --context "$CTX" patch nodestate "$PREEMPT_NODE" --subresource=status \
  --type=merge -p '{"status":{"spot":{"preemptionNoticeDetected":true}}}'
echo "{\"t_notice\": $T_NOTICE, \"preempt_node\": \"$PREEMPT_NODE\"}" > "$RUN_DIR/markers.json"
echo "notice injected on $PREEMPT_NODE at $T_NOTICE"

wait "$LOADGEN_PID"

kubectl --context "$CTX" get events --sort-by=.lastTimestamp -o custom-columns='TS:.lastTimestamp,TYPE:.type,REASON:.reason,OBJ:.involvedObject.name,MSG:.message' > "$RUN_DIR/events.txt"
kubectl --context "$CTX" get fleetservice rehearsal -o yaml > "$RUN_DIR/fleet-final.yaml"
kubectl --context "$CTX" get pods -o wide > "$RUN_DIR/pods-final.txt"
cp "$OPERATOR_LOG" "$RUN_DIR/operator.log"

# Reset for repeatability.
kubectl --context "$CTX" patch nodestate "$PREEMPT_NODE" --subresource=status \
  --type=merge -p '{"status":{"spot":{"preemptionNoticeDetected":false}}}'
echo "done -> $RUN_DIR"
