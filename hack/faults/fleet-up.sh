#!/usr/bin/env bash
# Bring up hack/faults/load-fleet.yaml and wait for two Ready replicas on two
# different nodes (ADR-0018).
set -euo pipefail
kubectl apply -f hack/faults/load-fleet.yaml
up=""
for i in $(seq 1 90); do
  up=$(kubectl get fleetservice load-fleet -o jsonpath='{.status.phase}/{.status.readyReplicas}' 2>/dev/null || true)
  [ "$up" = "Ready/2" ] && break
  sleep 2
done
[ "$up" = "Ready/2" ] || { echo "ERROR: load-fleet is '$up', want Ready/2"; exit 1; }
nodes=$(kubectl get pods -l inference.michelecampi.dev/fleet=load-fleet -o jsonpath='{.items[*].spec.nodeName}')
[ "$(echo "$nodes" | tr ' ' '\n' | sort -u | wc -l)" = 2 ] || { echo "ERROR: replicas on [$nodes], want two nodes"; exit 1; }
echo "load-fleet Ready/2 on [$nodes]"
