#!/usr/bin/env bash
# Install the KEDA HTTP add-on next to the KEDA that hack/keda-install.sh
# installs, from the chart and images the workflow pins
# (KEDA_HTTP_CHART_URL, KEDA_HTTP_CHART_SHA256, KEDA_HTTP_IMAGE_*), and
# fail unless every add-on pod runs exactly its pinned image and the
# interceptor runs its three replicas.
#
# The chart composes images.<component>:images.tag with one tag for all
# three components, so a digest in that tag would put one image on all of
# them. The script installs the chart's own tag, then sets each
# deployment to its pinned tag@digest and waits until no pod runs anything
# else.
#
# The interceptor is pinned to three replicas, the chart's minimum, with
# its autoscaling held there: the cold-start queue limit applies per
# interceptor replica, so a replica count that moved during the load would
# move the limit under the measurement (ADR-0015).
set -euo pipefail
curl -fsSL -o /tmp/keda-http.tgz "$KEDA_HTTP_CHART_URL"
echo "$KEDA_HTTP_CHART_SHA256  /tmp/keda-http.tgz" | sha256sum -c -
/tmp/linux-amd64/helm install keda-add-ons-http /tmp/keda-http.tgz --namespace keda \
  --set interceptor.replicas.min=3 --set interceptor.replicas.max=3 \
  --wait --timeout 5m
r=ghcr.io/kedacore
kubectl set image -n keda deployment/keda-add-ons-http-controller-manager \
  keda-add-ons-http-operator="$r/http-add-on-operator:$KEDA_HTTP_IMAGE_OPERATOR"
kubectl set image -n keda deployment/keda-add-ons-http-interceptor \
  keda-add-ons-http-interceptor="$r/http-add-on-interceptor:$KEDA_HTTP_IMAGE_INTERCEPTOR"
kubectl set image -n keda deployment/keda-add-ons-http-external-scaler \
  keda-add-ons-http-external-scaler="$r/http-add-on-scaler:$KEDA_HTTP_IMAGE_SCALER"
for d in keda-add-ons-http-controller-manager keda-add-ons-http-interceptor keda-add-ons-http-external-scaler; do
  kubectl rollout status "deployment/$d" -n keda --timeout=3m
done
want=$(printf '%s\n' "$r/http-add-on-operator:$KEDA_HTTP_IMAGE_OPERATOR" "$r/http-add-on-interceptor:$KEDA_HTTP_IMAGE_INTERCEPTOR" "$r/http-add-on-scaler:$KEDA_HTTP_IMAGE_SCALER" | sort)
for _ in $(seq 60); do
  got=$(kubectl get pods -n keda -l app.kubernetes.io/instance=keda-add-ons-http -o jsonpath='{range .items[*]}{.spec.containers[0].image}{"\n"}{end}' | sort -u)
  ready=$(kubectl get deployment keda-add-ons-http-interceptor -n keda -o jsonpath='{.status.readyReplicas}')
  if [ "$got" = "$want" ] && [ "$ready" = 3 ]; then break; fi
  sleep 2
done
[ "$got" = "$want" ] || { echo "::error::add-on pods run [$got], want [$want]"; exit 1; }
[ "$ready" = 3 ] || { echo "::error::$ready interceptor replicas ready, want 3"; exit 1; }
echo "The HTTP add-on runs the pinned images, with 3 interceptor replicas ready:"
echo "$got"
