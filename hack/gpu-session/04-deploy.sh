#!/usr/bin/env bash
# CRDs, operator (static musl binary as a systemd unit on the server
# node), NodeState CRs, fleet manifest. Run from optim-dev, repo root.
set -euo pipefail
: "$NODE_A_IP"
export KUBECONFIG=~/.kube/gpu-session.yaml
BIN=target/x86_64-unknown-linux-musl/release/vllm-coldstart-operator
[ -f "$BIN" ] || { echo "musl binary missing — build it first"; exit 1; }

# 1. CRDs.
kubectl apply -f deploy/crd.yaml

# 2. Operator binary -> server node, systemd unit. Runs with the node's
#    k3s admin kubeconfig (single-tenant benchmark cluster; RBAC hardening
#    is chart territory, tracked post-GPU).
scp -i "$GS_SSH_KEY" "$BIN" "ubuntu@$NODE_A_IP:/tmp/vcso"
ssh_a "sudo mv /tmp/vcso /usr/local/bin/vcso && sudo chmod +x /usr/local/bin/vcso \
  && sudo tee /etc/systemd/system/vcso.service > /dev/null <<'EOF'
[Unit]
Description=vllm-coldstart-operator (item-4 GPU session)
After=k3s.service
[Service]
Environment=KUBECONFIG=/etc/rancher/k3s/k3s.yaml
ExecStart=/usr/local/bin/vcso
Restart=on-failure
[Install]
WantedBy=multi-user.target
EOF
sudo systemctl daemon-reload && sudo systemctl enable --now vcso \
  && sleep 2 && sudo systemctl status vcso --no-pager -l | head -12"

# 3. NodeState CRs, one per k8s node name, then warmth seeding: A and B
#    Warm util 0 (win initial planning), C Warm util 0.30 (spare).
mapfile -t NODES < <(kubectl get nodes -o name | sed 's|node/||' | sort)
[ "${#NODES[@]}" -eq 3 ] || { echo "expected 3 nodes, got ${#NODES[@]}"; exit 1; }
for n in "${NODES[@]}"; do
  kubectl apply -f - <<EOF
apiVersion: inference.michelecampi.dev/v1alpha1
kind: NodeState
metadata:
  name: $n
  namespace: default
spec: {}
EOF
done
echo "NodeStates created for: ${NODES[*]}"
echo "IMPORTANT: map A/B/C roles to real node names, then seed warmth:"
echo "  kubectl patch nodestate <A> --subresource=status --type=merge -p '{\"status\":{\"warmth\":\"Warm\",\"gpuUtilization\":0.0,\"activeServiceCount\":0,\"spot\":{\"isSpotInstance\":true,\"preemptionNoticeDetected\":false}}}'"
echo "  (same for B; C with gpuUtilization 0.3)"

# 4. Fleet + NodePort: apply hack/gpu-session/fleet-gpu.yaml manually
#    after seeding, per the go/no-go checklist.
echo "deploy done. Seed warmth, then: kubectl apply -f hack/gpu-session/fleet-gpu.yaml"
