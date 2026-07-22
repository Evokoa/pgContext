//! Serving-cache, revision, and telemetry unit tests split from
//! `tests.rs` to keep both under the source-hygiene size target.

use super::*;
use crate::hnsw_am::bitmap::{
    checked_hnsw_bitmap_tid_count, hnsw_bitmap_tid_count, hnsw_bitmap_tids,
};

#[test]
#[allow(clippy::panic)]
fn hnsw_directory_record_round_trips_a_versioned_locator() {
    let record = HnswDirectoryRecord {
        key_kind: GraphDirectoryKeyKind::Adjacency,
        generation: 7,
        identity: 42,
        ordinal: 2,
        target_page: 19,
        target_slot: 4,
        revision: 11,
    };
    let Ok(decoded) = decode_hnsw_directory_record(&encode_hnsw_directory_record(record)) else {
        panic!("generated directory locator must decode");
    };
    assert_eq!(decoded, record);
}

#[test]
fn hnsw_directory_index_prefers_the_latest_node_locator() {
    let mut directory = HnswDirectoryIndex::default();
    let first = HnswDirectoryRecord {
        key_kind: GraphDirectoryKeyKind::Node,
        generation: 1,
        identity: 42,
        ordinal: 0,
        target_page: 7,
        target_slot: 2,
        revision: 3,
    };
    let newer = HnswDirectoryRecord {
        target_page: 9,
        target_slot: 4,
        revision: 4,
        ..first
    };
    let stale = HnswDirectoryRecord {
        target_page: 11,
        revision: 2,
        ..first
    };

    directory.observe(first);
    directory.observe(newer);
    directory.observe(stale);

    assert_eq!(directory.node(HnswNodeId::new(42)), Some(newer));
}

#[test]
#[should_panic]
fn hnsw_vector_record_rejects_truncated_neighbor_payload() {
    let record = HnswVectorRecord {
        node_id: HnswNodeId::new(1),
        heap_tid: 17,
        vector: valid_test_vector(vec![1.0, 2.5]),
        base_neighbors: vec![HnswNodeId::new(0)],
        layers: Vec::new(),
    };
    let payload = encode_hnsw_vector_record(&record);

    // SAFETY: The pointer is valid, but the length intentionally truncates the
    // encoded neighbor payload to verify defensive decode behavior.
    let _ = unsafe { decode_hnsw_vector_record(payload.as_ptr(), payload.len() - 1) };
}

#[test]
fn hnsw_graph_from_records_uses_persisted_neighbors() -> Result<(), Box<dyn std::error::Error>> {
    let records = vec![
        HnswVectorRecord {
            node_id: HnswNodeId::new(0),
            heap_tid: 10,
            vector: DenseVector::new(vec![0.0, 0.0])?,
            base_neighbors: vec![HnswNodeId::new(1)],
            layers: Vec::new(),
        },
        HnswVectorRecord {
            node_id: HnswNodeId::new(1),
            heap_tid: 20,
            vector: DenseVector::new(vec![1.0, 0.0])?,
            base_neighbors: vec![HnswNodeId::new(0)],
            layers: Vec::new(),
        },
    ];

    let graph = hnsw_graph_from_records_with_config(
        records,
        DistanceMetric::L2,
        test_hnsw_config()?,
        Some(HnswNodeId::new(0)),
    );

    assert_eq!(
        graph.neighbors(HnswNodeId::new(0), LayerIndex::base()),
        Some(&[HnswNodeId::new(1)][..])
    );
    assert_eq!(
        graph.neighbors(HnswNodeId::new(1), LayerIndex::base()),
        Some(&[HnswNodeId::new(0)][..])
    );
    Ok(())
}

#[test]
fn hnsw_unordered_scan_preserves_visible_append_order() -> Result<(), Box<dyn std::error::Error>> {
    let records = vec![
        HnswVectorRecord {
            node_id: HnswNodeId::new(0),
            heap_tid: 10,
            vector: DenseVector::new(vec![100.0, 0.0])?,
            base_neighbors: vec![HnswNodeId::new(1)],
            layers: Vec::new(),
        },
        HnswVectorRecord {
            node_id: HnswNodeId::new(1),
            heap_tid: 20,
            vector: DenseVector::new(vec![90.0, 0.0])?,
            base_neighbors: vec![HnswNodeId::new(0), HnswNodeId::new(2)],
            layers: Vec::new(),
        },
        HnswVectorRecord {
            node_id: HnswNodeId::new(2),
            heap_tid: 30,
            vector: DenseVector::new(vec![0.0, 0.0])?,
            base_neighbors: vec![HnswNodeId::new(1), HnswNodeId::new(3)],
            layers: Vec::new(),
        },
        HnswVectorRecord {
            node_id: HnswNodeId::new(3),
            heap_tid: 40,
            vector: DenseVector::new(vec![1.0, 0.0])?,
            base_neighbors: vec![HnswNodeId::new(2)],
            layers: Vec::new(),
        },
    ];
    let candidates = hnsw_unordered_scan_candidates(records);

    assert_eq!(
        candidates
            .iter()
            .map(|candidate| candidate.heap_tid)
            .collect::<Vec<_>>(),
        vec![10, 20, 30, 40]
    );
    Ok(())
}

#[test]
fn hnsw_unordered_scan_does_not_rank_disconnected_graph() -> Result<(), Box<dyn std::error::Error>>
{
    let records = vec![
        HnswVectorRecord {
            node_id: HnswNodeId::new(0),
            heap_tid: 10,
            vector: DenseVector::new(vec![100.0, 0.0])?,
            base_neighbors: vec![HnswNodeId::new(1)],
            layers: Vec::new(),
        },
        HnswVectorRecord {
            node_id: HnswNodeId::new(1),
            heap_tid: 20,
            vector: DenseVector::new(vec![90.0, 0.0])?,
            base_neighbors: vec![HnswNodeId::new(0)],
            layers: Vec::new(),
        },
        HnswVectorRecord {
            node_id: HnswNodeId::new(2),
            heap_tid: 30,
            vector: DenseVector::new(vec![0.0, 0.0])?,
            base_neighbors: vec![HnswNodeId::new(3)],
            layers: Vec::new(),
        },
        HnswVectorRecord {
            node_id: HnswNodeId::new(3),
            heap_tid: 40,
            vector: DenseVector::new(vec![1.0, 0.0])?,
            base_neighbors: vec![HnswNodeId::new(2)],
            layers: Vec::new(),
        },
    ];
    let candidates = hnsw_unordered_scan_candidates(records);

    assert_eq!(
        candidates
            .iter()
            .map(|candidate| candidate.heap_tid)
            .collect::<Vec<_>>(),
        vec![10, 20, 30, 40]
    );
    Ok(())
}

#[test]
fn hnsw_unordered_scan_does_not_rank_incremental_records() -> Result<(), Box<dyn std::error::Error>>
{
    let records = vec![
        HnswVectorRecord {
            node_id: HnswNodeId::new(0),
            heap_tid: 10,
            vector: DenseVector::new(vec![100.0, 0.0])?,
            base_neighbors: Vec::new(),
            layers: Vec::new(),
        },
        HnswVectorRecord {
            node_id: HnswNodeId::new(1),
            heap_tid: 20,
            vector: DenseVector::new(vec![0.0, 0.0])?,
            base_neighbors: Vec::new(),
            layers: Vec::new(),
        },
    ];
    let candidates = hnsw_unordered_scan_candidates(records);

    assert_eq!(
        candidates
            .iter()
            .map(|candidate| candidate.heap_tid)
            .collect::<Vec<_>>(),
        vec![10, 20]
    );
    Ok(())
}

#[test]
fn hnsw_unordered_scan_does_not_rank_mixed_append_only_records()
-> Result<(), Box<dyn std::error::Error>> {
    let records = vec![
        HnswVectorRecord {
            node_id: HnswNodeId::new(0),
            heap_tid: 10,
            vector: DenseVector::new(vec![100.0, 0.0])?,
            base_neighbors: vec![HnswNodeId::new(1)],
            layers: Vec::new(),
        },
        HnswVectorRecord {
            node_id: HnswNodeId::new(1),
            heap_tid: 20,
            vector: DenseVector::new(vec![90.0, 0.0])?,
            base_neighbors: vec![HnswNodeId::new(0)],
            layers: Vec::new(),
        },
        HnswVectorRecord {
            node_id: HnswNodeId::new(2),
            heap_tid: 30,
            vector: DenseVector::new(vec![0.0, 0.0])?,
            base_neighbors: Vec::new(),
            layers: Vec::new(),
        },
    ];
    let candidates = hnsw_unordered_scan_candidates(records);

    assert_eq!(
        candidates
            .iter()
            .map(|candidate| candidate.heap_tid)
            .collect::<Vec<_>>(),
        vec![10, 20, 30]
    );
    Ok(())
}

#[test]
fn hnsw_graph_from_records_rejects_corrupt_neighbor_ids() -> Result<(), Box<dyn std::error::Error>>
{
    let records = vec![HnswVectorRecord {
        node_id: HnswNodeId::new(0),
        heap_tid: 10,
        vector: valid_test_vector(vec![0.0, 0.0]),
        base_neighbors: vec![HnswNodeId::new(99)],
        layers: Vec::new(),
    }];

    assert!(matches!(
        try_hnsw_graph_from_records_with_config(
            records,
            DistanceMetric::L2,
            test_hnsw_config()?,
            Some(HnswNodeId::new(0)),
        ),
        Err(HnswError::InvalidParameter {
            parameter: "neighbor_id",
            value: 99,
        })
    ));
    Ok(())
}

#[test]
fn tombstone_snapshots_use_unique_traversal_only_point_ids() {
    let first = hnsw_tombstone_record(&HnswVectorRecord {
        node_id: HnswNodeId::new(0),
        heap_tid: 42,
        vector: valid_test_vector(vec![0.0, 0.0]),
        base_neighbors: vec![HnswNodeId::new(1)],
        layers: Vec::new(),
    });
    let second = hnsw_tombstone_record(&HnswVectorRecord {
        node_id: HnswNodeId::new(1),
        heap_tid: 42,
        vector: valid_test_vector(vec![1.0, 1.0]),
        base_neighbors: vec![HnswNodeId::new(0)],
        layers: Vec::new(),
    });

    let first = hnsw_graph_snapshot_from_record(first);
    let second = hnsw_graph_snapshot_from_record(second);

    assert_ne!(first.point_id(), second.point_id());
    assert!(hnsw_point_id_is_tombstoned(first.point_id()));
    assert!(hnsw_point_id_is_tombstoned(second.point_id()));
}

fn test_hnsw_config() -> context_index::Result<HnswConfig> {
    HnswConfig::new(
        context_core::policy::DEFAULT_HNSW_M,
        context_core::policy::DEFAULT_HNSW_EF_CONSTRUCTION,
        context_core::policy::DEFAULT_HNSW_EF_SEARCH,
    )
}

#[test]
fn hnsw_bitmap_tids_preserve_candidate_heap_tids() {
    let candidates = vec![
        HnswScanCandidate {
            heap_tid: 1,
            score: 0.0,
        },
        HnswScanCandidate {
            heap_tid: 42,
            score: 1.0,
        },
    ];

    let tids = hnsw_bitmap_tids(&candidates);
    let round_tripped = tids
        .iter()
        .copied()
        .map(item_pointer_to_u64)
        .collect::<Vec<_>>();

    assert_eq!(round_tripped, vec![1, 42]);
}

#[test]
fn hnsw_bitmap_tids_return_empty_vector_for_empty_candidates() {
    let tids = hnsw_bitmap_tids(&[]);

    assert!(tids.is_empty());
    assert_eq!(hnsw_bitmap_tid_count(tids.len()), 0);
}

#[test]
fn checked_hnsw_bitmap_tid_count_rejects_overflow() {
    let overflow = usize::try_from(i64::from(std::ffi::c_int::MAX) + 1).unwrap_or(usize::MAX);

    assert_eq!(checked_hnsw_bitmap_tid_count(overflow), Err(overflow));
}

#[test]
fn hnsw_metric_metapage_identity_round_trips_every_dense_metric() -> context_index::Result<()> {
    let config = HnswConfig::new(7, 31, 13)?;
    let cases = [
        (HnswScoreMetric::L2, DistanceMetric::L2, 1_u16),
        (
            HnswScoreMetric::NegativeInnerProduct,
            DistanceMetric::NegativeInnerProduct,
            2,
        ),
        (
            HnswScoreMetric::Cosine,
            DistanceMetric::NegativeInnerProduct,
            3,
        ),
        (HnswScoreMetric::L1, DistanceMetric::L1, 4),
        (HnswScoreMetric::BitHamming, DistanceMetric::Hamming, 5),
        (HnswScoreMetric::BitJaccard, DistanceMetric::Jaccard, 6),
    ];

    for (score_metric, graph_metric, storage_tag) in cases {
        let mut meta = HnswMetaPage::empty();
        meta.record_index_identity(score_metric, config);

        assert_eq!(score_metric.navigation_metric(), graph_metric);
        assert_eq!(score_metric.storage_tag(), storage_tag);
        assert_eq!(
            meta.stored_config(score_metric, 19),
            HnswConfig::new(7, 31, 19)?
        );
    }

    Ok(())
}

#[test]
fn bit_jaccard_orderby_lower_bound_covers_float4_navigation_collisions() {
    // This 8,001-bit pair is valid under the SQL type policy and pins the
    // precision hazard for a future multi-page or bit-native graph record.
    let row_a_exact = 3.0_f64 / 8_000.0;
    let row_b_exact = 3.0_f64 / 8_001.0;
    let row_a_navigation = 1.0_f32 - (7_997.0_f32 / 8_000.0_f32);
    let row_b_navigation = 1.0_f32 - (7_998.0_f32 / 8_001.0_f32);

    assert_eq!(row_a_navigation, row_b_navigation);
    assert!(row_b_exact < row_a_exact);
    let (row_a_bound, row_a_recheck) =
        float8_orderby_distance(HnswScoreMetric::BitJaccard, row_a_navigation);
    let (row_b_bound, row_b_recheck) =
        float8_orderby_distance(HnswScoreMetric::BitJaccard, row_b_navigation);
    assert!(row_a_recheck);
    assert!(row_b_recheck);
    assert!(row_a_bound <= row_a_exact);
    assert!(row_b_bound <= row_b_exact);

    assert_eq!(
        float8_orderby_distance(HnswScoreMetric::L2, row_a_navigation),
        (f64::from(row_a_navigation), false)
    );
}

#[test]
fn hnsw_metric_graph_reload_uses_selected_metric() -> context_index::Result<()> {
    let records = vec![
        HnswVectorRecord {
            node_id: HnswNodeId::new(0),
            heap_tid: 10,
            vector: DenseVector::new(vec![1.0, 0.0]).map_err(HnswError::from)?,
            base_neighbors: vec![HnswNodeId::new(1)],
            layers: vec![vec![HnswNodeId::new(1)]],
        },
        HnswVectorRecord {
            node_id: HnswNodeId::new(1),
            heap_tid: 20,
            vector: DenseVector::new(vec![3.0, 0.0]).map_err(HnswError::from)?,
            base_neighbors: vec![HnswNodeId::new(0)],
            layers: vec![vec![HnswNodeId::new(0)]],
        },
    ];
    let graph = try_hnsw_graph_from_records_with_config(
        records,
        DistanceMetric::NegativeInnerProduct,
        HnswConfig::new(2, 4, 4)?,
        Some(HnswNodeId::new(0)),
    )?;
    let query = DenseVector::new(vec![1.0, 0.0]).map_err(HnswError::from)?;
    let results = graph.search(&query, SearchLimit::new(2).map_err(HnswError::from)?)?;

    assert_eq!(
        results
            .iter()
            .map(|result| result.point_id().get())
            .collect::<Vec<_>>(),
        vec![20, 10]
    );
    Ok(())
}

#[allow(
    clippy::expect_used,
    reason = "test fixture vectors are fixed finite non-empty values"
)]
fn valid_test_vector(values: Vec<f32>) -> DenseVector {
    DenseVector::new(values).expect("fixed test vector should be valid")
}

#[test]
fn serving_stats_record_builds_and_reuses() {
    let baseline = hnsw_serving_stats_snapshot();
    record_hnsw_pack_build(1024, 7);
    record_hnsw_pack_reuse();
    record_hnsw_pack_reuse();
    record_hnsw_pack_build(2048, 3);
    let stats = hnsw_serving_stats_snapshot();
    assert_eq!(stats.pack_builds, baseline.pack_builds + 2);
    assert_eq!(stats.pack_reuses, baseline.pack_reuses + 2);
    assert_eq!(stats.last_pack_bytes, 2048);
    assert_eq!(stats.last_pack_millis, 3);
    assert_eq!(stats.total_pack_millis, baseline.total_pack_millis + 10);
}

#[test]
fn serving_stats_saturate_instead_of_overflowing() {
    HNSW_SERVING_STATS.with(|stats| {
        stats.set(HnswServingStats {
            pack_builds: u64::MAX,
            pack_reuses: u64::MAX,
            last_pack_bytes: 0,
            last_pack_millis: 0,
            total_pack_millis: u64::MAX,
            shared_attaches: 0,
            shared_publishes: 0,
            shared_publish_skips: 0,
            mapped_attaches: 0,
            mapped_publishes: 0,
            mapped_publish_skips: 0,
            page_native_fallbacks: 0,
            delta_segment_records: 0,
            delta_segment_scans: 0,
        });
    });
    record_hnsw_pack_build(1, 1);
    record_hnsw_pack_reuse();
    let stats = hnsw_serving_stats_snapshot();
    assert_eq!(stats.pack_builds, u64::MAX);
    assert_eq!(stats.pack_reuses, u64::MAX);
    assert_eq!(stats.total_pack_millis, u64::MAX);
    HNSW_SERVING_STATS.with(|stats| stats.set(HnswServingStats::default()));
}

#[test]
fn build_profile_records_latest_build_phases() {
    record_hnsw_build_profile(HnswBuildProfile {
        tuples: 10,
        graph_millis: 100,
        write_millis: 40,
    });
    record_hnsw_build_profile(HnswBuildProfile {
        tuples: 20,
        graph_millis: 7,
        write_millis: 3,
    });
    let profile = hnsw_build_profile_snapshot();
    assert_eq!(profile.tuples, 20);
    assert_eq!(profile.graph_millis, 7);
    assert_eq!(profile.write_millis, 3);
}

#[test]
fn directory_index_keeps_the_latest_revision_per_node() {
    let mut directory = HnswDirectoryIndex::default();
    let record = |revision: u64| HnswDirectoryRecord {
        key_kind: GraphDirectoryKeyKind::Node,
        generation: 1,
        identity: 7,
        ordinal: 0,
        target_page: 0,
        target_slot: 0,
        revision,
    };
    directory.observe(record(5));
    directory.observe(record(2));
    directory.observe(record(9));
    assert_eq!(
        directory
            .node(HnswNodeId::new(7))
            .map(|record| record.revision),
        Some(9)
    );
}
