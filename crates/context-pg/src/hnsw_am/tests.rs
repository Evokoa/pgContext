use super::*;

#[test]
fn scan_counters_reject_values_above_postgresql_bigint() {
    let Ok(maximum) = usize::try_from(i64::MAX) else {
        return;
    };
    assert_eq!(try_scan_counter_to_sql(maximum), Ok(i64::MAX));
    assert_eq!(try_scan_counter_to_sql(maximum + 1), Err(maximum + 1));
}
use crate::hnsw_am::vacuum::{
    HnswVacuumStats, hnsw_vacuum_stats_from_parts, write_hnsw_vacuum_stats,
};
use context_index::{GraphPageId, LayerIndex};

#[test]
fn hnsw_inventory_has_every_access_method_callback_contract() {
    assert_eq!(
        callback_contract::HNSW_CALLBACK_CONTRACTS
            .iter()
            .filter(|contract| {
                contract.class == callback_contract::HnswCallbackClass::AccessMethod
            })
            .count(),
        14
    );
}

#[test]
fn callback_allocation_and_rescan_bounds_fail_closed() {
    assert_eq!(callback_allocation_bytes::<u64>(3), Some(24));
    assert_eq!(callback_allocation_bytes::<u64>(usize::MAX), None);
    assert_eq!(
        bounded_scan_count(MAX_HNSW_SCAN_KEYS, MAX_HNSW_SCAN_KEYS),
        Some(MAX_HNSW_SCAN_KEYS)
    );
    assert_eq!(
        bounded_scan_count(MAX_HNSW_SCAN_KEYS + 1, MAX_HNSW_SCAN_KEYS),
        None
    );
    assert_eq!(
        bounded_scan_count(MAX_HNSW_SCAN_ORDERBYS, MAX_HNSW_SCAN_ORDERBYS),
        Some(MAX_HNSW_SCAN_ORDERBYS)
    );
    assert_eq!(
        bounded_scan_count(MAX_HNSW_SCAN_ORDERBYS + 1, MAX_HNSW_SCAN_ORDERBYS),
        None
    );
    assert_eq!(bounded_rescan_count(2, 2), Some(2));
    assert_eq!(bounded_rescan_count(3, 2), None);
}

#[test]
fn hnsw_page_item_ranges_reject_corrupt_line_pointers() {
    let header = offset_of!(pg_sys::PageHeaderData, pd_linp);
    let line_pointer_end = header + size_of::<pg_sys::ItemIdData>();
    let page_bytes = pg_sys::BLCKSZ as usize;
    let lower = line_pointer_end;
    let upper = page_bytes - 256;
    let special = page_bytes;

    assert_eq!(
        checked_hnsw_item_range(
            lower,
            upper,
            special,
            line_pointer_end,
            pg_sys::LP_NORMAL,
            upper,
            32,
        ),
        Ok((upper, upper + 32))
    );
    assert!(
        checked_hnsw_item_range(
            lower,
            upper,
            special,
            line_pointer_end,
            pg_sys::LP_DEAD,
            upper,
            32,
        )
        .is_err()
    );
    assert!(
        checked_hnsw_item_range(
            lower,
            upper,
            special,
            line_pointer_end,
            pg_sys::LP_NORMAL,
            upper,
            0,
        )
        .is_err()
    );
    assert!(
        checked_hnsw_item_range(
            lower,
            upper,
            special,
            line_pointer_end,
            pg_sys::LP_NORMAL,
            upper - 1,
            32,
        )
        .is_err()
    );
    assert!(
        checked_hnsw_item_range(
            lower,
            upper,
            special,
            line_pointer_end,
            pg_sys::LP_NORMAL,
            special - 16,
            32,
        )
        .is_err()
    );
    assert!(
        checked_hnsw_item_range(
            lower,
            upper,
            special,
            line_pointer_end,
            pg_sys::LP_NORMAL,
            usize::MAX,
            2,
        )
        .is_err()
    );
    assert!(
        checked_hnsw_item_range(
            lower,
            upper,
            special,
            lower + size_of::<pg_sys::ItemIdData>(),
            pg_sys::LP_NORMAL,
            upper,
            32,
        )
        .is_err()
    );
}

#[test]
#[allow(
    clippy::panic,
    reason = "fixture constants must fit PostgreSQL page fields"
)]
fn hnsw_page_item_copy_fails_closed_on_a_corrupt_physical_line_pointer() {
    #[repr(align(16))]
    struct AlignedPage([u8; pg_sys::BLCKSZ as usize]);

    let mut storage = AlignedPage([0; pg_sys::BLCKSZ as usize]);
    let page = storage.0.as_mut_ptr().cast::<i8>();
    let header_bytes = offset_of!(pg_sys::PageHeaderData, pd_linp);
    let lower = header_bytes + size_of::<pg_sys::ItemIdData>();
    let item_offset = pg_sys::BLCKSZ as usize - 32;
    let lower = match u16::try_from(lower) {
        Ok(value) => value,
        Err(_) => panic!("fixture line-pointer boundary must fit LocationIndex"),
    };
    let item_offset_u16 = match u16::try_from(item_offset) {
        Ok(value) => value,
        Err(_) => panic!("fixture item offset must fit LocationIndex"),
    };
    let item_offset_u32 = match u32::try_from(item_offset) {
        Ok(value) => value,
        Err(_) => panic!("fixture item offset must fit the line pointer"),
    };
    let page_bytes = match u16::try_from(pg_sys::BLCKSZ) {
        Ok(value) => value,
        Err(_) => panic!("PostgreSQL page size must fit LocationIndex"),
    };
    // SAFETY: the aligned fixture owns one BLCKSZ allocation. Every header,
    // line-pointer, and payload write below stays within that allocation.
    unsafe {
        let header = page.cast::<pg_sys::PageHeaderData>();
        header.write(pg_sys::PageHeaderData::default());
        (*header).pd_lower = lower;
        (*header).pd_upper = item_offset_u16;
        (*header).pd_special = page_bytes;

        let item_id = storage
            .0
            .as_mut_ptr()
            .add(header_bytes)
            .cast::<pg_sys::ItemIdData>();
        item_id.write(pg_sys::ItemIdData::default());
        (*item_id).set_lp_off(item_offset_u32);
        (*item_id).set_lp_len(32);
        (*item_id).set_lp_flags(pg_sys::LP_NORMAL);
        storage.0[item_offset..].fill(0x5a);

        assert_eq!(
            copy_hnsw_page_item(page, HNSW_FIRST_OFFSET),
            Ok(vec![0x5a; 32])
        );

        (*item_id).set_lp_flags(pg_sys::LP_DEAD);
        assert!(copy_hnsw_page_item(page, HNSW_FIRST_OFFSET).is_err());

        (*item_id).set_lp_flags(pg_sys::LP_NORMAL);
        assert!(copy_hnsw_page_item(page, 2).is_err());
    }
}

#[test]
fn hnsw_insert_lock_keys_are_namespaced_per_index() {
    assert_ne!(hnsw_insert_lock_key(41), hnsw_insert_lock_key(42));
    assert_eq!(hnsw_insert_lock_key(41), (0x5047_4358, 41));
    assert_eq!(hnsw_insert_lock_key(u32::MAX).1.cast_unsigned(), u32::MAX);
}

#[test]
fn hnsw_scan_state_rescan_discards_position_and_candidates() {
    let mut state = HnswScanState {
        prepared: true,
        position: 1,
        candidate_limit: 2,
        candidates: vec![
            HnswScanCandidate {
                heap_tid: 10,
                score: 1.0,
            },
            HnswScanCandidate {
                heap_tid: 20,
                score: 2.0,
            },
        ],
        returned_heap_tids: BTreeSet::new(),
        work: HnswScanWork::default(),
    };

    assert_eq!(state.next().map(|candidate| candidate.heap_tid), Some(20));
    state.reset();

    assert!(!state.prepared);
    assert_eq!(state.position, 0);
    assert!(state.candidates.is_empty());
    assert!(state.next().is_none());
}

#[test]
fn hnsw_routine_explicitly_rejects_mark_restore() {
    let routine = hnsw_index_am_routine();
    assert!(routine.ammarkpos.is_none());
    assert!(routine.amrestrpos.is_none());
}

#[test]
fn physical_failpoint_registry_names_every_wal_boundary() {
    let points = [
        HnswPhysicalFailpoint::BeforePageInitialization,
        HnswPhysicalFailpoint::AfterPageInitialization,
        HnswPhysicalFailpoint::BeforeAppend,
        HnswPhysicalFailpoint::AfterAppend,
        HnswPhysicalFailpoint::BeforeGenericXLogFinish,
        HnswPhysicalFailpoint::AfterGenericXLogFinish,
        HnswPhysicalFailpoint::BeforeMetapagePublication,
        HnswPhysicalFailpoint::AfterMetapagePublication,
        HnswPhysicalFailpoint::BeforeRewiring,
        HnswPhysicalFailpoint::AfterRewiring,
        HnswPhysicalFailpoint::BeforeDeltaAppend,
        HnswPhysicalFailpoint::AfterDeltaAppend,
        HnswPhysicalFailpoint::BeforeCompactionWrite,
        HnswPhysicalFailpoint::AfterCompactionWrite,
        HnswPhysicalFailpoint::AfterCompactionPublish,
    ];
    for point in points {
        hnsw_set_physical_failpoint(Some(point));
        assert_eq!(HNSW_PHYSICAL_FAILPOINT.load(Ordering::SeqCst), point as u8);
    }
    hnsw_set_physical_failpoint(None);
    assert_eq!(HNSW_PHYSICAL_FAILPOINT.load(Ordering::SeqCst), 0);
}

#[test]
fn rescan_key_copy_stages_an_aliasing_source_before_writing() {
    let mut key = pg_sys::ScanKeyData {
        sk_strategy: 7,
        ..pg_sys::ScanKeyData::default()
    };
    // SAFETY: The test scope does not outlive this stack frame.
    let scope = unsafe { PgCallbackScope::new() };
    // SAFETY: `key` is initialized and live for one readable element. The
    // destination intentionally aliases it to exercise stack staging.
    let source = unsafe { scope.borrow_slice(ptr::addr_of_mut!(key), 1, "fixture key") };
    // SAFETY: `key` is also writable, and copy_rescan_keys ends its source
    // reference before writing the stack-owned copy to the same address.
    unsafe { copy_rescan_keys(source, ptr::addr_of_mut!(key), "fixture key") };

    assert_eq!(key.sk_strategy, 7);
}

#[test]
fn hnsw_wal_page_api_is_visible_to_the_physical_adapter_boundary()
-> Result<(), Box<dyn std::error::Error>> {
    use super::wal_contract::{HnswWalPageAction, HnswWalPageSet};

    let action = HnswWalPageAction::append_node(GraphPageId::new(1), 1, HnswNodeId::new(7))?;
    let pages = HnswWalPageSet::new(&[action])?;

    assert_eq!(pages.len(), 1);
    assert_eq!(
        pages
            .iter()
            .map(HnswWalPageAction::page_id)
            .collect::<Vec<_>>(),
        vec![GraphPageId::new(1)]
    );
    Ok(())
}

#[test]
fn hnsw_vacuum_stats_report_page_and_heap_estimates() {
    let stats = hnsw_vacuum_stats_from_parts(7, 42.0, 3.0);

    assert_eq!(
        stats,
        HnswVacuumStats {
            num_pages: 7,
            estimated_count: true,
            num_index_tuples: 42.0,
            tuples_removed: 3.0,
            pages_newly_deleted: 0,
            pages_deleted: 0,
            pages_free: 0,
        }
    );
}

#[test]
fn hnsw_vacuum_stats_write_to_postgres_result_shape() {
    let mut target = pg_sys::IndexBulkDeleteResult::default();

    write_hnsw_vacuum_stats(&mut target, hnsw_vacuum_stats_from_parts(1, 2.0, 0.0));

    assert_eq!(target.num_pages, 1);
    assert!(target.estimated_count);
    assert_eq!(target.num_index_tuples, 2.0);
    assert_eq!(target.tuples_removed, 0.0);
    assert_eq!(target.pages_newly_deleted, 0);
    assert_eq!(target.pages_deleted, 0);
    assert_eq!(target.pages_free, 0);
}

#[test]
fn hnsw_scan_cost_ratio_tracks_graph_work_instead_of_using_a_fixed_cost() {
    let small = hnsw_scan_tuple_ratio(1_000.0, 16, 40);
    let large = hnsw_scan_tuple_ratio(1_000_000.0, 16, 40);
    let wider_search = hnsw_scan_tuple_ratio(1_000_000.0, 16, 400);

    assert!((0.0..=1.0).contains(&small));
    assert!((0.0..small).contains(&large));
    assert!(wider_search > large);
}

#[test]
fn hnsw_metapage_records_empty_build_state() {
    let mut meta = HnswMetaPage::empty();

    meta.record_build(None, 0, None);

    assert!(meta.is_valid());
    assert_eq!(meta.dimensions, 0);
    assert_eq!(meta.graph_nodes, 0);
}

#[test]
fn hnsw_metapage_advances_the_directory_cache_epoch_on_every_publication() {
    let mut meta = HnswMetaPage::empty();
    assert_eq!(meta.directory_epoch, 0);

    meta.record_build(Some(2), 1, Some(HnswNodeId::new(0)));
    assert_eq!(meta.directory_epoch, 1);
    let inserted = meta.record_insert(2, Some(HnswNodeId::new(0)));
    assert_eq!(inserted, HnswNodeId::new(1));
    assert_eq!(meta.directory_epoch, 2);
    meta.record_directory_mutation();
    assert_eq!(meta.directory_epoch, 3);
}

#[test]
fn hnsw_metapage_records_build_dimensions_and_nodes() {
    let mut meta = HnswMetaPage::empty();

    meta.record_build(Some(3), 42, Some(HnswNodeId::new(0)));

    assert_eq!(meta.dimensions, 3);
    assert_eq!(meta.graph_nodes, 42);
}

#[test]
fn hnsw_metapage_rejects_future_quantization_metadata_version() {
    let mut meta = HnswMetaPage::empty();

    meta.quantization_metadata_version = options::HNSW_QUANTIZATION_METADATA_VERSION + 1;

    assert!(!meta.is_valid());
}

#[test]
fn hnsw_metapage_insert_preserves_existing_dimensions() {
    let mut meta = HnswMetaPage::empty();
    meta.record_build(Some(3), 2, Some(HnswNodeId::new(0)));

    let node_id = meta.record_insert(3, Some(HnswNodeId::new(0)));

    assert_eq!(node_id, HnswNodeId::new(2));
    assert_eq!(meta.dimensions, 3);
    assert_eq!(meta.graph_nodes, 3);
}

#[test]
#[should_panic]
fn hnsw_metapage_insert_rejects_dimension_mismatch() {
    let mut meta = HnswMetaPage::empty();
    meta.record_build(Some(3), 2, Some(HnswNodeId::new(0)));

    meta.record_insert(9, Some(HnswNodeId::new(0)));
}

#[test]
fn hnsw_insert_node_ids_are_bounded_by_page_record_storage() {
    assert_eq!(
        checked_hnsw_node_id_from_graph_count(42),
        Ok(HnswNodeId::new(42))
    );

    let overflow = u64::from(u32::MAX) + 1;

    assert_eq!(
        checked_hnsw_node_id_from_graph_count(overflow),
        Err(overflow)
    );
}

#[test]
fn hnsw_vector_record_round_trips_heap_tid_and_values() -> context_core::Result<()> {
    let record = HnswVectorRecord {
        node_id: HnswNodeId::new(3),
        heap_tid: 17,
        vector: DenseVector::new(vec![1.0, 2.5, 4.0])?,
        base_neighbors: vec![HnswNodeId::new(1), HnswNodeId::new(2)],
        layers: Vec::new(),
    };

    let payload = encode_hnsw_vector_record(&record);
    // SAFETY: The payload was produced by `encode_hnsw_vector_record` and
    // remains alive for the duration of the decode.
    let decoded = unsafe { decode_hnsw_vector_record(payload.as_ptr(), payload.len()) };

    assert_eq!(decoded.node_id, HnswNodeId::new(3));
    assert_eq!(decoded.heap_tid, 17);
    assert_eq!(decoded.vector.as_slice(), &[1.0, 2.5, 4.0]);
    assert_eq!(
        decoded.base_neighbors,
        vec![HnswNodeId::new(1), HnswNodeId::new(2)]
    );
    Ok(())
}

#[test]
fn hnsw_vector_record_round_trips_each_hierarchy_layer() -> context_core::Result<()> {
    let layers = vec![
        vec![HnswNodeId::new(1), HnswNodeId::new(2)],
        vec![HnswNodeId::new(4)],
        Vec::new(),
    ];
    let record = HnswVectorRecord {
        node_id: HnswNodeId::new(3),
        heap_tid: 17,
        vector: DenseVector::new(vec![1.0, 2.5, 4.0])?,
        base_neighbors: layers[0].clone(),
        layers: layers.clone(),
    };

    let payload = encode_hnsw_vector_record(&record);
    // SAFETY: The payload was produced by `encode_hnsw_vector_record` and
    // remains alive for the duration of the decode.
    let decoded = unsafe { decode_hnsw_vector_record(payload.as_ptr(), payload.len()) };

    assert_eq!(decoded.layers, layers);
    assert_eq!(decoded.base_neighbors, decoded.layers[0]);
    Ok(())
}

#[test]
fn hnsw_adjacency_record_round_trips_a_complete_layer() {
    let record = HnswAdjacencyRecord {
        node_id: HnswNodeId::new(3),
        layer: LayerIndex::new(2),
        neighbors: vec![HnswNodeId::new(1), HnswNodeId::new(4)],
    };

    let payload = encode_hnsw_adjacency_record(&record);
    // SAFETY: The payload was produced by the paired encoder and remains live.
    let decoded = unsafe { decode_hnsw_adjacency_record(payload.as_ptr(), payload.len()) };

    assert_eq!(decoded, record);
}
