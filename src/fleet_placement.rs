use crate::fleet_types::Warmth;

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

/// Choose a replacement node for a placement displaced by preemption
/// (ADR-0005). Excludes the preempted node by name, then applies the same
/// warmth-first selection as initial placement over the survivors.
///
/// Returns None when no healthy survivor exists — every remaining candidate
/// is Cold or the candidate set is empty. The caller treats None as the
/// drain-and-hold case (decision 3): the replica stays Draining rather than
/// being forced onto an unsuitable node.
pub fn select_replacement_node(
    candidates: &[NodeCandidate],
    preempted_node: &str,
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
    select_node_for_placement(&healthy).map(|c| c.name.clone())
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
        let chosen = select_replacement_node(&candidates, "preempted");
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
        assert_eq!(select_replacement_node(&candidates, "preempted"), None);
    }

    #[test]
    fn replacement_holds_when_preempted_was_the_only_node() {
        let candidates = vec![candidate("preempted", Warmth::Warm, 0.1, 0)];
        // Excluding the only node leaves nothing => drain-and-hold.
        assert_eq!(select_replacement_node(&candidates, "preempted"), None);
    }
}
