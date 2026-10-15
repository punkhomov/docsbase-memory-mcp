//! Dense candidate generation: cosine top-`fetch` over cached vectors (FR-15).
//!
//! The scan is intentionally brute-force: iteration 2 selects the best
//! `fetch` chunks by cosine similarity against the query embedding; an ANN
//! index is a later iteration (DELTA-7 non-goals). Every stored vector is
//! scored with a true cosine (division by both norms), which degenerates to
//! the dot product for the L2-normalized vectors produced by the default
//! embedder configuration, so the same code serves `normalize = false`.

use crate::error::{Error, Result};

/// Returns the best `fetch` candidates by cosine similarity, best first.
///
/// Ties are broken deterministically: equal scores order by ascending
/// `chunk_id`. Zero-length or non-finite inputs score `0.0` instead of
/// poisoning the ranking; a dimension mismatch between the query and a
/// candidate is an internal invariant violation (the cache key includes the
/// embedding dimensions).
///
/// # Errors
/// [`Error::Internal`] when the query vector is empty or a candidate has a
/// different dimension.
pub fn top_k<V: AsRef<[f32]>>(
    query: &[f32],
    rows: &[(u64, V)],
    fetch: usize,
) -> Result<Vec<(u64, f32)>> {
    if query.is_empty() {
        return Err(Error::internal("dense query vector is empty"));
    }
    if fetch == 0 || rows.is_empty() {
        return Ok(Vec::new());
    }
    let query_norm = norm(query);
    let mut scored: Vec<(u64, f32)> = Vec::with_capacity(rows.len());
    for (chunk_id, vector) in rows {
        let vector = vector.as_ref();
        if vector.len() != query.len() {
            return Err(Error::internal(format!(
                "dense candidate vector dim {} does not match query dim {} (chunk {chunk_id})",
                vector.len(),
                query.len()
            )));
        }
        scored.push((*chunk_id, cosine(query_norm, query, vector)));
    }
    scored.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    scored.truncate(fetch);
    Ok(scored)
}

/// Cosine similarity; zero-length or non-finite inputs collapse to `0.0`
/// instead of poisoning the ranking with `NaN`.
fn cosine(query_norm: f32, query: &[f32], vector: &[f32]) -> f32 {
    if query_norm == 0.0 {
        return 0.0;
    }
    let vector_norm = norm(vector);
    if vector_norm == 0.0 {
        return 0.0;
    }
    let dot: f32 = query.iter().zip(vector).map(|(a, b)| a * b).sum();
    let score = dot / (query_norm * vector_norm);
    if score.is_finite() { score } else { 0.0 }
}

fn norm(vector: &[f32]) -> f32 {
    vector.iter().map(|value| value * value).sum::<f32>().sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() < 1e-6,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn orders_by_cosine_score() {
        let query = [1.0, 0.0];
        let rows: Vec<(u64, Vec<f32>)> = vec![
            (10, vec![1.0, 0.0]),
            (11, vec![0.0, 1.0]),
            (12, vec![0.6, 0.8]),
        ];
        let hits = top_k(&query, &rows, 10).expect("scan");
        let ids: Vec<u64> = hits.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, vec![10, 12, 11]);
        approx(hits[0].1, 1.0);
        approx(hits[1].1, 0.6);
        approx(hits[2].1, 0.0);
    }

    #[test]
    fn cosine_is_scale_invariant() {
        let query = [2.0, 0.0];
        let rows: Vec<(u64, Vec<f32>)> = vec![(10, vec![5.0, 0.0]), (11, vec![0.1, 0.0])];
        let hits = top_k(&query, &rows, 10).expect("scan");
        assert_eq!(hits[0].0, 10);
        approx(hits[0].1, 1.0);
        approx(hits[1].1, 1.0);
    }

    #[test]
    fn arrives_at_unit_vectors_as_plain_dot() {
        let query = [0.6, 0.8];
        let rows: Vec<(u64, Vec<f32>)> = vec![(10, vec![0.8, 0.6])];
        let hits = top_k(&query, &rows, 10).expect("scan");
        approx(hits[0].1, 0.96);
    }

    #[test]
    fn equal_scores_break_ties_by_chunk_id() {
        let query = [1.0, 0.0];
        let rows: Vec<(u64, Vec<f32>)> = vec![(12, vec![2.0, 0.0]), (10, vec![1.0, 0.0])];
        let hits = top_k(&query, &rows, 10).expect("scan");
        assert_eq!(hits.iter().map(|(id, _)| *id).collect::<Vec<_>>(), vec![10, 12]);
    }

    #[test]
    fn zero_query_scores_zero_for_every_candidate() {
        let query = [0.0, 0.0];
        let rows: Vec<(u64, Vec<f32>)> = vec![(12, vec![1.0, 0.0]), (10, vec![1.0, 0.0])];
        let hits = top_k(&query, &rows, 10).expect("scan");
        assert_eq!(hits.iter().map(|(id, _)| *id).collect::<Vec<_>>(), vec![10, 12]);
        assert!(hits.iter().all(|(_, score)| *score == 0.0));
    }

    #[test]
    fn zero_candidate_scores_zero() {
        let query = [1.0, 1.0];
        let rows: Vec<(u64, Vec<f32>)> = vec![(10, vec![0.0, 0.0])];
        let hits = top_k(&query, &rows, 10).expect("scan");
        approx(hits[0].1, 0.0);
    }

    #[test]
    fn non_finite_values_score_zero() {
        let query = [1.0, 0.0];
        let rows: Vec<(u64, Vec<f32>)> = vec![
            (10, vec![f32::NAN, 0.0]),
            (11, vec![f32::INFINITY, 0.0]),
            (12, vec![1.0, 0.0]),
        ];
        let hits = top_k(&query, &rows, 10).expect("scan");
        assert_eq!(hits[0].0, 12);
        approx(hits[0].1, 1.0);
        approx(hits[1].1, 0.0);
        approx(hits[2].1, 0.0);
    }

    #[test]
    fn fetch_caps_the_result() {
        let query = [1.0, 0.0];
        let rows: Vec<(u64, Vec<f32>)> = vec![
            (10, vec![1.0, 0.0]),
            (11, vec![0.9, 0.1]),
            (12, vec![0.8, 0.2]),
        ];
        let hits = top_k(&query, &rows, 2).expect("scan");
        assert_eq!(hits.iter().map(|(id, _)| *id).collect::<Vec<_>>(), vec![10, 11]);
        assert!(top_k(&query, &rows, 0).expect("scan").is_empty());
        assert_eq!(top_k(&query, &rows, 99).expect("scan").len(), 3);
    }

    #[test]
    fn empty_inputs_are_empty() {
        let query = [1.0, 0.0];
        let rows: Vec<(u64, Vec<f32>)> = Vec::new();
        assert!(top_k(&query, &rows, 10).expect("scan").is_empty());
    }

    #[test]
    fn empty_query_is_an_internal_error() {
        let rows: Vec<(u64, Vec<f32>)> = vec![(10, vec![1.0, 0.0])];
        let err = top_k(&[], &rows, 10).expect_err("empty query");
        assert!(err.to_string().contains("empty"), "{err}");
    }

    #[test]
    fn dimension_mismatch_is_an_internal_error() {
        let query = [1.0, 0.0];
        let rows: Vec<(u64, Vec<f32>)> = vec![(10, vec![1.0, 0.0, 0.0])];
        let err = top_k(&query, &rows, 10).expect_err("dim mismatch");
        assert!(err.to_string().contains("dim"), "{err}");
    }
}
