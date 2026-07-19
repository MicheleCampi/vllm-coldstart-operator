//! ADR-0007 property tests for the efficiency-aware placement comparator.
//! These pin the invariants the unit tests can only sample: warmth
//! dominance under arbitrary (even non-finite) signals, and the total
//! order that makes max_by deterministic.

use proptest::prelude::*;
use vllm_coldstart_operator::fleet_placement::{select_node_efficiency_aware, NodeCandidate};
use vllm_coldstart_operator::fleet_types::Warmth;

fn warmth_rank(w: &Warmth) -> u8 {
    match w {
        Warmth::Warm => 2,
        Warmth::Warming => 1,
        Warmth::Cold => 0,
    }
}

fn arb_warmth() -> impl Strategy<Value = Warmth> {
    prop_oneof![
        Just(Warmth::Cold),
        Just(Warmth::Warming),
        Just(Warmth::Warm),
    ]
}

/// Signals including hostile values: None, NaN, infinities, out-of-domain.
fn arb_signal() -> impl Strategy<Value = Option<f32>> {
    prop_oneof![
        Just(None),
        Just(Some(f32::NAN)),
        Just(Some(f32::INFINITY)),
        Just(Some(f32::NEG_INFINITY)),
        (-2.0f32..2.0).prop_map(Some),
        (0.0f32..1000.0).prop_map(Some),
    ]
}

fn arb_candidate(idx: usize) -> impl Strategy<Value = NodeCandidate> {
    (
        arb_warmth(),
        prop::option::weighted(0.75, 0.0f32..100.0),
        prop::option::weighted(0.75, 0i32..32),
        arb_signal(),
        arb_signal(),
    )
        .prop_map(move |(warmth, util, count, hit, tpj)| NodeCandidate {
            name: format!("node-{idx}"),
            warmth,
            gpu_utilization: util,
            active_service_count: count,
            kv_cache_hit_rate: hit,
            tokens_per_joule: tpj,
        })
}

fn arb_fleet() -> impl Strategy<Value = Vec<NodeCandidate>> {
    prop::collection::vec(any::<u8>(), 1..12).prop_flat_map(|seeds| {
        seeds
            .into_iter()
            .enumerate()
            .map(|(i, _)| arb_candidate(i))
            .collect::<Vec<_>>()
    })
}

proptest! {
    /// Dominance invariant: no combination of efficiency signals can make a
    /// candidate from a lower warmth class win. The chosen node always
    /// belongs to the maximum warmth class present in the fleet.
    #[test]
    fn winner_is_always_in_max_warmth_class(fleet in arb_fleet()) {
        let max_rank = fleet.iter().map(|c| warmth_rank(&c.warmth)).max().unwrap();
        let chosen = select_node_efficiency_aware(&fleet).unwrap();
        prop_assert_eq!(warmth_rank(&chosen.warmth), max_rank);
    }

    /// Fail-open degeneration: with every efficiency signal absent, the
    /// winner also belongs to the maximum warmth class (i.e. the dominant
    /// tier behaves exactly like warmth-first).
    #[test]
    fn no_signals_winner_matches_warmth_first_class(fleet in arb_fleet()) {
        let stripped: Vec<NodeCandidate> = fleet
            .into_iter()
            .map(|mut c| {
                c.kv_cache_hit_rate = None;
                c.tokens_per_joule = None;
                c
            })
            .collect();
        let max_rank = stripped.iter().map(|c| warmth_rank(&c.warmth)).max().unwrap();
        let chosen = select_node_efficiency_aware(&stripped).unwrap();
        prop_assert_eq!(warmth_rank(&chosen.warmth), max_rank);
    }

    /// The winner never changes when the fleet is rotated: max_by over the
    /// comparator is order-insensitive up to ties, and ties on distinct
    /// nodes can only occur on fully identical signal tuples. Restricting
    /// to fleets with pairwise-distinct tuples, the selection must be
    /// invariant under rotation — this fails if the order is not total
    /// (e.g. NaN handled inconsistently) without hand-picking cases.
    #[test]
    fn selection_is_rotation_invariant_on_distinct_fleets(fleet in arb_fleet(), rot in 0usize..12) {
        // Deduplicate by the full comparison tuple (name excluded).
        let mut seen: Vec<&NodeCandidate> = Vec::new();
        let mut distinct: Vec<NodeCandidate> = Vec::new();
        'outer: for c in &fleet {
            for s in &seen {
                let same = s.warmth == c.warmth
                    && s.gpu_utilization.map(f32::to_bits) == c.gpu_utilization.map(f32::to_bits)
                    && s.active_service_count == c.active_service_count
                    && s.kv_cache_hit_rate.map(f32::to_bits)
                        == c.kv_cache_hit_rate.map(f32::to_bits)
                    && s.tokens_per_joule.map(f32::to_bits)
                        == c.tokens_per_joule.map(f32::to_bits);
                if same {
                    continue 'outer;
                }
            }
            seen.push(c);
            distinct.push(c.clone());
        }
        let n = distinct.len();
        let mut rotated = distinct.clone();
        rotated.rotate_left(rot % n);
        let a = select_node_efficiency_aware(&distinct).unwrap().name.clone();
        let b = select_node_efficiency_aware(&rotated).unwrap().name.clone();
        prop_assert_eq!(a, b);
    }
}
