//! Fleet controller: reconciles `FleetService` into a set of owned
//! `VllmService` objects, one per replica, each pinned to a node chosen by
//! the pure placement planner.
//!
//! Design (ADR-0003 / ADR-0004):
//! - The controller owns *policy*: it reads `NodeState` objects (warmth,
//!   utilisation, load), converts them to the pure `NodeCandidate` input, and
//!   calls `plan_initial_placements` to choose one node per slot.
//! - The default scheduler owns *mechanism*: each owned `VllmService` carries
//!   `node_name`, which `build_deployment` translates into a
//!   `kubernetes.io/hostname` nodeSelector. The scheduler still does resource
//!   fit, admission and taint/toleration before binding.
//!
//! Node changes are observed two ways: every reconcile lists `NodeState`
//! fresh and plans from that snapshot, and a `.watches(NodeState)` mapper
//! (ADR-0005 dec.4) wakes the fleet reactively on a preemption notice instead
//! of waiting for the requeue interval.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use k8s_openapi::chrono::{DateTime, Utc};
use kube::{
    api::{Api, DeleteParams, ListParams, ObjectMeta, Patch, PatchParams},
    core::ErrorResponse,
    runtime::{
        controller::{Action, Controller},
        reflector::ObjectRef,
        watcher,
    },
    Client, Resource, ResourceExt,
};
use serde_json::json;
use tracing::{info, warn};

use vllm_coldstart_operator::fleet_placement::{select_replacement_node, NodeCandidate};
use vllm_coldstart_operator::fleet_planning::plan_initial_placements;
use vllm_coldstart_operator::fleet_types::{
    fleet_phase_for, placement_phase_for, surplus_hysteresis, FleetService, FleetServiceStatus,
    NodeState, PlacementDecisionInputs, PlacementStatus, PlacementStrategy,
};
use vllm_coldstart_operator::metrics::Metrics;
use vllm_coldstart_operator::{VllmService, VllmServiceSpec};

/// Field manager for server-side apply of owned VllmService objects. Distinct
/// from the VllmService reconciler's manager so the two do not contend on
/// shared object fields: the fleet owns the child spec, the VllmService
/// reconciler owns the child status.
const FLEET_MANAGER: &str = "fleet-controller";

/// Requeue cadence. Matches the VllmService reconciler's steady-state requeue
/// so fleet-level and service-level loops observe on the same clock.
const REQUEUE: Duration = Duration::from_secs(30);

#[derive(Debug, thiserror::Error)]
pub enum FleetError {
    #[error("Kube API error: {0}")]
    Kube(#[from] kube::Error),
    #[error("FleetService is missing its namespace")]
    MissingNamespace,
}

impl FleetError {
    fn metric_label(&self) -> &'static str {
        match self {
            FleetError::Kube(_) => "kube",
            FleetError::MissingNamespace => "missing_namespace",
        }
    }
}

pub struct FleetContext {
    pub client: Client,
    pub metrics: Metrics,
}

/// Convert a cluster `NodeState` into the pure `NodeCandidate` the planner
/// consumes. The node name comes from metadata; the three placement signals
/// come from `.status`, which is `Option` on a CustomResource — a node that
/// has never been reported yet has no status and is skipped by the caller.
/// ADR-0008 D2: `now` is passed in rather than read here, and read once per
/// reconcile by the caller. Two candidates built moments apart from the same
/// instant then carry comparable ages, and the comparator stays a pure function
/// of its inputs.
fn age_secs(observed_at: Option<&String>, now: DateTime<Utc>) -> Option<i64> {
    let t = observed_at?;
    // An unparseable timestamp yields None, which under a configured horizon
    // ranks as never observed. Silently treating it as fresh would let a
    // malformed status outrank a correctly reported stale one.
    let parsed = DateTime::parse_from_rfc3339(t).ok()?;
    Some((now - parsed.with_timezone(&Utc)).num_seconds())
}

/// ADR-0008 D1: what the planner ranked on for the node it chose, recorded as
/// the comparator saw it. The horizon is applied here for the same reason it is
/// applied in the comparator: a signal the reporter published but D2 expired did
/// not inform the decision, so reporting it would describe an input the planner
/// never had.
fn decision_inputs(
    candidates: &[NodeCandidate],
    node: &str,
    strategy: &PlacementStrategy,
    horizon_secs: Option<i64>,
) -> Option<PlacementDecisionInputs> {
    let c = candidates.iter().find(|c| c.name == node)?;
    let fresh = |v: Option<f32>, age: Option<i64>| -> Option<f32> {
        match (horizon_secs, age) {
            (None, _) => v,
            (Some(_), None) => None,
            (Some(h), Some(a)) if a >= 0 && a <= h => v,
            _ => None,
        }
    };
    Some(PlacementDecisionInputs {
        strategy: format!("{strategy:?}"),
        warmth: Some(format!("{:?}", c.warmth)),
        tokens_per_joule: fresh(c.tokens_per_joule, c.tokens_per_joule_age_secs),
        kv_cache_hit_rate: fresh(c.kv_cache_hit_rate, c.kv_cache_hit_rate_age_secs),
        gpu_utilization: c.gpu_utilization,
        active_service_count: c.active_service_count,
    })
}

fn node_state_to_candidate(ns: &NodeState, now: DateTime<Utc>) -> Option<NodeCandidate> {
    let status = ns.status.as_ref()?;
    Some(NodeCandidate {
        name: ns.name_any(),
        warmth: status.warmth.clone(),
        gpu_utilization: status.gpu_utilization,
        active_service_count: status.active_service_count,
        kv_cache_hit_rate: status.kv_cache_hit_rate,
        kv_cache_hit_rate_age_secs: age_secs(status.kv_cache_hit_rate_observed_at.as_ref(), now),
        tokens_per_joule: status.tokens_per_joule,
        tokens_per_joule_age_secs: age_secs(status.tokens_per_joule_observed_at.as_ref(), now),
    })
}

/// Number of this fleet's own placements currently pinned to each node.
/// The controller knows where it has already placed; that knowledge must not
/// depend on an external reporter refreshing NodeState.activeServiceCount in
/// time. Rehearsal finding (item 4): with a stale reporter the replacement
/// co-located two placements on one node while a free warm spare existed.
fn own_placements_per_node(current: &BTreeMap<usize, String>) -> BTreeMap<String, i32> {
    let mut counts = BTreeMap::new();
    for node in current.values() {
        *counts.entry(node.clone()).or_insert(0) += 1;
    }
    counts
}

/// Build an owned VllmService for one placement: the fleet's model/template
/// plus the chosen node, pinned via node_name (ADR-0004). Name is
/// deterministic (`<fleet>-<index>`) so re-reconciles apply the same object
/// rather than creating duplicates.
fn build_owned_vllm_service(
    fleet: &FleetService,
    index: usize,
    node_name: &str,
) -> Result<VllmService, FleetError> {
    let fleet_name = fleet.name_any();
    let ns = fleet.namespace().ok_or(FleetError::MissingNamespace)?;
    let child_name = format!("{fleet_name}-{index}");

    let t = &fleet.spec.template;
    let spec = VllmServiceSpec {
        model: fleet.spec.model.clone(),
        replicas: 1,
        warmup_strategy: Default::default(),
        image: t.image.clone(),
        gpu: t.gpu,
        health_path: t.health_path.clone(),
        runtime_class_name: t.runtime_class_name.clone(),
        extra_args: t.extra_args.clone(),
        model_cache_host_path: t.model_cache_host_path.clone(),
        node_name: Some(node_name.to_string()),
    };

    let mut labels = BTreeMap::new();
    labels.insert(
        "app.kubernetes.io/managed-by".to_string(),
        FLEET_MANAGER.to_string(),
    );
    labels.insert("inference.michelecampi.dev/fleet".to_string(), fleet_name);

    Ok(VllmService {
        metadata: ObjectMeta {
            name: Some(child_name),
            namespace: Some(ns),
            labels: Some(labels),
            owner_references: Some(vec![fleet.controller_owner_ref(&()).unwrap()]),
            ..Default::default()
        },
        spec,
        status: None,
    })
}

pub async fn reconcile(
    fleet: Arc<FleetService>,
    ctx: Arc<FleetContext>,
) -> Result<Action, FleetError> {
    let name = fleet.name_any();
    let _measurer = ctx.metrics.count_and_measure();
    let ns = fleet.namespace().ok_or(FleetError::MissingNamespace)?;
    info!(
        "reconciling FleetService '{}' in '{}': model={} replicas={}",
        name, ns, fleet.spec.model, fleet.spec.replicas
    );

    // 1. Snapshot the fleet-visible node states and convert to pure candidates.
    let node_states: Api<NodeState> = Api::all(ctx.client.clone());
    let states = node_states.list(&Default::default()).await?;
    // ADR-0008 D2: one clock read per reconcile, shared by every candidate.
    // Reading it per candidate would give two nodes different notions of "now"
    // within the same decision, which is a difference the ordering could see.
    let now = Utc::now();
    let candidates: Vec<NodeCandidate> = states
        .iter()
        .filter_map(|ns| node_state_to_candidate(ns, now))
        .collect();

    // Nodes that signalled a spot preemption notice. This is the only trigger
    // for a reschedule in v1 (ADR-0005 dec.1): warmth changes and node
    // disappearance deliberately do not move placements.
    let preempted: BTreeSet<String> = states
        .iter()
        .filter(|ns| {
            ns.status
                .as_ref()
                .map(|s| s.spot.preemption_notice_detected)
                .unwrap_or(false)
        })
        .map(|ns| ns.name_any())
        .collect();

    if candidates.is_empty() {
        warn!(
            "FleetService '{}': no reported NodeState objects, nothing to place",
            name
        );
        return Ok(Action::requeue(REQUEUE));
    }

    // 2. Read the current placements from the owned VllmService children,
    //    not from status: the children are the source of truth (ADR: a
    //    controller trusts its owned objects, status is a derived report).
    //    Map slot index -> currently pinned node, so stable placements are
    //    preserved verbatim across reconciles and do not oscillate when node
    //    warmth changes.
    let services: Api<VllmService> = Api::namespaced(ctx.client.clone(), &ns);
    let lp = ListParams::default().labels(&format!("inference.michelecampi.dev/fleet={name}"));
    let existing = services.list(&lp).await?;
    let mut current: BTreeMap<usize, String> = BTreeMap::new();
    // Observed readiness of each existing child, keyed by child name. A
    // placement is Ready only when its owned VllmService reports Ready (that
    // phase already folds in Deployment readiness + warmup).
    let mut child_ready: BTreeMap<String, bool> = BTreeMap::new();
    for child in &existing.items {
        // Child name is "<fleet>-<index>"; recover the slot index from the
        // suffix. Ignore any child that does not parse (not fleet-owned in
        // the expected shape).
        let child_name = child.name_any();
        let ready = child
            .status
            .as_ref()
            .map(|s| s.phase == "Ready")
            .unwrap_or(false);
        child_ready.insert(child_name.clone(), ready);
        if let Some(idx) = child_name
            .rsplit_once('-')
            .and_then(|(_, i)| i.parse::<usize>().ok())
        {
            if let Some(node) = &child.spec.node_name {
                current.insert(idx, node.clone());
            }
        }
    }

    // Previous per-placement phase from our own last status write. The fleet is
    // the sole writer of the placement lifecycle phase, so phases must be
    // advanced from here rather than reset to Pending every reconcile — else
    // the stateful phase (and the reschedule accounting the next pass builds on
    // it) would never persist across loops.
    let prev_phase: BTreeMap<String, String> = fleet
        .status
        .as_ref()
        .map(|s| {
            s.placements
                .iter()
                .map(|p| (p.vllm_service_ref.clone(), p.phase.clone()))
                .collect()
        })
        .unwrap_or_default();

    // ADR-0009 D4: the surplus counter is carried in our own last status for
    // the same reason the phase is — the fleet is its sole writer, and a
    // counter reset every reconcile would never reach the threshold.
    // ADR-0008 D1: what informed each existing placement, carried forward. A
    // placement made in an earlier reconcile keeps the inputs that produced it;
    // recomputing them here would report the node's state now, which answers a
    // different question and would quietly rewrite the record of a decision.
    let prev_decided: BTreeMap<String, PlacementDecisionInputs> = fleet
        .status
        .as_ref()
        .map(|s| {
            s.placements
                .iter()
                .filter_map(|p| {
                    p.decided_on
                        .clone()
                        .map(|d| (p.vllm_service_ref.clone(), d))
                })
                .collect()
        })
        .unwrap_or_default();

    let prev_surplus: BTreeMap<String, i32> = fleet
        .status
        .as_ref()
        .map(|s| {
            s.placements
                .iter()
                .map(|p| (p.vllm_service_ref.clone(), p.surplus_reconciles))
                .collect()
        })
        .unwrap_or_default();

    // Self-awareness: fold this fleet's own placements into the candidates'
    // load signal. Both fresh planning and replacement selection must see the
    // capacity the fleet itself has already consumed, even when the NodeState
    // reporter lags (on kind it is static). The tie-break order of the pure
    // seam (count before utilization) then does the right thing unchanged.
    let own_counts = own_placements_per_node(&current);
    let candidates: Vec<NodeCandidate> = candidates
        .into_iter()
        .map(|mut c| {
            // Fleet's own bookkeeping folded into the observed signal.
            // None + own 0 stays None (no fabricated measurement); any real
            // own load promotes, because it is knowledge the controller has
            // regardless of the reporter.
            let own = own_counts.get(c.name.as_str()).copied().unwrap_or(0);
            if own > 0 {
                c.active_service_count = Some(c.active_service_count.unwrap_or(0) + own);
            }
            c
        })
        .collect();

    // 3. Decide the node for each slot: preserve an existing pin, plan a
    //    fresh node only for slots that have no child yet. Planning for the
    //    missing slots still spreads load across candidates.
    let desired = fleet.spec.replicas.max(0) as usize;

    // Preemption pass (ADR-0005): a slot whose current pin sits on a preempted
    // node must move. The concurrency cap bounds the blast radius — count moves
    // already in flight (Draining/Rescheduling on a *healthy* node, i.e. already
    // consuming survivor capacity) and only start new moves up to the cap. A
    // Draining placement still on its preempted node is a drain-and-hold, not a
    // move in flight, so it does not consume the cap.
    let cap = fleet.spec.hysteresis.max_concurrent_reschedules.max(0) as usize;
    let in_flight_moves = fleet
        .status
        .as_ref()
        .map(|st| {
            st.placements
                .iter()
                .filter(|p| {
                    matches!(p.phase.as_str(), "Draining" | "Rescheduling")
                        && !preempted.contains(p.node_ref.as_str())
                })
                .count()
        })
        .unwrap_or(0);
    let mut budget = cap.saturating_sub(in_flight_moves);

    // Slots forced to Draining because their pin is preempted, plus the healthy
    // replacement chosen for them within budget. A slot with no replacement
    // (cap exhausted, or select_replacement_node returns None because every
    // survivor is Cold) stays pinned and drains-and-holds (ADR-0005 dec.3).
    // Replacement targets must exclude *every* preempted node, not just the one
    // being replaced: in a multi-node reclaim another preempted node is itself
    // draining and is not a safe destination. select_replacement_node already
    // drops the single node passed to it and rejects Cold; filtering the whole
    // preempted set here closes the multi-node case.
    let healthy_candidates: Vec<NodeCandidate> = candidates
        .iter()
        .filter(|c| !preempted.contains(c.name.as_str()))
        .cloned()
        .collect();

    let mut preempted_slots: BTreeSet<usize> = BTreeSet::new();
    let mut moved: BTreeMap<usize, String> = BTreeMap::new();
    for (i, node) in &current {
        if preempted.contains(node.as_str()) {
            preempted_slots.insert(*i);
            if budget == 0 {
                info!(
                    "FleetService '{}': preemption notice on '{}' — cap exhausted, slot {} drains and holds",
                    name, node, i
                );
                continue;
            }
            match select_replacement_node(
                &healthy_candidates,
                node.as_str(),
                &fleet.spec.placement.strategy,
                fleet.spec.placement.signal_max_age_seconds,
            ) {
                Some(target) => {
                    info!(
                        "FleetService '{}': preemption notice on '{}' — rescheduling slot {} to '{}'",
                        name, node, i, target
                    );
                    moved.insert(*i, target);
                    budget -= 1;
                }
                None => {
                    info!(
                        "FleetService '{}': preemption notice on '{}' — no healthy replacement for slot {}, drain-and-hold",
                        name, node, i
                    );
                }
            }
        }
    }

    let missing: Vec<usize> = (0..desired).filter(|i| !current.contains_key(i)).collect();
    // Fresh slots must not land on a preempted node either — it would need
    // rescheduling on the very next pass (same exclusion rationale as
    // replacement, ADR-0005).
    let fresh = plan_initial_placements(
        missing.len() as i32,
        &healthy_candidates,
        &fleet.spec.placement.strategy,
        fleet.spec.placement.signal_max_age_seconds,
    );
    let mut fresh_iter = fresh.into_iter();
    let mut slot_nodes: Vec<(usize, String)> = Vec::with_capacity(desired);
    for i in 0..desired {
        let node = match moved.get(&i) {
            // Preempted slot with a healthy replacement: pin moves to the target.
            Some(target) => target.clone(),
            None => match current.get(&i) {
                Some(existing_node) => existing_node.clone(),
                None => match fresh_iter.next() {
                    Some(n) => n,
                    None => continue, // no candidate available for this slot
                },
            },
        };
        slot_nodes.push((i, node));
    }

    // 4. Apply one owned VllmService per slot (server-side apply, distinct
    //    field manager). Deterministic child names => idempotent; preserved
    //    slots reapply the same node_name, so SSA is a no-op for them.
    let pp = PatchParams::apply(FLEET_MANAGER).force();
    let mut placements: Vec<PlacementStatus> = Vec::with_capacity(slot_nodes.len());
    for (index, node_name) in &slot_nodes {
        let child = build_owned_vllm_service(&fleet, *index, node_name)?;
        let child_name = child.name_any();
        services
            .patch(&child_name, &pp, &Patch::Apply(&child))
            .await?;
        // Advance the placement phase from its previous value via the pure
        // logic, rather than resetting to Pending. A slot flagged preempted
        // above forces Draining regardless of readiness (ADR-0005).
        let current_phase = prev_phase
            .get(&child_name)
            .map(String::as_str)
            .unwrap_or("Pending");
        let node_ready = child_ready.get(&child_name).copied().unwrap_or(false);
        let preemption = preempted_slots.contains(index);
        let phase = placement_phase_for(current_phase, node_ready, preemption);
        // Computed before the struct takes ownership of child_name. Existing
        // placements keep the inputs that produced them; only a slot decided in
        // this reconcile gets fresh ones.
        let decided = prev_decided.get(&child_name).cloned().or_else(|| {
            decision_inputs(
                &candidates,
                node_name,
                &fleet.spec.placement.strategy,
                fleet.spec.placement.signal_max_age_seconds,
            )
        });
        placements.push(PlacementStatus {
            vllm_service_ref: child_name,
            node_ref: node_name.clone(),
            phase: phase.to_string(),
            last_transition_time: String::new(),
            stable_since: String::new(),
            // In range by construction: this loop runs over 0..desired.
            surplus_reconciles: 0,
            decided_on: decided,
        });
    }

    // 4b. Scale-down (ADR-0009 D4). Slots beyond `desired` are surplus: the
    //     apply loop above never touches them, so without this pass they
    //     outlive the scale-down and `status.replicas` — the scale
    //     subresource's status path — would report `desired` instead of what
    //     is actually running, which is exactly the over-count D1 exists to
    //     prevent. Removal waits for `stable_reconciles_required` because
    //     deleting a replica is instant and recreating it costs a cold start;
    //     a surplus placement still waiting stays in `placements` and keeps
    //     being counted, since it is still serving.
    let dp = DeleteParams::default();
    for (index, node_name) in &current {
        if *index < desired {
            continue;
        }
        let child_name = format!("{name}-{index}");
        let prev = prev_surplus.get(&child_name).copied().unwrap_or(0);
        let (surplus_reconciles, remove) =
            surplus_hysteresis(prev, true, fleet.spec.hysteresis.stable_reconciles_required);
        if !remove {
            info!(
                "FleetService '{}': slot {} is surplus ({} of {} reconciles), holding",
                name, index, surplus_reconciles, fleet.spec.hysteresis.stable_reconciles_required
            );
            let current_phase = prev_phase
                .get(&child_name)
                .map(String::as_str)
                .unwrap_or("Pending");
            let node_ready = child_ready.get(&child_name).copied().unwrap_or(false);
            let phase = placement_phase_for(current_phase, node_ready, false);
            let decided = prev_decided.get(&child_name).cloned();
            placements.push(PlacementStatus {
                vllm_service_ref: child_name,
                node_ref: node_name.clone(),
                phase: phase.to_string(),
                last_transition_time: String::new(),
                stable_since: String::new(),
                surplus_reconciles,
                // Carry-forward only: this slot was decided in an earlier
                // reconcile and is on its way out, so there is no new decision
                // to record.
                decided_on: decided,
            });
            continue;
        }
        info!(
            "FleetService '{}': slot {} surplus for {} reconciles, removing '{}'",
            name, index, surplus_reconciles, child_name
        );
        // A child already gone is the desired end state, not an error: the
        // reconcile must stay idempotent across a retry that lost its ack.
        match services.delete(&child_name, &dp).await {
            Ok(_) => {}
            Err(kube::Error::Api(ErrorResponse { code: 404, .. })) => {
                info!(
                    "FleetService '{}': surplus child '{}' already gone",
                    name, child_name
                );
            }
            Err(e) => return Err(e.into()),
        }
    }

    // Two passes append to `placements` (in-range slots, then surplus slots
    // still inside the hysteresis window), so sort by slot index to keep the
    // status readable in `kubectl get -o yaml` and stable across reconciles.
    // Sorting by name would order slot 10 before slot 2.
    fn slot_index_of(p: &PlacementStatus) -> usize {
        p.vllm_service_ref
            .rsplit_once('-')
            .and_then(|(_, i)| i.parse::<usize>().ok())
            .unwrap_or(usize::MAX)
    }
    placements.sort_by_key(slot_index_of);

    // 5. Write fleet status on the /status subresource.
    let fleets: Api<FleetService> = Api::namespaced(ctx.client.clone(), &ns);
    let placed = placements.len() as i32;
    // Active reschedules = moves in flight on healthy nodes (what the cap reads
    // next reconcile). Drain-and-hold placements are Draining but still pinned
    // to a preempted node, so they are excluded — otherwise a mass reclaim
    // would spike the counter and deadlock the cap against its own forced
    // Draining.
    let active_reschedules = placements
        .iter()
        .filter(|p| {
            matches!(p.phase.as_str(), "Draining" | "Rescheduling")
                && !preempted.contains(p.node_ref.as_str())
        })
        .count() as i32;
    // Honest status: ready counts placements whose lifecycle phase is Ready
    // (that phase already folds in child Deployment readiness + warmup). A
    // drain-and-hold is a Draining placement still pinned to a preempted node
    // (cap exhausted or no healthy target): it drives the fleet phase to
    // Degraded so the deliberate degradation of ADR-0005 dec.3 is visible in
    // `kubectl get`, not hidden behind a perpetual Placing.
    let ready_replicas = placements.iter().filter(|p| p.phase == "Ready").count() as i32;
    // ADR-0009 D1/D3: three counts, not one. `replicas` is every live
    // placement (the scale subresource's status path); `warming` is the
    // subset being brought up. See the ADR postscript for why Draining and
    // Rescheduling are excluded.
    let replicas = placements.len() as i32;
    let warming_replicas = placements
        .iter()
        .filter(|p| matches!(p.phase.as_str(), "Pending" | "Warming"))
        .count() as i32;
    let drain_and_hold = placements
        .iter()
        .any(|p| p.phase == "Draining" && preempted.contains(p.node_ref.as_str()));
    let fleet_phase = fleet_phase_for(fleet.spec.replicas, ready_replicas, drain_and_hold);
    let status_patch = json!({
        "status": FleetServiceStatus {
            phase: fleet_phase.to_string(),
            ready_replicas,
            desired_replicas: fleet.spec.replicas,
            replicas,
            warming_replicas,
            active_reschedules,
            placements,
        }
    });
    fleets
        .patch_status(&name, &PatchParams::default(), &Patch::Merge(&status_patch))
        .await?;

    info!(
        "FleetService '{}': {} slots placed ({} preserved, {} newly planned) across {} candidates",
        name,
        placed,
        current.len().min(desired),
        missing.len(),
        candidates.len()
    );
    Ok(Action::requeue(REQUEUE))
}

pub fn error_policy(fleet: Arc<FleetService>, err: &FleetError, ctx: Arc<FleetContext>) -> Action {
    let name = fleet.name_any();
    ctx.metrics.set_failure(&name, err.metric_label());
    warn!("FleetService '{}' reconcile failed: {:?}", name, err);
    Action::requeue(Duration::from_secs(10))
}

/// Build and run the fleet controller stream. Returned as a future so `main`
/// can drive it concurrently with the VllmService controller via `join!`.
pub async fn run(client: Client, metrics: Metrics) {
    let fleets: Api<FleetService> = Api::all(client.clone());
    let services: Api<VllmService> = Api::all(client.clone());
    let node_states: Api<NodeState> = Api::all(client.clone());
    let context = Arc::new(FleetContext {
        client: client.clone(),
        metrics,
    });
    info!("starting fleet controller");

    let controller = Controller::new(fleets, watcher::Config::default());
    // Reader over the controller's own FleetService cache, captured before the
    // builder chain consumes the controller. The NodeState mapper reads it
    // synchronously to fan a node event out to fleets (ADR-0005 dec.4).
    let fleet_reader = controller.store();

    controller
        .owns(services, watcher::Config::default())
        .watches(
            node_states,
            watcher::Config::default(),
            move |node: NodeState| {
                // Namespace-wide fan-out: a NodeState change wakes every
                // FleetService in the node's namespace, not only those pinned
                // to it. The reconcile is idempotent and cheap, so a reverse
                // node->fleet index is a premature optimisation (ADR-0005
                // dec.4). Reads the in-memory cache, no API call.
                let node_ns = node.namespace();
                fleet_reader
                    .state()
                    .into_iter()
                    .filter(move |fs| fs.namespace() == node_ns)
                    .map(|fs| ObjectRef::from_obj(fs.as_ref()))
                    .collect::<Vec<_>>()
            },
        )
        .run(reconcile, error_policy, context)
        .for_each(|res| async move {
            match res {
                Ok((obj, _action)) => info!("reconciled FleetService {}", obj.name),
                Err(e) => warn!("fleet reconcile loop error: {:?}", e),
            }
        })
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(name: &str, tpj: Option<f32>, age: Option<i64>) -> NodeCandidate {
        NodeCandidate {
            name: name.to_string(),
            warmth: vllm_coldstart_operator::fleet_types::Warmth::Warm,
            gpu_utilization: Some(42.0),
            active_service_count: Some(1),
            kv_cache_hit_rate: Some(0.5),
            kv_cache_hit_rate_age_secs: age,
            tokens_per_joule: tpj,
            tokens_per_joule_age_secs: age,
        }
    }

    #[test]
    fn decision_inputs_record_what_the_planner_ranked_on() {
        let candidates = vec![cand("node-a", Some(9.5), Some(10))];
        let d = decision_inputs(
            &candidates,
            "node-a",
            &PlacementStrategy::EfficiencyAware,
            Some(600),
        )
        .expect("candidate exists");
        assert_eq!(d.tokens_per_joule, Some(9.5));
        assert_eq!(d.gpu_utilization, Some(42.0));
        assert!(d.strategy.contains("EfficiencyAware"));
    }

    #[test]
    fn decision_inputs_report_an_expired_signal_as_absent() {
        // ADR-0008 D1 + D2: the value exists on NodeState but did not inform
        // the decision, so recording it would describe an input the planner
        // never had.
        let candidates = vec![cand("node-a", Some(9.5), Some(3600))];
        let d = decision_inputs(
            &candidates,
            "node-a",
            &PlacementStrategy::EfficiencyAware,
            Some(60),
        )
        .unwrap();
        assert_eq!(d.tokens_per_joule, None);
        assert_eq!(d.kv_cache_hit_rate, None);
        // NVML signals carry no horizon and survive.
        assert_eq!(d.gpu_utilization, Some(42.0));
    }

    #[test]
    fn decision_inputs_none_for_an_unknown_node() {
        let candidates = vec![cand("node-a", Some(1.0), Some(0))];
        assert!(decision_inputs(
            &candidates,
            "node-gone",
            &PlacementStrategy::WarmthFirst,
            None
        )
        .is_none());
    }

    #[test]
    fn own_placements_counted_per_node() {
        let mut current = BTreeMap::new();
        current.insert(0, "node-a".to_string());
        current.insert(1, "node-a".to_string());
        current.insert(2, "node-b".to_string());
        let counts = own_placements_per_node(&current);
        assert_eq!(counts.get("node-a"), Some(&2));
        assert_eq!(counts.get("node-b"), Some(&1));
        assert_eq!(counts.get("node-c"), None);
    }

    #[test]
    fn owned_child_carries_template_image_and_pinned_node() {
        // API contract: the fleet's template image reaches the child spec
        // verbatim — no silent fallback to default_image(), which is a
        // :latest tag and therefore not a reproducible reference.
        let fleet: FleetService = serde_json::from_value(json!({
            "metadata": {
                "name": "rehearsal",
                "namespace": "default",
                "uid": "00000000-0000-0000-0000-000000000002"
            },
            "spec": {
                "model": "facebook/opt-125m",
                "replicas": 2,
                "template": {"image": "llmd-sim-rehearsal:v0.8.2", "gpu": 0}
            }
        }))
        .expect("valid fixture");
        let child = build_owned_vllm_service(&fleet, 0, "fleet-test-worker").expect("child");
        assert_eq!(child.spec.image, "llmd-sim-rehearsal:v0.8.2");
        assert_eq!(child.spec.node_name.as_deref(), Some("fleet-test-worker"));
        assert_eq!(child.metadata.name.as_deref(), Some("rehearsal-0"));
    }
}
