#!/usr/bin/env bash
# k3s server on node A + RuntimeClass nvidia + device plugin.
# Run from optim-dev after sourcing 00-env.sh.
set -euo pipefail
: "$NODE_A_IP" "$K3S_VERSION" "$DEVICE_PLUGIN_VERSION"

# 1. k3s server. --tls-san makes the public IP a valid apiserver SAN so
#    kubectl works from optim-dev. traefik/servicelb disabled: NodePort is
#    the entry (kube-proxy follows endpoints), nothing else needed.
ssh_a "curl -sfL https://get.k3s.io | INSTALL_K3S_VERSION='$K3S_VERSION' sh -s - server \
  --tls-san '$NODE_A_IP' --disable traefik --disable servicelb"

# 2. kubeconfig -> optim-dev, rewritten to the public IP.
mkdir -p ~/.kube
ssh_a "sudo cat /etc/rancher/k3s/k3s.yaml" \
  | sed "s/127.0.0.1/$NODE_A_IP/" > ~/.kube/gpu-session.yaml
chmod 600 ~/.kube/gpu-session.yaml
export KUBECONFIG=~/.kube/gpu-session.yaml
kubectl get nodes

# 3. Join token for the agents.
ssh_a "sudo cat /var/lib/rancher/k3s/server/node-token" > /tmp/k3s-token
echo "token saved to /tmp/k3s-token"

# 4. RuntimeClass: k3s auto-detects nvidia-container-runtime in containerd
#    config but does NOT create the RuntimeClass object.
kubectl apply -f - <<'EOF'
apiVersion: node.k8s.io/v1
kind: RuntimeClass
metadata:
  name: nvidia
handler: nvidia
EOF

# 5. Device plugin, pinned. The static manifest does not set
#    runtimeClassName, and on k3s the plugin itself needs the nvidia
#    runtime to reach NVML -> patch it in.
kubectl apply -f "https://raw.githubusercontent.com/NVIDIA/k8s-device-plugin/$DEVICE_PLUGIN_VERSION/deployments/static/nvidia-device-plugin.yml"
kubectl -n kube-system patch daemonset nvidia-device-plugin-daemonset \
  --type=merge -p '{"spec":{"template":{"spec":{"runtimeClassName":"nvidia"}}}}'

echo "server up. Next: 02-agent.sh"
