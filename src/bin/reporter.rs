//! Per-node signal reporter (ADR-0007 D2, phase B).
//!
//! Runs as a DaemonSet on GPU nodes. Owns the *measurement* side of the
//! measure-actuate loop: it writes raw observed signals to the status of
//! the NodeState named after the node it runs on. All *policy* stays in
//! the planner (fleet_controller / fleet_placement) — this binary never
//! interprets the numbers it reports.
//!
//! Ownership split on NodeStateStatus (merge-patched, fields not listed
//! here are left to their own writers, e.g. warmth/spot):
//!   - gpuUtilization, gpuMemoryUsedBytes, activeServiceCount
//!   - kvCacheHitRate, tokensPerJoule   (ADR-0007; absent = fail-open)
//!   - lastReportTime
//!
//! Identity comes from the downward API: NODE_NAME (spec.nodeName) and
//! POD_NAMESPACE (metadata.namespace) — NodeState is namespaced and named
//! after the node.
//!
//! Sources:
//!   - REPORTER_SYNTHETIC=1 selects the deterministic synthetic source
//!     (kind rehearsal, ADR-0007 falsification level 2). Values are
//!     overridable via REPORTER_SYNTHETIC_* env vars.
//!   - Otherwise the real source runs. In phase B it reports the ADR-0007
//!     signals as absent (NVML/vLLM scrape land with falsification level
//!     3); the planner fail-opens to warmth-first, by design.

use anyhow::{bail, Context as _};
use k8s_openapi::chrono::Utc;
use kube::{
    api::{Api, Patch, PatchParams, PostParams},
    Client,
};
use serde_json::json;
use tracing::{info, warn};
use vllm_coldstart_operator::fleet_types::{NodeState, NodeStateSpec};

/// One sample of everything this reporter owns on NodeStateStatus.
/// Raw f32 signals only — no thresholds, no scoring (ADR-0007 D2).
#[derive(Debug, Clone, Copy)]
struct Signals {
    gpu_utilization: f32,
    gpu_memory_used_bytes: i64,
    active_service_count: i32,
    kv_cache_hit_rate: Option<f32>,
    tokens_per_joule: Option<f32>,
}

trait SignalSource {
    fn name(&self) -> &'static str;
    fn sample(&mut self) -> Signals;
}

/// Deterministic source for the kind rehearsal. Every value can be pinned
/// via env so seed topologies (A/B/C) map to per-node DaemonSet env or
/// per-node overrides without touching code.
struct Synthetic {
    signals: Signals,
}

impl Synthetic {
    fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            signals: Signals {
                gpu_utilization: env_f32("REPORTER_SYNTHETIC_GPU_UTILIZATION")?.unwrap_or(0.0),
                gpu_memory_used_bytes: env_i64("REPORTER_SYNTHETIC_GPU_MEMORY_USED_BYTES")?
                    .unwrap_or(0),
                active_service_count: env_i32("REPORTER_SYNTHETIC_ACTIVE_SERVICE_COUNT")?
                    .unwrap_or(0),
                kv_cache_hit_rate: env_f32("REPORTER_SYNTHETIC_KV_CACHE_HIT_RATE")?,
                tokens_per_joule: env_f32("REPORTER_SYNTHETIC_TOKENS_PER_JOULE")?,
            },
        })
    }
}

impl SignalSource for Synthetic {
    fn name(&self) -> &'static str {
        "synthetic"
    }
    fn sample(&mut self) -> Signals {
        self.signals
    }
}

/// Real source, phase-B shape: GPU utilization/memory and the ADR-0007
/// signals are reported absent until the level-3 producers (NVML via
/// inferscope, vLLM prefix-cache scrape) are wired in. Absent is honest:
/// the planner must fail-open, and a fabricated 0.0 would instead be a
/// *bad* score (see phase-A sanitization semantics).
struct Real;

impl SignalSource for Real {
    fn name(&self) -> &'static str {
        "real"
    }
    fn sample(&mut self) -> Signals {
        Signals {
            gpu_utilization: 0.0,
            gpu_memory_used_bytes: 0,
            active_service_count: 0,
            kv_cache_hit_rate: None,
            tokens_per_joule: None,
        }
    }
}

fn env_f32(key: &str) -> anyhow::Result<Option<f32>> {
    match std::env::var(key) {
        Ok(v) => Ok(Some(v.parse().with_context(|| format!("{key}={v}"))?)),
        Err(_) => Ok(None),
    }
}

fn env_i64(key: &str) -> anyhow::Result<Option<i64>> {
    match std::env::var(key) {
        Ok(v) => Ok(Some(v.parse().with_context(|| format!("{key}={v}"))?)),
        Err(_) => Ok(None),
    }
}

fn env_i32(key: &str) -> anyhow::Result<Option<i32>> {
    match std::env::var(key) {
        Ok(v) => Ok(Some(v.parse().with_context(|| format!("{key}={v}"))?)),
        Err(_) => Ok(None),
    }
}

/// Ensure the NodeState for this node exists (empty spec -> server-side
/// defaults, e.g. reportIntervalSeconds: 15). Racing with another creator
/// is tolerated: AlreadyExists is success.
async fn ensure_node_state(api: &Api<NodeState>, node: &str) -> anyhow::Result<()> {
    let ns = NodeState::new(node, NodeStateSpec::default());
    match api.create(&PostParams::default(), &ns).await {
        Ok(_) => {
            info!("created NodeState '{node}'");
            Ok(())
        }
        Err(kube::Error::Api(e)) if e.code == 409 => Ok(()),
        Err(e) => Err(e.into()),
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,kube=warn".into()),
        )
        .init();

    let node = match std::env::var("NODE_NAME") {
        Ok(v) if !v.is_empty() => v,
        _ => bail!("NODE_NAME is required (downward API spec.nodeName)"),
    };
    let namespace = match std::env::var("POD_NAMESPACE") {
        Ok(v) if !v.is_empty() => v,
        _ => bail!("POD_NAMESPACE is required (downward API metadata.namespace)"),
    };

    let mut source: Box<dyn SignalSource> = if std::env::var("REPORTER_SYNTHETIC").is_ok() {
        Box::new(Synthetic::from_env()?)
    } else {
        Box::new(Real)
    };

    let client = Client::try_default().await?;
    let api: Api<NodeState> = Api::namespaced(client, &namespace);
    ensure_node_state(&api, &node).await?;
    info!(
        "reporter started: node='{node}' namespace='{namespace}' source='{}'",
        source.name()
    );

    loop {
        let interval = match api.get(&node).await {
            Ok(ns) => ns.spec.report_interval_seconds.max(1) as u64,
            Err(e) => {
                warn!("failed to read NodeState '{node}': {e}; retrying in 15s");
                tokio::time::sleep(std::time::Duration::from_secs(15)).await;
                continue;
            }
        };
        let s = source.sample();
        // Note on Option -> JSON: None serializes as null, and RFC 7386
        // merge-patch semantics *delete* the key on null. That is exactly
        // the ADR-0007 contract: a signal the source cannot measure must be
        // absent on status (fail-open), not a fabricated value.
        let patch = json!({"status": {
            "lastReportTime": Utc::now().to_rfc3339(),
            "gpuUtilization": s.gpu_utilization,
            "gpuMemoryUsedBytes": s.gpu_memory_used_bytes,
            "activeServiceCount": s.active_service_count,
            "kvCacheHitRate": s.kv_cache_hit_rate,
            "tokensPerJoule": s.tokens_per_joule,
        }});
        match api
            .patch_status(&node, &PatchParams::default(), &Patch::Merge(&patch))
            .await
        {
            Ok(_) => info!("reported: {s:?}"),
            Err(e) => warn!("status patch failed for '{node}': {e}"),
        }
        tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
    }
}
