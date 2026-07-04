#!/usr/bin/env bash
# Join B and C as k3s agents. Run from optim-dev after 01-server.sh.
set -euo pipefail
: "$NODE_A_IP" "$NODE_B_IP" "$NODE_C_IP" "$K3S_VERSION"
TOKEN="$(cat /tmp/k3s-token)"

join() { # $1 = ssh fn name
  "$1" "curl -sfL https://get.k3s.io | INSTALL_K3S_VERSION='$K3S_VERSION' \
    K3S_URL='https://$NODE_A_IP:6443' K3S_TOKEN='$TOKEN' sh -s - agent"
}
join ssh_b
join ssh_c

export KUBECONFIG=~/.kube/gpu-session.yaml
# Wait for 3 Ready nodes.
for i in $(seq 1 30); do
  n=$(kubectl get nodes --no-headers 2>/dev/null | grep -c ' Ready ' || true)
  [ "$n" -eq 3 ] && break
  sleep 5
done
kubectl get nodes -o wide
echo "check: nvidia.com/gpu allocatable on all 3 nodes:"
kubectl get nodes -o custom-columns='NAME:.metadata.name,GPU:.status.allocatable.nvidia\.com/gpu'
echo "agents joined. Next: 03-prepull.sh"
