#!/usr/bin/env bash
# Load sim-fleet through its NodePort with the keep 0.1 sequence, sample
# its replicas every 2 s, and check what ADR-0014 asserts: the fleet
# reaches two replicas and never exceeds its cap of three, at least two
# pods serve requests, and every request is served with the profile's
# tokens (slo_check: correctness asserted, objectives only reported).
# Needs FLEET_URL, kubectl on the cluster and /tmp/loadgen with aiohttp;
# writes scale-out.* in the current directory.
set -euo pipefail
OUT=scale-out
HPA=keda-hpa-sim-fleet-scale-out
SEQUENCE=b170da22aa6101c6c20fbbaa9c9d2b647006161e2dffc7bf8710ec8135acdf62
mkdir -p "$OUT.pods"

pod_metrics() { # $1 = before or after: each fleet pod's /metrics, via the API server
  for p in $(kubectl get pods -l inference.michelecampi.dev/fleet=sim-fleet -o jsonpath='{.items[*].metadata.name}'); do
    kubectl get --raw "/api/v1/namespaces/default/pods/$p:8000/proxy/metrics" > "$OUT.pods/$p.$1"
  done
}
fleet_sum() { # each series summed over the pods: slo_check reads one line per name
  awk '/^vllm:[A-Za-z0-9_:]+\{[^}]*\} [^ ]+$/ {s[$1] += $2} END {for (k in s) printf "%s %.17g\n", k, s[k]}' "$@"
}
e2e_count() { # completed requests in one scrape; 0 before the first
  awk '/^vllm:e2e_request_latency_seconds_count\{/ {n = $2} END {print n + 0}' "$1"
}

pod_metrics before
fleet_sum "$OUT".pods/*.before > "$OUT.before"
(
  while :; do
    echo "$(date +%s) $(kubectl get fleetservice sim-fleet -o jsonpath='{.spec.replicas} {.status.readyReplicas}') $(kubectl get hpa "$HPA" -o jsonpath='{.status.desiredReplicas}')"
    sleep 2
  done
) > "$OUT.replicas" &
sampler=$!
trap 'kill "$sampler" 2> /dev/null || true' EXIT
t0=$(date +%s)
/tmp/loadgen/bin/python hack/loadgen/loadgen.py --target sim="$FLEET_URL" --model facebook/opt-125m \
  --profile hack/loadgen/profiles/azure-conv-2023-peak.csv --keep 0.1 --seed 42 --force-close --out "$OUT.jsonl"
pod_metrics after
kill "$sampler"
fleet_sum "$OUT".pods/*.after > "$OUT.after"
python3 hack/loadgen/slo_check.py "$OUT.jsonl" "$OUT.before" "$OUT.after" --slo hack/loadgen/slo.json \
  --sequence "$SEQUENCE"

peak=$(awk '{if ($2 > m) m = $2} END {print m + 0}' "$OUT.replicas")
echo "peak spec.replicas: $peak (cap 3)"
[ "$peak" -ge 2 ] || { echo "::error::sim-fleet peaked at $peak replicas, want at least 2"; exit 1; }
[ "$peak" -le 3 ] || { echo "::error::sim-fleet peaked at $peak replicas, above its cap of 3"; exit 1; }
serving=0
for f in "$OUT".pods/*.after; do
  b="${f%.after}.before"
  before=0
  if [ -f "$b" ]; then before=$(e2e_count "$b"); fi
  served=$(( $(e2e_count "$f") - before ))
  echo "$(basename "${f%.after}") served $served requests"
  if [ "$served" -gt 0 ]; then serving=$((serving + 1)); fi
done
[ "$serving" -ge 2 ] || { echo "::error::$serving pods served requests, want at least 2"; exit 1; }
desired_at=$(awk -v t0="$t0" '$4 >= 2 {print $1 - t0; exit}' "$OUT.replicas")
ready_at=$(awk -v t0="$t0" '$3 >= 2 {print $1 - t0; exit}' "$OUT.replicas")
echo "from the start of the load: the HPA wanted 2 or more replicas at ${desired_at:-never} s and the second replica was Ready at ${ready_at:-never} s"
