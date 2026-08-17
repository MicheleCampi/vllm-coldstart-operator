# Scale-to-zero with KEDA

What works, what does not, and why — measured on kind, 2026-08-17.

    helm repo add kedacore https://kedacore.github.io/charts && helm repo update
    helm install keda kedacore/keda -n keda --create-namespace --wait
    kubectl apply -f queue-stub.yaml
    kubectl apply -f scaledobject-scale-to-zero.yaml

With the stub reporting `{"waiting": 0}` the fleet drains to zero children
in about twenty seconds. Set it to a positive number and the fleet comes
back in under ten:

    kubectl create configmap queue-stub-cfg \
      --from-literal=queue.json='{"waiting": 4}' --dry-run=client -o yaml \
      | kubectl apply -f - && kubectl rollout restart deployment/queue-stub

## The perimeter, and why it is not an oversight

KEDA drives this fleet **to and from zero only**. It does not scale 1 → N.

The two halves take different paths. Activation and idling go through
KEDA's own scale executor, which writes the `scale` subresource directly
(`scaleClient.Scales(ns).Update`) — exactly what ADR-0009 D1 exposes.
Everything above that boundary belongs to the HPA, and the HPA rejects a
`FleetService`:

    ScalingActive=False  InvalidSelector:
    the HPA target's scale is missing a selector

Autoscaling defines that field as *"the label query over pods that should
match the replicas count"*, and a fleet has none to give: it labels its
children — `VllmService` objects — in their metadata, while the pods carry
`app` and `managed-by` and nothing ties a pod to a fleet.

Adding the label to the pods is not available either. The same label map
feeds the Deployment's `LabelSelector`, which is immutable after creation,
so the change would break apply on every existing Deployment — the e2e job
asserts that rejection on purpose.

A `.status.selector` field could be declared and the HPA would accept it,
since the metric KEDA registers is `external` and carries its own
selector. It would also be a field claiming to select pods that selects
nothing. Not done, for the same reason ADR-0010 D1 declined to publish a
bound no writer could produce.

`maxReplicaCount` is therefore 1. A larger number would promise
proportional scaling that does not happen.

## Why scale-to-zero is the case worth having

A replica costs roughly eighteen seconds to come back — measured by the
[cold-start probe](https://github.com/MicheleCampi/vllm-coldstart-probe),
where kernel I/O is only ~7% of it and the rest is GPU warm-up. That is
the whole reason this operator treats warmth as a lifecycle state, and a
fleet that drops to zero on an idle queue and returns on demand is the
behaviour the arc was built for.

## What is not measured here

The trigger reads a stub, not vLLM. The activation latency above is a
pause container starting, not a replica loading weights — on real
hardware the cold start dominates it. The stub exists so the control path
can be exercised without a GPU, and it does not pretend to be more.
