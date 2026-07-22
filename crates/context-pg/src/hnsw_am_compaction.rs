// Segmented-index compaction fragment included by `hnsw_am.rs` (P2-S4).
//
// Compaction rebuilds one HNSW index from its own pages: it folds the live
// base graph together with the delta segment, reconstructs the graph, writes
// it to fresh pages, and publishes the result with a single metapage update.
// It never reads the heap, so it is cheaper than REINDEX, and it drains the
// delta segment so the fast append path resumes.
//
// ## Why the write order is the reverse of a build
//
// `hnsw_build_safe` publishes metapage state before it writes node pages,
// which is safe only because an interrupted CREATE INDEX is discarded whole.
// Compaction mutates an index that queries are already being served from, so
// it must write every fresh page first and flip the metapage last: until the
// flip, the previous base remains authoritative and a crash simply loses the
// work. The flip itself is one Generic WAL record over block 0, so no reader
// can observe a half-published graph.
//
// ## Why concurrent readers stay correct
//
// Fresh pages are appended past the relation's end and the superseded base is
// never overwritten, but that alone is not enough: `base_start_block` bounds a
// base read from below and nothing bounds it from above, because the inline
// insert path appends live base pages past the delta region and an upper bound
// would silently drop them. A reader therefore does visit the blocks a
// compaction is filling in.
//
// What keeps it correct is the generation stamp. Fresh pages carry the
// generation the flip will publish, not the live one, and readers skip pages
// whose stamp does not match the published `base_generation`. Before the flip
// those pages are inert; the flip makes the whole set live at once. A scan
// before the flip reads the old base (intact, just older), a scan after reads
// the new one, and index results are rechecked against the heap regardless.
//
// Removing the stamp reintroduces a wrong-results bug that needs no crash to
// trigger: the fresh base overwrites the live one by node id in any reader
// running concurrently with a compaction.

/// Reads, rebuilds, and republishes one HNSW index's graph from its own
/// pages, returning the number of rows the compacted graph holds.
///
/// # Safety
///
/// `index_relation` must be a live `pgcontext_hnsw` index relation held open
/// for the complete call, and the caller must already hold this index's
/// append advisory lock so no delta record can be appended concurrently.
unsafe fn hnsw_compact_relation(
    index_relation: pg_sys::Relation,
    score_metric: HnswScoreMetric,
    budget: HnswCompactionBudget,
) -> Option<HnswCompactionOutcome> {
    // SAFETY: the caller owns a live index relation for this call.
    let meta = unsafe { PgHnswGraphRead::new(index_relation).meta() };
    // SAFETY: the validated relation owns a live versioned metapage.
    let config = unsafe { hnsw_stored_config(index_relation, score_metric) };

    // SAFETY: the caller owns the relation and the append lock, so the base
    // and delta regions cannot change underneath these two reads.
    let base_records = unsafe { read_hnsw_vector_records(index_relation) };
    // SAFETY: `delta_start_block` was read from the metapage above.
    let delta_records = unsafe { read_hnsw_delta_records(index_relation, meta.delta_start_block) };

    // One ordered stream, oldest first: the base graph, then the delta in
    // append order. `fold_compaction_live_rows` applies last-write-wins over
    // the whole stream, so a delta write supersedes its base row and a delta
    // tombstone removes it.
    let base_entries = base_records.iter().map(|record| {
        let heap_tid = hnsw_record_heap_tid(record);
        if hnsw_record_is_tombstoned(record) {
            DeltaScanEntry::Tombstone { heap_tid }
        } else {
            DeltaScanEntry::Live {
                heap_tid,
                vector: record.vector.as_slice(),
            }
        }
    });
    let delta_entries = delta_records.iter().map(|record| match record.kind {
        DeltaRecordKind::Live => DeltaScanEntry::Live {
            heap_tid: record.heap_tid,
            vector: record.vector.as_slice(),
        },
        DeltaRecordKind::Tombstone => DeltaScanEntry::Tombstone {
            heap_tid: record.heap_tid,
        },
    });
    let live_rows = context_index::fold_compaction_live_rows(base_entries.chain(delta_entries));

    let row_count = live_rows.len();
    let builder =
        ConcurrentHnswBuilder::new(score_metric.navigation_metric(), config, row_count);
    let mut dimensions = None;
    // Rows are consumed, not cloned: the fold already owns them, and copying
    // every vector would hold two full graphs' worth of vectors at once.
    for row in live_rows {
        // Stored vectors already passed through `prepare_vector` on the way
        // in (both the build and the insert path normalize before writing),
        // so they are re-inserted as-is. Preparing them again would apply the
        // metric's transform twice.
        let vector =
            DenseVector::new(row.vector).unwrap_or_else(|error| raise_core_error(error));
        if dimensions.is_none() {
            dimensions = Some(dimension_to_u32(vector.dimension()));
        }
        builder
            .insert(HnswPointId::new(row.heap_tid), vector)
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                    format!("failed to rebuild HNSW graph during compaction: {error}"),
                )
            });
    }
    let graph = builder.finish().unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            format!("failed to finalize the compacted HNSW graph: {error}"),
        )
    });

    // Same budget CREATE INDEX enforces. Compaction holds a whole graph in
    // backend memory, sized by the index rather than by anything the caller
    // passes, so an index that has outgrown the session's budget would
    // otherwise OOM the backend instead of failing with a usable message.
    // Checked after the build rather than during it because the delta merge
    // decides the final row count; the graph is dropped on this error path
    // and nothing has been written.
    let estimated_bytes = graph.memory_estimate().total_bytes();
    let budget_bytes = maintenance_work_mem_budget_bytes();
    if estimated_bytes > budget_bytes {
        // A threshold-triggered compaction is a background optimization inside
        // somebody's INSERT, so it declines instead of failing that INSERT: the
        // caller falls back to the inline path, which is slower but correct.
        // An explicit `pgcontext.compact()` call is a request, so it reports.
        if budget == HnswCompactionBudget::Decline {
            return None;
        }
        let suggested_mib = estimated_bytes.div_ceil(1024 * 1024).max(1);
        raise_sql_error_with_hint(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!(
                "compacted HNSW graph estimated memory {estimated_bytes} bytes exceeds \
                 maintenance_work_mem budget {budget_bytes} bytes for {row_count} rows"
            ),
            format!(
                "Raise the budget for this session, for example \
                 SET maintenance_work_mem = '{suggested_mib}MB', then retry \
                 pgcontext.compact(). The index is unchanged."
            ),
        );
    }

    let snapshots = graph.node_snapshots();

    // Everything from here is durable-then-publish: capture where the fresh
    // base begins before writing it, so the metapage can name that block.
    // SAFETY: the caller owns a live index relation.
    let fresh_base_start = u64::from(unsafe {
        pg_sys::RelationGetNumberOfBlocksInFork(index_relation, pg_sys::ForkNumber::MAIN_FORKNUM)
    });
    // The fresh base is stamped with the generation the flip below will
    // publish, not the live one. Until that flip a reader sees these pages in
    // the range it scans but skips them as belonging to another generation, so
    // an interrupted compaction leaves orphans that are inert rather than a
    // second graph layered over the live one.
    let fresh_generation = meta.next_base_generation();
    hnsw_physical_failpoint(13, "before_compaction_write");
    // SAFETY: the caller owns a live index relation and the snapshots own
    // finalized graph payloads; every page is appended past the current end.
    unsafe { write_hnsw_node_revisions_bulk(index_relation, &snapshots, fresh_generation) };
    hnsw_physical_failpoint(14, "after_compaction_write");
    // SAFETY: the caller owns a live index relation; the fresh base is fully
    // written above, so the current end marks where the new delta begins.
    let post_write_block_count = u64::from(unsafe {
        pg_sys::RelationGetNumberOfBlocksInFork(index_relation, pg_sys::ForkNumber::MAIN_FORKNUM)
    });

    // Belt and braces behind the heap lock the caller took. If a mutation
    // still landed, a delta tombstone inside the range about to be published
    // as the base would be skipped as the wrong page kind, and a node
    // revision there would carry superseded node numbering — neither is
    // recoverable after the flip. Nothing has been published yet, so failing
    // here leaves the previous base authoritative and wastes only the freshly
    // written pages.
    // SAFETY: the caller owns a live index relation.
    let meta_after_write = unsafe { PgHnswGraphRead::new(index_relation).meta() };
    if meta_after_write.delta_record_count != meta.delta_record_count
        || meta_after_write.directory_epoch != meta.directory_epoch
    {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_T_R_SERIALIZATION_FAILURE,
            "pgcontext.compact() observed a concurrent index mutation and made no \
             change; retry once other maintenance on this table has finished",
        );
    }

    // The single publication point. One Generic WAL record moves the base,
    // resets the delta, and bumps the directory epoch (through `record_build`)
    // so other backends discard caches built on the superseded graph.
    // SAFETY: the caller owns a live index relation and every fresh page it
    // now names is durable.
    unsafe {
        update_hnsw_metapage(index_relation, |meta| {
            meta.open_base_generation();
            meta.open_base_region(fresh_base_start);
            meta.record_build(dimensions, graph_node_count(&snapshots), graph.entry_point());
            meta.open_delta_region(post_write_block_count);
        });
    }
    hnsw_physical_failpoint(15, "after_compaction_publish");

    Some(HnswCompactionOutcome {
        live_rows: snapshots.len(),
        base_records: base_records.len(),
        delta_records: delta_records.len(),
    })
}

/// Whether a compaction that does not fit `maintenance_work_mem` should fail
/// the caller or quietly decline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HnswCompactionBudget {
    /// Raise, with a hint naming the budget to set. For `pgcontext.compact()`,
    /// where the caller asked for a compaction and deserves to hear why not.
    Enforce,
    /// Return `None` and leave the index untouched. For the threshold trigger,
    /// where a raise would turn an optimization into a failed INSERT.
    Decline,
}

/// What one compaction folded away, for the SQL-visible report.
#[derive(Debug, Clone, Copy)]
struct HnswCompactionOutcome {
    live_rows: usize,
    base_records: usize,
    delta_records: usize,
}

fn graph_node_count(snapshots: &[HnswGraphNodeSnapshot]) -> u64 {
    u64::try_from(snapshots.len()).unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            "compacted HNSW graph exceeds the representable node count",
        )
    })
}

/// Rebuilds a `pgcontext_hnsw` index from its own pages, draining the
/// segmented-write delta so the fast append path resumes.
///
/// Unlike `REINDEX` this never rescans the heap: it reuses the vectors the
/// index already stores. Rows deleted since the last compaction are dropped,
/// so the graph shrinks to its live set.
///
/// The superseded pages are left in place — compaction reclaims write
/// throughput, not disk. Use `REINDEX` to shrink the relation on disk.
#[pg_extern(schema = "pgcontext", name = "compact")]
#[search_path(pg_catalog, pgcontext, public)]
fn hnsw_compact(
    index: PgRelation,
) -> TableIterator<
    'static,
    (
        name!(live_rows, i64),
        name!(base_records_read, i64),
        name!(delta_records_drained, i64),
    ),
> {
    let index_relation = index.as_ptr();
    let score_metric = ensure_compactable_hnsw_relation(index_relation);

    // Two locks, excluding the two ways pages reach this index.
    //
    // ShareUpdateExclusive on the parent table conflicts with itself, which
    // is the lock VACUUM takes, so no vacuum of this table can interleave.
    // It does not conflict with RowExclusive, so ordinary INSERT/UPDATE/
    // DELETE keep running. Detecting a concurrent vacuum instead of
    // excluding it cannot be made correct: any check has a window between
    // itself and the metapage flip, and vacuum's own metapage update lands
    // after the pages it appends.
    //
    // The per-index advisory lock then excludes concurrent delta appends and
    // any other compaction, for the rest of this transaction.
    // SAFETY: `PgRelation` keeps the relation cache entry live for this call.
    unsafe { lock_hnsw_compaction_table(index_relation) };
    // SAFETY: as above.
    unsafe { serialize_hnsw_insert(index_relation) };

    // SAFETY: the relation was validated as a pgcontext_hnsw index above,
    // `PgRelation` holds it open, and the append lock is held.
    // `Enforce`: this caller asked for a compaction, so an oversized graph is
    // reported rather than silently skipped. Never `None` under `Enforce`.
    let outcome = unsafe {
        hnsw_compact_relation(index_relation, score_metric, HnswCompactionBudget::Enforce)
            .unwrap_or_else(|| unreachable!("Enforce always raises instead of declining"))
    };

    TableIterator::once((
        usize_to_i64_report(outcome.live_rows),
        usize_to_i64_report(outcome.base_records),
        usize_to_i64_report(outcome.delta_records),
    ))
}

/// Compacts an index whose delta segment just filled up, from inside the
/// INSERT that found it full. Returns whether the graph was republished.
///
/// This runs on a user's write path, so it declines rather than blocking or
/// failing in every case where an explicit `pgcontext.compact()` would wait or
/// raise:
///
/// * the parent-table lock is taken *conditionally*. Compaction needs the same
///   `ShareUpdateExclusiveLock` that excludes VACUUM, but this backend already
///   holds the per-index advisory lock (taken at insert entry), whereas
///   `pgcontext.compact()` takes the table lock first. Waiting here with the
///   locks held in the opposite order is exactly the shape of a deadlock, so
///   it never waits: unavailable means decline.
/// * a graph too large for `maintenance_work_mem` declines instead of raising,
///   so the INSERT proceeds on the inline path rather than failing.
///
/// Declining is never a correctness problem: the caller falls back to splicing
/// the row into the base graph inline, which is slower but produces the same
/// index.
///
/// # Safety
///
/// `index_relation` must be a live `pgcontext_hnsw` index relation held open
/// for the complete call, with this index's append advisory lock already held.
unsafe fn hnsw_compact_on_threshold(
    index_relation: pg_sys::Relation,
    score_metric: HnswScoreMetric,
) -> bool {
    // Project the cost before doing any of it.
    //
    // The authoritative budget check inside `hnsw_compact_relation` runs after
    // the graph is built, because only the delta merge knows the final row
    // count. On this path that ordering is pathological: the delta stays full,
    // so *every* subsequent insert would read every base and delta record,
    // rebuild the whole graph, discover it does not fit, and throw it away.
    // Measured on a 100k-row 384-dimension index with the default 64MB
    // maintenance_work_mem, that is a full rebuild discarded per row.
    //
    // The projection is an upper bound (it cannot know what a later VACUUM
    // tombstoned), so an index whose live set has shrunk well below its
    // recorded size can be skipped here even though it would have fit.
    // `pgcontext.compact()` is unaffected -- it takes the accurate path and
    // reports -- so the escape hatch is explicit and documented.
    // SAFETY: the caller owns a live index relation for this call.
    let meta = unsafe { PgHnswGraphRead::new(index_relation).meta() };
    let projected = projected_compaction_bytes(meta);
    if projected > maintenance_work_mem_budget_bytes() {
        return false;
    }
    // Bound the stall, not just the memory.
    //
    // Compaction time grows with the graph, and this one runs inside somebody's
    // INSERT: on the reference 100,000-row 384-dimension index it takes about a
    // minute, which is a long time for a single statement to block. Past this
    // ceiling the insert declines and takes the inline path, leaving the
    // rebuild to `pgcontext.compact()` or REINDEX where the cost is expected.
    //
    // A background worker is the right home for this work -- it would decouple
    // the rebuild from any statement's latency -- but that is a separate
    // subsystem (shared-memory queue, per-database workers, its own restart
    // semantics), so the bound is the interim answer rather than the intended
    // end state.
    if let Some(max_bytes) = crate::settings::hnsw_compact_on_threshold_max_bytes_from_guc()
        && projected > max_bytes
    {
        return false;
    }
    // SAFETY: the caller owns a live index relation for this call.
    if !unsafe { try_lock_hnsw_compaction_table(index_relation) } {
        return false;
    }
    // SAFETY: the caller owns the relation and the advisory lock, and the
    // table lock above now excludes VACUUM for the rest of this transaction.
    unsafe { hnsw_compact_relation(index_relation, score_metric, HnswCompactionBudget::Decline) }
        .is_some()
}

/// Upper bound on the backend memory a compaction of this index would need,
/// derived from the metapage alone so it costs one buffer read.
///
/// Counts the vectors only. Link storage is real but small beside them and
/// depends on the built graph's layer assignment, so leaving it out keeps this
/// an honest bound on the dominant term rather than a guess at the total.
fn projected_compaction_bytes(meta: HnswMetaPage) -> usize {
    let rows = meta.graph_nodes.saturating_add(meta.delta_record_count);
    let dimensions = u64::from(meta.dimensions);
    let bytes = rows
        .saturating_mul(dimensions)
        .saturating_mul(size_of::<f32>() as u64);
    usize::try_from(bytes).unwrap_or(usize::MAX)
}

/// Takes `ShareUpdateExclusiveLock` on the table this index belongs to, the
/// same level VACUUM holds, so the two cannot interleave.
///
/// Held to end of transaction by PostgreSQL's lock manager, which is exactly
/// the window compaction needs: the fresh base must be written and published
/// without another maintenance operation appending pages into the range being
/// published.
///
/// # Safety
///
/// `index_relation` must be a live index relation held open for this call.
unsafe fn lock_hnsw_compaction_table(index_relation: pg_sys::Relation) {
    // SAFETY: the caller owns a live index relation for this call.
    let heap_oid = unsafe { hnsw_compaction_table_oid(index_relation) };
    // SAFETY: `heap_oid` came from this index's own catalog form, so it names
    // a live relation; the lock manager releases it at transaction end.
    unsafe {
        pg_sys::LockRelationOid(heap_oid, pg_sys::ShareUpdateExclusiveLock.cast_signed());
    }
}

/// Non-blocking `lock_hnsw_compaction_table`: reports whether the lock was
/// free, and never waits for it.
///
/// Used by the threshold trigger, which holds the per-index advisory lock and
/// so must not wait on a lock that `pgcontext.compact()` acquires *before* that
/// advisory lock — waiting would close a deadlock cycle between the two.
///
/// # Safety
///
/// `index_relation` must be a live index relation held open for this call.
unsafe fn try_lock_hnsw_compaction_table(index_relation: pg_sys::Relation) -> bool {
    // SAFETY: the caller owns a live index relation for this call.
    let heap_oid = unsafe { hnsw_compaction_table_oid(index_relation) };
    // SAFETY: `heap_oid` names this index's own table; the conditional variant
    // returns immediately either way and releases at transaction end.
    unsafe {
        pg_sys::ConditionalLockRelationOid(
            heap_oid,
            pg_sys::ShareUpdateExclusiveLock.cast_signed(),
        )
    }
}

/// Reads the OID of the table an index belongs to.
///
/// # Safety
///
/// `index_relation` must be a live index relation held open for this call.
unsafe fn hnsw_compaction_table_oid(index_relation: pg_sys::Relation) -> pg_sys::Oid {
    // SAFETY: the caller owns a live index relation, so its cached
    // `rd_index` form is readable for the duration of this call.
    unsafe {
        let index_form = (*index_relation).rd_index;
        if index_form.is_null() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                "pgcontext.compact() requires a pgcontext_hnsw index relation",
            );
        }
        (*index_form).indrelid
    }
}

/// Saturates a report counter into SQL's signed 64-bit integer.
///
/// These are advisory row counts, so clamping an impossibly large count is
/// preferable to failing a compaction that already succeeded.
fn usize_to_i64_report(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// Validates that a SQL caller passed a `pgcontext_hnsw` index whose metric
/// compaction can rebuild.
fn ensure_compactable_hnsw_relation(index_relation: pg_sys::Relation) -> HnswScoreMetric {
    // SAFETY: `PgRelation` owns a live relation cache entry for this function.
    // Reading its class form only validates that the caller passed an index
    // before HNSW opclass metadata is inspected below.
    let is_index = unsafe {
        !index_relation.is_null()
            && !(*index_relation).rd_rel.is_null()
            && u8::try_from((*(*index_relation).rd_rel).relkind).ok() == Some(pg_sys::RELKIND_INDEX)
    };
    if !is_index {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "pgcontext.compact() requires a pgcontext_hnsw index relation",
        );
    }
    // SAFETY: the relation was checked as an index above and stays locked by
    // `PgRelation`; this reads the same opclass metadata the AM scan does.
    unsafe { hnsw_score_metric(index_relation) }
}
