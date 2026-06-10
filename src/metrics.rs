use prometheus_client::{
    encoding::{text::encode, EncodeLabelSet},
    metrics::{counter::Counter, family::Family, gauge::Gauge, histogram::Histogram},
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

#[derive(Clone)]
pub struct Metrics {
    pub reconcile_runs: Counter,
    pub reconcile_failures: Family<ErrorLabels, Counter>,
    pub reconcile_duration: Histogram,
    pub phase: Family<PhaseLabels, Gauge>,
    pub registry: Arc<Registry>,
}

impl Default for Metrics {
    fn default() -> Self {
        let reconcile_runs = Counter::default();
        let reconcile_failures = Family::<ErrorLabels, Counter>::default();
        let reconcile_duration = Histogram::new([0.01, 0.1, 0.25, 0.5, 1.0, 5.0, 15.0, 60.0]);
        let phase = Family::<PhaseLabels, Gauge>::default();

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
        registry.register(
            "vllmservice_phase",
            "Current phase of each VllmService (1 = active)",
            phase.clone(),
        );

        Self {
            reconcile_runs,
            reconcile_failures,
            reconcile_duration,
            phase,
            registry: Arc::new(registry),
        }
    }
}

impl Metrics {
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

    /// Set the current phase to 1 and all other known phases to 0 for an object.
    pub fn set_phase(&self, name: &str, namespace: &str, current: &str, all: &[&str]) {
        for p in all {
            let value = if *p == current { 1 } else { 0 };
            self.phase
                .get_or_create(&PhaseLabels {
                    name: name.to_string(),
                    namespace: namespace.to_string(),
                    phase: (*p).to_string(),
                })
                .set(value);
        }
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

    #[test]
    fn encodes_expected_series() {
        let m = Metrics::default();
        {
            let _measurer = m.count_and_measure();
        }
        m.set_failure("svc-a", "ApplyError");
        m.set_phase("svc-a", "default", "Ready", &["Pending", "Ready", "Failed"]);

        let out = m.encode();
        assert!(out.contains("vcso_reconcile_runs_total"));
        assert!(out.contains("vcso_reconcile_failures_total"));
        assert!(out.contains("vcso_reconcile_duration_seconds"));
        assert!(out.contains("vcso_vllmservice_phase"));
        assert!(out.contains("phase=\"Ready\""));
    }
}
