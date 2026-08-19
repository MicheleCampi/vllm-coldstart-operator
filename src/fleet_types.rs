use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Desired state for a fleet of vLLM services scheduled across nodes.
///
/// `FleetService` does not manage warmup lifecycle itself — that stays owned
/// by `VllmService`, already validated E2E on GKE. This CRD owns placement:
/// which node runs which instance, and how the fleet reacts to node-level
/// state (warmth, load, spot preemption) that only exists once you have more
/// than one node. Deployment -> ReplicaSet -> Pod is the model: FleetService
/// creates and reconciles owned VllmService objects, it does not replace
/// their reconciler.
#[derive(CustomResource, Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "inference.michelecampi.dev",
    version = "v1alpha1",
    kind = "FleetService",
    namespaced,
    status = "FleetServiceStatus",
    shortname = "fleet",
    scale(
        spec_replicas_path = ".spec.replicas",
        status_replicas_path = ".status.replicas"
    )
)]
#[serde(rename_all = "camelCase")]
pub struct FleetServiceSpec {
    /// HuggingFace model id, passed through to every owned VllmService.
    pub model: String,
    /// Desired replica count at fleet level (across all nodes).
    pub replicas: i32,
    /// VllmService template applied to every placement. Same shape as
    /// VllmServiceSpec minus `model`/`replicas`, which the fleet controls.
    pub template: FleetServiceTemplate,
    /// Node pool this fleet is allowed to place onto.
    #[serde(default)]
    pub node_pool: NodePoolSpec,
    /// Placement strategy. WarmthFirst (default) and EfficiencyAware
    /// (ADR-0007) are implemented; Spread/BinPack are reserved variants
    /// that currently degrade to warmth-first (see ADR).
    #[serde(default)]
    pub placement: PlacementSpec,
    /// Anti-oscillation controls for the reconcile loop.
    #[serde(default)]
    pub hysteresis: HysteresisSpec,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct FleetServiceTemplate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_class_name: Option<String>,
    #[serde(default)]
    pub extra_args: Vec<String>,
    #[serde(default = "default_gpu")]
    pub gpu: i32,
    #[serde(default = "default_health_path")]
    pub health_path: String,
    /// Container image for every owned VllmService. Pin a digest or exact
    /// tag for reproducible fleets; the default (vllm/vllm-openai:latest)
    /// exists for interactive convenience only and is not a reproducible
    /// reference.
    #[serde(default = "crate::default_image")]
    pub image: String,
    /// Host directory mounted at the container's HuggingFace cache on every
    /// owned VllmService. See VllmServiceSpec.modelCacheHostPath.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_cache_host_path: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct NodePoolSpec {
    /// Label selector for nodes this fleet may place onto.
    #[serde(default)]
    pub selector: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub spot_policy: SpotPolicySpec,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct SpotPolicySpec {
    #[serde(default)]
    pub enabled: bool,
    /// Max fraction of fleet replicas allowed on spot nodes, 0.0-1.0.
    #[serde(default = "default_max_spot_fraction")]
    pub max_spot_fraction: f32,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default, PartialEq)]
pub enum PlacementStrategy {
    #[default]
    WarmthFirst,
    Spread,
    BinPack,
    /// ADR-0007: strict lexicographic ordering
    /// warmth > tokensPerJoule > kvCacheHitRate > gpuUtilization > activeServiceCount
    /// (ADR-0008 D4 reordered the two efficiency signals; ADR-0007 D3 had cache first).
    /// Missing efficiency signals rank below any observed value within the
    /// same warmth class (fail-open: a fleet with no reporters degenerates
    /// to WarmthFirst behaviour).
    EfficiencyAware,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct PlacementSpec {
    #[serde(default)]
    pub strategy: PlacementStrategy,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HysteresisSpec {
    /// Consecutive reconciles a node-state category must persist before the
    /// controller acts on it. Prevents flapping on a single noisy sample.
    #[serde(default = "default_stable_reconciles")]
    pub stable_reconciles_required: i32,
    /// Max simultaneous reschedules across the fleet, analogous to
    /// maxUnavailable. Caps blast radius of a bad placement decision.
    #[serde(default = "default_max_concurrent_reschedules")]
    pub max_concurrent_reschedules: i32,
}

impl Default for HysteresisSpec {
    fn default() -> Self {
        Self {
            stable_reconciles_required: default_stable_reconciles(),
            max_concurrent_reschedules: default_max_concurrent_reschedules(),
        }
    }
}

fn default_gpu() -> i32 {
    1
}
fn default_health_path() -> String {
    "/health".to_string()
}
fn default_max_spot_fraction() -> f32 {
    0.5
}
fn default_stable_reconciles() -> i32 {
    3
}
fn default_max_concurrent_reschedules() -> i32 {
    1
}

/// Observed state, written back by the fleet controller.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct FleetServiceStatus {
    /// Aggregated phase: worst-case across all placements.
    pub phase: String,
    pub ready_replicas: i32,
    pub desired_replicas: i32,
    /// ADR-0009 D1/D3: live placements whatever their phase. This is the
    /// scale subresource's status path, so it must report what exists, not
    /// what serves — pointing it at readyReplicas would hide a replica that
    /// is still coming up and make an autoscaler scale on top of it.
    #[serde(default)]
    pub replicas: i32,
    /// ADR-0009 D3 (as amended in the postscript): placements the fleet
    /// holds that are not serving yet but are being brought up — `Pending`
    /// or `Warming`. Excludes `Draining`/`Rescheduling`, which is capacity
    /// on its way out. The consumer subtracts this from `replicas` to see
    /// what will serve without scaling further.
    #[serde(default)]
    pub warming_replicas: i32,
    /// Counter backing the max_concurrent_reschedules cap.
    pub active_reschedules: i32,
    #[serde(default)]
    pub placements: Vec<PlacementStatus>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlacementStatus {
    pub vllm_service_ref: String,
    pub node_ref: String,
    pub phase: String,
    /// RFC3339 timestamp of the last phase transition.
    pub last_transition_time: String,
    /// RFC3339 timestamp since the current phase has held stable, used for
    /// hysteresis. None-equivalent is an empty string (kept JsonSchema-simple
    /// rather than Option<String> to avoid an extra null branch in the CRD
    /// schema).
    #[serde(default)]
    pub stable_since: String,
    /// ADR-0009 D4: consecutive reconciles this placement has been seen
    /// beyond `spec.replicas`. Scale-down is asymmetric: the surplus must
    /// persist for `stable_reconciles_required` before removal, because
    /// deleting a replica is instant while recreating it costs a cold
    /// start. Reset to zero as soon as the slot is back in range.
    #[serde(default)]
    pub surplus_reconciles: i32,
}

/// ADR-0009 D4: scale-down hysteresis, pure and unit-testable.
///
/// `prev` is the surplus counter carried in the placement's last status,
/// `in_surplus` whether the slot sits beyond `spec.replicas` this
/// reconcile. Returns the counter to persist and whether the placement
/// may be removed now.
///
/// Scale-up has no equivalent gate: demand that exists now is real, and a
/// slot back in range resets to zero at once. The asymmetry is the
/// decision -- removing a replica is instant, recreating it costs a cold
/// start.
///
/// A non-positive `required` means immediate removal rather than an
/// endless wait: zero asks for no hysteresis, and a negative value is not
/// a longer one.
pub fn surplus_hysteresis(prev: i32, in_surplus: bool, required: i32) -> (i32, bool) {
    if !in_surplus {
        return (0, false);
    }
    if required <= 0 {
        return (0, true);
    }
    let next = prev.max(0).saturating_add(1);
    (next, next >= required)
}

/// Placement lifecycle phase transition, pure and unit-testable — mirrors
/// `phase_for` in lib.rs for VllmService. Node preemption takes priority
/// over warmth-based transitions: a node can be Warm and preempted in the
/// same reconcile.
pub fn placement_phase_for(
    current_phase: &str,
    node_ready: bool,
    preemption_notice: bool,
) -> &'static str {
    if preemption_notice {
        return "Draining";
    }
    match current_phase {
        "Draining" | "Preempted" => "Rescheduling",
        _ if node_ready => "Ready",
        "Ready" => "Warming",
        _ => "Pending",
    }
}

/// Fleet-level phase derived from placement outcomes, pure and unit-testable.
/// `Degraded` surfaces drain-and-hold (ADR-0005 dec.3): the fleet deliberately
/// runs with fewer live replicas because no healthy target exists, and the
/// status must say so rather than hide it. `Ready` requires every desired
/// replica to report Ready (vacuously true at zero desired). Anything else is
/// still `Placing`.
pub fn fleet_phase_for(desired: i32, ready: i32, drain_and_hold: bool) -> &'static str {
    if drain_and_hold {
        "Degraded"
    } else if ready >= desired {
        "Ready"
    } else {
        "Placing"
    }
}

/// Desired state observed for a single node, written by a per-node reporter
/// DaemonSet (outside this operator's binary) and read by FleetController.
/// Kept separate from core Node objects: mutating Node annotations directly
/// risks collision with cluster-autoscaler and other node controllers.
#[derive(CustomResource, Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "inference.michelecampi.dev",
    version = "v1alpha1",
    kind = "NodeState",
    namespaced,
    status = "NodeStateStatus",
    shortname = "nstate"
)]
#[serde(rename_all = "camelCase")]
pub struct NodeStateSpec {
    /// Cadence at which the reporter refreshes status, in seconds.
    #[serde(default = "default_report_interval")]
    pub report_interval_seconds: i32,
}

fn default_report_interval() -> i32 {
    15
}

impl Default for NodeStateSpec {
    /// Matches the serde/CRD default so a reporter-created NodeState and a
    /// manifest-created one with an empty spec are indistinguishable.
    fn default() -> Self {
        Self {
            report_interval_seconds: default_report_interval(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default, PartialEq)]
pub enum Warmth {
    #[default]
    Cold,
    Warming,
    Warm,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct SpotStatus {
    #[serde(default)]
    pub is_spot_instance: bool,
    #[serde(default)]
    pub preemption_notice_detected: bool,
    /// RFC3339 timestamp; empty string until a notice is first detected.
    #[serde(default)]
    pub preemption_notice_time: String,
}

/// Observed state for a single node, written by the per-node reporter.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct NodeStateStatus {
    /// Every status field is schema-optional (serde defaults): the status
    /// is merge-patched by writers with disjoint field ownership (reporter
    /// vs warmth/spot writers), and a required field would force every
    /// writer to fabricate values it does not own on first write.
    #[serde(default)]
    pub observed_generation: i64,
    /// RFC3339 timestamp of the last reporter write.
    #[serde(default)]
    pub last_report_time: String,
    #[serde(default)]
    pub warmth: Warmth,
    /// Raw observed GPU utilization. Absent = signal not available
    /// (fail-open): a serde-default 0.0 would instead read as "idle node"
    /// and bias placement toward unmeasured nodes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_utilization: Option<f32>,
    /// Same absence semantics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_memory_used_bytes: Option<i64>,
    /// Same absence semantics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_service_count: Option<i32>,
    /// ADR-0007: raw observed KV-cache hit-rate in [0,1], written by the
    /// per-node reporter. Absent = signal not available (fail-open).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kv_cache_hit_rate: Option<f32>,
    /// ADR-0007: raw observed tokens-per-joule, written by the per-node
    /// reporter. Absent = signal not available (fail-open).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_per_joule: Option<f32>,
    /// ADR-0009 D2: raw observed queue depth (`vllm:num_requests_waiting`,
    /// summed across this node's scrape targets). This is the demand
    /// signal an external autoscaler reads; utilization is not, because a
    /// saturated GPU with an empty queue needs no replicas. Absent =
    /// signal not available (fail-open); a 0.0 would read as "no load"
    /// exactly when the engine is unreachable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requests_waiting: Option<f32>,
    /// ADR-0009 D2: raw observed in-flight requests
    /// (`vllm:num_requests_running`). Same absence semantics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requests_running: Option<f32>,
    #[serde(default)]
    pub spot: SpotStatus,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surplus_counter_resets_when_slot_is_back_in_range() {
        assert_eq!(surplus_hysteresis(2, false, 3), (0, false));
    }

    #[test]
    fn surplus_must_persist_before_removal_is_allowed() {
        let (c1, rm1) = surplus_hysteresis(0, true, 3);
        assert_eq!((c1, rm1), (1, false));
        let (c2, rm2) = surplus_hysteresis(c1, true, 3);
        assert_eq!((c2, rm2), (2, false));
        let (c3, rm3) = surplus_hysteresis(c2, true, 3);
        assert_eq!((c3, rm3), (3, true));
    }

    #[test]
    fn one_reconcile_back_in_range_undoes_the_whole_wait() {
        let (c, _) = surplus_hysteresis(2, true, 3);
        assert_eq!(c, 3);
        // Scale-up is immediate and forgiving: the counter does not decay,
        // it resets, so a single dip below the surplus threshold costs the
        // full wait again.
        assert_eq!(surplus_hysteresis(c, false, 3), (0, false));
    }

    #[test]
    fn non_positive_required_removes_immediately() {
        assert_eq!(surplus_hysteresis(0, true, 0), (0, true));
        assert_eq!(surplus_hysteresis(0, true, -1), (0, true));
    }

    #[test]
    fn required_of_one_removes_on_the_first_surplus_reconcile() {
        assert_eq!(surplus_hysteresis(0, true, 1), (1, true));
    }

    #[test]
    fn counter_saturates_instead_of_overflowing() {
        assert_eq!(surplus_hysteresis(i32::MAX, true, 3), (i32::MAX, true));
    }

    #[test]
    fn negative_persisted_counter_is_treated_as_zero() {
        // A hand-edited status must not buy extra grace.
        assert_eq!(surplus_hysteresis(-5, true, 3), (1, false));
    }

    #[test]
    fn preemption_notice_forces_draining_even_if_warm() {
        assert_eq!(placement_phase_for("Ready", true, true), "Draining");
    }

    #[test]
    fn draining_moves_to_rescheduling_next_reconcile() {
        assert_eq!(
            placement_phase_for("Draining", false, false),
            "Rescheduling"
        );
    }

    #[test]
    fn preempted_moves_to_rescheduling() {
        assert_eq!(
            placement_phase_for("Preempted", false, false),
            "Rescheduling"
        );
    }

    #[test]
    fn node_ready_reaches_ready_from_any_non_draining_phase() {
        assert_eq!(placement_phase_for("Pending", true, false), "Ready");
        assert_eq!(placement_phase_for("Rescheduling", true, false), "Ready");
    }

    #[test]
    fn ready_drops_to_warming_when_node_not_ready() {
        assert_eq!(placement_phase_for("Ready", false, false), "Warming");
    }

    #[test]
    fn unknown_or_pending_stays_pending_without_node_ready() {
        assert_eq!(placement_phase_for("Pending", false, false), "Pending");
        assert_eq!(placement_phase_for("Warming", false, false), "Pending");
    }

    /// ADR-0009 D3 as amended: a placement reaches `Warming` only by
    /// falling out of `Ready`; a fresh one goes `Pending -> Ready`. Both
    /// count as warming because both are slots not serving yet that the
    /// fleet is bringing up — counting only `Warming` would report
    /// capacity on its way out as capacity arriving.
    #[test]
    fn a_fresh_placement_is_warming_before_it_is_ready() {
        assert_eq!(placement_phase_for("Pending", false, false), "Pending");
        assert_eq!(placement_phase_for("Pending", true, false), "Ready");
        assert_eq!(placement_phase_for("Ready", false, false), "Warming");
        for phase in ["Pending", "Warming"] {
            assert!(
                matches!(phase, "Pending" | "Warming"),
                "{phase} must count toward warming_replicas"
            );
        }
        for phase in ["Draining", "Rescheduling", "Ready"] {
            assert!(
                !matches!(phase, "Pending" | "Warming"),
                "{phase} must not count toward warming_replicas"
            );
        }
    }

    #[test]
    fn fleet_ready_when_all_desired_ready() {
        assert_eq!(fleet_phase_for(2, 2, false), "Ready");
    }

    #[test]
    fn fleet_degraded_when_any_drain_and_hold() {
        // Drain-and-hold wins even when the other replicas are Ready: the
        // fleet is deliberately short (ADR-0005 dec.3) and the status says so.
        assert_eq!(fleet_phase_for(2, 1, true), "Degraded");
        assert_eq!(fleet_phase_for(2, 2, true), "Degraded");
    }

    #[test]
    fn fleet_placing_until_all_ready_and_vacuously_ready_at_zero() {
        assert_eq!(fleet_phase_for(2, 1, false), "Placing");
        assert_eq!(fleet_phase_for(0, 0, false), "Ready");
    }
}
