//! Bitmap scan helpers for the pgContext HNSW access method.

use pgrx::itemptr::u64_to_item_pointer;
use pgrx::prelude::*;

use crate::error::raise_sql_error;

use super::HnswScanCandidate;

pub(super) fn hnsw_bitmap_tids(candidates: &[HnswScanCandidate]) -> Vec<pg_sys::ItemPointerData> {
    candidates
        .iter()
        .map(|candidate| {
            let mut tid = pg_sys::ItemPointerData::default();
            u64_to_item_pointer(candidate.heap_tid, &mut tid);
            tid
        })
        .collect()
}

pub(super) fn hnsw_bitmap_tid_count(count: usize) -> std::ffi::c_int {
    match checked_hnsw_bitmap_tid_count(count) {
        Ok(count) => count,
        Err(count) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            format!("HNSW bitmap scan produced too many TIDs: {count}"),
        ),
    }
}

#[cfg(test)]
pub(super) fn checked_hnsw_bitmap_tid_count(count: usize) -> Result<std::ffi::c_int, usize> {
    std::ffi::c_int::try_from(count).map_err(|_| count)
}

#[cfg(not(test))]
fn checked_hnsw_bitmap_tid_count(count: usize) -> Result<std::ffi::c_int, usize> {
    std::ffi::c_int::try_from(count).map_err(|_| count)
}
