#[pg_test]
fn table_search_executor_matches_legacy_exact_rows_byte_for_byte() {
    create_search_collection("stage_b_executor_parity");
    upsert_search_points("stage_b_executor_parity", &["10", "20", "30"]);

    let (legacy, executor) = crate::retrieval::differential_exact_rows_for_test(
        "stage_b_executor_parity".to_owned(),
        crate::Vector::from_validated_values(vec![0.0, 0.0]),
        3,
    );

    assert_eq!(executor, legacy);
    assert_eq!(
        executor
            .iter()
            .map(|row| (row.1.as_str(), row.2.to_bits()))
            .collect::<Vec<_>>(),
        vec![
            ("20", 1.0_f32.to_bits()),
            ("30", 2.0_f32.to_bits()),
            ("10", 3.0_f32.to_bits())
        ]
    );
}

#[pg_test]
fn table_search_postgres_adapters_obey_bounded_executor_contracts() {
    create_filter_search_collection("stage_b_adapter_contracts");
    upsert_search_points("stage_b_adapter_contracts", &["10", "20", "30", "40"]);
    Spi::run(
        "SELECT pgcontext.register_filter_column(
            'stage_b_adapter_contracts',
            'tenant_id',
            'tenant_id'
        )",
    )
    .expect("tenant filter should be registered");

    let snapshot = crate::retrieval::adapter_conformance_snapshot_for_test(
        "stage_b_adapter_contracts".to_owned(),
    );

    assert_eq!(snapshot.exact_rows, snapshot.hnsw_rows);
    assert_eq!(
        snapshot
            .exact_rows
            .iter()
            .map(|row| (row.1.as_str(), row.2.to_bits()))
            .collect::<Vec<_>>(),
        vec![("40", 0.5_f32.to_bits()), ("30", 2.0_f32.to_bits())]
    );
    assert_eq!(snapshot.filter_candidates, 3);
    assert_eq!(snapshot.exact_candidates, 2);
    assert_eq!(snapshot.hnsw_candidates, 3);
    assert_eq!(snapshot.exact_rechecks, 2);
    assert_eq!(snapshot.hnsw_rechecks, 3);
}
