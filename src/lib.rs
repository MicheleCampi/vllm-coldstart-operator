use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Desired state for a vLLM inference service.
///
/// The cold-start-aware field (`warmup_strategy`) is what makes this
/// operator different from a generic Deployment wrapper: the operator
/// treats time-to-first-token as a first-class concern, configuring the
/// pod for the cold-start/throughput trade-off the spec asks for.
#[derive(CustomResource, Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "inference.michelecampi.dev",
    version = "v1alpha1",
    kind = "VllmService",
    namespaced,
    status = "VllmServiceStatus",
    shortname = "vllm"
)]
#[serde(rename_all = "camelCase")]
pub struct VllmServiceSpec {
    /// HuggingFace model id to serve, e.g. "Qwen/Qwen2.5-7B-Instruct".
    pub model: String,
    /// Number of replicas to run.
    #[serde(default = "default_replicas")]
    pub replicas: i32,
    /// Cold-start vs throughput trade-off. "eager" disables CUDA graphs
    /// for a faster cold start (better for scale-to-zero); "graph" enables
    /// them for faster steady-state inference at a higher cold-start cost.
    #[serde(default)]
    pub warmup_strategy: WarmupStrategy,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default, PartialEq)]
pub enum WarmupStrategy {
    /// enforce_eager=true — fast cold start, slower steady-state.
    #[default]
    Eager,
    /// CUDA graphs on — slow cold start, faster steady-state.
    Graph,
}

fn default_replicas() -> i32 {
    1
}

/// Observed state, written back by the operator.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default)]
pub struct VllmServiceStatus {
    /// Lifecycle phase: Pending -> Warming -> Ready.
    pub phase: String,
    /// Human-readable detail about the current phase.
    pub message: String,
}

/// Lifecycle phase derived from the owned Deployment's ready replicas.
///
/// This is the heart of the operator: a vLLM pod that Kubernetes considers
/// "Running" is not yet able to serve a token — it still has to load weights
/// and warm up. The cold-start study quantified that gap; here it becomes an
/// observable phase. "Ready" means warm, not merely alive.
pub fn phase_for(desired: i32, ready: i32) -> (&'static str, String) {
    if ready == 0 {
        (
            "Pending",
            "Deployment created; no replica ready yet".to_string(),
        )
    } else if ready < desired {
        (
            "Warming",
            format!("{ready}/{desired} replicas ready; warming up"),
        )
    } else {
        (
            "Ready",
            format!("{ready}/{desired} replicas ready and warm"),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_ready_is_pending() {
        let (phase, _) = phase_for(3, 0);
        assert_eq!(phase, "Pending");
    }

    #[test]
    fn partial_ready_is_warming() {
        let (phase, msg) = phase_for(3, 1);
        assert_eq!(phase, "Warming");
        assert!(msg.contains("1/3"));
    }

    #[test]
    fn all_ready_is_ready() {
        let (phase, msg) = phase_for(3, 3);
        assert_eq!(phase, "Ready");
        assert!(msg.contains("3/3"));
    }

    #[test]
    fn single_replica_ready() {
        // The common scale-to-one case: one desired, one ready -> Ready.
        let (phase, _) = phase_for(1, 1);
        assert_eq!(phase, "Ready");
    }

    #[test]
    fn ready_exceeding_desired_is_still_ready() {
        // During a scale-down a Deployment can briefly report more ready
        // than desired; that is still a serving state, not a warming one.
        let (phase, _) = phase_for(2, 3);
        assert_eq!(phase, "Ready");
    }
}
