//! ADR-0007 property tests for the efficiency-aware placement comparator.
//! These pin the invariants the unit tests can only sample: warmth
//! dominance under arbitrary (even non-finite) signals, and the total
//! order that makes max_by deterministic.

use proptest::prelude::*;
use vllm_coldstart_operator::fleet_placement::{
    comparison_key, select_node_efficiency_aware, NodeCandidate,
};
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

/// ADR-0008 D2: ages, generated with the same appetite for pathology as the
/// signals. None is a reporter that published no timestamp; a negative age is a
/// clock skew putting the observation in the future; the wide positive range
/// straddles any horizon a test picks.
fn arb_age() -> impl Strategy<Value = Option<i64>> {
    prop_oneof![
        Just(None),
        Just(Some(0i64)),
        Just(Some(-1i64)),
        (-3600i64..3600).prop_map(Some),
        (0i64..86_400).prop_map(Some),
    ]
}

fn arb_candidate(idx: usize) -> impl Strategy<Value = NodeCandidate> {
    (
        arb_warmth(),
        prop::option::weighted(0.75, 0.0f32..100.0),
        prop::option::weighted(0.75, 0i32..32),
        arb_signal(),
        arb_signal(),
        arb_age(),
        arb_age(),
    )
        .prop_map(
            move |(warmth, util, count, hit, tpj, hit_age, tpj_age)| NodeCandidate {
                name: format!("node-{idx}"),
                warmth,
                gpu_utilization: util,
                active_service_count: count,
                kv_cache_hit_rate: hit,
                kv_cache_hit_rate_age_secs: hit_age,
                tokens_per_joule: tpj,
                tokens_per_joule_age_secs: tpj_age,
            },
        )
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
        let chosen = select_node_efficiency_aware(&fleet, None).unwrap();
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
        let chosen = select_node_efficiency_aware(&stripped, None).unwrap();
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
        // Deduplicate by the key the comparator decides on, not by raw
        // fields. Sanitisation collapses hostile values (Some(NaN) ->
        // None), so two candidates that differ raw can be indifferent to
        // placement; asserting a unique winner between those would be
        // asserting a preference the comparator deliberately does not have.
        // No NaN survives comparison_key (every valid_* filters is_finite),
        // so == on the tuple is a total comparison here.
        let mut distinct: Vec<NodeCandidate> = Vec::new();
        for c in &fleet {
            if !distinct.iter().any(|d| comparison_key(d) == comparison_key(c)) {
                distinct.push(c.clone());
            }
        }
        let n = distinct.len();
        let mut rotated = distinct.clone();
        rotated.rotate_left(rot % n);
        let a = select_node_efficiency_aware(&distinct, None).unwrap().name.clone();
        let b = select_node_efficiency_aware(&rotated, None).unwrap().name.clone();
        prop_assert_eq!(a, b);
    }
}

proptest! {
    /// ADR-0008 D2, the equivalence the horizon rests on: a signal older than
    /// the horizon must rank *exactly* as one never observed — not merely
    /// lower, not "usually" lower. Stated as a property rather than a comment
    /// because the two paths through the code are different (a None that was
    /// never set, versus a Some filtered out by age) and nothing but a test
    /// keeps them from drifting apart.
    #[test]
    fn expired_signal_ranks_exactly_as_never_observed(
        fleet in arb_fleet(),
        horizon in 1i64..7200,
    ) {
        // The same fleet with every signal older than the horizon stripped to
        // absent. If the horizon works, ranking either one must give the same
        // winner.
        let stripped: Vec<NodeCandidate> = fleet
            .iter()
            .map(|c| {
                let hit_stale = c.kv_cache_hit_rate_age_secs.is_none_or(|a| a < 0 || a > horizon);
                let tpj_stale = c.tokens_per_joule_age_secs.is_none_or(|a| a < 0 || a > horizon);
                NodeCandidate {
                    kv_cache_hit_rate: if hit_stale { None } else { c.kv_cache_hit_rate },
                    tokens_per_joule: if tpj_stale { None } else { c.tokens_per_joule },
                    ..c.clone()
                }
            })
            .collect();

        let a = select_node_efficiency_aware(&fleet, Some(horizon)).map(|c| c.name.clone());
        let b = select_node_efficiency_aware(&stripped, Some(horizon)).map(|c| c.name.clone());
        prop_assert_eq!(a, b);
    }

    /// And the horizon must not reach past the two ADR-0007 signals: NVML
    /// reports utilisation and memory on an idle node, so those need no
    /// freshness and must not acquire one. With both efficiency signals absent
    /// everywhere, the horizon can change nothing.
    #[test]
    fn horizon_does_not_touch_nvml_signals(fleet in arb_fleet(), horizon in 1i64..7200) {
        let no_efficiency: Vec<NodeCandidate> = fleet
            .iter()
            .map(|c| NodeCandidate {
                kv_cache_hit_rate: None,
                tokens_per_joule: None,
                ..c.clone()
            })
            .collect();

        let with = select_node_efficiency_aware(&no_efficiency, Some(horizon)).map(|c| c.name.clone());
        let without = select_node_efficiency_aware(&no_efficiency, None).map(|c| c.name.clone());
        prop_assert_eq!(with, without);
    }
}
