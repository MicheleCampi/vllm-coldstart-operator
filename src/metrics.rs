use crate::VllmService;
use kube::runtime::reflector::Store;
use kube::ResourceExt;
use prometheus_client::{
    collector::Collector,
    encoding::{text::encode, DescriptorEncoder, EncodeLabelSet, EncodeMetric},
    metrics::{counter::Counter, family::Family, gauge::ConstGauge, histogram::Histogram},
    registry::{Registry, Unit},
};
use std::sync::Arc;
use std::time::Instant;

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct ErrorLabels {
    pub instance: String,
    pub error: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct PhaseLabels {
    pub name: String,
    pub namespace: String,
    pub phase: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct ServiceLabels {
    pub name: String,
    pub namespace: String,
}

/// The phases a VllmService reports, one series each of vllmservice_phase.
pub const PHASES: [&str; 3] = ["Pending", "Warming", "Ready"];

#[derive(Clone)]
pub struct Metrics {
    pub reconcile_runs: Counter,
    pub reconcile_failures: Family<ErrorLabels, Counter>,
    pub reconcile_duration: Histogram,
    pub registry: Arc<Registry>,
}

impl Default for Metrics {
    fn default() -> Self {
        Self::build(None)
    }
}

/// The per-VllmService series, read from the controller's cache on each
/// scrape (ADR-0023). A deleted VllmService leaves the cache and its series
/// leave the scrape with it; a gauge set in the reconcile would stay until
/// the operator restarts.
#[derive(Debug)]
struct ServiceCollector {
    services: Store<VllmService>,
}

impl Collector for ServiceCollector {
    fn encode(&self, mut encoder: DescriptorEncoder) -> Result<(), std::fmt::Error> {
        let services = self.services.state();
        {
            let mut family = encoder.encode_descriptor(
                "vllmservice_phase",
                "Current phase of each VllmService (1 = active)",
                None,
                ConstGauge::new(0i64).metric_type(),
            )?;
            for svc in &services {
                // No status yet: the first reconcile has not written a phase.
                let Some(current) = svc.status.as_ref().map(|s| s.phase.as_str()) else {
                    continue;
                };
                for p in PHASES {
                    let labels = PhaseLabels {
                        name: svc.name_any(),
                        namespace: svc.namespace().unwrap_or_default(),
                        phase: p.to_string(),
                    };
                    ConstGauge::new(i64::from(p == current))
                        .encode(family.encode_family(&labels)?)?;
                }
            }
        }
        {
            let mut family = encoder.encode_descriptor(
                "vllmservice_objective_ttft_seconds",
                "Time to first token objective declared by the team (spec.objective.ttftSeconds)",
                None,
                ConstGauge::new(0f64).metric_type(),
            )?;
            for svc in &services {
                // Exported only when the team declared it (spec.objective / spec.budget).
                let Some(v) = svc.spec.objective.as_ref().map(|o| o.ttft_seconds) else {
                    continue;
                };
                let labels = ServiceLabels {
                    name: svc.name_any(),
                    namespace: svc.namespace().unwrap_or_default(),
                };
                ConstGauge::new(f64::from(v)).encode(family.encode_family(&labels)?)?;
            }
        }
        {
            let mut family = encoder.encode_descriptor(
                "vllmservice_objective_percentile",
                "Share of requests that must meet the TTFT objective, in percent (spec.objective.percentile)",
                None,
                ConstGauge::new(0f64).metric_type(),
            )?;
            for svc in &services {
                // Exported only when the team declared it (spec.objective / spec.budget).
                let Some(v) = svc.spec.objective.as_ref().map(|o| o.percentile) else {
                    continue;
                };
                let labels = ServiceLabels {
                    name: svc.name_any(),
                    namespace: svc.namespace().unwrap_or_default(),
                };
                ConstGauge::new(f64::from(v)).encode(family.encode_family(&labels)?)?;
            }
        }
        {
            let mut family = encoder.encode_descriptor(
                "vllmservice_budget_eur_per_month",
                "Monthly budget declared by the team, in EUR (spec.budget.eurPerMonth)",
                None,
                ConstGauge::new(0f64).metric_type(),
            )?;
            for svc in &services {
                // Exported only when the team declared it (spec.objective / spec.budget).
                let Some(v) = svc.spec.budget.as_ref().map(|b| b.eur_per_month) else {
                    continue;
                };
                let labels = ServiceLabels {
                    name: svc.name_any(),
                    namespace: svc.namespace().unwrap_or_default(),
                };
                ConstGauge::new(f64::from(v)).encode(family.encode_family(&labels)?)?;
            }
        }
        let mut family = encoder.encode_descriptor(
            "vllmservice_desired_replicas",
            "Replicas each VllmService asks for (spec.replicas)",
            None,
            ConstGauge::new(0i64).metric_type(),
        )?;
        for svc in &services {
            let labels = ServiceLabels {
                name: svc.name_any(),
                namespace: svc.namespace().unwrap_or_default(),
            };
            ConstGauge::new(i64::from(svc.spec.replicas)).encode(family.encode_family(&labels)?)?;
        }
        Ok(())
    }
}

impl Metrics {
    /// Metrics whose per-VllmService series come from the controller's cache
    /// (ADR-0023).
    pub fn with_services(services: Store<VllmService>) -> Self {
        Self::build(Some(services))
    }

    fn build(services: Option<Store<VllmService>>) -> Self {
        let reconcile_runs = Counter::default();
        let reconcile_failures = Family::<ErrorLabels, Counter>::default();
        let reconcile_duration = Histogram::new([0.01, 0.1, 0.25, 0.5, 1.0, 5.0, 15.0, 60.0]);

        let mut registry = Registry::with_prefix("vcso");
        registry.register(
            "reconcile_runs",
            "Total number of reconciliations",
            reconcile_runs.clone(),
        );
        registry.register(
            "reconcile_failures",
            "Total number of reconciliation errors",
            reconcile_failures.clone(),
        );
        registry.register_with_unit(
            "reconcile_duration",
            "Reconcile duration",
            Unit::Seconds,
            reconcile_duration.clone(),
        );
        if let Some(services) = services {
            registry.register_collector(Box::new(ServiceCollector { services }));
        }

        Self {
            reconcile_runs,
            reconcile_failures,
            reconcile_duration,
            registry: Arc::new(registry),
        }
    }

    /// Encode the registry in OpenMetrics text format.
    pub fn encode(&self) -> String {
        let mut buffer = String::new();
        encode(&mut buffer, &self.registry).expect("encoding metrics into string");
        buffer
    }

    /// Record one reconcile failure for a given object and error label.
    pub fn set_failure(&self, instance: &str, error: &str) {
        self.reconcile_failures
            .get_or_create(&ErrorLabels {
                instance: instance.to_string(),
                error: error.to_string(),
            })
            .inc();
    }

    /// Increment the reconcile counter and return a measurer that records
    /// duration on drop.
    pub fn count_and_measure(&self) -> ReconcileMeasurer {
        self.reconcile_runs.inc();
        ReconcileMeasurer {
            start: Instant::now(),
            metric: self.reconcile_duration.clone(),
        }
    }
}

/// Records reconcile duration into the histogram when dropped.
pub struct ReconcileMeasurer {
    start: Instant,
    metric: Histogram,
}

impl Drop for ReconcileMeasurer {
    fn drop(&mut self) {
        self.metric.observe(self.start.elapsed().as_secs_f64());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kube::runtime::{reflector::store, watcher};

    fn service(name: &str, replicas: i32, phase: &str) -> VllmService {
        serde_json::from_value(serde_json::json!({
            "apiVersion": "inference.michelecampi.dev/v1alpha1",
            "kind": "VllmService",
            "metadata": {"name": name, "namespace": "vllm-system"},
            "spec": {"model": "m", "replicas": replicas},
            "status": {"phase": phase, "message": ""}
        }))
        .unwrap()
    }

    #[test]
    fn encodes_expected_series() {
        let m = Metrics::default();
        {
            let _measurer = m.count_and_measure();
        }
        m.set_failure("svc-a", "ApplyError");

        let out = m.encode();
        assert!(out.contains("vcso_reconcile_runs_total"));
        assert!(out.contains("vcso_reconcile_failures_total"));
        assert!(out.contains("vcso_reconcile_duration_seconds"));
        assert!(
            !out.contains("vllmservice_"),
            "no cache, no per-object series"
        );
    }

    #[test]
    fn per_service_series_follow_the_cache() {
        let (reader, mut writer) = store::<VllmService>();
        let m = Metrics::with_services(reader);
        writer.apply_watcher_event(&watcher::Event::Apply(service("a", 1, "Ready")));
        writer.apply_watcher_event(&watcher::Event::Apply(service("z", 0, "Pending")));

        let out = m.encode();
        assert!(!out.contains("vcso_vcso"), "{out}");
        assert!(
            out.contains(
                "vcso_vllmservice_desired_replicas{name=\"a\",namespace=\"vllm-system\"} 1"
            ),
            "{out}"
        );
        assert!(
            out.contains(
                "vcso_vllmservice_desired_replicas{name=\"z\",namespace=\"vllm-system\"} 0"
            ),
            "{out}"
        );
        assert!(
            out.contains(
                "vcso_vllmservice_phase{name=\"a\",namespace=\"vllm-system\",phase=\"Ready\"} 1"
            ),
            "{out}"
        );
        assert!(
            out.contains(
                "vcso_vllmservice_phase{name=\"a\",namespace=\"vllm-system\",phase=\"Pending\"} 0"
            ),
            "{out}"
        );

        // Nothing declared, nothing exported.
        // The HELP/TYPE lines stay; no series does.
        for series in [
            "vllmservice_objective_ttft_seconds{",
            "vllmservice_objective_percentile{",
            "vllmservice_budget_eur_per_month{",
        ] {
            assert!(!out.contains(series), "{series} in {out}");
        }

        // A deleted VllmService leaves no series behind.
        writer.apply_watcher_event(&watcher::Event::Delete(service("a", 1, "Ready")));
        let out = m.encode();
        assert!(!out.contains("name=\"a\""), "{out}");
        assert!(
            out.contains(
                "vcso_vllmservice_desired_replicas{name=\"z\",namespace=\"vllm-system\"} 0"
            ),
            "{out}"
        );
    }

    #[test]
    fn objective_and_budget_series_when_declared() {
        let (reader, mut writer) = store::<VllmService>();
        let m = Metrics::with_services(reader);
        let mut svc = service("b", 1, "Ready");
        svc.spec.objective = Some(crate::Objective {
            ttft_seconds: 1.0,
            percentile: 95.0,
        });
        svc.spec.budget = Some(crate::Budget {
            eur_per_month: 500.0,
        });
        writer.apply_watcher_event(&watcher::Event::Apply(svc));

        let out = m.encode();
        for want in [
            "vcso_vllmservice_objective_ttft_seconds{name=\"b\",namespace=\"vllm-system\"} 1",
            "vcso_vllmservice_objective_percentile{name=\"b\",namespace=\"vllm-system\"} 95",
            "vcso_vllmservice_budget_eur_per_month{name=\"b\",namespace=\"vllm-system\"} 500",
        ] {
            assert!(out.contains(want), "{want} missing in {out}");
        }
    }
}
