use crate::fleet_types::{PlacementStrategy, Warmth};
use std::cmp::Ordering;

/// A node eligible for placement, already filtered by the caller for
/// selector match, non-draining status, and spot-fraction cap. This
/// function only ranks among nodes already known to be eligible; it holds
/// no cluster-wide context (total spot count, node pool membership) on
/// purpose, so it stays a pure function testable without a cluster.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeCandidate {
    pub name: String,
    pub warmth: Warmth,
    pub gpu_utilization: f32,
    pub active_service_count: i32,
    /// ADR-0007: raw observed KV-cache hit-rate in [0,1]. None = signal not
    /// available; ranks below any valid observed value within a warmth class.
    pub kv_cache_hit_rate: Option<f32>,
    /// ADR-0007: raw observed tokens-per-joule (>= 0). Same absence semantics.
    pub tokens_per_joule: Option<f32>,
}

fn warmth_rank(w: &Warmth) -> u8 {
    match w {
        Warmth::Warm => 2,
        Warmth::Warming => 1,
        Warmth::Cold => 0,
    }
}

/// Warmth-first placement: prefer the warmest node, tie-break on lowest
/// active service count (spread load), then lowest GPU utilization.
///
/// Returns None on an empty candidate list — the caller's job to decide
/// what that means (no eligible node this reconcile, requeue and retry).
pub fn select_node_for_placement(candidates: &[NodeCandidate]) -> Option<&NodeCandidate> {
    candidates.iter().max_by(|a, b| {
        warmth_rank(&a.warmth)
            .cmp(&warmth_rank(&b.warmth))
            .then_with(|| b.active_service_count.cmp(&a.active_service_count))
            .then_with(|| b.gpu_utilization.total_cmp(&a.gpu_utilization))
    })
}

/// ADR-0007 signal sanitisation: a reported value outside its physical
/// domain (or non-finite) is indistinguishable from a broken reporter, so
/// it degrades to "signal not available" rather than poisoning the order.
fn valid_hit_rate(v: Option<f32>) -> Option<f32> {
    v.filter(|x| x.is_finite() && (0.0..=1.0).contains(x))
}

fn valid_tokens_per_joule(v: Option<f32>) -> Option<f32> {
    v.filter(|x| x.is_finite() && *x >= 0.0)
}

/// Total order on an optional efficiency signal, higher is better and
/// None sorts below any valid observed value. Using a total order here is
/// deliberate: deciding None-vs-Some at a later tier would break
/// transitivity and make max_by non-deterministic.
fn cmp_signal(a: Option<f32>, b: Option<f32>) -> Ordering {
    match (a, b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(x), Some(y)) => x.total_cmp(&y),
    }
}

/// ADR-0007 efficiency-aware placement: strict lexicographic order
/// warmth > kvCacheHitRate > tokensPerJoule > gpuUtilization >
/// activeServiceCount. Cache before energy (cause before effect); the two
/// load tiers are deliberately inverted w.r.t. warmth-first. With no
/// efficiency signal on any candidate the first two tiers are always Equal
/// and the order degenerates to warmth > lowest util > lowest count.
pub fn select_node_efficiency_aware(candidates: &[NodeCandidate]) -> Option<&NodeCandidate> {
    candidates.iter().max_by(|a, b| {
        warmth_rank(&a.warmth)
            .cmp(&warmth_rank(&b.warmth))
            .then_with(|| {
                cmp_signal(
                    valid_hit_rate(a.kv_cache_hit_rate),
                    valid_hit_rate(b.kv_cache_hit_rate),
                )
            })
            .then_with(|| {
                cmp_signal(
                    valid_tokens_per_joule(a.tokens_per_joule),
                    valid_tokens_per_joule(b.tokens_per_joule),
                )
            })
            .then_with(|| b.gpu_utilization.total_cmp(&a.gpu_utilization))
            .then_with(|| b.active_service_count.cmp(&a.active_service_count))
    })
}

/// Strategy dispatch (ADR-0007): EfficiencyAware activates the comparator
/// above; WarmthFirst keeps today's behaviour. Spread and BinPack are still
/// unimplemented and fall back to warmth-first, unchanged from before.
pub fn select_node_with_strategy<'a>(
    candidates: &'a [NodeCandidate],
    strategy: &PlacementStrategy,
) -> Option<&'a NodeCandidate> {
    match strategy {
        PlacementStrategy::EfficiencyAware => select_node_efficiency_aware(candidates),
        PlacementStrategy::WarmthFirst | PlacementStrategy::Spread | PlacementStrategy::BinPack => {
            select_node_for_placement(candidates)
        }
    }
}

/// Choose a replacement node for a placement displaced by preemption
/// (ADR-0005). Excludes the preempted node by name, then applies the
/// fleet's placement strategy over the survivors — replacement is one of
/// the natural decision points of ADR-0007 D4, so it must honour the same
/// strategy as initial planning or the two paths diverge semantically.
///
/// Returns None when no healthy survivor exists — every remaining candidate
/// is Cold or the candidate set is empty. The caller treats None as the
/// drain-and-hold case (decision 3): the replica stays Draining rather than
/// being forced onto an unsuitable node.
pub fn select_replacement_node(
    candidates: &[NodeCandidate],
    preempted_node: &str,
    strategy: &PlacementStrategy,
) -> Option<String> {
    let survivors: Vec<NodeCandidate> = candidates
        .iter()
        .filter(|c| c.name != preempted_node)
        .cloned()
        .collect();
    // A Cold-only survivor set is not a healthy target: warmth-first would
    // still pick one, so drop Cold explicitly to honour drain-and-hold.
    let healthy: Vec<NodeCandidate> = survivors
        .into_iter()
        .filter(|c| !matches!(c.warmth, Warmth::Cold))
        .collect();
    select_node_with_strategy(&healthy, strategy).map(|c| c.name.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(name: &str, warmth: Warmth, util: f32, count: i32) -> NodeCandidate {
        NodeCandidate {
            name: name.to_string(),
            warmth,
            gpu_utilization: util,
            active_service_count: count,
            kv_cache_hit_rate: None,
            tokens_per_joule: None,
        }
    }

    #[test]
    fn empty_candidates_returns_none() {
        assert!(select_node_for_placement(&[]).is_none());
    }

    #[test]
    fn warm_node_beats_cold_node_regardless_of_load() {
        let candidates = vec![
            candidate("cold-idle", Warmth::Cold, 0.0, 0),
            candidate("warm-loaded", Warmth::Warm, 90.0, 5),
        ];
        let chosen = select_node_for_placement(&candidates).unwrap();
        assert_eq!(chosen.name, "warm-loaded");
    }

    #[test]
    fn ties_on_warmth_break_on_active_service_count() {
        let candidates = vec![
            candidate("warm-busy", Warmth::Warm, 50.0, 4),
            candidate("warm-idle", Warmth::Warm, 50.0, 1),
        ];
        let chosen = select_node_for_placement(&candidates).unwrap();
        assert_eq!(chosen.name, "warm-idle");
    }

    #[test]
    fn ties_on_warmth_and_count_break_on_gpu_utilization() {
        let candidates = vec![
            candidate("warm-hot", Warmth::Warm, 80.0, 2),
            candidate("warm-cool", Warmth::Warm, 20.0, 2),
        ];
        let chosen = select_node_for_placement(&candidates).unwrap();
        assert_eq!(chosen.name, "warm-cool");
    }

    #[test]
    fn single_candidate_is_always_chosen() {
        let candidates = vec![candidate("only-node", Warmth::Cold, 99.0, 10)];
        let chosen = select_node_for_placement(&candidates).unwrap();
        assert_eq!(chosen.name, "only-node");
    }

    #[test]
    fn replacement_excludes_preempted_and_picks_warmest_survivor() {
        let candidates = vec![
            candidate("preempted", Warmth::Warm, 0.1, 0),
            candidate("survivor-warm", Warmth::Warm, 0.2, 0),
            candidate("survivor-warming", Warmth::Warming, 0.1, 0),
        ];
        let chosen =
            select_replacement_node(&candidates, "preempted", &PlacementStrategy::WarmthFirst);
        // preempted is excluded; among survivors the Warm one wins over Warming.
        assert_eq!(chosen, Some("survivor-warm".to_string()));
    }

    #[test]
    fn replacement_holds_when_only_cold_survivors() {
        let candidates = vec![
            candidate("preempted", Warmth::Warm, 0.1, 0),
            candidate("cold-a", Warmth::Cold, 0.0, 0),
            candidate("cold-b", Warmth::Cold, 0.0, 0),
        ];
        // Every survivor is Cold: not a healthy target => drain-and-hold.
        assert_eq!(
            select_replacement_node(&candidates, "preempted", &PlacementStrategy::WarmthFirst),
            None
        );
    }

    #[test]
    fn replacement_holds_when_preempted_was_the_only_node() {
        let candidates = vec![candidate("preempted", Warmth::Warm, 0.1, 0)];
        // Excluding the only node leaves nothing => drain-and-hold.
        assert_eq!(
            select_replacement_node(&candidates, "preempted", &PlacementStrategy::WarmthFirst),
            None
        );
    }

    // --- ADR-0007 efficiency-aware tests ---

    #[allow(clippy::too_many_arguments)]
    fn eff(
        name: &str,
        warmth: Warmth,
        util: f32,
        count: i32,
        hit: Option<f32>,
        tpj: Option<f32>,
    ) -> NodeCandidate {
        NodeCandidate {
            name: name.to_string(),
            warmth,
            gpu_utilization: util,
            active_service_count: count,
            kv_cache_hit_rate: hit,
            tokens_per_joule: tpj,
        }
    }

    #[test]
    fn ea_warmth_dominates_perfect_efficiency_signals() {
        let candidates = vec![
            eff("cold-perfect", Warmth::Cold, 0.0, 0, Some(0.99), Some(50.0)),
            eff("warm-unmeasured", Warmth::Warm, 90.0, 5, None, None),
        ];
        let chosen = select_node_efficiency_aware(&candidates).unwrap();
        assert_eq!(chosen.name, "warm-unmeasured");
    }

    #[test]
    fn ea_hit_rate_beats_tokens_per_joule_within_class() {
        // Cache before energy: higher hit-rate wins even against much
        // higher tokens-per-joule.
        let candidates = vec![
            eff("high-tpj", Warmth::Warm, 10.0, 1, Some(0.30), Some(90.0)),
            eff("high-hit", Warmth::Warm, 80.0, 4, Some(0.70), Some(5.0)),
        ];
        let chosen = select_node_efficiency_aware(&candidates).unwrap();
        assert_eq!(chosen.name, "high-hit");
    }

    #[test]
    fn ea_tokens_per_joule_breaks_hit_rate_tie() {
        let candidates = vec![
            eff("low-tpj", Warmth::Warm, 10.0, 1, Some(0.50), Some(5.0)),
            eff("high-tpj", Warmth::Warm, 80.0, 4, Some(0.50), Some(9.0)),
        ];
        let chosen = select_node_efficiency_aware(&candidates).unwrap();
        assert_eq!(chosen.name, "high-tpj");
    }

    #[test]
    fn ea_measured_beats_unmeasured_within_class() {
        // Even a poor observed hit-rate outranks an absent signal.
        let candidates = vec![
            eff("unmeasured", Warmth::Warm, 5.0, 0, None, None),
            eff("measured-poor", Warmth::Warm, 50.0, 3, Some(0.05), None),
        ];
        let chosen = select_node_efficiency_aware(&candidates).unwrap();
        assert_eq!(chosen.name, "measured-poor");
    }

    #[test]
    fn ea_invalid_signal_degrades_to_absent() {
        // NaN / out-of-domain values must not poison the order: the NaN
        // candidate ranks as unmeasured and loses to a valid observation.
        let candidates = vec![
            eff(
                "nan-hit",
                Warmth::Warm,
                5.0,
                0,
                Some(f32::NAN),
                Some(f32::INFINITY),
            ),
            eff("neg-hit", Warmth::Warm, 5.0, 0, Some(-0.2), Some(-1.0)),
            eff("valid", Warmth::Warm, 80.0, 4, Some(0.10), Some(0.5)),
        ];
        let chosen = select_node_efficiency_aware(&candidates).unwrap();
        assert_eq!(chosen.name, "valid");
    }

    #[test]
    fn ea_no_signals_degenerates_to_warmth_then_util_then_count() {
        // Fail-open tail: with no reporter anywhere the efficiency tiers are
        // always Equal. Note the tail is util > count per ADR-0007, the
        // inverse of WarmthFirst's count > util — documented divergence.
        let candidates = vec![
            eff("high-util", Warmth::Warm, 80.0, 1, None, None),
            eff("low-util", Warmth::Warm, 20.0, 4, None, None),
        ];
        let chosen = select_node_efficiency_aware(&candidates).unwrap();
        assert_eq!(chosen.name, "low-util");
    }

    #[test]
    fn ea_dispatch_selects_comparator_per_strategy() {
        // Same fleet, same tie situation: EfficiencyAware resolves on the
        // hit-rate tier, WarmthFirst ignores it and resolves on count.
        let candidates = vec![
            eff("good-cache-busy", Warmth::Warm, 50.0, 4, Some(0.80), None),
            eff("bad-cache-idle", Warmth::Warm, 50.0, 1, Some(0.20), None),
        ];
        let ea = select_node_with_strategy(&candidates, &PlacementStrategy::EfficiencyAware);
        let wf = select_node_with_strategy(&candidates, &PlacementStrategy::WarmthFirst);
        assert_eq!(ea.unwrap().name, "good-cache-busy");
        assert_eq!(wf.unwrap().name, "bad-cache-idle");
    }
}
