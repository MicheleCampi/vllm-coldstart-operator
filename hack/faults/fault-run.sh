#!/usr/bin/env bash
# One run of the faults-under-load job: the within-capacity load (keep 0.025,
# seed 42, ADR-0013) through the fleet's NodePort, one fault on the node of
# slot 0 after FAULT_AT seconds, and the fleet's readiness every second.
# Usage: fault-run.sh drain|notice PREFIX   (needs FLEET_URL and FLEET)
set -euo pipefail
fault=$1
out=$2
at=${FAULT_AT:-60}
node=$(kubectl get fleetservice "$FLEET" -o jsonpath='{.status.placements[0].nodeRef}')
[ -n "$node" ] || { echo "ERROR: slot 0 of $FLEET has no node"; exit 1; }
echo "run $fault: slot 0 on $node, fault after ${at}s"

(while :; do
  echo "$(date +%s.%N) $(kubectl get fleetservice "$FLEET" -o jsonpath='{.status.readyReplicas}' 2>/dev/null || echo -)"
  sleep 1
done) > "$out.ready" &
watcher=$!

(sleep "$at"
 echo "$(date +%s.%N) $fault $node" > "$out.fault"
 case $fault in
   drain)
     kubectl cordon "$node"
     kubectl drain "$node" --ignore-daemonsets --delete-emptydir-data --timeout=240s ;;
   notice)
     kubectl patch nodestate "$node" --subresource=status --type=merge \
       -p '{"status":{"spot":{"preemptionNoticeDetected":true}}}' ;;
   *) echo "unknown fault $fault"; exit 1 ;;
 esac) > "$out.fault.log" 2>&1 &
injector=$!

# The replay leaves long gaps at this load, so a fault can find no request
# in flight. A closed-loop probe, four streams of 512 tokens for 20 s from
# 5 s before the fault, puts some there; it is recorded apart from the
# replay, whose objectives it does not enter.
(sleep $((at - 5))
 /tmp/loadgen/bin/python hack/loadgen/loadgen.py --target probe="$FLEET_URL" \
   --model facebook/opt-125m --concurrency 4 --duration 20 --max-tokens 589 \
   --out "$out.probe.jsonl") > "$out.probe.log" 2>&1 &
prober=$!

/tmp/loadgen/bin/python hack/loadgen/loadgen.py --target sim="$FLEET_URL" \
  --model facebook/opt-125m --profile hack/loadgen/profiles/azure-conv-2023-peak.csv \
  --keep 0.025 --seed 42 --force-close --out "$out.jsonl"
wait "$prober" || { echo "ERROR: the probe did not run"; cat "$out.probe.log"; kill "$watcher"; exit 1; }
wait "$injector" || { echo "ERROR: the fault did not apply"; cat "$out.fault.log"; kill "$watcher"; exit 1; }
sleep 30
kill "$watcher"

# A failed request makes slo_check exit 1: here that is the measurement, not
# an error. Only a run that is not the pinned one is.
python3 hack/loadgen/slo_check.py "$out.jsonl" --client-only --slo hack/loadgen/slo-l4-qwen7b-bf16.json \
  --sequence 6c10349917a6877be7e1a94ade7a936dcb70337be864a275a13e6b5a56c9a3cc > "$out.slo" || true
cat "$out.slo"
if grep -q -E 'is not the pinned|scheduled requests|generator fell behind' "$out.slo"; then
  echo "ERROR: the run is not the pinned replay, its numbers do not count"
  exit 1
fi
python3 hack/faults/timeline.py "$out.jsonl" "$out.fault" "$out.ready" "$out.probe.jsonl"
