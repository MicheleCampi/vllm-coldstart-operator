#!/usr/bin/env bash
# Deterministic warmth seeding for the item-4 rehearsal (A/B/C topology):
# worker + worker2 Warm at util 0 (win initial planning), worker3 Warm at
# util 0.30 (loses the tie-break -> stays spare, eligible replacement),
# control-plane Cold (never a candidate).
set -euo pipefail
CTX=kind-fleet-test
seed() { kubectl --context "$CTX" patch nodestate "$1" --subresource=status --type=merge -p "$2"; }
seed fleet-test-worker        '{"status":{"warmth":"Warm","gpuUtilization":0.0,"activeServiceCount":0,"spot":{"preemptionNoticeDetected":false}}}'
seed fleet-test-worker2       '{"status":{"warmth":"Warm","gpuUtilization":0.0,"activeServiceCount":0,"spot":{"preemptionNoticeDetected":false}}}'
seed fleet-test-worker3       '{"status":{"warmth":"Warm","gpuUtilization":0.3,"activeServiceCount":0,"spot":{"preemptionNoticeDetected":false}}}'
seed fleet-test-control-plane '{"status":{"warmth":"Cold","gpuUtilization":0.0,"activeServiceCount":0,"spot":{"preemptionNoticeDetected":false}}}'
kubectl --context "$CTX" get nodestates -o custom-columns='NAME:.metadata.name,WARMTH:.status.warmth,PREEMPT:.status.spot.preemptionNoticeDetected,GPU:.status.gpuUtilization'
