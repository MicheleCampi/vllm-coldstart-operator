use crate::fleet_placement::{select_node_with_strategy, NodeCandidate};
use crate::fleet_types::PlacementStrategy;

/// Plan node assignments for `slots_to_fill` new placements, given the
/// current eligible candidates.
///
/// Greedy: pick the best candidate, simulate its active_service_count
/// incrementing by one (as if the instance we are about to place already
/// landed there), then pick again. Without this simulation every slot in
/// a batch would land on the same single warmest node instead of spreading
/// across the fleet, since real cluster state has not caught up yet
/// between one reconcile and the next.
///
/// Returns fewer than `slots_to_fill` entries if candidates run out or all
/// remaining candidates are exhausted (empty list) - the caller's job to
/// requeue and retry the remainder next reconcile.
pub fn plan_initial_placements(
    slots_to_fill: i32,
    candidates: &[NodeCandidate],
    strategy: &PlacementStrategy,
) -> Vec<String> {
    let mut working: Vec<NodeCandidate> = candidates.to_vec();
    let mut chosen = Vec::new();

    for _ in 0..slots_to_fill {
        let Some(best) = select_node_with_strategy(&working, strategy) else {
            break;
        };
        let best_name = best.name.clone();
        chosen.push(best_name.clone());

        // Simulate the load this placement adds, so the next pick in this
        // batch sees an updated picture instead of re-choosing the same node.
        if let Some(c) = working.iter_mut().find(|c| c.name == best_name) {
            c.active_service_count += 1;
        }
    }

    chosen
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet_types::Warmth;

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
    fn zero_slots_returns_empty() {
        let candidates = vec![candidate("a", Warmth::Warm, 10.0, 0)];
        assert!(
            plan_initial_placements(0, &candidates, &PlacementStrategy::WarmthFirst).is_empty()
        );
    }

    #[test]
    fn no_candidates_returns_empty_regardless_of_slots() {
        assert!(plan_initial_placements(3, &[], &PlacementStrategy::WarmthFirst).is_empty());
    }

    #[test]
    fn single_warm_node_absorbs_multiple_slots() {
        let candidates = vec![candidate("only", Warmth::Warm, 10.0, 0)];
        let plan = plan_initial_placements(3, &candidates, &PlacementStrategy::WarmthFirst);
        assert_eq!(plan, vec!["only", "only", "only"]);
    }

    #[test]
    fn two_equally_warm_nodes_spread_load_across_slots() {
        let candidates = vec![
            candidate("node-a", Warmth::Warm, 10.0, 0),
            candidate("node-b", Warmth::Warm, 10.0, 0),
        ];
        let plan = plan_initial_placements(2, &candidates, &PlacementStrategy::WarmthFirst);
        // First slot picks either (tie), second slot must go to the other
        // since the first one's simulated count is now higher.
        assert_eq!(plan.len(), 2);
        assert_ne!(plan[0], plan[1]);
    }

    #[test]
    fn more_slots_than_capacity_returns_partial_plan_when_candidates_exhausted() {
        // With no upper bound modeled per node, candidates never truly
        // "exhaust" in this simple version - this test documents that the
        // function always returns exactly slots_to_fill when candidates is
        // non-empty, and is the seam where a future per-node capacity cap
        // would change behavior.
        let candidates = vec![candidate("only", Warmth::Cold, 0.0, 0)];
        let plan = plan_initial_placements(5, &candidates, &PlacementStrategy::WarmthFirst);
        assert_eq!(plan.len(), 5);
    }
    #[test]
    fn strategy_threads_through_to_selection() {
        // Same warmth, same util, same count: WarmthFirst is indifferent
        // (falls to name-independent tie-breaks), EfficiencyAware must
        // prefer the better kvCacheHitRate. This pins the *threading*, not
        // the comparator itself (owned by fleet_placement tests).
        let mut low = candidate("low-cache", Warmth::Warm, 0.2, 1);
        low.kv_cache_hit_rate = Some(0.10);
        let mut high = candidate("high-cache", Warmth::Warm, 0.2, 1);
        high.kv_cache_hit_rate = Some(0.90);
        let candidates = vec![low, high];
        let plan = plan_initial_placements(1, &candidates, &PlacementStrategy::EfficiencyAware);
        assert_eq!(plan, vec!["high-cache".to_string()]);
    }
}
