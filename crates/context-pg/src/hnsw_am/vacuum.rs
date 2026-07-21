//! VACUUM result helpers for the pgContext HNSW access method.

use pgrx::prelude::*;
use pgrx::{AllocatedByRust, PgBox};

pub(super) fn new_hnsw_vacuum_result() -> *mut pg_sys::IndexBulkDeleteResult {
    // SAFETY: PostgreSQL calls vacuum callbacks inside a valid current memory
    // context. `IndexBulkDeleteResult` is a plain FFI result struct.
    unsafe { PgBox::<pg_sys::IndexBulkDeleteResult, AllocatedByRust>::alloc0().into_pg() }
}

/// Computes VACUUM statistics from a live index relation.
///
/// # Safety
///
/// `index_relation` must be a valid PostgreSQL relation for the active VACUUM
/// callback and remain live for the duration of this call.
pub(super) unsafe fn hnsw_vacuum_stats(
    index_relation: pg_sys::Relation,
    num_heap_tuples: f64,
    previous: &pg_sys::IndexBulkDeleteResult,
) -> HnswVacuumStats {
    // SAFETY: PostgreSQL owns the relation pointer for the duration of AM vacuum
    // callbacks. This reads only the main fork block count.
    let num_pages = unsafe {
        pg_sys::RelationGetNumberOfBlocksInFork(index_relation, pg_sys::ForkNumber::MAIN_FORKNUM)
    };
    hnsw_vacuum_stats_from_parts(num_pages, num_heap_tuples, previous.tuples_removed)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct HnswVacuumStats {
    pub(super) num_pages: pg_sys::BlockNumber,
    pub(super) estimated_count: bool,
    pub(super) num_index_tuples: f64,
    pub(super) tuples_removed: f64,
    pub(super) pages_newly_deleted: pg_sys::BlockNumber,
    pub(super) pages_deleted: pg_sys::BlockNumber,
    pub(super) pages_free: pg_sys::BlockNumber,
}

pub(super) fn hnsw_vacuum_stats_from_parts(
    num_pages: pg_sys::BlockNumber,
    num_heap_tuples: f64,
    tuples_removed: f64,
) -> HnswVacuumStats {
    HnswVacuumStats {
        num_pages,
        estimated_count: true,
        num_index_tuples: num_heap_tuples,
        tuples_removed,
        pages_newly_deleted: 0,
        pages_deleted: 0,
        pages_free: 0,
    }
}

pub(super) fn write_hnsw_vacuum_stats(
    target: &mut pg_sys::IndexBulkDeleteResult,
    stats: HnswVacuumStats,
) {
    target.num_pages = stats.num_pages;
    target.estimated_count = stats.estimated_count;
    target.num_index_tuples = stats.num_index_tuples;
    target.tuples_removed = stats.tuples_removed;
    target.pages_newly_deleted = stats.pages_newly_deleted;
    target.pages_deleted = stats.pages_deleted;
    target.pages_free = stats.pages_free;
}
