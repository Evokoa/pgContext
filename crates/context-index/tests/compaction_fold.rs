//! Compaction live-set fold contract tests.
#![allow(
    clippy::expect_used,
    reason = "test fixtures expect on operations the test itself constructed"
)]

use std::collections::BTreeMap;

use context_index::{DeltaScanEntry, fold_compaction_live_rows};
use proptest::prelude::*;

fn live(heap_tid: u64, vector: &[f32]) -> DeltaScanEntry<'_> {
    DeltaScanEntry::Live { heap_tid, vector }
}

const fn tombstone<'a>(heap_tid: u64) -> DeltaScanEntry<'a> {
    DeltaScanEntry::Tombstone { heap_tid }
}

#[test]
fn fold_keeps_base_rows_that_the_delta_never_mentions() {
    let a = [1.0_f32, 0.0];
    let b = [0.0_f32, 1.0];
    let rows = fold_compaction_live_rows([live(1, &a), live(2, &b)]);

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].heap_tid, 1);
    assert_eq!(rows[0].vector, a);
    assert_eq!(rows[1].heap_tid, 2);
    assert_eq!(rows[1].vector, b);
}

#[test]
fn fold_lets_a_later_live_entry_supersede_an_earlier_one() {
    let stale = [1.0_f32, 0.0];
    let fresh = [9.0_f32, 9.0];
    // Base first, delta second: the delta's vector is the current one.
    let rows = fold_compaction_live_rows([live(7, &stale), live(7, &fresh)]);

    assert_eq!(rows.len(), 1, "an update must not duplicate the row");
    assert_eq!(rows[0].heap_tid, 7);
    assert_eq!(rows[0].vector, fresh, "the newest write wins");
}

#[test]
fn fold_drops_rows_a_later_tombstone_retires() {
    let a = [1.0_f32, 0.0];
    let b = [0.0_f32, 1.0];
    let rows = fold_compaction_live_rows([live(1, &a), live(2, &b), tombstone(1)]);

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].heap_tid, 2);
}

#[test]
fn fold_lets_a_live_entry_resurrect_a_tombstoned_tid() {
    // A TID can be deleted and its slot reused by a later insert; ordering,
    // not entry kind, decides the outcome.
    let gone = [1.0_f32, 0.0];
    let back = [5.0_f32, 5.0];
    let rows = fold_compaction_live_rows([live(3, &gone), tombstone(3), live(3, &back)]);

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].vector, back);
}

#[test]
fn fold_ignores_a_tombstone_for_a_tid_that_was_never_live() {
    let a = [1.0_f32, 0.0];
    let rows = fold_compaction_live_rows([tombstone(42), live(1, &a)]);

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].heap_tid, 1);
}

#[test]
fn fold_orders_rows_by_heap_tid_regardless_of_entry_order() {
    let v = [1.0_f32];
    let rows = fold_compaction_live_rows([live(30, &v), live(10, &v), live(20, &v)]);

    let tids: Vec<u64> = rows.iter().map(|row| row.heap_tid).collect();
    assert_eq!(
        tids,
        [10, 20, 30],
        "a deterministic order keeps compaction reproducible"
    );
}

#[test]
fn fold_of_no_entries_is_empty() {
    assert!(fold_compaction_live_rows([]).is_empty());
}

/// The obvious, slow definition of the same thing: replay every entry into a
/// map and read the map out. The real fold must agree with it on every input.
fn oracle(entries: &[DeltaScanEntry<'_>]) -> Vec<(u64, Vec<f32>)> {
    let mut state: BTreeMap<u64, Vec<f32>> = BTreeMap::new();
    for entry in entries {
        match *entry {
            DeltaScanEntry::Live { heap_tid, vector } => {
                state.insert(heap_tid, vector.to_vec());
            }
            DeltaScanEntry::Tombstone { heap_tid } => {
                state.remove(&heap_tid);
            }
        }
    }
    state.into_iter().collect()
}

proptest! {
    #[test]
    fn fold_matches_the_replay_oracle_on_arbitrary_entry_streams(
        raw in prop::collection::vec(
            (0_u64..8, prop::option::of(prop::collection::vec(-4.0_f32..4.0, 2..=2))),
            0..40,
        ),
    ) {
        // `None` is a tombstone; `Some(vector)` is a live write. A small TID
        // space makes updates, deletes, and resurrections collide often.
        let vectors: Vec<Option<Vec<f32>>> = raw.iter().map(|(_, v)| v.clone()).collect();
        let entries: Vec<DeltaScanEntry<'_>> = raw
            .iter()
            .zip(&vectors)
            .map(|((heap_tid, _), vector)| match vector {
                Some(vector) => DeltaScanEntry::Live { heap_tid: *heap_tid, vector },
                None => DeltaScanEntry::Tombstone { heap_tid: *heap_tid },
            })
            .collect();

        let folded: Vec<(u64, Vec<f32>)> = fold_compaction_live_rows(entries.iter().copied())
            .into_iter()
            .map(|row| (row.heap_tid, row.vector))
            .collect();

        prop_assert_eq!(folded, oracle(&entries));
    }
}
