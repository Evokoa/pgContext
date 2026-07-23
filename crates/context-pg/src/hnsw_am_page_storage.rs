// PostgreSQL page-storage operations included by `hnsw_am.rs`. This fragment
// concentrates buffer, page, and relation unsafe boundaries behind the access
// method's safe callback layer.

// Reserve space for PostgreSQL's page header, line pointers, and pgContext's
// typed page header. Node and delta records are intentionally single-page.
const HNSW_MAX_PAGE_RECORD_BYTES: usize = pg_sys::BLCKSZ as usize - 128;

fn checked_hnsw_item_range(
    page_lower: usize,
    page_upper: usize,
    page_special: usize,
    line_pointer_end: usize,
    item_flags: u32,
    item_offset: usize,
    item_len: usize,
) -> Result<(usize, usize), &'static str> {
    let page_bytes = pg_sys::BLCKSZ as usize;
    let header_bytes = offset_of!(pg_sys::PageHeaderData, pd_linp);
    if page_lower < header_bytes
        || page_lower > page_upper
        || page_upper > page_special
        || page_special > page_bytes
        || line_pointer_end > page_lower
    {
        return Err("HNSW page header or line-pointer array is corrupt");
    }
    if item_flags != pg_sys::LP_NORMAL || item_len == 0 {
        return Err("HNSW page item line pointer is not normal");
    }
    let item_end = item_offset
        .checked_add(item_len)
        .ok_or("HNSW page item range overflows")?;
    if item_offset < page_upper || item_end > page_special {
        return Err("HNSW page item lies outside the item region");
    }
    Ok((item_offset, item_end))
}

/// Returns a validated mutable span for one normal PostgreSQL page item.
///
/// # Safety
///
/// `page` must point to a pinned BLCKSZ PostgreSQL page that remains locked for
/// the returned span's complete use. Mutable access additionally requires the
/// caller to hold the buffer lock exclusively.
unsafe fn checked_hnsw_page_item_span(
    page: pg_sys::Page,
    offset: pg_sys::OffsetNumber,
) -> Result<(*mut u8, usize), &'static str> {
    if page.is_null() || offset < HNSW_FIRST_OFFSET {
        return Err("HNSW page or item offset is invalid");
    }
    // SAFETY: The caller owns a pinned page. Reading the fixed PostgreSQL page
    // header is valid for the BLCKSZ allocation; all derived offsets are
    // validated before a line pointer or payload pointer is dereferenced.
    let header = unsafe { &*page.cast::<pg_sys::PageHeaderData>() };
    let line_pointer_end = offset_of!(pg_sys::PageHeaderData, pd_linp)
        .checked_add(usize::from(offset) * size_of::<pg_sys::ItemIdData>())
        .ok_or("HNSW line-pointer range overflows")?;
    let page_lower = usize::from(header.pd_lower);
    let page_upper = usize::from(header.pd_upper);
    let page_special = usize::from(header.pd_special);
    let page_bytes = pg_sys::BLCKSZ as usize;
    let header_bytes = offset_of!(pg_sys::PageHeaderData, pd_linp);
    if page_lower < header_bytes
        || page_lower > page_upper
        || page_upper > page_special
        || page_special > page_bytes
        || line_pointer_end > page_lower
    {
        return Err("HNSW item offset exceeds the line-pointer array");
    }
    // SAFETY: `line_pointer_end <= pd_lower <= BLCKSZ` proves this one-based
    // line pointer is inside the page's line-pointer array.
    let item_id = unsafe { pg_sys::PageGetItemId(page, offset) };
    // SAFETY: `item_id` is inside the validated line-pointer array.
    let item = unsafe { &*item_id };
    let item_offset = item.lp_off() as usize;
    let item_len = item.lp_len() as usize;
    let (start, end) = checked_hnsw_item_range(
        page_lower,
        page_upper,
        page_special,
        line_pointer_end,
        item.lp_flags(),
        item_offset,
        item_len,
    )?;
    // SAFETY: the checked range is contained within this BLCKSZ page.
    let pointer = unsafe { page.cast::<u8>().add(start) };
    Ok((pointer, end - start))
}

/// Copies one validated PostgreSQL page item into Rust-owned bytes.
///
/// # Safety
///
/// `page` must point to a pinned BLCKSZ PostgreSQL page held under at least a
/// shared buffer lock for this complete call.
unsafe fn copy_hnsw_page_item(
    page: pg_sys::Page,
    offset: pg_sys::OffsetNumber,
) -> Result<Vec<u8>, &'static str> {
    // SAFETY: delegated to the checked span boundary above.
    let (pointer, len) = unsafe { checked_hnsw_page_item_span(page, offset)? };
    // SAFETY: the pointer and length were proven to lie inside the pinned page.
    Ok(unsafe { slice::from_raw_parts(pointer, len) }.to_vec())
}

unsafe fn ensure_hnsw_metapage(index_relation: pg_sys::Relation) {
    // SAFETY: PostgreSQL owns the relation pointer for the duration of AM build
    // callbacks. This reads only the main fork block count.
    let block_count = unsafe {
        pg_sys::RelationGetNumberOfBlocksInFork(index_relation, pg_sys::ForkNumber::MAIN_FORKNUM)
    };
    if block_count > 0 {
        return;
    }

    // SAFETY: `InvalidBlockNumber` is the append-block sentinel used by the
    // pgrx `IndexBuildHeapScan` wrapper. The returned buffer is exclusively
    // locked by `RBM_ZERO_AND_LOCK` and released below.
    let buffer = unsafe {
        pg_sys::ReadBufferExtended(
            index_relation,
            pg_sys::ForkNumber::MAIN_FORKNUM,
            pg_sys::InvalidBlockNumber,
            pg_sys::ReadBufferMode::RBM_ZERO_AND_LOCK,
            ptr::null_mut(),
        )
    };
    // SAFETY: The buffer is valid and locked. HNSW stores metapage metadata as
    // a regular page item so PostgreSQL retains full ownership of the page
    // header and LSN fields.
    unsafe {
        let page = pg_sys::BufferGetPage(buffer);
        pg_sys::PageInit(page, pg_sys::BLCKSZ as pg_sys::Size, 0);
        write_hnsw_meta_page(page, HnswMetaPage::empty());
        pg_sys::MarkBufferDirty(buffer);
        pg_sys::UnlockReleaseBuffer(buffer);
    }
}

unsafe fn append_hnsw_vector_record(
    index_relation: pg_sys::Relation,
    record: &HnswVectorRecord,
) -> HnswPageItemLocation {
    let payload = encode_hnsw_vector_record(record);
    // SAFETY: The owned payload is valid for the complete append call.
    unsafe { append_hnsw_typed_record(index_relation, &payload, GraphPageKind::Node, "vector") }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HnswPageItemLocation {
    page: u64,
    slot: u16,
}

#[allow(dead_code, reason = "retained to decode pre-v6 experimental index pages")]
unsafe fn append_hnsw_adjacency_record(
    index_relation: pg_sys::Relation,
    record: &HnswAdjacencyRecord,
) -> HnswPageItemLocation {
    let payload = encode_hnsw_adjacency_record(record);
    // SAFETY: The owned payload is valid for the complete append call.
    unsafe { append_hnsw_typed_record(index_relation, &payload, GraphPageKind::Adjacency, "adjacency") }
}

unsafe fn append_hnsw_directory_record(
    index_relation: pg_sys::Relation,
    record: HnswDirectoryRecord,
) -> HnswPageItemLocation {
    let payload = encode_hnsw_directory_record(record);
    // SAFETY: The owned locator payload remains valid for the complete append.
    unsafe { append_hnsw_typed_record(index_relation, &payload, GraphPageKind::Directory, "directory") }
}

const fn hnsw_location_revision(location: HnswPageItemLocation) -> u64 {
    (location.page << u16::BITS) | location.slot as u64
}

/// Appends one segmented-write-path delta record (live insert or
/// tombstone) as a single page item.
///
/// # Safety
///
/// `index_relation` must be a live index relation for the complete append.
unsafe fn append_hnsw_delta_record(
    index_relation: pg_sys::Relation,
    record: &context_storage::DeltaRecord,
) -> HnswPageItemLocation {
    let payload = context_storage::encode_delta_record(record).unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            format!("failed to encode HNSW delta record: {error}"),
        )
    });
    // SAFETY: The owned payload is valid for the complete append call.
    unsafe { append_hnsw_typed_record(index_relation, &payload, GraphPageKind::Delta, "delta") }
}

/// Narrows a metapage-published block number to the platform block-number
/// type, failing closed when a corrupt metapage names an unreachable block.
fn block_number_from_u64(value: u64, label: &str) -> pg_sys::BlockNumber {
    pg_sys::BlockNumber::try_from(value).unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            format!("{label} exceeds the platform block-number range"),
        )
    })
}

/// Reads every segmented-write-path delta record appended since
/// `delta_start_block`, in append order.
///
/// Bounded by the delta region, not the base graph: only blocks from
/// `delta_start_block` to the relation's current end are visited, and
/// (post-S3) nothing but delta pages is appended to that range during
/// normal operation.
///
/// # Safety
///
/// `index_relation` must be a live index relation for the complete scan.
unsafe fn read_hnsw_delta_records(
    index_relation: pg_sys::Relation,
    delta_start_block: u64,
) -> Vec<context_storage::DeltaRecord> {
    if delta_start_block == u64::MAX {
        return Vec::new();
    }
    // SAFETY: PostgreSQL relation metadata is valid for this AM callback.
    let block_count = u64::from(unsafe {
        pg_sys::RelationGetNumberOfBlocksInFork(index_relation, pg_sys::ForkNumber::MAIN_FORKNUM)
    });
    if delta_start_block >= block_count {
        return Vec::new();
    }
    let start_block = block_number_from_u64(delta_start_block, "HNSW delta_start_block");
    let end_block = block_number_from_u64(block_count, "HNSW relation block count");

    let mut records = Vec::new();
    for block_number in start_block..end_block {
        // SAFETY: PostgreSQL owns the relation pointer and returns a pinned
        // buffer for the requested block until it is released below.
        let buffer = unsafe {
            pg_sys::ReadBufferExtended(
                index_relation,
                pg_sys::ForkNumber::MAIN_FORKNUM,
                block_number,
                pg_sys::ReadBufferMode::RBM_NORMAL,
                ptr::null_mut(),
            )
        };
        // SAFETY: The buffer is pinned and locked shared while every checked
        // page item is copied into Rust-owned bytes. Decoding happens only
        // after the buffer lock and pin are released.
        let page_items = unsafe {
            pg_sys::LockBuffer(buffer, pg_sys::BUFFER_LOCK_SHARE.cast_signed());
            let page = pg_sys::BufferGetPage(buffer);
            if pg_sys::PageIsNew(page) {
                pg_sys::UnlockReleaseBuffer(buffer);
                continue;
            }
            let max_offset = pg_sys::PageGetMaxOffsetNumber(page);
            if max_offset < HNSW_FIRST_OFFSET {
                pg_sys::UnlockReleaseBuffer(buffer);
                continue;
            }
            let mut page_items = Vec::with_capacity(usize::from(max_offset));
            for offset in HNSW_FIRST_OFFSET..=max_offset {
                match copy_hnsw_page_item(page, offset) {
                    Ok(item) => page_items.push(item),
                    Err(error) => {
                        pg_sys::UnlockReleaseBuffer(buffer);
                        raise_sql_error(PgSqlErrorCode::ERRCODE_DATA_CORRUPTED, error);
                    }
                }
            }
            pg_sys::UnlockReleaseBuffer(buffer);
            page_items
        };
        let header = match decode_page_header(&page_items[0]) {
            Ok(header) => header,
            Err(error) => {
                raise_sql_error(PgSqlErrorCode::ERRCODE_DATA_CORRUPTED, error.to_string());
            }
        };
        // A page in the delta region that is not (or not yet) a Delta page
        // is either mid-append (page initialized, item not yet visible to
        // this snapshot) or a corrupt index; either way, skip rather than
        // fail the whole scan on a single racing page.
        if header.kind != GraphPageKind::Delta {
            continue;
        }
        for item in page_items.iter().skip(1) {
            match context_storage::decode_delta_record(item) {
                Ok(record) => records.push(record),
                Err(error) => {
                    raise_sql_error(
                        PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                        format!("invalid HNSW delta record: {error}"),
                    );
                }
            }
        }
    }
    records
}

unsafe fn append_hnsw_node_revision(
    index_relation: pg_sys::Relation,
    record: &HnswVectorRecord,
) -> HnswPageItemLocation {
    // SAFETY: the caller owns a live index relation and the record remains
    // borrowed through both append operations.
    let location = unsafe { append_hnsw_vector_record(index_relation, record) };
    let identity = u64::try_from(record.node_id.get()).unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            "HNSW node id exceeds directory storage",
        )
    });
    // SAFETY: the locator is derived from the node record that was just made
    // durable through Generic WAL on this same live relation.
    unsafe {
        append_hnsw_directory_record(
            index_relation,
            HnswDirectoryRecord {
                key_kind: GraphDirectoryKeyKind::Node,
                generation: HNSW_INITIAL_PAGE_GENERATION,
                identity,
                ordinal: 0,
                target_page: location.page,
                target_slot: location.slot,
                revision: hnsw_location_revision(location),
            },
        )
    };
    location
}

/// Writes a whole base graph as page batches, stamping every page with
/// `generation`.
///
/// The build path passes the live generation, because its output *is* the
/// published base. Compaction passes the generation its metapage flip will
/// publish, so its pages stay invisible until that flip.
unsafe fn write_hnsw_node_revisions_bulk(
    index_relation: pg_sys::Relation,
    snapshots: &[HnswGraphNodeSnapshot],
    generation: u64,
) {
    // SAFETY: The build callback owns the live relation and every encoded
    // snapshot is Rust-owned for the complete synchronous bulk write.
    let locations = unsafe {
        append_hnsw_bulk_typed_records(
            index_relation,
            snapshots.len(),
            GraphPageKind::Node,
            "vector",
            generation,
            |index| {
                let record = hnsw_vector_record_from_snapshot(&snapshots[index]);
                encode_hnsw_vector_record(&record)
            },
        )
    };
    // SAFETY: Each directory payload refers to the corresponding node record
    // committed by the preceding page batches in this same index build.
    unsafe {
        append_hnsw_bulk_typed_records(
            index_relation,
            snapshots.len(),
            GraphPageKind::Directory,
            "directory",
            generation,
            |index| {
                let snapshot = &snapshots[index];
                let location = locations[index];
                let identity = u64::try_from(snapshot.node_id().get()).unwrap_or_else(|_| {
                    raise_sql_error(
                        PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                        "HNSW node id exceeds directory storage",
                    )
                });
                encode_hnsw_directory_record(HnswDirectoryRecord {
                    key_kind: GraphDirectoryKeyKind::Node,
                    generation: HNSW_INITIAL_PAGE_GENERATION,
                    identity,
                    ordinal: 0,
                    target_page: location.page,
                    target_slot: location.slot,
                    revision: hnsw_location_revision(location),
                })
            },
        )
    };
}

#[allow(dead_code, reason = "retained for pre-v6 WAL/page compatibility tests")]
unsafe fn append_hnsw_adjacency_revision(
    index_relation: pg_sys::Relation,
    record: &HnswAdjacencyRecord,
) -> HnswPageItemLocation {
    // SAFETY: the caller owns a live index relation and the record remains
    // borrowed through both append operations.
    let location = unsafe { append_hnsw_adjacency_record(index_relation, record) };
    let identity = u64::try_from(record.node_id.get()).unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            "HNSW node id exceeds directory storage",
        )
    });
    let ordinal = u16::try_from(record.layer.get()).unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            "HNSW layer exceeds directory storage",
        )
    });
    // SAFETY: the locator is derived from the adjacency record that was just
    // made durable through Generic WAL on this same live relation.
    unsafe {
        append_hnsw_directory_record(
            index_relation,
            HnswDirectoryRecord {
                key_kind: GraphDirectoryKeyKind::Adjacency,
                generation: HNSW_INITIAL_PAGE_GENERATION,
                identity,
                ordinal,
                target_page: location.page,
                target_slot: location.slot,
                revision: hnsw_location_revision(location),
            },
        )
    };
    location
}

/// Reports whether an already-initialized page may receive a record stamped
/// for `generation`.
///
/// A page whose header cannot be decoded is refused rather than treated as
/// corruption: the caller's fallback allocates a fresh page, so refusing costs
/// one page and keeps a malformed neighbour from capturing live records.
///
/// # Safety
///
/// `page` must be a pinned BLCKSZ PostgreSQL page held under an exclusive
/// buffer lock for this call.
unsafe fn page_accepts_generation(page: pg_sys::Page, generation: u64) -> bool {
    // SAFETY: the caller holds the page exclusively for this read.
    let Ok(item) = (unsafe { copy_hnsw_page_item(page, HNSW_FIRST_OFFSET) }) else {
        return false;
    };
    decode_page_header(&item).is_ok_and(|header| header.generation == generation)
}

unsafe fn append_hnsw_typed_record(
    index_relation: pg_sys::Relation,
    payload: &[u8],
    kind: GraphPageKind,
    label: &str,
) -> HnswPageItemLocation {
    ensure_hnsw_page_record_fits(payload, label);

    // SAFETY: The caller passes a valid index relation owned by PostgreSQL.
    unsafe { ensure_hnsw_metapage(index_relation) };
    // Incremental appends always join the base generation that is already
    // published — only build and compaction create a new one — so the stamp is
    // read from the live metapage rather than passed in.
    // SAFETY: the relation is live and its metapage was ensured above.
    let generation = unsafe { PgHnswGraphRead::new(index_relation).meta().page_generation() };
    // SAFETY: The relation remains live for this append operation, and `kind`
    // selects the typed page chain whose final block is inspected.
    let target_block = unsafe { find_last_hnsw_page(index_relation, kind) }
        .unwrap_or(pg_sys::InvalidBlockNumber);
    // SAFETY: The relation is valid and `payload` remains borrowed for the
    // duration of the append attempt.
    if let Some(location) = unsafe { try_append_hnsw_typed_record(index_relation, target_block, payload, kind, generation) } {
        return location;
    }
    // SAFETY: Appending to `InvalidBlockNumber` asks PostgreSQL to allocate a
    // fresh page; the relation and payload remain valid for this final attempt.
    if let Some(location) = unsafe {
        try_append_hnsw_typed_record(
            index_relation,
            pg_sys::InvalidBlockNumber,
            payload,
            kind,
            generation,
        )
    } {
        return location;
    }

    raise_sql_error(
        PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
        format!("failed to append HNSW {label} record to a fresh index page"),
    );
}

unsafe fn append_hnsw_bulk_typed_records(
    index_relation: pg_sys::Relation,
    record_count: usize,
    kind: GraphPageKind,
    label: &str,
    generation: u64,
    mut encode: impl FnMut(usize) -> Vec<u8>,
) -> Vec<HnswPageItemLocation> {
    // SAFETY: The caller owns a live index relation for the bulk build.
    unsafe { ensure_hnsw_metapage(index_relation) };
    let mut locations = Vec::with_capacity(record_count);
    while locations.len() < record_count {
        // SAFETY: The append sentinel allocates a fresh page already locked
        // exclusively for this backend.
        let buffer = unsafe {
            pg_sys::ReadBufferExtended(
                index_relation,
                pg_sys::ForkNumber::MAIN_FORKNUM,
                pg_sys::InvalidBlockNumber,
                pg_sys::ReadBufferMode::RBM_ZERO_AND_LOCK,
                ptr::null_mut(),
            )
        };
        // SAFETY: The fresh pinned buffer is exclusively locked. All mutation
        // occurs on the Generic-WAL shadow page and is committed as one page
        // batch before the buffer is released.
        unsafe {
            let state = pg_sys::GenericXLogStart(index_relation);
            let registered =
                wal_contract::critical_section::HnswWalRegisteredSinglePage::register(
                    state,
                    buffer,
                    pg_sys::GENERIC_XLOG_FULL_IMAGE.cast_signed(),
                );
            let page = registered.page();
            let block_number = pg_sys::BufferGetBlockNumber(buffer);
            hnsw_physical_failpoint(1, "before_page_initialization");
            initialize_hnsw_data_page(page, u64::from(block_number), kind, generation);
            hnsw_physical_failpoint(2, "after_page_initialization");
            let page_start = locations.len();
            while locations.len() < record_count {
                let payload = encode(locations.len());
                if payload.len() > HNSW_MAX_PAGE_RECORD_BYTES {
                    pg_sys::GenericXLogAbort(state);
                    pg_sys::UnlockReleaseBuffer(buffer);
                    ensure_hnsw_page_record_fits(&payload, label);
                }
                hnsw_physical_failpoint(3, "before_append");
                let offset = pg_sys::PageAddItemExtended(
                    page,
                    payload.as_ptr().cast_mut().cast(),
                    payload.len() as pg_sys::Size,
                    HNSW_INVALID_OFFSET,
                    0,
                );
                if offset == HNSW_INVALID_OFFSET {
                    break;
                }
                hnsw_physical_failpoint(4, "after_append");
                locations.push(HnswPageItemLocation {
                    page: u64::from(block_number),
                    slot: offset,
                });
            }
            if locations.len() == page_start {
                pg_sys::GenericXLogAbort(state);
                pg_sys::UnlockReleaseBuffer(buffer);
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to append HNSW {label} record to a fresh index page"),
                );
            }
            hnsw_physical_failpoint(5, "before_generic_xlog_finish");
            registered.seal().finish();
            pg_sys::UnlockReleaseBuffer(buffer);
            hnsw_physical_failpoint(6, "after_generic_xlog_finish");
        }
    }
    locations
}

fn ensure_hnsw_page_record_fits(payload: &[u8], label: &str) {
    if payload.len() <= HNSW_MAX_PAGE_RECORD_BYTES {
        return;
    }
    raise_sql_error(
        PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
        format!(
            "HNSW {label} record exceeds single-page storage limit: {} bytes (maximum {HNSW_MAX_PAGE_RECORD_BYTES}); reduce vector dimensions or hnsw_m",
            payload.len()
        ),
    );
}

unsafe fn try_append_hnsw_typed_record(
    index_relation: pg_sys::Relation,
    block_number: pg_sys::BlockNumber,
    payload: &[u8],
    kind: GraphPageKind,
    generation: u64,
) -> Option<HnswPageItemLocation> {
    let mode = if block_number == pg_sys::InvalidBlockNumber {
        pg_sys::ReadBufferMode::RBM_ZERO_AND_LOCK
    } else {
        pg_sys::ReadBufferMode::RBM_NORMAL
    };
    // SAFETY: PostgreSQL owns the relation pointer, and `ReadBufferExtended`
    // returns a buffer pinned for this backend until explicitly released below.
    let buffer = unsafe {
        pg_sys::ReadBufferExtended(
            index_relation,
            pg_sys::ForkNumber::MAIN_FORKNUM,
            block_number,
            mode,
            ptr::null_mut(),
        )
    };
    // SAFETY: The buffer is pinned. Existing pages are explicitly locked before
    // mutation; fresh pages from `RBM_ZERO_AND_LOCK` are already locked. The
    // append itself targets the Generic-WAL shadow page, never the live page.
    unsafe {
        if block_number != pg_sys::InvalidBlockNumber {
            pg_sys::LockBuffer(buffer, pg_sys::BUFFER_LOCK_EXCLUSIVE.cast_signed());
        }
        let state = pg_sys::GenericXLogStart(index_relation);
        let registered =
            wal_contract::critical_section::HnswWalRegisteredSinglePage::register(
            state,
            buffer,
            pg_sys::GENERIC_XLOG_FULL_IMAGE.cast_signed(),
        );
        let page = registered.page();
        if block_number == pg_sys::InvalidBlockNumber || pg_sys::PageIsNew(page) {
            hnsw_physical_failpoint(1, "before_page_initialization");
            let page_id = u64::from(pg_sys::BufferGetBlockNumber(buffer));
            initialize_hnsw_data_page(page, page_id, kind, generation);
            hnsw_physical_failpoint(2, "after_page_initialization");
        } else if !page_accepts_generation(page, generation) {
            // This page belongs to another base generation, so a record placed
            // on it would inherit that stamp and be skipped by every reader —
            // a silently lost row rather than a visible failure.
            //
            // Reachable because the page chosen for reuse is simply the last
            // page of this kind, and a crashed compaction leaves its orphaned
            // fresh base at exactly the end of the relation. Declining sends
            // the caller to the fresh-page path, which stamps correctly.
            pg_sys::GenericXLogAbort(state);
            pg_sys::UnlockReleaseBuffer(buffer);
            return None;
        }
        hnsw_physical_failpoint(3, "before_append");
        let offset = pg_sys::PageAddItemExtended(
            page,
            payload.as_ptr().cast_mut().cast(),
            payload.len() as pg_sys::Size,
            HNSW_INVALID_OFFSET,
            0,
        );
        if offset == HNSW_INVALID_OFFSET {
            pg_sys::GenericXLogAbort(state);
            pg_sys::UnlockReleaseBuffer(buffer);
            return None;
        }
        hnsw_physical_failpoint(4, "after_append");
        let location = HnswPageItemLocation {
            page: u64::from(pg_sys::BufferGetBlockNumber(buffer)),
            slot: offset,
        };
        hnsw_physical_failpoint(5, "before_generic_xlog_finish");
        let finish_permit = registered.seal();
        finish_permit.finish();
        pg_sys::UnlockReleaseBuffer(buffer);
        hnsw_physical_failpoint(6, "after_generic_xlog_finish");
        Some(location)
    }
}

unsafe fn read_hnsw_vector_records(index_relation: pg_sys::Relation) -> Vec<HnswVectorRecord> {
    // SAFETY: PostgreSQL relation metadata is valid for this AM callback.
    let block_count = unsafe {
        pg_sys::RelationGetNumberOfBlocksInFork(index_relation, pg_sys::ForkNumber::MAIN_FORKNUM)
    };
    if block_count <= HNSW_FIRST_VECTOR_BLOCK {
        return Vec::new();
    }

    // The live base starts where the metapage says, not at block 1: a
    // compacted relation still physically holds the base it superseded, and
    // reading that too would duplicate every node. Derived here rather than
    // passed in so no caller can read a superseded region by omission.
    // `meta()` releases its buffer before returning, so no block-0 lock is
    // held while the base is scanned below.
    // SAFETY: the caller owns a live index relation whose metapage was
    // initialized before any typed page could be appended.
    let meta = unsafe { PgHnswGraphRead::new(index_relation).meta() };
    let base_start = block_number_from_u64(meta.base_scan_start(), "HNSW base start block");
    if block_count <= base_start {
        return Vec::new();
    }

    let mut records = BTreeMap::new();
    let mut adjacency = BTreeMap::new();
    for block_number in base_start..block_count {
        // SAFETY: PostgreSQL owns the relation pointer and returns a pinned
        // buffer for the requested block until it is released below.
        let buffer = unsafe {
            pg_sys::ReadBufferExtended(
                index_relation,
                pg_sys::ForkNumber::MAIN_FORKNUM,
                block_number,
                pg_sys::ReadBufferMode::RBM_NORMAL,
                ptr::null_mut(),
            )
        };
        // SAFETY: The buffer is pinned and locked shared while every checked
        // page item is copied into Rust-owned bytes. Decoding occurs only
        // after the buffer lock and pin have been released.
        let page_items = unsafe {
            pg_sys::LockBuffer(buffer, pg_sys::BUFFER_LOCK_SHARE.cast_signed());
            let page = pg_sys::BufferGetPage(buffer);
            if pg_sys::PageIsNew(page) {
                pg_sys::UnlockReleaseBuffer(buffer);
                continue;
            }
            let max_offset = pg_sys::PageGetMaxOffsetNumber(page);
            if max_offset < HNSW_FIRST_VECTOR_RECORD_OFFSET {
                pg_sys::UnlockReleaseBuffer(buffer);
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                    "HNSW vector page has no typed header and record region",
                );
            }
            let mut page_items = Vec::with_capacity(usize::from(max_offset));
            for offset in HNSW_FIRST_OFFSET..=max_offset {
                match copy_hnsw_page_item(page, offset) {
                    Ok(item) => page_items.push(item),
                    Err(error) => {
                        pg_sys::UnlockReleaseBuffer(buffer);
                        raise_sql_error(PgSqlErrorCode::ERRCODE_DATA_CORRUPTED, error);
                    }
                }
            }
            pg_sys::UnlockReleaseBuffer(buffer);
            page_items
        };
        let header = match decode_page_header(&page_items[0]) {
            Ok(header) => header,
            Err(error) => {
                raise_sql_error(PgSqlErrorCode::ERRCODE_DATA_CORRUPTED, error.to_string());
            }
        };
        let expected_page_id = u64::from(block_number);
        if header.page_id != expected_page_id {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                "HNSW vector page header does not match its physical node page",
            );
        }
        // Pages stamped for another base generation are physically present but
        // not part of the live graph: a compaction writes its whole fresh base
        // before publishing it, so between the write and the metapage flip —
        // and forever, if a crash lands in that window — those pages sit inside
        // the range scanned here. They are skipped rather than treated as
        // corruption, because their presence is the normal, expected outcome of
        // an interrupted compaction and the live graph is unaffected.
        //
        // Without this the newer base silently overwrites the live one by node
        // id, which is a wrong-results bug for any reader running concurrently
        // with a compaction, not only after a crash.
        if header.generation != meta.base_generation {
            continue;
        }
        for item in page_items.iter().skip(1) {
            match header.kind {
                GraphPageKind::Node => {
                    // SAFETY: the decoder receives the complete owned item.
                    let record = unsafe {
                        decode_hnsw_vector_record(item.as_ptr().cast_mut(), item.len())
                    };
                    records.insert(record.node_id.get(), record);
                }
                GraphPageKind::Adjacency => {
                    // SAFETY: the decoder receives the complete owned item.
                    let record = unsafe {
                        decode_hnsw_adjacency_record(item.as_ptr().cast_mut(), item.len())
                    };
                    // Newer append-only revisions follow older records in
                    // page order; retain the latest complete layer.
                    adjacency.insert(
                        (record.node_id.get(), record.layer.get()),
                        record.neighbors,
                    );
                }
                GraphPageKind::Directory => {
                    decode_hnsw_directory_record(item).unwrap_or_else(|reason| {
                        raise_sql_error(PgSqlErrorCode::ERRCODE_DATA_CORRUPTED, reason);
                    });
                }
                _ => {}
            }
        }
    }
    for ((node_id, layer), neighbors) in adjacency {
        let Some(record) = records.get_mut(&node_id) else {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                "HNSW adjacency record references a missing node record",
            );
        };
        let Some(expected) = record.layers.get(layer) else {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                "HNSW adjacency record references a missing node layer",
            );
        };
        if expected != &neighbors {
            // Node records carry a complete self-contained hierarchy snapshot.
            // An immediate crash can preserve a later node revision before its
            // separately append-only adjacency mirror; retain the validated
            // node snapshot and ignore that stale mirror until its replacement
            // reaches WAL on a later retry.
            continue;
        }
    }
    let present_node_ids = records.keys().copied().collect::<BTreeSet<_>>();
    for record in records.values_mut() {
        for neighbors in &mut record.layers {
            neighbors.retain(|neighbor| present_node_ids.contains(&neighbor.get()));
        }
        record.base_neighbors = record.layers.first().cloned().unwrap_or_default();
    }
    records.into_values().collect()
}
