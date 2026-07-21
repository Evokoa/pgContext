//! Delta scan and top-k merge contract tests.
#![allow(
    clippy::expect_used,
    reason = "test fixtures expect on operations the test itself constructed"
)]

use std::collections::BTreeMap;

use context_core::DistanceMetric;
use context_index::{DeltaHit, DeltaScanEntry, merge_topk, scan_delta_topk};
use proptest::prelude::*;

#[test]
fn delta_scan_applies_last_write_wins_and_retires_base() {
    let first = [1.0_f32, 0.0];
    let updated = [10.0_f32, 0.0];
    let other = [2.0_f32, 0.0];
    let entries = vec![
        DeltaScanEntry::Live {
            heap_tid: 1,
            vector: &first,
        },
        // Update supersedes the earlier live entry for TID 1.
        DeltaScanEntry::Live {
            heap_tid: 1,
            vector: &updated,
        },
        DeltaScanEntry::Live {
            heap_tid: 2,
            vector: &other,
        },
        // Delete retires TID 3 (present only in the base).
        DeltaScanEntry::Tombstone { heap_tid: 3 },
        // Insert-then-delete never surfaces.
        DeltaScanEntry::Live {
            heap_tid: 4,
            vector: &first,
        },
        DeltaScanEntry::Tombstone { heap_tid: 4 },
    ];
    let outcome =
        scan_delta_topk(entries, DistanceMetric::L2, &[0.0, 0.0], 10).expect("delta scan");
    assert_eq!(
        outcome
            .hits
            .iter()
            .map(|hit| hit.heap_tid)
            .collect::<Vec<_>>(),
        vec![2, 1],
        "TID 2 (distance 2) precedes updated TID 1 (distance 10); 4 was deleted"
    );
    assert_eq!(
        outcome.retired.iter().copied().collect::<Vec<_>>(),
        vec![1, 2, 3, 4]
    );

    // Base candidates: stale TID 1 (must be superseded), TID 3 (deleted),
    // TID 9 (untouched, survives).
    let base = vec![
        DeltaHit {
            heap_tid: 1,
            score: 0.5,
        },
        DeltaHit {
            heap_tid: 3,
            score: 0.7,
        },
        DeltaHit {
            heap_tid: 9,
            score: 5.0,
        },
    ];
    let merged = merge_topk(base, &outcome, 3);
    assert_eq!(
        merged.iter().map(|hit| hit.heap_tid).collect::<Vec<_>>(),
        vec![2, 9, 1],
        "stale base TID 1 and deleted TID 3 must not surface; delta scores win"
    );
}

proptest! {
    /// Merged top-k equals an exact oracle over the final logical state:
    /// (base rows minus delta-mentioned TIDs) plus the delta's last-write
    /// live rows.
    #[test]
    fn delta_merge_matches_exact_oracle(
        base_rows in proptest::collection::btree_map(0_u64..40, proptest::collection::vec(-100.0_f32..100.0, 4), 0..24),
        delta_ops in proptest::collection::vec(
            (0_u64..40, proptest::option::of(proptest::collection::vec(-100.0_f32..100.0, 4))),
            0..32,
        ),
        query in proptest::collection::vec(-100.0_f32..100.0, 4),
        k in 1_usize..12,
    ) {
        let metric = DistanceMetric::L2;
        // Final logical state per the oracle.
        let mut logical: BTreeMap<u64, Vec<f32>> = base_rows.clone();
        for (tid, op) in &delta_ops {
            match op {
                Some(vector) => { logical.insert(*tid, vector.clone()); }
                None => { logical.remove(tid); }
            }
        }
        let mut oracle: Vec<DeltaHit> = logical
            .iter()
            .map(|(tid, vector)| DeltaHit {
                heap_tid: *tid,
                score: metric.distance_slices(&query, vector).expect("finite fixtures"),
            })
            .collect();
        oracle.sort_by(|a, b| {
            a.score
                .partial_cmp(&b.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.heap_tid.cmp(&b.heap_tid))
        });
        oracle.truncate(k);

        // System under test: base = exact scores over base rows only
        // (a superset of any correct base-graph candidate list), delta =
        // the op sequence.
        let base_hits: Vec<DeltaHit> = base_rows
            .iter()
            .map(|(tid, vector)| DeltaHit {
                heap_tid: *tid,
                score: metric.distance_slices(&query, vector).expect("finite fixtures"),
            })
            .collect();
        let entries: Vec<DeltaScanEntry<'_>> = delta_ops
            .iter()
            .map(|(tid, op)| match op {
                Some(vector) => DeltaScanEntry::Live { heap_tid: *tid, vector },
                None => DeltaScanEntry::Tombstone { heap_tid: *tid },
            })
            .collect();
        let outcome = scan_delta_topk(entries, metric, &query, k).expect("delta scan");
        let merged = merge_topk(base_hits, &outcome, k);

        prop_assert_eq!(merged, oracle);
    }
}

/// Not a correctness test: measures the exact-scan ceiling that sizes the
/// default delta-segment limit. Run explicitly with `--ignored --release`.
#[test]
#[ignore = "timing measurement, run explicitly in release mode"]
#[allow(
    clippy::cast_precision_loss,
    clippy::print_stderr,
    reason = "fixture values stay far below the f32 mantissa limit and the measurement exists to be printed"
)]
fn delta_scan_brute_force_ceiling_10k_by_384() {
    let rows: Vec<(u64, Vec<f32>)> = (0..10_000_u64)
        .map(|tid| {
            (
                tid,
                (0..384)
                    .map(|d| ((tid * 31 + d) % 997) as f32 / 997.0)
                    .collect(),
            )
        })
        .collect();
    let query: Vec<f32> = (0..384).map(|d| (d % 13) as f32 / 13.0).collect();
    let started = std::time::Instant::now();
    let mut worst = std::time::Duration::ZERO;
    for _ in 0..20 {
        let single = std::time::Instant::now();
        let entries = rows.iter().map(|(tid, vector)| DeltaScanEntry::Live {
            heap_tid: *tid,
            vector,
        });
        let outcome = scan_delta_topk(entries, DistanceMetric::L2, &query, 10).expect("scan");
        assert_eq!(outcome.hits.len(), 10);
        worst = worst.max(single.elapsed());
    }
    let mean = started.elapsed() / 20;
    eprintln!("delta exact scan 10k x 384: mean={mean:?} worst={worst:?}");
}
