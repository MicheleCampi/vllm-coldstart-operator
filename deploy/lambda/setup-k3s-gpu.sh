#!/usr/bin/env bash
# Set up single-node K3s with NVIDIA GPU scheduling on a Lambda Cloud instance.
# Lambda Stack preinstalls the NVIDIA driver and Container Toolkit, so this
# script only installs K3s, points containerd at the NVIDIA runtime, registers
# the RuntimeClass, and deploys the device plugin. Run once on a fresh instance.
set -euo pipefail

echo "==> [1/4] Installing K3s (single node)"
curl -sfL https://get.k3s.io | sh -
# Make kubectl usable without sudo for the rest of the script.
export KUBECONFIG=/etc/rancher/k3s/k3s.yaml
sudo chmod 644 /etc/rancher/k3s/k3s.yaml
kubectl wait --for=condition=Ready node --all --timeout=120s
echo "    K3s ready."

echo "==> [2/4] Configuring K3s containerd for the NVIDIA runtime"
# K3s ships its own containerd. Recent K3s autodetects the NVIDIA runtime when
# the Container Toolkit is present, generating an "nvidia" runtime in its
# containerd config. Restart K3s so detection runs now that the toolkit is here.
sudo systemctl restart k3s
sleep 10
kubectl wait --for=condition=Ready node --all --timeout=120s
# Verify the nvidia runtime was registered in K3s containerd config.
if sudo grep -q 'nvidia' /var/lib/rancher/k3s/agent/etc/containerd/config.toml*; then
    echo "    NVIDIA runtime detected in K3s containerd config."
else
    echo "    WARNING: nvidia runtime not found in containerd config; GPU pods may fail."
fi

echo "==> [3/4] Registering the nvidia RuntimeClass"
kubectl apply -f - <<'RTC'
apiVersion: node.k8s.io/v1
kind: RuntimeClass
metadata:
  name: nvidia
handler: nvidia
RTC
echo "    RuntimeClass 'nvidia' applied."

echo "==> [4/4] Deploying the NVIDIA device plugin"
kubectl apply -f https://raw.githubusercontent.com/NVIDIA/k8s-device-plugin/v0.17.1/deployments/static/nvidia-device-plugin.yml
kubectl -n kube-system rollout status daemonset/nvidia-device-plugin-daemonset --timeout=120s

echo "==> Verifying GPU is advertised to the scheduler"
for i in $(seq 1 12); do
    gpu=$(kubectl get nodes -o jsonpath='{.items[0].status.allocatable.nvidia\.com/gpu}' 2>/dev/null || true)
    if [ -n "${gpu:-}" ] && [ "$gpu" != "0" ]; then
        echo "    Node advertises nvidia.com/gpu=${gpu}. Cluster is GPU-ready."
        exit 0
    fi
    echo "    [$i] waiting for GPU to be advertised..."
    sleep 5
done
echo "    ERROR: no GPU advertised after 60s. Check device plugin logs:"
echo "      kubectl -n kube-system logs daemonset/nvidia-device-plugin-daemonset"
exit 1
