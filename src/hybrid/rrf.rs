//! Reciprocal Rank Fusion over the lexical and dense candidate lists (FR-16).

use std::collections::HashMap;

/// `k` of the RRF formula (FR-16; Elasticsearch default).
pub const RRF_K: u32 = 60;

/// Merges the BM25 and dense ranked lists (best first) into a fused list,
/// best first, with scores `sum(1 / (RRF_K + rank))` at `rank` starting from 1.
///
/// Deterministic tie-breaks (FR-16): a chunk in both lists wins over one in a
/// single list, then the better single-list rank, then the higher BM25 score,
/// then the lower `chunk_id`. A duplicate `chunk_id` inside one list keeps
/// only its best rank.
#[must_use]
pub fn rrf_merge(bm25: &[(u64, f32)], dense: &[(u64, f32)]) -> Vec<(u64, f32)> {
    #[derive(Default)]
    struct Acc {
        score: f32,
        lists: u8,
        best_rank: u32,
        bm25: f32,
    }

    fn add_list(acc: &mut HashMap<u64, Acc>, list: &[(u64, f32)], is_bm25: bool) {
        let mut first_seen: HashMap<u64, (u32, f32)> = HashMap::new();
        for (index, (id, value)) in list.iter().enumerate() {
            let rank = u32::try_from(index).map_or(u32::MAX, |i| i + 1);
            first_seen.entry(*id).or_insert((rank, *value));
        }
        for (id, (rank, value)) in first_seen {
            let entry = acc.entry(id).or_insert(Acc {
                score: 0.0,
                lists: 0,
                best_rank: u32::MAX,
                bm25: f32::NEG_INFINITY,
            });
            #[allow(clippy::cast_precision_loss)]
            let term = 1.0 / RRF_K.saturating_add(rank) as f32;
            entry.score += term;
            entry.lists += 1;
            entry.best_rank = entry.best_rank.min(rank);
            if is_bm25 {
                entry.bm25 = value;
            }
        }
    }

    let mut acc: HashMap<u64, Acc> = HashMap::new();
    add_list(&mut acc, bm25, true);
    add_list(&mut acc, dense, false);
    let mut fused: Vec<(u64, f32, u8, u32, f32)> = acc
        .into_iter()
        .map(|(id, entry)| (id, entry.score, entry.lists, entry.best_rank, entry.bm25))
        .collect();
    fused.sort_by(|a, b| {
        b.1.total_cmp(&a.1)
            .then_with(|| b.2.cmp(&a.2))
            .then_with(|| a.3.cmp(&b.3))
            .then_with(|| b.4.total_cmp(&a.4))
            .then_with(|| a.0.cmp(&b.0))
    });
    fused
        .into_iter()
        .map(|(id, score, _, _, _)| (id, score))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(actual: f32, expected: f32, what: &str) {
        assert!(
            (actual - expected).abs() < 1e-6,
            "{what}: expected {expected}, got {actual}"
        );
    }

    #[test]
    fn manual_math_at_k_60() {
        let bm25 = [(1, 9.0), (2, 8.0)];
        let dense = [(2, 0.9), (3, 0.8)];
        let fused = rrf_merge(&bm25, &dense);
        let ids: Vec<u64> = fused.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, vec![2, 1, 3]);
        approx(fused[0].1, 1.0 / 62.0 + 1.0 / 61.0, "both lists");
        approx(fused[1].1, 1.0 / 61.0, "bm25 only");
        approx(fused[2].1, 1.0 / 62.0, "dense only");
    }

    #[test]
    fn both_lists_beat_a_single_membership() {
        let bm25 = [(1, 9.0), (2, 8.0), (3, 7.0)];
        let dense = [(2, 0.9), (1, 0.8), (3, 0.7)];
        let fused = rrf_merge(&bm25, &dense);
        let ids: Vec<u64> = fused.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, vec![1, 2, 3]);
    }

    #[test]
    fn equal_scores_break_ties_by_bm25_score_desc() {
        let bm25 = [(1, 5.0), (2, 5.0)];
        let dense = [(3, 7.0), (4, 7.0)];
        let fused = rrf_merge(&bm25, &dense);
        let ids: Vec<u64> = fused.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, vec![1, 3, 2, 4]);
    }

    #[test]
    fn tie_on_rank_and_membership_prefers_the_bm25_side() {
        let bm25 = [(7, 1.0)];
        let dense = [(3, 1.0)];
        let fused = rrf_merge(&bm25, &dense);
        let ids: Vec<u64> = fused.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, vec![7, 3]);
    }

    #[test]
    fn rank_order_inside_a_list_is_significant() {
        let bm25 = [(1, 9.0)];
        let dense_a = [(2, 0.9), (3, 0.8)];
        let dense_b = [(3, 0.9), (2, 0.8)];
        let fused_a = rrf_merge(&bm25, &dense_a);
        let fused_b = rrf_merge(&bm25, &dense_b);
        let position = |fused: &[(u64, f32)], id: u64| {
            fused.iter().position(|(hit, _)| *hit == id).expect("hit")
        };
        assert!(position(&fused_a, 2) < position(&fused_a, 3));
        assert!(position(&fused_b, 3) < position(&fused_b, 2));
    }

    #[test]
    fn empty_lists_are_handled() {
        let bm25 = [(1, 9.0), (2, 8.0)];
        let dense: [(u64, f32); 0] = [];
        let fused = rrf_merge(&bm25, &dense);
        assert_eq!(fused.len(), 2);
        approx(fused[0].1, 1.0 / 61.0, "rank 1");
        approx(fused[1].1, 1.0 / 62.0, "rank 2");

        let both: [(u64, f32); 0] = [];
        assert!(rrf_merge(&both, &both).is_empty());
    }

    #[test]
    fn duplicate_within_one_list_keeps_the_best_rank_only() {
        let bm25 = [(1, 9.0), (1, 8.0)];
        let dense: [(u64, f32); 0] = [];
        let fused = rrf_merge(&bm25, &dense);
        assert_eq!(fused.len(), 1);
        approx(fused[0].1, 1.0 / 61.0, "best rank wins");
    }
}
