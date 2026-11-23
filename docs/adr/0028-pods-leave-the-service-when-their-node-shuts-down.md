# ADR-0028: Pods leave the Service when their node shuts down

Status: Accepted; unit-tested; CI job `node-shutdown` on kind; not yet run live
Date: 2026-11-23
Amends: ADR-0019 (a pod leaves the Service before it stops serving)

## Context

The EKS game day re-run of 2026-11-21 (eks-llm-inference-platform,
`docs/evidence/gameday-rerun-2026-11-21.md`, scenario S3') terminated the
instance of one of two replicas with Karpenter scaled to 0, so nothing
drained the node through the API. Every request succeeded until 19:36:07;
the one sent at 19:36:13, about 111 s after `terminate-instances`
(19:34:22), timed out after 30 s; the pod was `Succeeded` at 19:36:32. The
availability SLI of the window was 99.32 %, below its 99.5 % objective.

## Facts that constrain the design

1. EKS AL2023 nodes on Kubernetes 1.34 and later run the kubelet with
   graceful node shutdown on: `shutdownGracePeriod` 150 s, 30 s of it for
   critical pods (amazon-eks-ami, `nodeadm/internal/kubelet/config.go`,
   `withVersionToggles`). On an instance shutdown the kubelet terminates
   its pods itself, with grace min(pod grace, 120 s), and runs the
   pre-stop hook.
2. It does so without the API: the pod gets no deletionTimestamp
   (`pkg/kubelet/nodeshutdown/nodeshutdown_manager.go`, `killPods`).
3. An EndpointSlice endpoint is `ready` when the pod is Ready and has no
   deletionTimestamp (`staging/src/k8s.io/endpointslice/utils.go`).
4. **Readiness cannot remove the pod on this path.** `SyncTerminatingPod`
   applies the shutdown manager's status function (phase `Failed`) to the
   kubelet's status cache when the termination starts; the readiness worker
   reads that cache, sees a terminal phase and exits
   (`pkg/kubelet/prober/worker.go`, "Pod is terminated, exiting probe
   worker"), and the API keeps the last result, Ready, until the containers
   stop. Seen on kind (CI run 45287001456, kubelet at verbosity 4): the
   worker exited 4 s after "Shutdown manager killing pod", and the pod stayed
   Ready=True for its whole 110 s pre-stop sleep. Open upstream as
   kubernetes/kubernetes#105780 and #116965.
5. With the ADR-0019 pre-stop sleep (110 s since ADR-0024) the server stays
   up and routed for the whole sleep, and SIGTERM reaches a routed server:
   110 s plus the hook's start is the 111 s of the evidence.
6. The kubelet does tell the API at once: its Node turns Ready=False with
   the message "node is shutting down" (`ShutdownStatus`, read by the
   `ReadyCondition` setter).
7. A drain through the API (eviction, Karpenter, a rollout) sets the
   deletionTimestamp, so ADR-0019 holds there: the game day's Spot
   interruption (S1') lost no request of 97. In production Karpenter's
   interruption handling drains on EC2 state changes too; S3' removed it on
   purpose, and this ADR covers the path without it.

## Decisions

- The operator watches Nodes. When a node's Ready condition is not True
  and its message says "node is shutting down", it deletes through the API
  the pods it manages on that node (label
  `app.kubernetes.io/managed-by=vllm-coldstart-operator`, field
  `spec.nodeName`), with their own grace period, skipping pods already
  deleting. The deletionTimestamp takes the endpoint out of the ready set
  within a second or two, the ADR-0019 sleep drains it, the kubelet's
  termination goes on, and the Deployment starts the replacement at once.
- No other NotReady triggers it: a node NotReady for a network or kubelet
  fault is the node lifecycle controller's, and deleting pods there would
  turn a partition into a restart of every replica on it.
- RBAC: `nodes` gains `watch`, `pods` gains `list`; `delete` on pods exists
  since D-0145. The chart job checks the operator's ServiceAccount can use
  them and that its node watch starts.
- A CI job, `node-shutdown`, reproduces facts 2 and 4 on kind: two workers
  with graceful node shutdown on (150 s and 30 s, as EKS), D-Bus and logind
  added, and logind's `PrepareForShutdown` signal sent to the node of one
  of two replicas, under a client that sends a request every 0.2 s through
  the Service, from the control plane, without retry. With the operator
  running no request may be lost; with the operator stopped (v0.17.0 had no
  node watch) some must be, or the job could not see the defect.
- Rejected:
  - a pre-stop hook that turns readiness off (a drain flag the readiness
    probe checks). Built and run first: the flag was there and the check
    failed by hand, but by fact 4 the kubelet no longer probed; one run lost
    no request and the next lost one at +110.0 s, so the first green run was
    luck of timing;
  - a client or gateway retry alone (it hides the defect from the SLI and
    needs a second endpoint at the right moment);
  - a pre-stop hook that deletes its own pod (a service account token and
    RBAC in every serving pod);
  - a readiness gate set by the operator (the kubelet computes Ready, and by
    fact 4 it stops updating this pod's status until the end).

## Consequences

- One more watch in the operator: all Nodes, a few events per node per
  minute. A shutdown costs one pod list on that node and one delete per
  managed pod.
- The pod template does not change: no rollout on upgrade.
- The replacement starts as soon as the node reports the shutdown, not
  when the old pod ends: for a model with a long cold start that is up to
  110 s earlier.

## What this ADR does not prove

- Not on EKS yet: the next live session of eks-llm-inference-platform
  repeats S3' with this release.
- The operator must be running: if its own pod is on the node that shuts
  down, it is terminated with the others and may miss the event; a second replica of the operator, or the node lifecycle controller
  after the node stops reporting, is the fallback.
- A request already in flight on the node when the instance's network goes
  down is still lost; only the client can retry it (eks-llm-inference-
  platform: one retry on connection errors in the game day probe).
