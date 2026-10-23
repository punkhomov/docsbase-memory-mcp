//! Shared fixtures for `ann-recall` / `ann-spike` (spec ann-index T3/T6).

#![allow(clippy::cast_precision_loss, dead_code)]

use std::sync::Arc;

/// Deterministic LCG in [-1, 1).
pub fn lcg(state: &mut u64) -> f32 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    (((*state >> 32) as u32) as f32 / u32::MAX as f32) * 2.0 - 1.0
}

/// `count` unit vectors of `dim` dimensions, biased along axis 0 by `offset`
/// (positive and negative offsets produce well-separated clusters).
pub fn cluster_vectors(count: usize, dim: usize, offset: f32) -> Vec<Arc<[f32]>> {
    let mut state = 0x9E37_79B9_7F4A_7C15_u64 ^ (count as u64) ^ u64::from(offset.to_bits());
    (0..count)
        .map(|_| {
            let mut vector: Vec<f32> = (0..dim).map(|_| lcg(&mut state)).collect();
            vector[0] += offset;
            let norm = vector.iter().map(|x| x * x).sum::<f32>().sqrt();
            for x in &mut vector {
                *x /= norm;
            }
            Arc::from(vector)
        })
        .collect()
}

/// Deterministic row ids alternating between two clusters.
pub fn interleaved_rows(a: &[Arc<[f32]>], b: &[Arc<[f32]>]) -> Vec<(u64, Arc<[f32]>)> {
    let mut rows = Vec::with_capacity(a.len() + b.len());
    for i in 0..a.len() {
        rows.push((u64::try_from(2 * i).unwrap(), a[i].clone()));
        rows.push((u64::try_from(2 * i + 1).unwrap(), b[i].clone()));
    }
    rows
}
