# ADR-0006: Disaggregation-aware orchestration builds on the InferencePool contract

Status: Accepted; D1 superseded 2026-09-19 (see postscript)
Date: 2026-07-11

## Context

The operator's fleet layer is validated but role-blind. ADR-0003..0005 gave
it placement, bounded reschedule, and a make-before-break replacement path
measured at 57s Ready with zero cross-service errors (3xA10, commit
`0f00486`) — but every vLLM pod is treated as an identical monolith. In a
prefill/decode disaggregated deployment that assumption is wrong in both
directions: it wastes protection on stateless pods and under-protects
stateful ones.

The upstream state of the art (verified 2026-07-11, full study in
`docs/notes/2026-07-11-gie-study.md`) is: Gateway API Inference Extension
v1.5.0 with **InferencePool graduated to v1 stable**; the EPP and its
disaggregation plugins living in `llm-d/llm-d-router`. Disaggregation does
**not** use separate pools per role: prefill and decode pods share a single
InferencePool, separated by the `llm-d.ai/role` label and EPP
schedulingProfiles. The `disagg-profile-handler` picks the decode pod first
and the `prefix-based-pd-decider` triggers remote prefill only when the
non-cached prompt suffix exceeds a threshold — disaggregation is
**per-request and cache-driven**, not static. A multi-pool standard
("independently scaling pools") is roadmap, not reality.

This fixes the division of responsibility. The EPP already does per-request
scheduling, cache-aware, and it is not this operator's job to redo it. What
nobody covers is role-aware **pod lifecycle**: warmth, placement, recovery.
That gap is exactly the operator's existing domain, extended per role.

Why it matters is quantified. The EPP scores endpoints by prefix-cache
state, and the P/D decision itself is a function of prefix hit rate. A
placement or recovery policy that destroys cache content shifts an agentic
workload's regime from H2 toward H0 — a measured cost of up to **-69.2%
tokens/joule** (18/18 confirmation matrix, Qwen2.5-32B on H100,
agentic-kv-energy-experiment). Orchestration policy is energy policy.

Five decisions follow, D1-D5.

## Decision

### D1 — Roles are a refinement of `FleetService`, not a new CRD

`FleetService.spec` gains a `roles` map: role name -> replicas, resources,
warmth policy. The P/D graph is a partition of a fleet, and everything the
roles need — placement pinning, bounded reschedule, drain-and-hold — is the
machinery ADR-0003..0005 already validated. A role is a dimension of an
existing placement, not a new object.

Rejected alternative: a dedicated CRD (e.g. `DisaggregatedService`)
composing FleetServices per role. Rejected because it duplicates the
reconcile machinery at a second scope — the same control-plane-above-a-
control-plane anti-pattern ADR-0003 rejected — and because cross-role
decisions (D4) need a single reconciler seeing both roles at once.

### D2 — InferencePool v1 is the routing contract; reference by default, own optionally

The operator's side of the contract is minimal: label managed pods with
`llm-d.ai/role` per the llm-d convention and guarantee the InferencePool
`selector` matches them. `FleetService.spec` gains an `inferencePoolRef`;
by default the operator **references** an existing pool (the common case: a
llm-d Helm deployment already owns it, and one EPP serves exactly one pool).
An opt-in flag lets the operator create and own the pool for standalone
installs.

Rejected alternative 1: always owning the InferencePool as a child
resource. Rejected: fights the existing llm-d deployment model for zero
benefit in the common case.

Rejected alternative 2: an operator-side pool/role abstraction parallel to
GIE (e.g. one pool per role). Rejected: the multi-pool standard does not
exist yet; single-pool + role labels is what ships today, and parallel
primitives become migration debt the day upstream generalizes.

### D3 — Warmth is per-role and asymmetric

Decode warmth has two levels: `ModelLoaded` (weights up, /health green — the
current definition) and `CacheWarm` (live prefix-cache signal, window-delta
on `vllm:prefix_cache_hits/queries_total` — the same metrics path already
built and validated in inferscope `is-metrics`). Prefill warmth is
`ModelLoaded` only: the prefiller is stateless between requests, KV is
handed off per-request via the `x-prefiller-host-port` flow.

Rejected alternative: one warmth scale for all roles (status quo). Rejected
because it is wrong on both sides — it implies a freshly replaced decode pod
is as good as the one it replaced (the EPP's prefix scorer says otherwise
and will penalize it until it re-warms), and it implies prefill pods have
cache state worth protecting (they do not).

### D4 — Recovery and placement differentiate by role

- **Decode**: warm spare + cache-aware drain-and-hold. Replacing a decode
  pod resets its prefix score at the EPP, so lost cache content is a
  first-class cost: surfaced in status (fed by the D3 signal) and weighed in
  the decision to replace versus hold. This is where the -69.2% tok/J
  regime shift lives.
- **Prefill**: fast replace, no hold, plus a stranded-memory guard —
  prefiller crash stranding transferred KV is a failure mode the upstream
  project itself documents.

The validated make-before-break mechanism (57s replacement) is kept as-is;
what becomes role-conditional is the *policy* deciding when a spare is
staged and whether a drain holds.

Rejected alternative: uniform make-before-break for both roles. Rejected:
it spends a warm spare on stateless prefillers and under-protects decoders,
whose replacement cost is not the 57s gap but the cache regime shift.

### D5 — Scope is P/D; E/P/D is out

Prefill/decode only. Encode disaggregation (E/PD, E/P/D) is an active PoC
in both vLLM and llm-d-router and subject to change. Nothing in D1-D4
hard-codes two roles — `roles` is a map and the label convention already
admits more values — so deferral costs nothing and chasing a moving PoC
buys nothing.

## Consequences

- `FleetService` grows `spec.roles` and `spec.inferencePoolRef`; status
  grows per-role conditions including `CacheWarm` for decode. Existing
  single-role FleetServices remain valid (empty `roles` = current
  behaviour).
- `CacheWarm` adds a runtime dependency on vLLM Prometheus metrics being
  scrapable. Degraded mode is defined: if the signal is unavailable, decode
  warmth falls back to `ModelLoaded` and recovery behaves as today.
- Validation path: sim-first on kind against llm-d-inference-sim (epponly
  topology already running), then a GPU session with the standard node-off
  dress rehearsal gate. Implementation is the August block; this ADR is
  design only.
- This ADR encodes the upstream state **as of 2026-07-11** (GIE v1.5.0,
  EPP in `llm-d/llm-d-router`, single-pool + role labels). If the roadmap
  item "disaggregated serving with independently scaling pools" ships, D2
  must be revisited; the reference-not-own default is chosen partly to
  minimize that blast radius.
- The agentic-kv-energy article (go-live ~2026-08-11) closes on the
  disaggregation-aware angle and references this ADR as the orchestration
  consequence of the measured energy signature.

## Postscript, 2026-09-19 — D1 superseded

This ADR rejected "a dedicated CRD (e.g. `DisaggregatedService`) composing
FleetServices per role" on the grounds that it would duplicate the reconcile
machinery. That reasoning assumed no such object existed. One did:
`DisaggregatedSet`, in `kubernetes-sigs/lws`, shipped in v0.9.0 on 2026-06-17
— three weeks before this ADR was written — under KEP 766, filed 2026-03-05.
The alternative was dismissed without knowing it was already available.

What that object already owns, at v0.10.0 (API group
`disaggregatedset.x-k8s.io/v1`):
- `spec.roles`: a list of roles, each carrying its own
  `LeaderWorkerSetTemplateSpec`; the set handles rollout across roles and
  labels the LeaderWorkerSets and both pod templates with
  `disaggregatedset.x-k8s.io/{name,role,slice,revision}`;
- per-role scaling: `Static` inline, or `External`, where the controller
  creates a `DisaggregatedSetRoleScaler` exposed through `/scale` for HPA;
- `spec.slices` and `spec.placementPolicy` (`None`, `ExclusiveSlice`,
  `ExclusiveTopology`, over a node-label `topology` key), which co-locate a
  slice's roles in one domain and keep slices apart;
- `status.roles[].readyReplicas`.

D1 proposed adding a `roles` map to `FleetService`. Building it now would
duplicate an object the ecosystem already reads: `llm-d-router` gates
prefill/decode pairing on `disaggregatedset.x-k8s.io/revision` (PR #2141,
merged 2026-08-05).

### The decision that replaces D1

`FleetService` does not own roles. It observes them.

Nothing in the LWS DisaggregatedSet controller reads engine metrics: across
its nine non-test source files — six controllers, two utils, one webhook —
`metrics`, `scrape` and `prometheus` appear zero times. Its placement is
topological — affinity terms over a `topologyKey`, saying together or apart,
never which node. Its readiness is pod readiness.

That leaves this operator exactly the claim ADR-0002 already makes: Ready
means warm, not merely running. Concretely, and each as its own follow-up:
- per-role warmth (`CacheWarm` for decode, from the engine's prefix-cache
  series) as a condition this operator publishes;
- placement by warmth within the topology constraints the DisaggregatedSet
  already imposes, rather than instead of them;
- make-before-break recovery on preemption, measured at 57s Ready with a
  2.3s service gap (this ADR's own Context, and the README's table), which
  the DisaggregatedSet controller does not do.

### No dependency on the LWS API

Observing roles does not require reading `DisaggregatedSet` objects or
importing the LWS Go types. The controller injects its five labels — `app`
plus `disaggregatedset.x-k8s.io/{name,role,slice,revision}` — into both the
worker and the leader pod templates when it creates each LeaderWorkerSet
(`lws_manager.go`, Create): "Inject system labels (role, name, revision) into
pod templates. These don't come from the user's spec — services select pods
by them." Automatic labels win over user-supplied ones (`mergeLabels`).

So the pods carry the role. This operator watches VllmService,
FleetService, the Deployments and
Services it owns, and NodeState; it does not watch pods today. Reading the
role means adding a pod watch filtered by that label, which is a smaller
change than tracking an alpha-stage API and keeps LWS out of the build.

The limit is that a label contract is weaker than a versioned API: it is not
covered by deprecation policy, and a rename would be silent. Against that,
these are the labels the LWS controller's own Services select pods on
(`service_manager.go`), so a rename would break more than this operator.

Not verified: whether a pod under a DisaggregatedSet also carries
`llm-d.ai/role` for the InferencePool selector of D2. That belongs to the
implementation, not to this decision.

### D2 stands

The single-pool-plus-labels contract is still documented: llm-d's coordinator
architecture describes one EPP over one InferencePool spanning all three
roles, with separate pools "chosen by configuration". `llm-d.ai/role` remains
in use across llm-d's deploy components and docs. No revision needed.

### No date

ADR-0006 said implementation was "the August block" and it did not happen.
This postscript records a decision, not a schedule.
