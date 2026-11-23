# ADR-0028: A serving pod turns NotReady before SIGTERM

Status: Accepted; unit-tested; rehearsed on kind in CI (run 45264021013); not yet run live
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
   (`pkg/kubelet/nodeshutdown/nodeshutdown_manager.go`, `killPods`, sets
   only the phase and a `DisruptionTarget` condition, applied when the pod
   is terminal).
3. An EndpointSlice endpoint is `ready` when the pod is Ready and has no
   deletionTimestamp (`staging/src/k8s.io/endpointslice/utils.go`). With
   no deletionTimestamp, only a failing readiness probe removes it; the
   kubelet keeps probing during termination for exactly that reason
   (`pkg/kubelet/prober/prober_manager.go`, `AddPod`).
4. The ADR-0019 pre-stop sleep (110 s since ADR-0024) keeps the server up
   and its `/health` answering, so the endpoint stayed ready for the whole
   sleep, and SIGTERM reached a server that was still routed: 110 s plus
   the hook's start is the 111 s of the evidence. Then the readiness probe
   allowed 60 failures (5 min), a budget from before the startup probe of
   v0.16.0 gated the cold start.
5. A drain through the API (eviction, Karpenter, a rollout) sets the
   deletionTimestamp, so ADR-0019 holds there: the game day's Spot
   interruption (S1') lost no request of 97.

## Decisions

- Serving pods (GPU, or CPU with `cpu` and `memory`) get a pre-stop hook
  that writes `/tmp/vllm-coldstart-draining`, then sleeps the same 110 s,
  in `python3 -I -S` from the image, as the startup probe of v0.16.0
  already requires.
- Their readiness probe is an exec probe in the same interpreter: it fails
  while the flag exists, else it GETs the health path (8 s timeout). Period
  5 s, timeout 10 s, 3 failures: a draining or dead server is NotReady
  within 15 s, inside the sleep, in every termination path.
- The placeholder and the simulator (no python, not serving) keep the
  native sleep and the HTTP readiness probe of ADR-0019.
- A CI job, `node-shutdown`, reproduces fact 2 on kind: two workers with
  graceful node shutdown on (150 s and 30 s, as EKS), D-Bus and logind
  added, and logind's `PrepareForShutdown` signal sent to the node of one
  of two replicas, under a client that sends a request every 0.2 s through
  the Service, from the control plane, without retry. The pod spec of this
  release must lose no request; the v0.17.0 pod spec, as a control, must
  lose some, or the job could not see the defect.
- Rejected: a client or gateway retry alone (it hides the defect from the
  SLI and needs a second endpoint at the right moment); a preStop that
  deletes its own pod through the API (a service account token and RBAC in
  every serving pod); a readiness gate set by a controller watching node
  shutdown (a new component for what the kubelet already reports).

## Consequences

- Serving pods roll once on upgrade (the pod template changes).
- A probe starts one Python interpreter every 5 s, without site-packages:
  a few milliseconds of CPU.
- A server that stops answering `/health` leaves the Service after 15 s
  instead of 5 min. The cold start is not affected: readiness runs only
  after the startup probe has passed.

## Rehearsed, 2026-11-23

CI run 45264021013, job `node-shutdown` (kind v0.33.0, node v1.36.4, logind
`InhibitDelayMaxUSec` 150 s on both workers, so the kubelet's shutdown
manager was active):

- this release: target pod `Succeeded` 111.3 s after `PrepareForShutdown`;
  607 requests in the window, **0 failed**; the target answered 26 requests
  in the 10 s before the signal and its last at +109.8 s;
- control, the v0.17.0 pod spec: target `Succeeded` after 110.3 s; 601
  requests, **1 failed, at +110.0 s**, when SIGTERM reached a pod still
  routed: the EKS game day's failure (111 s after the terminate), reproduced.

## What this ADR does not prove

- Not on EKS yet: the next live session of eks-llm-inference-platform
  repeats S3' with this release.
- A request already in flight on the node when the instance's network goes
  down is still lost; only the client can retry it (eks-llm-inference-
  platform: one retry on connection errors in the game day probe).
- The simulator path keeps the ADR-0019 behaviour under a node shutdown.
