#!/usr/bin/env bash
# The per-node reporter for the e2e job on kind, run as a process against
# the job's kubeconfig, as the operator is. At start the reporter creates the
# node's NodeState, then merge-patches the status fields it owns every
# spec.reportIntervalSeconds (src/bin/reporter.rs). It does not own warmth,
# and nothing in this repository writes it, so start writes it once the
# reporter has reported, standing in for a warmth writer: Warm, the value the
# e2e steps assert and rank on.
#
#   start  launch it with the synthetic source and one signal set, wait for
#          its first report, check that signal, write warmth Warm
#   stop   kill the process start launched and wait until it has exited
set -euo pipefail
cd "$(dirname "$0")/.."
NODE=$(kubectl get nodes -o jsonpath='{.items[0].metadata.name}')
UTIL=0.25
field() { kubectl get nodestate "$NODE" -o jsonpath="{.status.$1}" 2>/dev/null || true; }
case "${1:-}" in
  start)
    NODE_NAME="$NODE" POD_NAMESPACE=default REPORTER_SYNTHETIC=1 \
      REPORTER_SYNTHETIC_GPU_UTILIZATION="$UTIL" \
      ./target/debug/reporter >> reporter.log 2>&1 &
    echo $! > reporter.pid
    t=""
    for _ in $(seq 1 30); do
      t=$(field lastReportTime)
      [ -n "$t" ] && break
      sleep 1
    done
    [ -n "$t" ] || { echo "ERROR: no report on NodeState $NODE within 30s"; cat reporter.log; exit 1; }
    u=$(field gpuUtilization)
    [ "$u" = "$UTIL" ] || { echo "ERROR: gpuUtilization is '$u', the synthetic source was given $UTIL"; exit 1; }
    kubectl patch nodestate "$NODE" --subresource=status --type=merge -p '{"status":{"warmth":"Warm"}}'
    echo "reporter (pid $(cat reporter.pid)) reported on $NODE at $t; warmth written Warm"
    ;;
  stop)
    pid=$(cat reporter.pid)
    kill "$pid" 2>/dev/null || true
    for _ in $(seq 1 10); do
      kill -0 "$pid" 2>/dev/null || break
      sleep 1
    done
    if kill -0 "$pid" 2>/dev/null; then echo "ERROR: reporter (pid $pid) still running"; exit 1; fi
    rm -f reporter.pid
    echo "reporter (pid $pid) stopped"
    ;;
  *)
    echo "usage: $0 start|stop" >&2
    exit 2
    ;;
esac
