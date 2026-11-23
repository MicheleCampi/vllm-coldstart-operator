#!/usr/bin/env bash
# kind nodes run systemd but no D-Bus or logind, so the kubelet's graceful
# node shutdown manager cannot start (it takes a logind inhibitor lock and
# listens for PrepareForShutdown). Install them on each worker, restart the
# kubelet, and prove the manager is active: it overrides logind's
# InhibitDelayMaxSec to shutdownGracePeriod (150 s, as on EKS AL2023 1.34+).
set -euo pipefail
CLUSTER=$1
for n in $(kind get nodes --name "$CLUSTER" | grep worker); do
  docker exec "$n" bash -c 'apt-get update -qq && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq dbus systemd-logind >/dev/null 2>&1 || DEBIAN_FRONTEND=noninteractive apt-get install -y -qq dbus >/dev/null'
  docker exec "$n" systemctl start dbus systemd-logind
  docker exec "$n" systemctl restart kubelet
done
kubectl wait --for=condition=Ready nodes --all --timeout=180s
sleep 10
for n in $(kind get nodes --name "$CLUSTER" | grep worker); do
  d=$(docker exec "$n" busctl get-property org.freedesktop.login1 /org/freedesktop/login1 org.freedesktop.login1.Manager InhibitDelayMaxUSec | awk '{print $2}')
  echo "$n: logind InhibitDelayMaxUSec=$d"
  docker exec "$n" journalctl -u kubelet --no-pager | grep -i -E "shutdown" | tail -3 || true
  [ "$d" = 150000000 ] || { echo "ERROR: $n: graceful node shutdown not active"; exit 1; }
done
