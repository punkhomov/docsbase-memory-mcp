//! Request-local document-cap refill. No shared state, payload or source IO.

use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};

use super::{Dedupe, SEARCH_UX_OVERSCAN, SearchArgs};
use crate::error::{Error, Result};
use crate::index::tantivy_index::chunk_id_parts;

pub(super) struct Refill {
    ceiling: usize,
    base: HashSet<u64>,
    rounds: usize,
    candidates: usize,
    capacity: usize,
    stop: &'static str,
    dense_window: usize,
}

impl Refill {
    pub(super) fn new(
        args: &SearchArgs,
        base: usize,
        configured: usize,
        rrf: bool,
    ) -> Option<Self> {
        (args.dedupe == Dedupe::Path).then(|| Self {
            ceiling: base.max(configured),
            base: HashSet::new(),
            rounds: 0,
            candidates: 0,
            capacity: 0,
            stop: "candidate-budget",
            dense_window: if rrf { base.max(configured) } else { 0 },
        })
    }

    pub(super) const fn ceiling(&self) -> usize {
        self.ceiling
    }

    /// Reserve base chunks before choosing additional candidates. Do not cap
    /// any document before the single rerank operation.
    pub(super) fn select(&mut self, ordered: &mut Vec<(u64, f32)>, fetch: usize) -> Result<()> {
        if self.rounds == 0 {
            ordered.truncate(fetch);
            self.base.extend(ordered.iter().map(|(id, _)| *id));
            return Ok(());
        }
        let present = ordered
            .iter()
            .filter(|(id, _)| self.base.contains(id))
            .count();
        if present != self.base.len() {
            return Err(Error::Project {
                message: "candidate source changed during search refill".to_owned(),
                instruction: Some("retry search".to_owned()),
            });
        }
        let mut additional = fetch.saturating_sub(self.base.len());
        ordered.retain(|(id, _)| {
            if self.base.contains(id) {
                true
            } else if additional > 0 {
                additional -= 1;
                true
            } else {
                false
            }
        });
        Ok(())
    }

    /// Stop only on capacity, proved source exhaustion, or the explicit width
    /// budget. Full source lists without a sentinel cannot prove exhaustion.
    pub(super) fn next(
        &mut self,
        ordered: &[(u64, f32)],
        args: &SearchArgs,
        fetch: usize,
        exhausted: bool,
    ) -> Option<usize> {
        self.rounds += 1;
        self.candidates = ordered.len();
        let mut counts = HashMap::<i64, usize>::new();
        for (id, _) in ordered {
            let count = counts.entry(chunk_id_parts(*id).0).or_default();
            *count = (*count + 1).min(args.n_per_doc);
        }
        self.capacity = counts.values().sum();
        self.stop = if self.capacity >= args.limit {
            "satisfied"
        } else if exhausted {
            "source-exhausted"
        } else if fetch >= self.ceiling {
            "candidate-budget"
        } else {
            return Some(fetch.saturating_mul(SEARCH_UX_OVERSCAN).min(self.ceiling));
        };
        None
    }

    pub(super) fn to_json(&self, returned: usize, limit: usize) -> Value {
        let stop = if self.stop == "satisfied" && returned < limit {
            "post-filter-shortfall"
        } else {
            self.stop
        };
        json!({
            "stop_reason": stop,
            "candidate_limit": self.ceiling,
            "candidates": self.candidates,
            "capacity": self.capacity,
            "rounds": self.rounds,
            "dense_window": self.dense_window,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hybrid::rrf::rrf_merge;
    use crate::index::tantivy_index::chunk_id;

    #[test]
    fn clamped_rrf_widening_preserves_base_chunks_that_plain_top_k_displaces() {
        let args = super::super::parse_search_args(&json!({
            "query":"widgets", "limit":3, "dedupe":"path"
        }))
        .expect("args");
        let mut refill = Refill::new(&args, 10, 12, true).expect("refill");
        let mut lexical: Vec<_> = (0..10).map(|i| (chunk_id(1, i), 20.0)).collect();
        let mut dense: Vec<_> = (10..20).map(|i| (chunk_id(1, i), 1.0)).collect();
        let mut base = rrf_merge(&lexical, &dense);
        refill.select(&mut base, 10).expect("initial pool");
        let base_ids: Vec<_> = base.iter().map(|(id, _)| *id).collect();
        assert_eq!(refill.next(&base, &args, 10, false), Some(12));

        // Four previously single-source chunks gain a second contribution.
        // At the clamped ceiling12, ordinary truncation drops two base chunks.
        lexical.extend([(chunk_id(1, 17), 10.0), (chunk_id(1, 18), 10.0)]);
        dense.extend([(chunk_id(1, 7), 0.5), (chunk_id(1, 8), 0.5)]);
        let mut widened = rrf_merge(&lexical, &dense);
        assert!(!widened[..12].iter().any(|(id, _)| *id == chunk_id(1, 4)));
        assert!(!widened[..12].iter().any(|(id, _)| *id == chunk_id(1, 14)));
        refill.select(&mut widened, 12).expect("preserved base");
        assert_eq!(widened.len(), 12);
        assert!(
            base_ids
                .iter()
                .all(|id| widened.iter().any(|(selected, _)| selected == id))
        );
        assert_eq!(refill.next(&widened, &args, 12, false), None);
        assert_eq!(refill.to_json(2, 3)["stop_reason"], "candidate-budget");
    }
}
