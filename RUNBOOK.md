# Runbook

Operational scenarios for vllm-coldstart-operator, structured Detection → Diagnosis → Fix. Scope is the control plane this operator owns; data-plane (real vLLM) failures are out of scope until the GPU integration on the roadmap lands.

## 1. Operator process exits immediately on start

**Detection.** `vllm-coldstart-operator` exits within seconds of starting; logs show an error before the "starting controller" line, or a connection error to the API server.

**Diagnosis.** The operator builds its client from the ambient kubeconfig (`Client::try_default`). The usual causes are: no reachable cluster in the current context, an expired or wrong kubeconfig, or the context pointing at a cluster that is down.

**Fix.** Confirm the cluster is reachable with `kubectl cluster-info`. Confirm the current context is the intended one with `kubectl config current-context`. If `kubectl` works but the operator does not, they are reading different kubeconfigs — check the `KUBECONFIG` environment variable the operator runs with.

## 2. Applying a VllmService is rejected by the API server

**Detection.** `kubectl apply -f` of a VllmService returns an error such as "no matches for kind VllmService" or "unknown field".

**Diagnosis.** Either the CRD is not installed, or the manifest uses field names that do not match the schema. The spec uses camelCase (`warmupStrategy`), not snake_case.

**Fix.** Install or reinstall the CRD: `cargo run --bin crdgen > deploy/crd.yaml && kubectl apply -f deploy/crd.yaml`. Confirm it is established: `kubectl get crd vllmservices.inference.michelecampi.dev`. Check the manifest field names against `deploy/examples/qwen-7b.yaml`.

## 3. VllmService created but no Deployment appears

**Detection.** `kubectl get vllmservice` shows the resource, but `kubectl get deployment <name>` returns NotFound, and the status phase stays empty or Pending.

**Diagnosis.** Most often the operator is not running, or is running against a different cluster/namespace than the one the VllmService was created in. Less often, the operator lacks RBAC permission to create Deployments (relevant when running in-cluster rather than as a local process).

**Fix.** Confirm the operator process is running and its logs show a reconcile for the resource ("reconciling VllmService ..."). Confirm operator and resource target the same cluster. If running in-cluster, confirm the ServiceAccount has create/patch on deployments.

## 4. Deployment exists but status never reaches Ready

**Detection.** Status stays at Pending or Warming; `kubectl get deployment <name>` shows ready replicas below desired.

**Diagnosis.** This is the control plane correctly reporting that the pods are not ready — the operator is working; the pods are the problem. Inspect the pods: image pull failure, scheduling failure (no node fits), or CrashLoopBackOff.

**Fix.** `kubectl describe deployment <name>` and `kubectl get pods -l app=<name>` to find the pod state, then `kubectl describe pod <pod>` for events (ImagePullBackOff, Insufficient cpu/memory, etc.). The status reaching Ready follows automatically once the pods become ready — no operator action needed.

## 5. Status looks stale after editing the spec

**Detection.** A spec change (e.g. replicas) is applied, but the status takes longer than expected to reflect it.

**Diagnosis.** The reconcile requeues on a slow interval once Ready, and reacts to owned-Deployment events in between. A spec edit triggers a reconcile, but the status reflects Deployment readiness, which itself takes time to converge after a scale change.

**Fix.** This is expected convergence, not a fault. Watch it settle with `kubectl get vllmservice <name> -w`. If it genuinely never converges, treat it as scenario 4 (inspect the pods).

## 6. Deleting a VllmService leaves the Deployment behind

**Detection.** After `kubectl delete vllmservice <name>`, the Deployment is still present.

**Diagnosis.** Garbage collection is performed by Kubernetes via the owner reference the operator sets, not by operator cleanup code. If the Deployment was created without the owner reference (e.g. an old operator version, or a manually created Deployment of the same name), it will not be collected.

**Fix.** Confirm the owner reference: `kubectl get deployment <name> -o jsonpath='{.metadata.ownerReferences}'`. If absent, delete the Deployment manually and let the current operator recreate it correctly on the next reconcile.
