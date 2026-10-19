#!/usr/bin/env bash
# Install KEDA into the current cluster from the chart and images the
# workflow pins (KEDA_CHART_URL, KEDA_CHART_SHA256, KEDA_IMAGE_*), with
# Helm where the Install Helm step leaves it, and fail unless every KEDA
# pod runs exactly its pinned image. The keda and scale-out jobs share
# this script, so the pin check cannot drift between them.
#
# The chart composes registry/repository:tag, so a tag carrying a
# digest makes each pod run exactly the image pinned in env.
set -euo pipefail
curl -fsSL -o /tmp/keda.tgz "$KEDA_CHART_URL"
echo "$KEDA_CHART_SHA256  /tmp/keda.tgz" | sha256sum -c -
/tmp/linux-amd64/helm install keda /tmp/keda.tgz --namespace keda --create-namespace \
  --set image.keda.tag="$KEDA_IMAGE_KEDA" \
  --set image.metricsApiServer.tag="$KEDA_IMAGE_METRICS" \
  --set image.webhooks.tag="$KEDA_IMAGE_WEBHOOKS" \
  --wait --timeout 5m
for d in keda-operator keda-operator-metrics-apiserver keda-admission-webhooks; do
  kubectl rollout status "deployment/$d" -n keda --timeout=2m
done
got=$(kubectl get pods -n keda -o jsonpath='{range .items[*]}{.spec.containers[0].image}{"\n"}{end}' | sort)
want=$(printf '%s\n' "ghcr.io/kedacore/keda:$KEDA_IMAGE_KEDA" "ghcr.io/kedacore/keda-metrics-apiserver:$KEDA_IMAGE_METRICS" "ghcr.io/kedacore/keda-admission-webhooks:$KEDA_IMAGE_WEBHOOKS" | sort)
[ "$got" = "$want" ] || { echo "::error::KEDA pods run [$got], want [$want]"; exit 1; }
echo "KEDA runs the pinned images:"
echo "$got"
