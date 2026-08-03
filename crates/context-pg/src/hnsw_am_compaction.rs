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

fn residual_tombstones(
    records: &[context_storage::DeltaRecord],
    covered_tids: Option<&BTreeSet<u64>>,
) -> Vec<context_storage::DeltaRecord> {
    let mut last_kind = BTreeMap::new();
    for record in records {
        last_kind.insert(record.heap_tid, record.kind);
    }
    last_kind
        .into_iter()
        .filter(|(heap_tid, kind)| {
            *kind == DeltaRecordKind::Tombstone
                && covered_tids.is_none_or(|covered| !covered.contains(heap_tid))
        })
        .map(|(heap_tid, _)| context_storage::DeltaRecord::tombstone(heap_tid))
        .collect()
}

fn bounded_compaction_working_set_bytes(
    rows: u64,
    mutation_records: usize,
    dimensions: u32,
    config: HnswConfig,
) -> Option<usize> {
    let rows = usize::try_from(rows).ok()?.checked_add(mutation_records)?;
    let vector_bytes = rows
        .checked_mul(usize::try_from(dimensions).ok()?)?
        .checked_mul(size_of::<f32>())?;
    // Source records, folded rows, and the graph overlap during construction.
    let vector_working_sets = vector_bytes.checked_mul(3)?;
    // Include reciprocal base-layer links and conservative container/identity
    // overhead. This intentionally over-admits neither allocator metadata nor
    // upper-layer links at the maintenance boundary.
    let per_row_graph = config
        .m()
        .checked_mul(2)?
        .checked_mul(size_of::<HnswNodeId>())?
        .checked_add(128)?;
    vector_working_sets.checked_add(rows.checked_mul(per_row_graph)?)
}

fn enforce_bounded_compaction_budget(
    rows: u64,
    mutation_records: usize,
    dimensions: u32,
    config: HnswConfig,
) {
    let projected = bounded_compaction_working_set_bytes(
        rows,
        mutation_records,
        dimensions,
        config,
    )
    .unwrap_or(usize::MAX);
    let budget = maintenance_work_mem_budget_bytes();
    if projected > budget {
        let suggested_mib = projected.div_ceil(1024 * 1024).max(1);
        raise_sql_error_with_hint(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!(
                "bounded HNSW compaction projected memory {projected} bytes exceeds maintenance_work_mem budget {budget} bytes"
            ),
            format!(
                "Raise the session budget, for example SET maintenance_work_mem = '{suggested_mib}MB', then retry. No segment publication occurred."
            ),
        );
    }
}

/// Freezes the full active delta and publishes its live rows as one additional
/// immutable graph segment. The original mutation extent remains part of the
/// descriptor so tombstones and replacements retire candidates from older
/// segments in durable append order.
///
/// # Safety
///
/// `index_relation` must be live and the caller must hold the per-index append
/// lock for the complete durable-write-then-publish sequence.
unsafe fn hnsw_rotate_delta_relation(
    index_relation: pg_sys::Relation,
    score_metric: HnswScoreMetric,
) -> bool {
    // SAFETY: the caller owns a live locked relation.
    let meta = unsafe { PgHnswGraphRead::new(index_relation).meta() };
    if meta.delta_start_block == u64::MAX
        || meta.delta_record_count == 0
        || usize::from(meta.segment_count) >= HNSW_MAX_SEGMENTS
    {
        return false;
    }
    let config = meta.stored_config(score_metric, hnsw_config_from_gucs().ef_search());
    enforce_bounded_compaction_budget(
        0,
        usize::try_from(meta.delta_record_count).unwrap_or(usize::MAX),
        meta.dimensions,
        config,
    );
    // SAFETY: the active delta boundary belongs to this publication.
    let delta_records = unsafe { read_hnsw_delta_records(index_relation, meta) };
    if delta_records.is_empty() {
        return false;
    }
    let entries = delta_records.iter().map(|record| match record.kind {
        DeltaRecordKind::Live => DeltaScanEntry::Live {
            heap_tid: record.heap_tid,
            vector: record.vector.as_slice(),
        },
        DeltaRecordKind::Tombstone => DeltaScanEntry::Tombstone {
            heap_tid: record.heap_tid,
        },
    });
    let live_rows = context_index::fold_compaction_live_rows(entries);
    let builder = ConcurrentHnswBuilder::new(
        score_metric.navigation_metric(),
        config,
        live_rows.len(),
    );
    for row in live_rows {
        let vector = DenseVector::new(row.vector).unwrap_or_else(|error| raise_core_error(error));
        builder
            .insert(HnswPointId::new(row.heap_tid), vector)
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                    format!("failed to build rotated HNSW segment: {error}"),
                )
            });
    }
    let graph = builder.finish().unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            format!("failed to finalize rotated HNSW segment: {error}"),
        )
    });
    let entry_point = graph.entry_point();
    let snapshots = graph.into_node_snapshots();
    let delta_owned_tids = delta_records
        .iter()
        .filter(|record| record.kind == DeltaRecordKind::Live)
        .map(|record| record.heap_tid)
        .collect::<BTreeSet<_>>();
    let residual = residual_tombstones(&delta_records, Some(&delta_owned_tids));
    let graph_start = u64::from(unsafe {
        pg_sys::RelationGetNumberOfBlocksInFork(index_relation, pg_sys::ForkNumber::MAIN_FORKNUM)
    });
    let generation = meta.next_segment_generation();
    if !snapshots.is_empty() {
        // SAFETY: the locked relation and owned snapshots live through the
        // complete append; this generation is invisible until publication.
        unsafe { write_hnsw_node_revisions_bulk(index_relation, &snapshots, generation) };
    }
    let graph_end = u64::from(unsafe {
        pg_sys::RelationGetNumberOfBlocksInFork(index_relation, pg_sys::ForkNumber::MAIN_FORKNUM)
    });
    let mutation_start = if residual.is_empty() {
        u64::MAX
    } else {
        graph_end
    };
    if !residual.is_empty() {
        // SAFETY: residual tombstones are immutable and generation-invisible
        // until the directory publication below.
        unsafe { write_hnsw_frozen_delta_records(index_relation, &residual, generation) };
    }
    let mutation_end = if residual.is_empty() {
        u64::MAX
    } else {
        u64::from(unsafe {
            pg_sys::RelationGetNumberOfBlocksInFork(
                index_relation,
                pg_sys::ForkNumber::MAIN_FORKNUM,
            )
        })
    };
    // SAFETY: re-read under the caller's append lock to fence unexpected
    // maintenance before publishing the prepared segment.
    let after_write = unsafe { PgHnswGraphRead::new(index_relation).meta() };
    if after_write.directory_epoch != meta.directory_epoch
        || after_write.delta_record_count != meta.delta_record_count
    {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_T_R_SERIALIZATION_FAILURE,
            "HNSW delta rotation observed a concurrent directory mutation",
        );
    }
    // SAFETY: all graph pages are durable and the mutation extent was already
    // durable before this single metapage publication.
    unsafe {
        update_hnsw_metapage(index_relation, |meta| {
            meta.publish_additional_segment(
                generation,
                graph_start,
                graph_end,
                graph_node_count(&snapshots),
                entry_point,
                mutation_start,
                mutation_end,
                residual.len() as u64,
            );
            let active_start = if mutation_end == u64::MAX {
                graph_end
            } else {
                mutation_end
            };
            meta.open_delta_region(active_start);
        });
    }
    record_hnsw_segment_rotation();
    true
}

/// Compacts the smallest adjacent immutable pair and atomically replaces only
/// those two descriptors. The read/build/write cost is bounded by the chosen
/// pair; every other segment and the active delta remain untouched.
///
/// # Safety
///
/// `index_relation` must be live and the caller must hold the append advisory
/// lock. This function conditionally acquires the parent-table maintenance
/// lock and returns `false` instead of waiting behind conflicting maintenance.
unsafe fn hnsw_compact_smallest_pair(
    index_relation: pg_sys::Relation,
    score_metric: HnswScoreMetric,
) -> bool {
    // SAFETY: the relation is live for this non-blocking maintenance lock.
    if !unsafe { try_lock_hnsw_compaction_table(index_relation) } {
        return false;
    }
    // SAFETY: the caller owns the relation and append lock.
    let meta = unsafe { PgHnswGraphRead::new(index_relation).meta() };
    if meta.segments().len() < 2 {
        return false;
    }
    let first = meta
        .segments()
        .windows(2)
        .enumerate()
        .min_by_key(|(index, pair)| {
            (
                pair[0]
                    .graph_nodes
                    .saturating_add(pair[1].graph_nodes)
                    .saturating_add(pair[0].mutation_record_count)
                    .saturating_add(pair[1].mutation_record_count),
                *index,
            )
        })
        .map_or(0, |(index, _)| index);
    let pair = [meta.segments()[first], meta.segments()[first + 1]];
    let pair_rows = pair[0].graph_nodes.saturating_add(pair[1].graph_nodes);
    let config = meta.stored_config(score_metric, hnsw_config_from_gucs().ef_search());
    let active_record_count = usize::try_from(meta.delta_record_count).unwrap_or(usize::MAX);
    let mut projected_bytes = bounded_compaction_working_set_bytes(
        pair_rows,
        active_record_count,
        meta.dimensions,
        config,
    )
    .unwrap_or(usize::MAX);
    enforce_bounded_compaction_budget(
        pair_rows,
        active_record_count,
        meta.dimensions,
        config,
    );
    // Pair compaction writes beyond the old active delta. Preserve its exact
    // logical contents now so they can be republished on a new contiguous
    // extent after the replacement graph.
    let active_records = if meta.delta_record_count == 0 {
        Vec::new()
    } else {
        // SAFETY: the active extent belongs to the validated metapage and the
        // append lock keeps it stable through publication.
        unsafe { read_hnsw_delta_records(index_relation, meta) }
    };
    let frozen_record_count = pair.iter().fold(0_usize, |total, segment| {
        total.saturating_add(
            usize::try_from(segment.mutation_record_count).unwrap_or(usize::MAX),
        )
    });
    let preflight_records = active_record_count.saturating_add(frozen_record_count);
    enforce_bounded_compaction_budget(pair_rows, preflight_records, meta.dimensions, config);
    projected_bytes = projected_bytes.max(
        bounded_compaction_working_set_bytes(
            pair_rows,
            preflight_records,
            meta.dimensions,
            config,
        )
        .unwrap_or(usize::MAX),
    );
    let mut segment_records = Vec::with_capacity(pair.len());
    for segment in pair {
        // SAFETY: every descriptor belongs to this validated publication.
        let base = unsafe { read_hnsw_segment_records(index_relation, segment) };
        let mutations = if segment.mutation_start_block != u64::MAX {
            // SAFETY: the immutable mutation extent belongs to this segment.
            unsafe {
                read_hnsw_delta_records_range(
                    index_relation,
                    segment.mutation_start_block,
                    segment.mutation_end_block,
                    segment.mutation_generation,
                    GraphPageKind::FrozenDelta,
                    segment.mutation_record_count,
                )
            }
        } else {
            Vec::new()
        };
        segment_records.push((base, mutations));
    }
    let covered_tids = segment_records
        .iter()
        .flat_map(|(base, _)| base.iter())
        .map(hnsw_record_heap_tid)
        .collect::<BTreeSet<_>>();
    let residual = residual_tombstones(
        &segment_records
            .iter()
            .flat_map(|(_, mutations)| mutations.iter().cloned())
            .collect::<Vec<_>>(),
        Some(&covered_tids),
    );
    let live_rows = context_index::fold_compaction_live_rows(segment_records.iter().flat_map(
        |(base, mutations)| {
            base.iter()
                .map(|record| {
                    let heap_tid = hnsw_record_heap_tid(record);
                    if hnsw_record_is_tombstoned(record) {
                        DeltaScanEntry::Tombstone { heap_tid }
                    } else {
                        DeltaScanEntry::Live {
                            heap_tid,
                            vector: record.vector.as_slice(),
                        }
                    }
                })
                .chain(mutations.iter().map(|record| match record.kind {
                    DeltaRecordKind::Live => DeltaScanEntry::Live {
                        heap_tid: record.heap_tid,
                        vector: record.vector.as_slice(),
                    },
                    DeltaRecordKind::Tombstone => DeltaScanEntry::Tombstone {
                        heap_tid: record.heap_tid,
                    },
                }))
        },
    ));
    let builder = ConcurrentHnswBuilder::new(
        score_metric.navigation_metric(),
        config,
        live_rows.len(),
    );
    for row in live_rows {
        let vector = DenseVector::new(row.vector).unwrap_or_else(|error| raise_core_error(error));
        builder
            .insert(HnswPointId::new(row.heap_tid), vector)
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                    format!("failed to build bounded compacted HNSW segment: {error}"),
                )
            });
    }
    let graph = builder.finish().unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            format!("failed to finalize bounded compacted HNSW segment: {error}"),
        )
    });
    let entry_point = graph.entry_point();
    let snapshots = graph.into_node_snapshots();
    let generation = meta.next_segment_generation();
    let graph_start = u64::from(unsafe {
        pg_sys::RelationGetNumberOfBlocksInFork(index_relation, pg_sys::ForkNumber::MAIN_FORKNUM)
    });
    if !snapshots.is_empty() {
        // SAFETY: these pages are generation-invisible until the metapage flip.
        unsafe { write_hnsw_node_revisions_bulk(index_relation, &snapshots, generation) };
    }
    let graph_end = u64::from(unsafe {
        pg_sys::RelationGetNumberOfBlocksInFork(index_relation, pg_sys::ForkNumber::MAIN_FORKNUM)
    });
    let mutation_start = if residual.is_empty() {
        u64::MAX
    } else {
        graph_end
    };
    if !residual.is_empty() {
        // SAFETY: FrozenDelta pages are skipped by old active-delta readers.
        unsafe { write_hnsw_frozen_delta_records(index_relation, &residual, generation) };
    }
    let mutation_end = if residual.is_empty() {
        u64::MAX
    } else {
        u64::from(unsafe {
            pg_sys::RelationGetNumberOfBlocksInFork(
                index_relation,
                pg_sys::ForkNumber::MAIN_FORKNUM,
            )
        })
    };
    let active_generation = generation.saturating_add(1);
    let active_start = u64::from(unsafe {
        pg_sys::RelationGetNumberOfBlocksInFork(
            index_relation,
            pg_sys::ForkNumber::MAIN_FORKNUM,
        )
    });
    if !active_records.is_empty() {
        // SAFETY: active records are owned, append-locked, and stamped with a
        // generation that remains invisible until the metapage flip below.
        unsafe {
            write_hnsw_active_delta_records(
                index_relation,
                &active_records,
                active_generation,
            );
        }
    }
    let active_end = u64::from(unsafe {
        pg_sys::RelationGetNumberOfBlocksInFork(
            index_relation,
            pg_sys::ForkNumber::MAIN_FORKNUM,
        )
    });
    // SAFETY: fence against unexpected mutation before publication.
    let after_write = unsafe { PgHnswGraphRead::new(index_relation).meta() };
    if after_write.directory_epoch != meta.directory_epoch
        || after_write.delta_record_count != meta.delta_record_count
    {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_T_R_SERIALIZATION_FAILURE,
            "bounded HNSW compaction observed a concurrent directory mutation",
        );
    }
    // SAFETY: every new page is durable; this is the single publication point.
    unsafe {
        update_hnsw_metapage(index_relation, |meta| {
            meta.replace_segment_pair(
                first,
                generation,
                graph_start,
                graph_end,
                graph_node_count(&snapshots),
                entry_point,
                mutation_start,
                mutation_end,
                residual.len() as u64,
            );
            meta.relocate_active_delta(
                active_start,
                active_end,
                active_generation,
                active_records.len() as u64,
            );
        });
    }
    hnsw_physical_failpoint(15, "after_compaction_publish");
    // SAFETY: these backend and relation identities are stable for the call.
    let database_oid = unsafe { pg_sys::MyDatabaseId.to_u32() };
    let index_oid = unsafe { (*index_relation).rd_id.to_u32() };
    let rel_file_number = unsafe { (*index_relation).rd_locator.relNumber.to_u32() };
    retire_hnsw_segment_caches(
        database_oid,
        index_oid,
        rel_file_number,
        &[pair[0].segment_id, pair[1].segment_id],
    );
    let mutation_pages = if mutation_start == u64::MAX {
        0
    } else {
        mutation_end.saturating_sub(mutation_start)
    };
    record_hnsw_segment_compaction(
        pair_rows,
        snapshots.len(),
        graph_end
            .saturating_sub(graph_start)
            .saturating_add(mutation_pages),
        projected_bytes,
    );
    true
}

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
) -> HnswCompactionOutcome {
    // SAFETY: the caller owns a live index relation for this call.
    let meta = unsafe { PgHnswGraphRead::new(index_relation).meta() };
    // SAFETY: the validated relation owns a live versioned metapage.
    let config = unsafe { hnsw_stored_config(index_relation, score_metric) };

    let mutation_count = meta
        .segments()
        .iter()
        .fold(meta.delta_record_count, |total, segment| {
            total.saturating_add(segment.mutation_record_count)
        });
    enforce_bounded_compaction_budget(
        meta.graph_nodes,
        usize::try_from(mutation_count).unwrap_or(usize::MAX),
        meta.dimensions,
        config,
    );
    // Preserve publication chronology: each segment graph is followed by its
    // own mutation log, then the next segment may resurrect a reused heap TID.
    let mut segment_records = Vec::with_capacity(meta.segments().len());
    for segment in meta.segments() {
        let base = unsafe { read_hnsw_segment_records(index_relation, *segment) };
        let mutations = if segment.mutation_start_block == u64::MAX {
            Vec::new()
        } else {
            unsafe {
                read_hnsw_delta_records_range(
                    index_relation,
                    segment.mutation_start_block,
                    segment.mutation_end_block,
                    segment.mutation_generation,
                    GraphPageKind::FrozenDelta,
                    segment.mutation_record_count,
                )
            }
        };
        segment_records.push((base, mutations));
    }
    let active_records = unsafe { read_hnsw_delta_records(index_relation, meta) };
    let chronological = segment_records
        .iter()
        .flat_map(|(base, mutations)| {
            base.iter()
                .map(|record| {
                    let heap_tid = hnsw_record_heap_tid(record);
                    if hnsw_record_is_tombstoned(record) {
                        DeltaScanEntry::Tombstone { heap_tid }
                    } else {
                        DeltaScanEntry::Live {
                            heap_tid,
                            vector: record.vector.as_slice(),
                        }
                    }
                })
                .chain(mutations.iter().map(|record| match record.kind {
                    DeltaRecordKind::Live => DeltaScanEntry::Live {
                        heap_tid: record.heap_tid,
                        vector: record.vector.as_slice(),
                    },
                    DeltaRecordKind::Tombstone => DeltaScanEntry::Tombstone {
                        heap_tid: record.heap_tid,
                    },
                }))
        })
        .chain(active_records.iter().map(|record| match record.kind {
            DeltaRecordKind::Live => DeltaScanEntry::Live {
                heap_tid: record.heap_tid,
                vector: record.vector.as_slice(),
            },
            DeltaRecordKind::Tombstone => DeltaScanEntry::Tombstone {
                heap_tid: record.heap_tid,
            },
        }));
    let live_rows = context_index::fold_compaction_live_rows(chronological);

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

    let entry_point = graph.entry_point();
    let snapshots = graph.into_node_snapshots();

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
            meta.record_build(dimensions, graph_node_count(&snapshots), entry_point);
            meta.publish_single_segment(
                fresh_base_start,
                post_write_block_count,
                graph_node_count(&snapshots),
                entry_point,
            );
            meta.open_delta_region(post_write_block_count);
        });
    }
    hnsw_physical_failpoint(15, "after_compaction_publish");

    HnswCompactionOutcome {
        live_rows: snapshots.len(),
        base_records: usize::try_from(meta.graph_nodes).unwrap_or(usize::MAX),
        delta_records: usize::try_from(mutation_count).unwrap_or(usize::MAX),
    }
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
#[pg_extern(name = "compact")]
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
    ensure_hnsw_maintenance_privilege(index_relation);

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
    let outcome = unsafe { hnsw_compact_relation(index_relation, score_metric) };

    TableIterator::once((
        usize_to_i64_report(outcome.live_rows),
        usize_to_i64_report(outcome.base_records),
        usize_to_i64_report(outcome.delta_records),
    ))
}

/// Compacts at most one adjacent immutable HNSW segment pair.
///
/// This is the bounded execution seam used by the supervised build worker.
/// Publication remains durable-write-then-metapage-flip, and an index with
/// fewer than two immutable segments is a successful no-op.
#[pg_extern(name = "_compact_hnsw_segment_pair")]
#[search_path(pg_catalog, pgcontext, public)]
fn hnsw_compact_segment_pair(index: PgRelation, expected_directory_epoch: i64) -> bool {
    let index_relation = index.as_ptr();
    let score_metric = ensure_compactable_hnsw_relation(index_relation);
    ensure_hnsw_maintenance_privilege(index_relation);
    // SAFETY: PgRelation owns the validated relation; the table lock excludes
    // VACUUM and the advisory lock excludes delta append/publication.
    unsafe { lock_hnsw_compaction_table(index_relation) };
    // SAFETY: as above.
    unsafe { serialize_hnsw_insert(index_relation) };
    let expected_directory_epoch = u64::try_from(expected_directory_epoch).unwrap_or(u64::MAX);
    // A retry after publication, or a job made stale by any intervening
    // rotation/VACUUM, is a successful no-op. It must never select a new pair.
    if unsafe { PgHnswGraphRead::new(index_relation).meta() }.directory_epoch
        != expected_directory_epoch
    {
        return false;
    }
    // SAFETY: the complete bounded build and publication occur under both
    // required locks. Re-taking the table lock conditionally is reentrant.
    unsafe { hnsw_compact_smallest_pair(index_relation, score_metric) }
}

pub(crate) fn hnsw_directory_epoch(index_relation: pg_sys::Relation) -> i64 {
    // SAFETY: callers hold a PgRelation or AM callback reference for the
    // immediate metapage copy.
    let epoch = unsafe { PgHnswGraphRead::new(index_relation).meta() }.directory_epoch;
    i64::try_from(epoch).unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            "HNSW directory epoch exceeds supervised job storage",
        )
    })
}

/// Reports the current bounded segmented-HNSW publication and compaction
/// advice for one index.
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL generation requires the explicit table row tuple"
)]
#[pg_extern(name = "hnsw_segment_stats")]
#[search_path(pg_catalog, pgcontext, public)]
fn hnsw_segment_stats(
    index: PgRelation,
) -> TableIterator<
    'static,
    (
        name!(segment_count, i32),
        name!(active_delta_records, i64),
        name!(immutable_rows, i64),
        name!(smallest_pair_rows, Option<i64>),
        name!(compaction_debt, bool),
        name!(parallel_eligible, bool),
        name!(serving_mode, String),
        name!(frozen_mutation_records, i64),
        name!(active_delta_blocks, i64),
        name!(directory_epoch, i64),
        name!(codec, String),
        name!(codec_revision, Option<String>),
        name!(codec_code_width, Option<i32>),
        name!(candidate_budget, i32),
        name!(exact_source_rerank, bool),
        name!(codec_serving_capability, String),
    ),
> {
    let index_relation = index.as_ptr();
    let _metric = ensure_compactable_hnsw_relation(index_relation);
    // SAFETY: PgRelation owns the validated HNSW relation for this call.
    let meta = unsafe { PgHnswGraphRead::new(index_relation).meta() };
    let segment_count = meta.segments().len();
    let immutable_rows = meta
        .segments()
        .iter()
        .fold(0_u64, |rows, segment| rows.saturating_add(segment.graph_nodes));
    let smallest_pair = meta
        .segments()
        .windows(2)
        .map(|pair| pair[0].graph_nodes.saturating_add(pair[1].graph_nodes))
        .min();
    let parallel_workers = crate::settings::hnsw_segment_parallel_workers_from_guc();
    let parallel_eligible = segment_count >= 3 && parallel_workers >= 2;
    let frozen_mutation_records = meta.segments().iter().fold(0_usize, |total, segment| {
        if segment.mutation_start_block == u64::MAX {
            total
        } else {
            total.saturating_add(unsafe {
                read_hnsw_delta_records_range(
                    index_relation,
                    segment.mutation_start_block,
                    segment.mutation_end_block,
                    segment.mutation_generation,
                    GraphPageKind::FrozenDelta,
                    segment.mutation_record_count,
                )
                .len()
            })
        }
    });
    TableIterator::once((
        i32::try_from(segment_count).unwrap_or(i32::MAX),
        i64::try_from(meta.delta_record_count).unwrap_or(i64::MAX),
        i64::try_from(immutable_rows).unwrap_or(i64::MAX),
        smallest_pair.map(|rows| i64::try_from(rows).unwrap_or(i64::MAX)),
        segment_count >= HNSW_MAX_SEGMENTS.saturating_sub(1),
        parallel_eligible,
        if parallel_eligible {
            "parallel_owned_pack_when_admitted"
        } else {
            "serial_backend_affine"
        }
        .to_owned(),
        i64::try_from(frozen_mutation_records).unwrap_or(i64::MAX),
        i64::try_from(meta.delta_end_block.saturating_sub(meta.delta_start_block))
            .unwrap_or(i64::MAX),
        i64::try_from(meta.directory_epoch).unwrap_or(i64::MAX),
        meta.codec_name().to_owned(),
        (meta.codec_config_revision != 0).then(|| meta.codec_config_revision.to_string()),
        meta.codec_code_width()
            .map(|width| i32::try_from(width).unwrap_or(i32::MAX)),
        i32::try_from(crate::settings::hnsw_candidate_budget_from_guc()).unwrap_or(i32::MAX),
        meta.quantization_mode != options::HNSW_QUANTIZATION_NONE_U16,
        if meta.quantization_mode == options::HNSW_QUANTIZATION_NONE_U16 {
            "full_precision_pages"
        } else {
            "packed_generation_required"
        }
        .to_owned(),
    ))
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
/// Used by bounded rotation/compaction paths that already hold the per-index
/// advisory lock and therefore must not wait behind table-first maintenance.
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

/// Requires the session user to own the index (directly or through role
/// membership) before a SQL-callable maintenance function mutates it.
fn ensure_hnsw_maintenance_privilege(index_relation: pg_sys::Relation) {
    // SAFETY: PgRelation keeps the relcache entry live for this SQL call.
    let index_oid = unsafe { (*index_relation).rd_id };
    let allowed = Spi::get_one_with_args::<bool>(
        "SELECT pg_catalog.pg_has_role(SESSION_USER, class.relowner, 'MEMBER')
           FROM pg_catalog.pg_class AS class
          WHERE class.oid = $1",
        &[index_oid.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("HNSW maintenance ownership check failed: {error}"),
        )
    })
    .unwrap_or(false);
    if !allowed {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
            "permission denied for HNSW index maintenance",
        );
    }
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
