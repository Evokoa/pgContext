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
    assert!(snapshot.exact_complete);
    assert!(snapshot.hnsw_complete);
    assert!(snapshot.hnsw_work_candidates > 0);
    assert!(snapshot.hnsw_work_candidates <= 4);
}

#[pg_test]
fn hnsw_adapter_binds_attached_index_for_every_dense_metric() {
    let cases = [
        ("l2", "l2", "vector_hnsw_ops"),
        ("ip", "inner_product", "vector_hnsw_ip_ops"),
        ("cosine", "cosine", "vector_hnsw_cosine_ops"),
        ("l1", "l1", "vector_hnsw_l1_ops"),
    ];

    for (suffix, metric, opclass) in cases {
        let collection = format!("stage_b_hnsw_{suffix}");
        create_dense_hnsw_adapter_collection(&collection, metric, opclass);
        let snapshot =
            crate::retrieval::dense_metric_adapter_snapshot_for_test(collection.clone());

        assert_eq!(snapshot.hnsw_rows, snapshot.exact_rows, "metric {metric}");
        assert!(snapshot.exact_complete, "metric {metric}");
        assert!(snapshot.hnsw_complete, "metric {metric}");
        assert!(snapshot.hnsw_work_candidates > 0, "metric {metric}");
        assert!(snapshot.hnsw_work_candidates <= 3, "metric {metric}");
    }
}

#[pg_test]
#[should_panic(expected = "max_candidate_budget 2 exceeded")]
fn hnsw_adapter_enforces_strict_collection_candidate_budget() {
    create_dense_hnsw_adapter_collection(
        "stage_b_hnsw_strict_budget",
        "l2",
        "vector_hnsw_ops",
    );
    Spi::run(
        "SELECT pgcontext.configure_collection_limits(
            'stage_b_hnsw_strict_budget',
            true,
            NULL,
            NULL,
            NULL,
            NULL,
            NULL,
            2,
            NULL,
            NULL
        )",
    )
    .expect("strict candidate budget should configure");

    crate::retrieval::run_hnsw_for_test("stage_b_hnsw_strict_budget".to_owned());
}

#[pg_test]
#[should_panic(expected = "query source is not ready: GenerationMissing")]
fn hnsw_adapter_reports_missing_index_as_not_ready() {
    create_search_collection("stage_b_hnsw_not_ready");
    upsert_search_points("stage_b_hnsw_not_ready", &["10", "20", "30"]);

    crate::retrieval::run_hnsw_for_test("stage_b_hnsw_not_ready".to_owned());
}

fn create_dense_hnsw_adapter_collection(collection: &str, metric: &str, opclass: &str) {
    Spi::run(&format!(
        "CREATE TABLE public.{collection} (
             id bigint PRIMARY KEY,
             embedding vector NOT NULL
         );
         INSERT INTO public.{collection} (id, embedding)
         VALUES (10, '[1,0]'::vector),
                (20, '[0.5,0.5]'::vector),
                (30, '[0,1]'::vector);
         SELECT pgcontext.create_collection('{collection}', 'public.{collection}');
         SELECT pgcontext.register_vector(
             '{collection}',
             'embedding',
             'embedding',
             2,
             '{metric}'
         );
         SELECT pgcontext.upsert_points('{collection}', ARRAY['10', '20', '30']);
         CREATE INDEX {collection}_hnsw_idx
             ON public.{collection}
             USING pgcontext_hnsw (embedding pgcontext.{opclass});
         SELECT pgcontext.attach_hnsw_index(
             '{collection}',
             'embedding',
             'public.{collection}_hnsw_idx'
         )"
    ))
    .expect("dense HNSW adapter collection should be created");
}
