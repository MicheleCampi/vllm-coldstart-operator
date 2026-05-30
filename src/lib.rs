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
