#!/usr/bin/env bash
# One graceful node shutdown under load (ADR-0028), as the kubelet sees it on
# EKS AL2023 1.34+ (shutdownGracePeriod 150 s, critical pods 30 s): the
# PrepareForShutdown signal of logind on the node of one of two replicas.
# MODE fixed: the operator runs, and its node watch deletes the pods it
# manages on a node whose kubelet reports "node is shutting down". MODE
# control: the operator is stopped before the signal, as v0.17.0 had no node
# watch: only the kubelet acts, as in the EKS game day re-run of 21/11.
# Writes $MODE.client.jsonl, $MODE.timeline and $MODE.result.json.
set -euo pipefail
MODE=$1; NAME=drain-svc; D=hack/node-shutdown
kubectl apply -f "$D/service.yaml"
for i in $(seq 1 60); do kubectl get deploy "$NAME" >/dev/null 2>&1 && break; sleep 2; done
kubectl rollout status deploy/"$NAME" --timeout=300s
if [ "$MODE" = control ]; then
  kill "$(cat operator.pid)" 2>/dev/null || true
  sleep 2
fi
pods() { kubectl get pods -l app="$NAME" --field-selector=status.phase=Running -o jsonpath='{range .items[*]}{.metadata.name} {.spec.nodeName}{"\n"}{end}'; }
# Two replicas on two nodes: move one if the scheduler put both together.
read -r P1 N1 <<< "$(pods | sed -n 1p)"; read -r P2 N2 <<< "$(pods | sed -n 2p)"
if [ "$N1" = "$N2" ]; then
  kubectl cordon "$N1"; kubectl delete pod "$P2" --wait=false
  sleep 5; kubectl rollout status deploy/"$NAME" --timeout=300s; kubectl uncordon "$N1"
fi
kubectl wait --for=condition=Ready pod -l app="$NAME" --timeout=300s
read -r TARGET NODE <<< "$(pods | sed -n 1p)"
echo "target $TARGET on $NODE; pods: $(pods | tr '\n' ' ')"
pgrep -f target/debug/vllm-coldstart-operator >/dev/null && echo "operator: running" || echo "operator: stopped"
# The client, on the control plane: it must not share the fate of the node.
kubectl run client --image=fake-vllm:node-shutdown --restart=Never \
  --labels="inference.michelecampi.dev/client=$NAME" \
  --overrides='{"spec":{"nodeSelector":{"node-role.kubernetes.io/control-plane":""},"tolerations":[{"key":"node-role.kubernetes.io/control-plane","operator":"Exists","effect":"NoSchedule"}]}}' \
  --command -- python3 -u /client.py "http://$NAME:8000/v1/completions"
kubectl wait --for=condition=Ready pod/client --timeout=120s
sleep 20
# Timeline, once a second: the target pod's phase, Ready condition and the
# ready/terminating flags of its endpoint.
( while :; do
    printf '%s %s ready=%s ep=%s\n' "$(date +%s.%N | cut -c1-14)" \
      "$(kubectl get pod "$TARGET" -o jsonpath='{.status.phase}' 2>/dev/null || echo gone)" \
      "$(kubectl get pod "$TARGET" -o jsonpath='{.status.conditions[?(@.type=="Ready")].status}' 2>/dev/null)" \
      "$(kubectl get endpointslices -l kubernetes.io/service-name="$NAME" -o jsonpath="{.items[*].endpoints[?(@.targetRef.name==\"$TARGET\")].conditions}" 2>/dev/null)"
    sleep 1
  done ) > "$MODE.timeline" &
TL=$!
T_SIG=$(date +%s.%N)
docker exec "$NODE" dbus-send --system --type=signal /org/freedesktop/login1 org.freedesktop.login1.Manager.PrepareForShutdown boolean:true
echo "PrepareForShutdown sent to $NODE at $T_SIG"
# Inside the drain (in the job log): the node's Ready condition as the
# kubelet reports it, and the target's deletionTimestamp and conditions.
( for at in 5 30; do
    sleep $(( at == 5 ? 5 : 25 ))
    echo "--- +${at}s"
    kubectl get node "$NODE" -o jsonpath='{range .status.conditions[?(@.type=="Ready")]}  node Ready={.status} {.message}{end}{"\n"}' 2>&1
    kubectl get pod "$TARGET" -o jsonpath='  pod deletionTimestamp={.metadata.deletionTimestamp} {range .status.conditions[*]}{.type}={.status} {end}{"\n"}' 2>&1
  done ) > "$MODE.diag" 2>&1 || true &
for i in $(seq 1 200); do
  ph=$(kubectl get pod "$TARGET" -o jsonpath='{.status.phase}' 2>/dev/null || echo gone)
  case "$ph" in Succeeded|Failed|gone) break;; esac
  sleep 1
done
T_END=$(date +%s.%N)
echo "target pod $ph after $(python3 -c "print(round($T_END - $T_SIG, 1))") s"
sleep 12
kill "$TL"
kubectl logs client > "$MODE.client.jsonl"
echo "=== $MODE: diagnostics"; cat "$MODE.diag" || true
echo "=== $MODE: timeline (phase, Ready, endpoint conditions)"; cat "$MODE.timeline" || true
echo "=== $MODE: kubelet on $NODE (probes, status, shutdown)"
docker exec "$NODE" journalctl -u kubelet --no-pager --since "-4min" 2>/dev/null \
  | grep -F "$TARGET" | grep -E "probe worker|SyncLoop (DELETE|UPDATE)|Shutdown manager|PreStop|Killing container" \
  | cut -c1-400 | head -n 40 || true
kubectl delete pod client --wait=false
rc=0; python3 "$D/analyze.py" "$MODE" "$MODE.client.jsonl" "$T_SIG" "$T_END" "$TARGET" | tee "$MODE.result.json" || rc=$?
# Back to a clean cluster for the next mode: cancel the shutdown, node Ready.
kubectl delete vllmservice "$NAME" --wait=false
docker exec "$NODE" dbus-send --system --type=signal /org/freedesktop/login1 org.freedesktop.login1.Manager.PrepareForShutdown boolean:false
kubectl wait --for=condition=Ready nodes --all --timeout=180s || true
kubectl wait --for=delete pod -l app="$NAME" --timeout=180s || true
exit $rc
