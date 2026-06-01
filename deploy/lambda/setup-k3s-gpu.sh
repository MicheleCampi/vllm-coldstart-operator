#!/usr/bin/env bash
# Set up single-node K3s with NVIDIA GPU scheduling on a Lambda Cloud instance.
# Lambda Stack preinstalls the NVIDIA driver, so we keep it (driver.enabled=false)
# and let the NVIDIA GPU Operator manage the container toolkit and configure
# K3s's containerd at its non-standard paths. This is more robust than hand-
# editing the containerd template, which is version-sensitive on K3s. Run once.
set -euo pipefail

export KUBECONFIG=/etc/rancher/k3s/k3s.yaml

echo "==> [1/4] Installing K3s (single node)"
curl -sfL https://get.k3s.io | sh -
sudo chmod 644 /etc/rancher/k3s/k3s.yaml
# Wait for the node to register AND become Ready (the API may not be up the
# instant the installer returns, so retry instead of failing fast).
for i in $(seq 1 30); do
    if kubectl get nodes 2>/dev/null | grep -q ' Ready '; then
        echo "    K3s node Ready."
        break
    fi
    echo "    [$i] waiting for K3s node..."
    sleep 4
done

echo "==> [2/4] Installing Helm"
if ! command -v helm >/dev/null 2>&1; then
    curl -fsSL https://raw.githubusercontent.com/helm/helm/main/scripts/get-helm-3 | bash
fi
helm version

echo "==> [3/4] Installing the NVIDIA GPU Operator (driver from Lambda Stack)"
helm repo add nvidia https://helm.ngc.nvidia.com/nvidia
helm repo update
# driver.enabled=false  -> keep Lambda's preinstalled driver.
# The toolkit envs point the operator at K3s's non-standard containerd paths;
# without these the runtime is written to the wrong place and never registers.
helm install --wait gpu-operator nvidia/gpu-operator \
    -n gpu-operator --create-namespace \
    --set driver.enabled=false \
    --set toolkit.env[0].name=CONTAINERD_CONFIG \
    --set toolkit.env[0].value=/var/lib/rancher/k3s/agent/etc/containerd/config.toml.tmpl \
    --set toolkit.env[1].name=CONTAINERD_SOCKET \
    --set toolkit.env[1].value=/run/k3s/containerd/containerd.sock \
    --set toolkit.env[2].name=CONTAINERD_RUNTIME_CLASS \
    --set toolkit.env[2].value=nvidia \
    --set toolkit.env[3].name=CONTAINERD_SET_AS_DEFAULT \
    --set-string toolkit.env[3].value=true
echo "    GPU Operator installed."

echo "==> [4/4] Verifying GPU is advertised to the scheduler"
# The operator's toolkit pod reconfigures containerd and restarts it, so the
# node may briefly go NotReady; allow generous time for nvidia.com/gpu to appear.
for i in $(seq 1 36); do
    gpu=$(kubectl get nodes -o jsonpath='{.items[0].status.allocatable.nvidia\.com/gpu}' 2>/dev/null || true)
    if [ -n "${gpu:-}" ] && [ "$gpu" != "0" ]; then
        echo "    Node advertises nvidia.com/gpu=${gpu}. Cluster is GPU-ready."
        exit 0
    fi
    echo "    [$i] waiting for nvidia.com/gpu (operator configuring containerd)..."
    sleep 10
done
echo "    ERROR: no GPU advertised after ~6min. Inspect the operator:"
echo "      kubectl -n gpu-operator get pods"
echo "      kubectl -n gpu-operator logs -l app=nvidia-container-toolkit-daemonset"
exit 1
