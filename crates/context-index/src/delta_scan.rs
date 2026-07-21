//! Exact scan and top-k merge over a delta segment.
//!
//! The segmented write path appends inserts/updates/deletes to a bounded
//! delta instead of splicing the HNSW graph. At query time the delta is
//! scanned exactly (it is bounded by `hnsw_delta_segment_limit`) and merged
//! with the base-graph candidates. This module is pure: entries are
//! borrowed views so the access method can adapt storage records without a
//! crate dependency from `context-index` onto `context-storage`.
//!
//! ## Merge semantics
//!
//! Delta entries are ordered (append order). Within the delta, the **last**
//! entry for a heap TID wins: a later live entry replaces an earlier one
//! (update-after-insert), and a later tombstone retires the TID. Every TID
//! that appears in the delta at all — live or tombstone — is **retired from
//! the base**: a live delta entry supersedes the row's stale base-graph
//! vector, and a tombstone deletes it. Base candidates surviving retirement
//! merge with the delta's exact scores into one ascending-score top-k.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use context_core::DistanceMetric;

use crate::{HnswError, Result};

/// One borrowed delta entry in append order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DeltaScanEntry<'a> {
    /// A written row: insert, or update superseding earlier state.
    Live {
        /// Canonical u64 heap TID.
        heap_tid: u64,
        /// The row's vector values.
        vector: &'a [f32],
    },
    /// A deleted row.
    Tombstone {
        /// Canonical u64 heap TID.
        heap_tid: u64,
    },
}

/// One scored candidate in the merged result order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DeltaHit {
    /// Canonical u64 heap TID.
    pub heap_tid: u64,
    /// Ascending-is-better metric score.
    pub score: f32,
}

/// Outcome of one exact delta scan.
#[derive(Debug, Clone, PartialEq)]
pub struct DeltaScanOutcome {
    /// Exact top-k hits over the delta's final live state, ascending score.
    pub hits: Vec<DeltaHit>,
    /// Every heap TID the delta mentions; base candidates with these TIDs
    /// are stale (superseded or deleted) and must not surface.
    pub retired: BTreeSet<u64>,
}

/// One row of a compacted graph: the current vector for a live heap TID.
#[derive(Debug, Clone, PartialEq)]
pub struct CompactionLiveRow {
    /// Canonical u64 heap TID.
    pub heap_tid: u64,
    /// The row's current vector values.
    pub vector: Vec<f32>,
}

/// Folds an ordered entry stream to its final live state.
///
/// Shared by the query path ([`scan_delta_topk`]) and the compaction path
/// ([`fold_compaction_live_rows`]) so a row that a scan treats as retired
/// cannot survive a compaction, and vice versa.
fn fold_live_state<'a>(
    entries: impl IntoIterator<Item = DeltaScanEntry<'a>>,
) -> (BTreeMap<u64, &'a [f32]>, BTreeSet<u64>) {
    let mut retired = BTreeSet::new();
    let mut live: BTreeMap<u64, &[f32]> = BTreeMap::new();
    for entry in entries {
        match entry {
            DeltaScanEntry::Live { heap_tid, vector } => {
                retired.insert(heap_tid);
                live.insert(heap_tid, vector);
            }
            DeltaScanEntry::Tombstone { heap_tid } => {
                retired.insert(heap_tid);
                live.remove(&heap_tid);
            }
        }
    }
    (live, retired)
}

/// Folds base-graph and delta entries into the live rows a compacted graph
/// must contain, ordered by heap TID.
///
/// Callers pass one ordered stream: base-graph rows first, then the delta in
/// append order. Because both regions fold under the same last-write-wins
/// rule, a delta write supersedes the base row for its TID and a delta
/// tombstone removes it — no separate base-retirement step is needed.
///
/// The heap-TID ordering makes a compaction reproducible: the same live set
/// always produces the same node numbering.
#[must_use]
pub fn fold_compaction_live_rows<'a>(
    entries: impl IntoIterator<Item = DeltaScanEntry<'a>>,
) -> Vec<CompactionLiveRow> {
    let (live, _retired) = fold_live_state(entries);
    live.into_iter()
        .map(|(heap_tid, vector)| CompactionLiveRow {
            heap_tid,
            vector: vector.to_vec(),
        })
        .collect()
}

/// Exactly scans delta entries and returns the top-k over the delta's final
/// live state plus the base-retirement set.
///
/// # Errors
///
/// Returns [`HnswError::Core`] when a live vector's dimension does not
/// match `query` or a distance evaluation fails.
pub fn scan_delta_topk<'a>(
    entries: impl IntoIterator<Item = DeltaScanEntry<'a>>,
    metric: DistanceMetric,
    query: &[f32],
    k: usize,
) -> Result<DeltaScanOutcome> {
    let (live, retired) = fold_live_state(entries);

    let mut hits = Vec::with_capacity(live.len().min(k));
    for (heap_tid, vector) in live {
        let score = metric
            .distance_slices(query, vector)
            .map_err(HnswError::from)?;
        hits.push(DeltaHit { heap_tid, score });
    }
    sort_hits(&mut hits);
    hits.truncate(k);
    Ok(DeltaScanOutcome { hits, retired })
}

/// Merges base-graph candidates with a delta scan into one ascending-score
/// top-k, applying the delta's base retirement.
#[must_use]
pub fn merge_topk(
    base: impl IntoIterator<Item = DeltaHit>,
    delta: &DeltaScanOutcome,
    k: usize,
) -> Vec<DeltaHit> {
    let mut merged: Vec<DeltaHit> = base
        .into_iter()
        .filter(|hit| !delta.retired.contains(&hit.heap_tid))
        .chain(delta.hits.iter().copied())
        .collect();
    sort_hits(&mut merged);
    merged.truncate(k);
    merged
}

fn sort_hits(hits: &mut [DeltaHit]) {
    hits.sort_by(|left, right| {
        left.score
            .partial_cmp(&right.score)
            .unwrap_or(core::cmp::Ordering::Equal)
            .then_with(|| left.heap_tid.cmp(&right.heap_tid))
    });
}
