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
    /// All signals are Option: None = not measured, and merge-patch null
    /// deletes the status key (fail-open). A source must never fabricate.
    gpu_utilization: Option<f32>,
    gpu_memory_used_bytes: Option<i64>,
    active_service_count: Option<i32>,
    kv_cache_hit_rate: Option<f32>,
    tokens_per_joule: Option<f32>,
}

#[async_trait::async_trait]
trait SignalSource {
    fn name(&self) -> &'static str;
    async fn sample(&mut self) -> Signals;
}

/// Deterministic source for the kind rehearsal. Every value can be pinned
/// via env so seed topologies (A/B/C) map to per-node DaemonSet env or
/// per-node overrides without touching code.
struct Synthetic {
    signals: Signals,
}

impl Synthetic {
    /// Two-level env resolution: `REPORTER_SYNTHETIC_<SIGNAL>_NODE_<NODE>`
    /// wins over the global `REPORTER_SYNTHETIC_<SIGNAL>`. A DaemonSet has
    /// one pod template, so per-node signal differentiation (required to
    /// falsify the EA comparator in the rehearsal) must happen here, keyed
    /// on downward-API identity — not in per-node infrastructure.
    fn from_env(node: &str) -> anyhow::Result<Self> {
        let suffix = format!(
            "_NODE_{}",
            node.replace(['-', '.'], "_").to_ascii_uppercase()
        );
        let f32_of = |key: &str| -> anyhow::Result<Option<f32>> {
            match env_f32(&format!("{key}{suffix}"))? {
                Some(v) => Ok(Some(v)),
                None => env_f32(key),
            }
        };
        let i64_of = |key: &str| -> anyhow::Result<Option<i64>> {
            match env_i64(&format!("{key}{suffix}"))? {
                Some(v) => Ok(Some(v)),
                None => env_i64(key),
            }
        };
        let i32_of = |key: &str| -> anyhow::Result<Option<i32>> {
            match env_i32(&format!("{key}{suffix}"))? {
                Some(v) => Ok(Some(v)),
                None => env_i32(key),
            }
        };
        Ok(Self {
            signals: Signals {
                // Unset env now means absent, not zero: the synthetic
                // source must be able to rehearse a missing signal too.
                gpu_utilization: f32_of("REPORTER_SYNTHETIC_GPU_UTILIZATION")?,
                gpu_memory_used_bytes: i64_of("REPORTER_SYNTHETIC_GPU_MEMORY_USED_BYTES")?,
                active_service_count: i32_of("REPORTER_SYNTHETIC_ACTIVE_SERVICE_COUNT")?,
                kv_cache_hit_rate: f32_of("REPORTER_SYNTHETIC_KV_CACHE_HIT_RATE")?,
                tokens_per_joule: f32_of("REPORTER_SYNTHETIC_TOKENS_PER_JOULE")?,
            },
        })
    }
}

#[async_trait::async_trait]
impl SignalSource for Synthetic {
    fn name(&self) -> &'static str {
        "synthetic"
    }
    async fn sample(&mut self) -> Signals {
        self.signals
    }
}

/// Real source, phase-B shape: GPU utilization/memory and the ADR-0007
/// signals are reported absent until the level-3 producers (NVML via
/// inferscope, vLLM prefix-cache scrape) are wired in. Absent is honest:
/// the planner must fail-open, and a fabricated 0.0 would instead be a
/// *bad* score (see phase-A sanitization semantics).
struct Real {
    scrape: Option<VllmScrape>,
}

impl Real {
    /// `REPORTER_SCRAPE_TARGETS`: comma-separated `/metrics` URLs of the
    /// vLLM services on this node. Unset or empty = scraping off, every
    /// signal absent — today's behaviour, no chart change required.
    fn from_env() -> Self {
        let targets: Vec<String> = std::env::var("REPORTER_SCRAPE_TARGETS")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(String::from)
            .collect();
        Self {
            scrape: (!targets.is_empty()).then(|| VllmScrape::new(targets)),
        }
    }
}

#[async_trait::async_trait]
impl SignalSource for Real {
    fn name(&self) -> &'static str {
        "real"
    }
    async fn sample(&mut self) -> Signals {
        let mut out = Signals {
            gpu_utilization: None,
            gpu_memory_used_bytes: None,
            active_service_count: None,
            kv_cache_hit_rate: None,
            tokens_per_joule: None,
        };
        if let Some(s) = &mut self.scrape {
            let round = s.sample().await;
            out.kv_cache_hit_rate = round.kv_cache_hit_rate;
            out.active_service_count = Some(round.responding_targets);
        }
        out
    }
}

/// Scrapes the vLLM-schema Prometheus text endpoint of each configured
/// target and derives `kv_cache_hit_rate` from counter deltas between
/// consecutive rounds (ADR-011 series: `vllm:prefix_cache_hits` /
/// `vllm:prefix_cache_queries`). Per-target fail-open: an unreachable or
/// unparseable target contributes nothing and raises no error.
struct VllmScrape {
    client: reqwest::Client,
    targets: Vec<String>,
    /// Per-target counter baseline (hits, queries), keyed by URL.
    prev: std::collections::HashMap<String, (f64, f64)>,
}

struct ScrapeRound {
    kv_cache_hit_rate: Option<f32>,
    responding_targets: i32,
}

impl VllmScrape {
    fn new(targets: Vec<String>) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(2))
                .build()
                .expect("reqwest client with static config cannot fail"),
            targets,
            prev: std::collections::HashMap::new(),
        }
    }

    async fn sample(&mut self) -> ScrapeRound {
        let mut responding = 0i32;
        let mut hits_delta = 0.0f64;
        let mut queries_delta = 0.0f64;
        let mut have_delta = false;
        for url in &self.targets {
            let body = match self.client.get(url).send().await {
                Ok(resp) => match resp.text().await {
                    Ok(b) => b,
                    Err(e) => {
                        warn!("scrape '{url}': body read failed: {e}");
                        continue;
                    }
                },
                Err(e) => {
                    warn!("scrape '{url}': request failed: {e}");
                    continue;
                }
            };
            let (Some(h), Some(q)) = (
                sum_family(&body, "vllm:prefix_cache_hits"),
                sum_family(&body, "vllm:prefix_cache_queries"),
            ) else {
                warn!("scrape '{url}': vllm prefix-cache series missing");
                continue;
            };
            responding += 1;
            if let Some(&(ph, pq)) = self.prev.get(url.as_str()) {
                let (dh, dq) = (h - ph, q - pq);
                if dh >= 0.0 && dq >= 0.0 {
                    hits_delta += dh;
                    queries_delta += dq;
                    have_delta = true;
                }
                // Negative delta = counter reset (engine restart): discard
                // this round's delta for the target, realign the baseline.
            }
            self.prev.insert(url.clone(), (h, q));
        }
        ScrapeRound {
            // First round has no baseline; a round with zero queries has no
            // rate. Both are honestly absent, not 0.0 (which would read as
            // "measured perfectly cold cache").
            kv_cache_hit_rate: (have_delta && queries_delta > 0.0)
                .then(|| (hits_delta / queries_delta) as f32),
            responding_targets: responding,
        }
    }
}

/// Sum every sample of one metric family in a Prometheus text body across
/// label sets. Guard on the char after the family name (`{` or whitespace)
/// so a longer family sharing the prefix never matches. None = family not
/// present in the body.
fn sum_family(body: &str, family: &str) -> Option<f64> {
    let mut sum = 0.0f64;
    let mut seen = false;
    for raw in body.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(rest) = line.strip_prefix(family) else {
            continue;
        };
        let rest = match rest.chars().next() {
            Some('{') => match rest.find('}') {
                Some(i) => &rest[i + 1..],
                None => continue,
            },
            Some(c) if c.is_whitespace() => rest,
            _ => continue,
        };
        let Some(tok) = rest.split_whitespace().next() else {
            continue;
        };
        let Ok(v) = tok.parse::<f64>() else { continue };
        sum += v;
        seen = true;
    }
    seen.then_some(sum)
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
        Box::new(Synthetic::from_env(&node)?)
    } else {
        Box::new(Real::from_env())
    };

    let client = Client::try_default().await?;
    let api: Api<NodeState> = Api::namespaced(client, &namespace);
    ensure_node_state(&api, &node).await?;
    info!(
        "reporter started: node='{node}' namespace='{namespace}' source='{}'",
        source.name()
    );

    loop {
        let (interval, generation) = match api.get(&node).await {
            Ok(ns) => (
                ns.spec.report_interval_seconds.max(1) as u64,
                ns.metadata.generation.unwrap_or(0),
            ),
            Err(e) => {
                warn!("failed to read NodeState '{node}': {e}; retrying in 15s");
                tokio::time::sleep(std::time::Duration::from_secs(15)).await;
                continue;
            }
        };
        let s = source.sample().await;
        // Note on Option -> JSON: None serializes as null, and RFC 7386
        // merge-patch semantics *delete* the key on null. That is exactly
        // the ADR-0007 contract: a signal the source cannot measure must be
        // absent on status (fail-open), not a fabricated value.
        let patch = json!({"status": {
            "observedGeneration": generation,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sum_family_sums_label_sets_and_guards_prefix() {
        let body = "\
# HELP vllm:prefix_cache_hits hits\n\
# TYPE vllm:prefix_cache_hits counter\n\
vllm:prefix_cache_hits{model=\"a\"} 10\n\
vllm:prefix_cache_hits{model=\"b\"} 5\n\
vllm:prefix_cache_hits_extended{model=\"a\"} 999\n\
vllm:prefix_cache_queries 40\n";
        assert_eq!(sum_family(body, "vllm:prefix_cache_hits"), Some(15.0));
        assert_eq!(sum_family(body, "vllm:prefix_cache_queries"), Some(40.0));
        assert_eq!(sum_family(body, "vllm:absent"), None);
    }

    #[test]
    fn sum_family_skips_garbage_values() {
        let body = "vllm:prefix_cache_hits notanumber\nvllm:prefix_cache_hits 3\n";
        assert_eq!(sum_family(body, "vllm:prefix_cache_hits"), Some(3.0));
    }

    /// Drives the delta logic without HTTP: same VllmScrape state machine,
    /// feeding parsed (hits, queries) pairs the way sample() would after a
    /// successful scrape of one target.
    fn feed(s: &mut VllmScrape, url: &str, h: f64, q: f64) -> Option<f32> {
        let mut hits_delta = 0.0f64;
        let mut queries_delta = 0.0f64;
        let mut have_delta = false;
        if let Some(&(ph, pq)) = s.prev.get(url) {
            let (dh, dq) = (h - ph, q - pq);
            if dh >= 0.0 && dq >= 0.0 {
                hits_delta += dh;
                queries_delta += dq;
                have_delta = true;
            }
        }
        s.prev.insert(url.to_string(), (h, q));
        (have_delta && queries_delta > 0.0).then(|| (hits_delta / queries_delta) as f32)
    }

    #[test]
    fn delta_rate_first_round_absent_then_measured_then_reset() {
        let mut s = VllmScrape::new(vec!["u".into()]);
        assert_eq!(feed(&mut s, "u", 100.0, 200.0), None); // no baseline
        assert_eq!(feed(&mut s, "u", 130.0, 240.0), Some(0.75)); // 30/40
        assert_eq!(feed(&mut s, "u", 5.0, 8.0), None); // counter reset
        assert_eq!(feed(&mut s, "u", 8.0, 12.0), Some(0.75)); // realigned: 3/4
    }

    #[test]
    fn delta_rate_zero_queries_is_absent_not_zero() {
        let mut s = VllmScrape::new(vec!["u".into()]);
        assert_eq!(feed(&mut s, "u", 10.0, 20.0), None);
        assert_eq!(feed(&mut s, "u", 10.0, 20.0), None); // no traffic
    }
}
