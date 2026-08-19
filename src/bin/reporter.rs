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
    /// ADR-0009 D2 demand signals.
    requests_waiting: Option<f32>,
    requests_running: Option<f32>,
}

/// Two-level env resolution shared by every source: the per-node key
/// `<KEY>_NODE_<NODE>` wins over the global `<KEY>`. One DaemonSet pod
/// template, per-node differentiation keyed on downward-API identity.
fn env_for_node(key: &str, node: &str) -> Option<String> {
    let suffix = format!(
        "_NODE_{}",
        node.replace(['-', '.'], "_").to_ascii_uppercase()
    );
    std::env::var(format!("{key}{suffix}"))
        .or_else(|_| std::env::var(key))
        .ok()
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
                requests_waiting: f32_of("REPORTER_SYNTHETIC_REQUESTS_WAITING")?,
                requests_running: f32_of("REPORTER_SYNTHETIC_REQUESTS_RUNNING")?,
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
    #[cfg(feature = "gpu-nvidia")]
    nvml: Option<NvmlSampler>,
}

impl Real {
    /// `REPORTER_SCRAPE_TARGETS`: comma-separated `/metrics` URLs of the
    /// vLLM services on this node. Unset or empty = scraping off, every
    /// signal absent — today's behaviour, no chart change required.
    fn from_env(node: &str) -> Self {
        let targets: Vec<String> = env_for_node("REPORTER_SCRAPE_TARGETS", node)
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(String::from)
            .collect();
        // Double gate, session-a lesson codified: the binary must be
        // built with feature `gpu-nvidia` AND the runtime must opt in
        // via REPORTER_GPU=nvidia (ADR-005 semantics, reporter is
        // env-driven). Either gate missing = GPU signals absent.
        #[cfg(feature = "gpu-nvidia")]
        let nvml = match std::env::var("REPORTER_GPU").as_deref() {
            Ok("nvidia") => NvmlSampler::init(),
            Ok(other) => {
                warn!(
                    "REPORTER_GPU='{other}' not recognized (expected \
                     'nvidia'); GPU signals stay absent"
                );
                None
            }
            Err(_) => None,
        };
        #[cfg(not(feature = "gpu-nvidia"))]
        if std::env::var("REPORTER_GPU").is_ok() {
            warn!(
                "REPORTER_GPU is set but this binary was built without \
                 feature 'gpu-nvidia'; GPU signals stay absent"
            );
        }
        Self {
            scrape: (!targets.is_empty()).then(|| VllmScrape::new(targets)),
            #[cfg(feature = "gpu-nvidia")]
            nvml,
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
            requests_waiting: None,
            requests_running: None,
        };
        let mut generation_tokens_delta: Option<f64> = None;
        if let Some(s) = &mut self.scrape {
            let round = s.sample().await;
            out.kv_cache_hit_rate = round.kv_cache_hit_rate;
            out.active_service_count = Some(round.responding_targets);
            out.requests_waiting = round.requests_waiting;
            out.requests_running = round.requests_running;
            generation_tokens_delta = round.generation_tokens_delta;
        }
        #[cfg(feature = "gpu-nvidia")]
        if let Some(n) = &mut self.nvml {
            let g = n.sample();
            out.gpu_utilization = g.gpu_utilization;
            out.gpu_memory_used_bytes = g.gpu_memory_used_bytes;
            // ADR-0007 tokens/joule: cross-source join on the same round.
            // Both deltas must exist and energy must be non-zero; anything
            // else is honestly absent (first round, reset, no traffic,
            // series missing). Never fabricate a 0.0.
            if let (Some(t), Some(j)) = (generation_tokens_delta, g.energy_delta_joules) {
                if j > 0.0 {
                    out.tokens_per_joule = Some((t / j) as f32);
                }
            }
        }
        #[cfg(not(feature = "gpu-nvidia"))]
        let _ = generation_tokens_delta;
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
    /// Per-target counter baseline (hits, queries, generation-tokens),
    /// keyed by URL. The token series is Option: a target may expose the
    /// prefix-cache series without `generation_tokens_total`.
    prev: std::collections::HashMap<String, (f64, f64, Option<f64>)>,
}

struct ScrapeRound {
    kv_cache_hit_rate: Option<f32>,
    /// ADR-0009 D2: summed gauges, read independently of the prefix-cache
    /// gate below — a vLLM without prefix caching still has a queue.
    requests_waiting: Option<f32>,
    requests_running: Option<f32>,
    responding_targets: i32,
    /// Summed `vllm:generation_tokens_total` delta across targets this
    /// round. Feeds the ADR-0007 tokens/joule join in `Real::sample`.
    generation_tokens_delta: Option<f64>,
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

    /// Per-target delta step of the counter state machine: returns the
    /// (hits, queries) delta and the generation-tokens delta for this
    /// round, or None where no valid delta exists (first round, counter
    /// reset, series missing on either side). Extracted from `sample()`
    /// so tests drive this exact code instead of a copy.
    fn advance(
        &mut self,
        url: &str,
        h: f64,
        q: f64,
        g: Option<f64>,
    ) -> (Option<(f64, f64)>, Option<f64>) {
        let mut hq = None;
        let mut dg_out = None;
        if let Some(&(ph, pq, pg)) = self.prev.get(url) {
            let (dh, dq) = (h - ph, q - pq);
            if dh >= 0.0 && dq >= 0.0 {
                hq = Some((dh, dq));
            }
            // Negative delta = counter reset (engine restart): discard
            // this round's delta for the target, realign the baseline.
            if let (Some(gc), Some(gp)) = (g, pg) {
                let dg = gc - gp;
                if dg >= 0.0 {
                    dg_out = Some(dg);
                }
            }
        }
        self.prev.insert(url.to_string(), (h, q, g));
        (hq, dg_out)
    }

    async fn sample(&mut self) -> ScrapeRound {
        let mut responding = 0i32;
        let mut hits_delta = 0.0f64;
        let mut queries_delta = 0.0f64;
        let mut have_delta = false;
        let mut tokens_delta: Option<f64> = None;
        let mut waiting: Option<f64> = None;
        let mut running: Option<f64> = None;
        for i in 0..self.targets.len() {
            let url = self.targets[i].clone();
            let body = match self.client.get(&url).send().await {
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
            // ADR-0009 D2: read before the prefix-cache gate. These two
            // are unrelated to KV caching, and a vLLM started without
            // prefix caching must still report its queue.
            if let Some(v) = sum_family(&body, "vllm:num_requests_waiting") {
                *waiting.get_or_insert(0.0) += v;
            }
            if let Some(v) = sum_family(&body, "vllm:num_requests_running") {
                *running.get_or_insert(0.0) += v;
            }
            let (Some(h), Some(q)) = (
                sum_family_totaled(&body, "vllm:prefix_cache_hits"),
                sum_family_totaled(&body, "vllm:prefix_cache_queries"),
            ) else {
                warn!("scrape '{url}': vllm prefix-cache series missing");
                continue;
            };
            responding += 1;
            let g = sum_family(&body, "vllm:generation_tokens_total");
            let (hq, dg) = self.advance(&url, h, q, g);
            if let Some((dh, dq)) = hq {
                hits_delta += dh;
                queries_delta += dq;
                have_delta = true;
            }
            if let Some(dg) = dg {
                *tokens_delta.get_or_insert(0.0) += dg;
            }
        }
        ScrapeRound {
            // First round has no baseline; a round with zero queries has no
            // rate. Both are honestly absent, not 0.0 (which would read as
            // "measured perfectly cold cache").
            kv_cache_hit_rate: (have_delta && queries_delta > 0.0)
                .then(|| (hits_delta / queries_delta) as f32),
            responding_targets: responding,
            requests_waiting: waiting.map(|v| v as f32),
            requests_running: running.map(|v| v as f32),
            generation_tokens_delta: tokens_delta,
        }
    }
}

/// vLLM >=0.10 emits Prometheus counters with the OpenMetrics `_total`
/// suffix (`vllm:prefix_cache_hits_total`); older engines and the ADR-011
/// validation fixtures use the bare name. Try suffixed first, then bare.
/// (Found live on A10 + vLLM v0.23.0, level-3 session: bare-name lookup
/// missed every real series -> kv/tpj permanently absent.)
fn sum_family_totaled(body: &str, family: &str) -> Option<f64> {
    sum_family(body, &format!("{family}_total")).or_else(|| sum_family(body, family))
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

/// NVML sampler (ADR-0007 level 3). Same library and counter strategy
/// validated in inferscope ADR-010: `total_energy_consumption` (mJ) read
/// once per round, delta between consecutive rounds = round energy.
/// Multi-device nodes: utilization averaged, memory and energy summed
/// (NodeState signals are per-node). Fail-open at every stage: NVML
/// unavailable at startup disables the sampler for the process lifetime;
/// a failed per-round read yields absent signals for that round.
#[cfg(feature = "gpu-nvidia")]
struct NvmlSampler {
    nvml: nvml_wrapper::Nvml,
    /// Summed energy counter baseline (mJ) of the previous *complete*
    /// round. A partial sum joined against a complete baseline would
    /// fabricate a shrunken or negative delta, so incomplete rounds
    /// clear the baseline instead.
    prev_energy_mj: Option<u64>,
}

#[cfg(feature = "gpu-nvidia")]
struct NvmlRound {
    gpu_utilization: Option<f32>,
    gpu_memory_used_bytes: Option<i64>,
    energy_delta_joules: Option<f64>,
}

#[cfg(feature = "gpu-nvidia")]
impl NvmlSampler {
    fn init() -> Option<Self> {
        match nvml_wrapper::Nvml::init() {
            Ok(nvml) => Some(Self {
                nvml,
                prev_energy_mj: None,
            }),
            Err(e) => {
                warn!(
                    "REPORTER_GPU=nvidia but NVML init failed: {e}; \
                     GPU signals stay absent (fail-open)"
                );
                None
            }
        }
    }

    fn sample(&mut self) -> NvmlRound {
        let count = self.nvml.device_count().unwrap_or(0);
        let mut util_sum = 0.0f64;
        let mut util_n = 0u32;
        let mut mem_sum: i64 = 0;
        let mut mem_seen = false;
        let mut energy_sum: u64 = 0;
        let mut energy_all = count > 0;
        for index in 0..count {
            let Ok(device) = self.nvml.device_by_index(index) else {
                energy_all = false;
                continue;
            };
            if let Ok(u) = device.utilization_rates() {
                util_sum += u.gpu as f64;
                util_n += 1;
            }
            if let Ok(m) = device.memory_info() {
                mem_sum += m.used as i64;
                mem_seen = true;
            }
            match device.total_energy_consumption() {
                Ok(mj) => energy_sum += mj,
                Err(_) => energy_all = false,
            }
        }
        let energy_delta_joules = if energy_all {
            // checked_sub: a lower reading than the baseline (counter
            // reset) discards this round's delta and realigns below.
            let delta = self
                .prev_energy_mj
                .and_then(|prev| energy_sum.checked_sub(prev))
                .map(|mj| mj as f64 / 1000.0);
            self.prev_energy_mj = Some(energy_sum);
            delta
        } else {
            self.prev_energy_mj = None;
            None
        };
        NvmlRound {
            gpu_utilization: (util_n > 0).then(|| (util_sum / f64::from(util_n)) as f32),
            gpu_memory_used_bytes: mem_seen.then_some(mem_sum),
            energy_delta_joules,
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
    // Two rustls crypto providers reach the tree (kube pulls ring,
    // reqwest's feature graph pulls aws-lc-rs): auto-detection refuses to
    // pick one at runtime, so install explicitly before any TLS init.
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("install rustls ring provider before any TLS use");
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
        Box::new(Real::from_env(&node))
    };

    // ADR-0008 D2 retention. Only the two ADR-0007 signals are retained: they
    // are the ones constraint P makes unobservable at decision time, because a
    // node producing them cannot simultaneously be a placement target. NVML's
    // utilisation and memory remain available on an idle node and must not
    // acquire a horizon. Retention is unconditional — a reporter-side timeout
    // would be a threshold, and thresholds belong to the planner.
    let mut last_hit_rate: Option<(f32, String)> = None;
    let mut last_tokens_per_joule: Option<(f32, String)> = None;

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
        // A fresh measurement replaces the retained one and stamps *now* as the
        // instant it was taken; a sample that produced nothing leaves the
        // previous pair untouched, so the value republished below keeps the
        // timestamp of when it was actually measured.
        let measured_at = Utc::now().to_rfc3339();
        if let Some(v) = s.kv_cache_hit_rate {
            last_hit_rate = Some((v, measured_at.clone()));
        }
        if let Some(v) = s.tokens_per_joule {
            last_tokens_per_joule = Some((v, measured_at.clone()));
        }

        let patch = json!({"status": {
            "observedGeneration": generation,
            "lastReportTime": Utc::now().to_rfc3339(),
            "gpuUtilization": s.gpu_utilization,
            "gpuMemoryUsedBytes": s.gpu_memory_used_bytes,
            "activeServiceCount": s.active_service_count,
            "kvCacheHitRate": last_hit_rate.as_ref().map(|(v, _)| *v),
            "kvCacheHitRateObservedAt": last_hit_rate.as_ref().map(|(_, t)| t.clone()),
            "tokensPerJoule": last_tokens_per_joule.as_ref().map(|(v, _)| *v),
            "tokensPerJouleObservedAt": last_tokens_per_joule.as_ref().map(|(_, t)| t.clone()),
            "requestsWaiting": s.requests_waiting,
            "requestsRunning": s.requests_running,
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
    fn demand_gauges_are_read_without_prefix_cache_series() {
        // ADR-0009 D2: a vLLM started without prefix caching exposes no
        // KV series. The prefix-cache gate must not take the queue with it.
        let body = "\
# TYPE vllm:num_requests_waiting gauge\n\
vllm:num_requests_waiting{model=\"a\"} 7\n\
vllm:num_requests_waiting{model=\"b\"} 5\n\
# TYPE vllm:num_requests_running gauge\n\
vllm:num_requests_running 3\n";
        assert_eq!(sum_family(body, "vllm:num_requests_waiting"), Some(12.0));
        assert_eq!(sum_family(body, "vllm:num_requests_running"), Some(3.0));
        assert_eq!(sum_family(body, "vllm:prefix_cache_hits"), None);
    }

    #[test]
    fn absent_demand_series_stay_none() {
        // Never fabricate 0.0: "no load" and "engine unreachable" must not
        // be the same value on status.
        let body = "# TYPE vllm:prefix_cache_hits counter\nvllm:prefix_cache_hits 1\n";
        assert_eq!(sum_family(body, "vllm:num_requests_waiting"), None);
    }

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
    fn sum_family_totaled_matches_openmetrics_and_bare_schemas() {
        // Real vLLM v0.23.0 shape (A10 session 2026-07-22): counters carry
        // the OpenMetrics `_total` suffix, with `_created` gauges alongside
        // that must never be summed into the family.
        let v023 = "\
vllm:prefix_cache_queries_total{engine=\"0\"} 135\n\
vllm:prefix_cache_queries_created{engine=\"0\"} 1.78e9\n\
vllm:prefix_cache_hits_total{engine=\"0\"} 62\n\
vllm:prefix_cache_hits_created{engine=\"0\"} 1.78e9\n";
        assert_eq!(
            sum_family_totaled(v023, "vllm:prefix_cache_hits"),
            Some(62.0)
        );
        assert_eq!(
            sum_family_totaled(v023, "vllm:prefix_cache_queries"),
            Some(135.0)
        );
        // Legacy / fixture shape: bare names, no suffix -> fallback path.
        let bare = "vllm:prefix_cache_hits 10\nvllm:prefix_cache_queries 40\n";
        assert_eq!(
            sum_family_totaled(bare, "vllm:prefix_cache_hits"),
            Some(10.0)
        );
        assert_eq!(
            sum_family_totaled(bare, "vllm:prefix_cache_queries"),
            Some(40.0)
        );
        assert_eq!(sum_family_totaled(v023, "vllm:absent"), None);
    }

    #[test]
    fn sum_family_skips_garbage_values() {
        let body = "vllm:prefix_cache_hits notanumber\nvllm:prefix_cache_hits 3\n";
        assert_eq!(sum_family(body, "vllm:prefix_cache_hits"), Some(3.0));
    }

    /// Hit-rate view over `advance()` for a single-target round, shaped
    /// the way `sample()` aggregates it. The tests drive the exact state
    /// machine `sample()` uses -- no duplicated delta logic.
    fn feed(s: &mut VllmScrape, url: &str, h: f64, q: f64) -> Option<f32> {
        let (hq, _) = s.advance(url, h, q, None);
        hq.and_then(|(dh, dq)| (dq > 0.0).then(|| (dh / dq) as f32))
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

    #[test]
    fn token_delta_fail_open_per_series_reset_and_realign() {
        let mut s = VllmScrape::new(vec!["u".into()]);
        assert_eq!(s.advance("u", 0.0, 0.0, Some(100.0)).1, None); // no baseline
        assert_eq!(s.advance("u", 0.0, 0.0, Some(160.0)).1, Some(60.0)); // measured
        assert_eq!(s.advance("u", 0.0, 0.0, None).1, None); // series missing this round
        assert_eq!(s.advance("u", 0.0, 0.0, Some(5.0)).1, None); // missing on prev side
        assert_eq!(s.advance("u", 0.0, 0.0, Some(2.0)).1, None); // counter reset
        assert_eq!(s.advance("u", 0.0, 0.0, Some(9.0)).1, Some(7.0)); // realigned
    }
}
