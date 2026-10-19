# Scale-to-zero with KEDA

What works, what does not, and why — measured on kind, 2026-08-17. The
`keda` CI job repeats the round trip on every push, with the KEDA 2.21.0
chart pinned by SHA-256 and its three images by digest
(`.github/workflows/ci.yml`).

    helm repo add kedacore https://kedacore.github.io/charts && helm repo update
    helm install keda kedacore/keda --version 2.21.0 -n keda --create-namespace --wait
    kubectl apply -f queue-stub.yaml
    kubectl apply -f scaledobject-scale-to-zero.yaml

With the stub reporting `{"waiting": 0}` the fleet drains to zero children
in about twenty seconds. Set it to a positive number and the fleet comes
back in under ten:

    kubectl create configmap queue-stub-cfg \
      --from-literal=queue.json='{"waiting": 4}' --dry-run=client -o yaml \
      | kubectl apply -f - && kubectl rollout restart deployment/queue-stub

## Two paths: to and from zero, and 1 → N

KEDA drives this fleet **to and from zero** and, since ADR-0014, **from 1
to N** as well. The two halves take different paths.

Activation and idling go through KEDA's own scale executor, which writes
the `scale` subresource directly (`scaleClient.Scales(ns).Update`),
exactly what ADR-0009 D1 exposes. No HPA is involved.

Everything above that boundary belongs to the HPA, and until ADR-0014 the
HPA rejected a `FleetService`:

    ScalingActive=False  InvalidSelector:
    the HPA target's scale is missing a selector

Autoscaling defines that field as *"the label query over pods that should
match the replicas count"*, and a fleet had none to give: it labeled its
children, `VllmService` objects, and not their pods. The label could not
simply be added to the pods, because the same map fed the Deployment's
`LabelSelector`, which is immutable after creation; the e2e job asserts
that the API server rejects such a change.

ADR-0014 separates the two. A fleet's child adds the fleet label to its
pods' template only, leaving the Deployment's selector as it was, byte for
byte, and the fleet publishes the matching query as `.status.selector`
(`inference.michelecampi.dev/fleet=<name>`). The field selects exactly the
fleet's pods, unlike the one ADR-0009 declined. The cost is one rollout
of each fleet child when the operator is upgraded, surge first
(`maxSurge: 1`, `maxUnavailable: 0`).

`scaledobject-scale-to-zero.yaml` keeps `maxReplicaCount: 1`: it is the
zero round trip the `keda` CI job repeats. `scaledobject-scale-out.yaml`
is the 1 → N example the `scale-out` CI job runs: a Prometheus trigger on
the fleet's running plus waiting requests, averaged over a minute, with a
threshold of 1.3 per replica and a cap of 3 replicas.

## Why scale-to-zero is the case worth having

In eager mode, the operator's default, constructing the vLLM engine for a
7B model on an A10 with a cold page cache took 11.7 to 19.0 s, measured by
the [cold-start probe](https://github.com/MicheleCampi/vllm-coldstart-probe/blob/main/analysis/CAPTURES.md); a replica
serves only after that. That is
the whole reason this operator treats warmth as a lifecycle state, and a
fleet that drops to zero on an idle queue and returns on demand is the
behaviour the arc was built for.

## What is not measured here

The trigger reads a stub, not vLLM. The activation latency above is a
pause container starting, not a replica loading weights — on real
hardware the cold start dominates it. The stub exists so the control path
can be exercised without a GPU, and it does not pretend to be more.
