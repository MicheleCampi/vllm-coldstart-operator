pub mod fleet_placement;
pub mod fleet_planning;
pub mod fleet_types;
pub mod metrics;
pub mod reconcile_timeout;

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Desired state for a vLLM inference service.
///
/// The cold-start-aware field (`warmup_strategy`) is what makes this
/// operator different from a generic Deployment wrapper: the operator
/// treats time-to-first-token as a first-class concern, configuring the
/// pod for the cold-start/decode-speed trade-off the spec asks for.
#[derive(CustomResource, Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "inference.michelecampi.dev",
    version = "v1alpha1",
    kind = "VllmService",
    namespaced,
    status = "VllmServiceStatus",
    shortname = "vllm",
    printcolumn = r#"{"name":"Phase","type":"string","jsonPath":".status.phase"}"#,
    printcolumn = r#"{"name":"Model","type":"string","jsonPath":".spec.model"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#,
    printcolumn = r#"{"name":"Message","type":"string","priority":1,"jsonPath":".status.message"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct VllmServiceSpec {
    /// HuggingFace model id to serve, e.g. "Qwen/Qwen2.5-7B-Instruct".
    pub model: String,
    /// Number of replicas to run.
    #[serde(default = "default_replicas")]
    pub replicas: i32,
    /// Cold-start vs decode-speed trade-off. `Eager` disables CUDA graphs
    /// for a faster cold start (better for scale-to-zero); `Graph` enables
    /// them for faster per-token decode at a higher cold-start cost, which
    /// is not always more end-to-end throughput (see docs/BENCHMARKS.md).
    #[serde(default)]
    pub warmup_strategy: WarmupStrategy,
    /// Container image to run. Defaults to the official vLLM OpenAI server,
    /// pinned by tag and digest (v0.34.0): each operator release moves it
    /// on purpose, never a floating tag.
    #[serde(default = "default_image")]
    pub image: String,
    /// Number of GPUs to request per replica. With 0, the replica serves
    /// the model on CPU when both `cpu` and `memory` are set (a CPU image,
    /// e.g. the vLLM CPU release); otherwise it is an inert placeholder that
    /// runs the image's own entrypoint (e.g. CI on a cluster without GPUs).
    #[serde(default = "default_gpu")]
    pub gpu: i32,
    /// HTTP path for the readiness probe, e.g. "/health". When non-empty,
    /// the operator gates readiness (and thus the Warming->Ready transition)
    /// on this endpoint, so "Ready" means the server can actually serve.
    /// On serving pods it also adds a startup probe that sends one 1-token
    /// completion first, so "Ready" means warm: the first client request
    /// does not pay the server's one-time costs (15.9 s against 2.1 s on
    /// the vLLM CPU image). Set to an empty string to disable both probes,
    /// for inert placeholder images that expose no HTTP health endpoint.
    #[serde(default = "default_health_path")]
    pub health_path: String,
    /// RuntimeClass for the workload pod. Cluster-dependent: K3s GPU nodes
    /// need "nvidia" to route the pod to the NVIDIA container runtime, while
    /// managed clusters (GKE, EKS, AKS) expose GPUs through the device plugin
    /// with the default runtime and define no such RuntimeClass. Leave unset
    /// (the default) on managed clusters; setting a non-existent RuntimeClass
    /// makes the API server reject the pod.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_class_name: Option<String>,
    /// Extra command-line arguments appended to `vllm serve <model>`, e.g.
    /// ["--max-model-len", "8192", "--gpu-memory-utilization", "0.90"].
    /// Keeps engine tuning (context length, memory fraction, quantization)
    /// in the resource spec rather than baked into the operator binary.
    #[serde(default)]
    pub extra_args: Vec<String>,
    /// CPU to request per replica, as a Kubernetes quantity (e.g. "4",
    /// "500m"). Request only, no limit: a CPU limit throttles the tokenizer
    /// and HTTP front-end under load. Unset (the default): the operator
    /// emits no CPU request and the namespace LimitRange default applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu: Option<String>,
    /// Memory per replica, as a Kubernetes quantity (e.g. "24Gi"). Set as
    /// both request and limit, so the pod gets a guaranteed amount and an
    /// OOM is a sizing signal rather than a noisy-neighbour surprise. Unset
    /// (the default): the namespace LimitRange default applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<String>,
    /// Exact node this replica must land on, as a `kubernetes.io/hostname`
    /// nodeSelector. Set by the fleet controller (ADR-0004) to pin a placement
    /// to its chosen node while keeping the default scheduler in the loop
    /// (resource fit, admission, taint/toleration). Unset for standalone /
    /// CI use, where no node constraint is emitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_name: Option<String>,
    /// Host directory mounted at the container's HuggingFace cache
    /// (/root/.cache/huggingface). Point it at a node-local path holding a
    /// pre-downloaded model so a cold start loads weights from local disk
    /// instead of re-downloading them from the HF CDN, making reschedule
    /// latency an operator property rather than a network property. Unset
    /// (the default) keeps the download-in-pod behaviour. hostPath-backed:
    /// meant for single-tenant benchmark/edge clusters, not multi-tenant
    /// production.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_cache_host_path: Option<String>,
    /// Secret key holding the API key for vLLM, injected as the
    /// VLLM_API_KEY env var (never as a command-line arg, which `get pod`
    /// would show). The reference is not optional: a missing Secret keeps
    /// the pod from starting rather than starting it open. Unset (the
    /// default): no key, any client the NetworkPolicy admits can call it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_secret_ref: Option<ApiKeySecretRef>,
    /// Latency objective the team declares for this service. Unset: the
    /// platform-wide objective applies (ADR-0013: TTFT <= 1s for 95%).
    /// Bounds are enforced by the API server through the CRD schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub objective: Option<Objective>,
    /// Monthly budget the team declares for this service, in EUR. Used for
    /// showback against the measured cost per million tokens (ADR-0025).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<Budget>,
    /// Extra environment variables for a serving replica, plain values only
    /// (credentials go through `apiKeySecretRef`, never here). For engine
    /// settings that have no command-line flag, e.g. `VLLM_CPU_KVCACHE_SPACE`:
    /// GiB of KV cache the vLLM CPU image reserves, 4 by default whatever the
    /// memory limit. Names the operator sets itself (`LD_LIBRARY_PATH`,
    /// `VLLM_API_KEY`) are reserved and ignored here. Not applied to the
    /// placeholder pod.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<PlainEnvVar>,
    /// How replicas spread across zones (`topology.kubernetes.io/zone`),
    /// maxSkew 1. `Preferred` (the default) asks the scheduler to spread and
    /// still places a replica when it cannot. `Required` keeps a replica
    /// Pending rather than stacking it in a zone that already has more, so
    /// an autoscaler such as Karpenter launches a node in the other zone
    /// (EKS game day #1, 2026-11-20: after a Spot interruption both replicas
    /// ended in one zone). Required needs zone labels on the nodes: on a
    /// cluster without them (kind) the pods stay Pending. `Disabled` emits no
    /// constraint. Ignored when `nodeName` pins the replica.
    #[serde(default)]
    pub zone_spread: ZoneSpread,
}

/// Zone spread of a service's replicas.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, JsonSchema, Default, PartialEq)]
pub enum ZoneSpread {
    /// whenUnsatisfiable: ScheduleAnyway.
    #[default]
    Preferred,
    /// whenUnsatisfiable: DoNotSchedule.
    Required,
    /// No zone constraint.
    Disabled,
}

/// Environment variables the operator sets itself; `spec.env` cannot
/// override them.
pub const RESERVED_ENV: [&str; 2] = ["LD_LIBRARY_PATH", "VLLM_API_KEY"];

/// One environment variable with a literal value.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct PlainEnvVar {
    /// Variable name.
    pub name: String,
    /// Literal value.
    pub value: String,
}

/// Time-to-first-token objective: `percentile`% of requests must see the
/// first token within `ttftSeconds`.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Objective {
    /// TTFT target in seconds.
    #[schemars(range(min = 0.05, max = 60.0))]
    pub ttft_seconds: f64,
    /// Share of requests, in percent, that must meet the target.
    #[schemars(range(min = 50.0, max = 99.9))]
    pub percentile: f64,
}

/// Monthly spending limit for the service.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Budget {
    /// Budget in EUR per calendar month.
    #[schemars(range(min = 0.0))]
    pub eur_per_month: f64,
}

/// Reference to one key of a Secret in the VllmService's namespace.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct ApiKeySecretRef {
    /// Secret name.
    pub name: String,
    /// Key within the Secret whose value is the API key.
    pub key: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default, PartialEq)]
pub enum WarmupStrategy {
    /// enforce_eager=true — fast cold start, slower per-token decode.
    #[default]
    Eager,
    /// CUDA graphs on — slow cold start, faster per-token decode.
    Graph,
}

fn default_replicas() -> i32 {
    1
}
/// Pinned so that a VllmService without `image` is reproducible and passes
/// admission policies that reject `:latest` (e.g. Kyverno disallow-latest-tag).
pub const DEFAULT_IMAGE: &str =
    "vllm/vllm-openai:v0.34.0@sha256:20fdb089454afbb324efa4110a2c025ff7b27cc1a086e07aa580b18df8ad1ce1";
pub fn default_image() -> String {
    DEFAULT_IMAGE.to_string()
}
fn default_gpu() -> i32 {
    1
}
fn default_health_path() -> String {
    "/health".to_string()
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
    #[test]
    fn default_image_is_pinned_not_latest() {
        // Admission policies such as Kyverno disallow-latest-tag reject
        // :latest; the default must be a tag and a digest.
        let (name, digest) = DEFAULT_IMAGE.split_once('@').expect("digest");
        assert!(!name.ends_with(":latest") && name.contains(':'), "{name}");
        assert!(
            digest.starts_with("sha256:") && digest.len() == 71,
            "{digest}"
        );
    }

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

#[cfg(test)]
mod spec_contract_tests {
    use super::*;

    #[test]
    fn spec_without_objective_or_budget_still_parses() {
        let s: VllmServiceSpec = serde_json::from_str(r#"{"model":"m"}"#).unwrap();
        assert_eq!(s.objective, None);
        assert_eq!(s.budget, None);
    }

    #[test]
    fn objective_and_budget_parse_in_camel_case() {
        let s: VllmServiceSpec = serde_json::from_str(
            r#"{"model":"m","objective":{"ttftSeconds":0.8,"percentile":95.0},"budget":{"eurPerMonth":300.0}}"#,
        )
        .unwrap();
        assert_eq!(
            s.objective,
            Some(Objective {
                ttft_seconds: 0.8,
                percentile: 95.0
            })
        );
        assert_eq!(
            s.budget,
            Some(Budget {
                eur_per_month: 300.0
            })
        );
    }

    #[test]
    fn schema_bounds_are_enforced_by_the_api_server() {
        let schema = serde_json::to_string(&schemars::schema_for!(Objective)).unwrap();
        for bound in [
            r#""minimum":50.0"#,
            r#""maximum":99.9"#,
            r#""minimum":0.05"#,
            r#""maximum":60.0"#,
        ] {
            assert!(schema.contains(bound), "missing {bound} in {schema}");
        }
    }
}
pub mod provision;
