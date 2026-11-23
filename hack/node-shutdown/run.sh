#!/usr/bin/env bash
# One graceful node shutdown under load (ADR-0028), as the kubelet sees it on
# EKS AL2023 1.34+ (shutdownGracePeriod 150 s, critical pods 30 s): the
# PrepareForShutdown signal of logind on the node of one of two replicas.
# MODE fixed: the pod spec the operator builds. MODE control: the same
# Deployment patched back to the v0.17.0 pod spec (readiness httpGet with 60
# failures allowed, native pre-stop sleep); the operator must be stopped.
# Writes $MODE.client.jsonl, $MODE.timeline and $MODE.result.json.
set -euo pipefail
MODE=$1; NAME=drain-svc; D=hack/node-shutdown
kubectl apply -f "$D/service.yaml"
for i in $(seq 1 60); do kubectl get deploy "$NAME" >/dev/null 2>&1 && break; sleep 2; done
kubectl rollout status deploy/"$NAME" --timeout=300s
if [ "$MODE" = control ]; then
  # Stopped, or it would reconcile the patch away.
  kill "$(cat operator.pid)" 2>/dev/null || true
  kubectl patch deploy "$NAME" --type=json -p '[
    {"op":"replace","path":"/spec/template/spec/containers/0/readinessProbe","value":{"httpGet":{"path":"/health","port":8000},"initialDelaySeconds":5,"periodSeconds":5,"failureThreshold":60}},
    {"op":"replace","path":"/spec/template/spec/containers/0/lifecycle","value":{"preStop":{"sleep":{"seconds":110}}}}]'
  kubectl rollout status deploy/"$NAME" --timeout=300s
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
grep -q "\"exec\"" <(kubectl get pod "$TARGET" -o jsonpath='{.spec.containers[0].lifecycle}') && echo "pre-stop: exec (drain flag)" || echo "pre-stop: sleep"
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
# What the node sees inside the drain (diagnostics, in the job log): the
# drain flag, the readiness command run by hand, and the pod's conditions.
( for at in 20 60; do
    sleep $(( at == 20 ? 20 : 40 ))
    CID=$(docker exec "$NODE" crictl ps --name inference -q 2>/dev/null | head -1)
    echo "--- +${at}s container ${CID:-none}"
    [ -n "$CID" ] && docker exec "$NODE" crictl exec "$CID" ls -la /tmp 2>&1 | sed 's/^/  /'
    [ -n "$CID" ] && { docker exec "$NODE" crictl exec "$CID" python3 -I -S -c 'import os,sys; sys.exit("draining" if os.path.exists("/tmp/vllm-coldstart-draining") else 0)' 2>&1 && echo "  ready-check rc=0" || echo "  ready-check rc=$?"; }
    kubectl get pod "$TARGET" -o jsonpath='{range .status.conditions[*]}{.type}={.status} {end}{"\n"}' 2>&1 | sed 's/^/  conditions: /'
  done ) > "$MODE.diag" 2>&1 &
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
  | grep -F "$TARGET" | grep -E "[Pp]robe|[Rr]eadiness|Patch status|updated successfully|SyncLoop|[Kk]ill|PreStop|[Tt]erminat" \
  | grep -v "Generating pod status" | cut -c1-700 | head -n 200 || true
kubectl delete pod client --wait=false
rc=0; python3 "$D/analyze.py" "$MODE" "$MODE.client.jsonl" "$T_SIG" "$T_END" "$TARGET" | tee "$MODE.result.json" || rc=$?
# Back to a clean cluster for the next mode: cancel the shutdown, node Ready.
kubectl delete vllmservice "$NAME" --wait=false
docker exec "$NODE" dbus-send --system --type=signal /org/freedesktop/login1 org.freedesktop.login1.Manager.PrepareForShutdown boolean:false
kubectl wait --for=condition=Ready nodes --all --timeout=180s || true
kubectl wait --for=delete pod -l app="$NAME" --timeout=180s || true
exit $rc
