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
//! This block is list-only: every reconcile lists `NodeState` fresh and plans
//! from that snapshot. Reacting to node changes via a `.watches(NodeState)`
//! mapper is the spot-preemption block (ADR-0005), not this one.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use kube::{
    api::{Api, ObjectMeta, Patch, PatchParams},
    runtime::{
        controller::{Action, Controller},
        watcher,
    },
    Client, Resource, ResourceExt,
};
use serde_json::json;
use tracing::{info, warn};

use vllm_coldstart_operator::fleet_placement::NodeCandidate;
use vllm_coldstart_operator::fleet_planning::plan_initial_placements;
use vllm_coldstart_operator::fleet_types::{
    FleetService, FleetServiceStatus, NodeState, PlacementStatus,
};
use vllm_coldstart_operator::metrics::Metrics;
use vllm_coldstart_operator::{default_image, VllmService, VllmServiceSpec};

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
fn node_state_to_candidate(ns: &NodeState) -> Option<NodeCandidate> {
    let status = ns.status.as_ref()?;
    Some(NodeCandidate {
        name: ns.name_any(),
        warmth: status.warmth.clone(),
        gpu_utilization: status.gpu_utilization,
        active_service_count: status.active_service_count,
    })
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
        image: default_image(),
        gpu: t.gpu,
        health_path: t.health_path.clone(),
        runtime_class_name: t.runtime_class_name.clone(),
        extra_args: t.extra_args.clone(),
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
    //    List-only: this block plans from a fresh snapshot each reconcile.
    let node_states: Api<NodeState> = Api::all(ctx.client.clone());
    let states = node_states.list(&Default::default()).await?;
    let candidates: Vec<NodeCandidate> =
        states.iter().filter_map(node_state_to_candidate).collect();

    if candidates.is_empty() {
        warn!(
            "FleetService '{}': no reported NodeState objects, nothing to place",
            name
        );
        return Ok(Action::requeue(REQUEUE));
    }

    // 2. Pure planning: fill `replicas` slots across candidates, spreading load.
    let plan = plan_initial_placements(fleet.spec.replicas, &candidates);

    // 3. Apply one owned VllmService per planned slot (server-side apply,
    //    distinct field manager). Deterministic child names => idempotent.
    let services: Api<VllmService> = Api::namespaced(ctx.client.clone(), &ns);
    let pp = PatchParams::apply(FLEET_MANAGER).force();
    let mut placements: Vec<PlacementStatus> = Vec::with_capacity(plan.len());
    for (index, node_name) in plan.iter().enumerate() {
        let child = build_owned_vllm_service(&fleet, index, node_name)?;
        let child_name = child.name_any();
        services
            .patch(&child_name, &pp, &Patch::Apply(&child))
            .await?;
        placements.push(PlacementStatus {
            vllm_service_ref: child_name,
            node_ref: node_name.clone(),
            phase: "Pending".to_string(),
            last_transition_time: String::new(),
            stable_since: String::new(),
        });
    }

    // 4. Write fleet status on the /status subresource.
    let fleets: Api<FleetService> = Api::namespaced(ctx.client.clone(), &ns);
    let placed = placements.len() as i32;
    let status_patch = json!({
        "status": FleetServiceStatus {
            phase: "Placing".to_string(),
            ready_replicas: 0,
            desired_replicas: fleet.spec.replicas,
            active_reschedules: 0,
            placements,
        }
    });
    fleets
        .patch_status(&name, &PatchParams::default(), &Patch::Merge(&status_patch))
        .await?;

    info!(
        "FleetService '{}': applied {} placements across {} candidate nodes",
        name,
        placed,
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
    let context = Arc::new(FleetContext {
        client: client.clone(),
        metrics,
    });
    info!("starting fleet controller");
    Controller::new(fleets, watcher::Config::default())
        .owns(services, watcher::Config::default())
        .run(reconcile, error_policy, context)
        .for_each(|res| async move {
            match res {
                Ok((obj, _action)) => info!("reconciled FleetService {}", obj.name),
                Err(e) => warn!("fleet reconcile loop error: {:?}", e),
            }
        })
        .await;
}
