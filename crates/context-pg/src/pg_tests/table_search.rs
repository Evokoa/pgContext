#[pg_test]
fn table_search_returns_registered_points_by_distance() {
    create_search_collection("m2_search_docs");
    upsert_search_points("m2_search_docs", &["10", "20", "30"]);

    let rows = table_search_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.search('m2_search_docs', '[0,0]'::vector, 2)",
    );

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].1, "20");
    assert_eq!(rows[0].2, 1.0);
    assert_eq!(rows[1].1, "30");
    assert_eq!(rows[1].2, 2.0);
}

#[pg_test]
fn table_search_excludes_deleted_points() {
    create_search_collection("m2_search_deleted");
    upsert_search_points("m2_search_deleted", &["10", "20", "30"]);
    Spi::run("SELECT pgcontext.delete_points('m2_search_deleted', ARRAY['20'])")
        .expect("point should be deleted");

    let rows = table_search_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.search('m2_search_deleted', '[0,0]'::vector, 3)",
    );

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].1, "30");
    assert_eq!(rows[0].2, 2.0);
    assert_eq!(rows[1].1, "10");
    assert_eq!(rows[1].2, 3.0);
}

#[pg_test]
fn table_search_filter_first_candidates_restrict_exact_scores() {
    create_filter_search_collection("m8_filter_search_docs");
    upsert_search_points("m8_filter_search_docs", &["10", "20", "30", "40"]);
    Spi::run(
        "SELECT pgcontext.register_filter_column(
            'm8_filter_search_docs',
            'tenant_id',
            'tenant_id'
        )",
    )
    .expect("tenant filter should be registered");
    Spi::run(
        "SELECT pgcontext.register_filter_column('m8_filter_search_docs', 'status', 'status')",
    )
    .expect("status filter should be registered");

    let rows = table_search_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.search(
                'm8_filter_search_docs',
                '[0,0]'::vector,
                '{\"must\":[
                    {\"key\":\"tenant_id\",\"match\":\"acme\"},
                    {\"key\":\"status\",\"match\":\"open\"}
                ]}',
                3
           )",
    );

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].1, "40");
    assert_eq!(rows[0].2, 0.5);
    assert_eq!(rows[1].1, "10");
    assert_eq!(rows[1].2, 3.0);
}

#[pg_test]
fn table_search_filtered_path_adaptively_uses_exact_for_a_small_mask() {
    create_filter_search_collection("m8_filter_mask_docs");
    upsert_search_points("m8_filter_mask_docs", &["10", "20", "30", "40"]);
    Spi::run(
        "SELECT pgcontext.register_filter_column(
            'm8_filter_mask_docs',
            'tenant_id',
            'tenant_id'
        )",
    )
    .expect("tenant filter should be registered");

    let rows = table_search_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.search(
                'm8_filter_mask_docs',
                '[0,0]'::vector,
                '{\"must\":[{\"key\":\"tenant_id\",\"match\":\"acme\"}]}',
                3
           )",
    );
    let work = Spi::get_one::<String>(
        "SELECT format('%s,%s', candidates, exact_strategy)
           FROM pgcontext.hnsw_last_scan_work()",
    )
    .expect("masked HNSW scan work should load")
    .expect("masked HNSW scan work should be present");

    assert_eq!(rows.len(), 3);
    assert_eq!(
        rows.iter()
            .map(|(_, source_key, _)| source_key.as_str())
            .collect::<Vec<_>>(),
        vec!["40", "30", "10"]
    );
    assert_eq!(work, "3,t");
}

#[pg_test]
fn table_search_filtered_path_avoids_repeated_small_mask_batches() {
    create_filter_search_collection("m8_filter_iterative_docs");
    upsert_search_points("m8_filter_iterative_docs", &["10", "20", "30", "40"]);
    Spi::run(
        "SELECT pgcontext.register_filter_column(
            'm8_filter_iterative_docs',
            'tenant_id',
            'tenant_id'
        )",
    )
    .expect("tenant filter should be registered");
    Spi::run("SET LOCAL pgcontext.hnsw_candidate_budget = 2")
        .expect("candidate budget should be accepted");
    Spi::run("SET LOCAL pgcontext.hnsw_iterative_expansion_limit = 4")
        .expect("iterative expansion limit should be accepted");

    let rows = table_search_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.search(
                'm8_filter_iterative_docs',
                '[0,0]'::vector,
                '{\"must\":[{\"key\":\"tenant_id\",\"match\":\"acme\"}]}',
                2
           )",
    );
    let work = Spi::get_one::<String>(
        "SELECT format('%s,%s', candidates, exact_strategy)
           FROM pgcontext.hnsw_last_scan_work()",
    )
    .expect("iterative masked HNSW scan work should load")
    .expect("iterative masked HNSW scan work should be present");

    assert_eq!(
        rows.iter()
            .map(|(_, source_key, _)| source_key.as_str())
            .collect::<Vec<_>>(),
        vec!["40", "30"]
    );
    assert_eq!(work, "3,t");
}

#[pg_test]
fn table_search_filtered_path_excludes_deleted_masked_points() {
    create_filter_search_collection("m8_filter_deleted_docs");
    upsert_search_points("m8_filter_deleted_docs", &["10", "20", "30", "40"]);
    Spi::run(
        "SELECT pgcontext.register_filter_column(
            'm8_filter_deleted_docs',
            'tenant_id',
            'tenant_id'
        )",
    )
    .expect("tenant filter should be registered");
    Spi::run("SELECT pgcontext.delete_points('m8_filter_deleted_docs', ARRAY['40'])")
        .expect("nearest masked point should be deleted");

    let rows = table_search_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.search(
                'm8_filter_deleted_docs',
                '[0,0]'::vector,
                '{\"must\":[{\"key\":\"tenant_id\",\"match\":\"acme\"}]}',
                3
           )",
    );

    assert_eq!(
        rows.iter()
            .map(|(_, source_key, _)| source_key.as_str())
            .collect::<Vec<_>>(),
        vec!["30", "10"]
    );
}

#[pg_test]
fn table_search_filtered_path_uses_current_rows_after_stats_go_stale() {
    create_filter_search_collection("m8_filter_stale_stats_docs");
    upsert_search_points("m8_filter_stale_stats_docs", &["10", "20", "30", "40"]);
    Spi::run(
        "SELECT pgcontext.register_filter_column(
            'm8_filter_stale_stats_docs',
            'tenant_id',
            'tenant_id'
        )",
    )
    .expect("tenant filter should be registered");
    Spi::run("ANALYZE public.m8_filter_stale_stats_docs")
        .expect("source table should be analyzed before stale-stat mutation");
    Spi::run("UPDATE public.m8_filter_stale_stats_docs SET tenant_id = 'other' WHERE id = 40")
        .expect("source row should change after statistics are collected");

    let rows = table_search_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.search(
                'm8_filter_stale_stats_docs',
                '[0,0]'::vector,
                '{\"must\":[{\"key\":\"tenant_id\",\"match\":\"acme\"}]}',
                3
           )",
    );

    assert_eq!(
        rows.iter()
            .map(|(_, source_key, _)| source_key.as_str())
            .collect::<Vec<_>>(),
        vec!["30", "10"]
    );
}

#[pg_test]
fn table_search_filtered_path_supports_typed_jsonb_paths() {
    Spi::run(
        "CREATE TABLE public.m8_filter_jsonb_docs (
             id bigint PRIMARY KEY,
             embedding vector NOT NULL,
             metadata jsonb NOT NULL
         )",
    )
    .expect("JSONB filtered source table should be created");
    Spi::run(
        "INSERT INTO public.m8_filter_jsonb_docs (id, embedding, metadata)
         VALUES (10, '[3,0]'::vector, '{\"topic\":\"rust\",\"priority\":1,\"archived\":false}'::jsonb),
                (20, '[1,0]'::vector, '{\"topic\":\"postgres\",\"priority\":2,\"archived\":false}'::jsonb),
                (30, '[2,0]'::vector, '{\"topic\":\"rust\",\"priority\":3,\"archived\":false}'::jsonb),
                (40, '[0.5,0]'::vector, '{\"topic\":\"rust\",\"priority\":2,\"archived\":true}'::jsonb)",
    )
    .expect("JSONB filtered source rows should be inserted");
    Spi::run(
        "SELECT pgcontext.create_collection(
            'm8_filter_jsonb_docs',
            'public.m8_filter_jsonb_docs'
        )",
    )
    .expect("JSONB filtered collection should be created");
    Spi::run(
        "SELECT pgcontext.register_vector(
            'm8_filter_jsonb_docs',
            'embedding',
            'embedding',
            2,
            'l2'
        )",
    )
    .expect("JSONB filtered vector should be registered");
    Spi::run(
        "CREATE INDEX m8_filter_jsonb_docs_hnsw_idx
            ON public.m8_filter_jsonb_docs USING pgcontext_hnsw (embedding)",
    )
    .expect("JSONB filtered HNSW index should be created");
    Spi::run(
        "SELECT pgcontext.attach_hnsw_index(
            'm8_filter_jsonb_docs',
            'embedding',
            'public.m8_filter_jsonb_docs_hnsw_idx'
        )",
    )
    .expect("JSONB filtered HNSW index should attach");
    for (filter_key, jsonb_key) in [
        ("metadata_topic", "topic"),
        ("metadata_priority", "priority"),
        ("metadata_archived", "archived"),
    ] {
        Spi::run(&format!(
            "SELECT pgcontext.register_jsonb_path(
                'm8_filter_jsonb_docs',
                '{filter_key}',
                'metadata',
                ARRAY['{jsonb_key}']
            )"
        ))
        .expect("JSONB path should be registered");
    }
    upsert_search_points("m8_filter_jsonb_docs", &["10", "20", "30", "40"]);

    let rows = table_search_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.search(
                'm8_filter_jsonb_docs',
                '[0,0]'::vector,
                '{\"must\":[
                    {\"key\":\"metadata_topic\",\"match\":\"rust\"},
                    {\"key\":\"metadata_priority\",\"range\":{\"gte\":2,\"lt\":4}},
                    {\"key\":\"metadata_archived\",\"match\":false}
                ]}',
                3
           )",
    );

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].1, "30");
    assert_eq!(rows[0].2, 2.0);
}

#[pg_test]
fn table_search_filter_first_candidates_return_empty_when_filter_matches_none() {
    create_filter_search_collection("m8_filter_search_empty");
    upsert_search_points("m8_filter_search_empty", &["10", "20", "30", "40"]);
    Spi::run(
        "SELECT pgcontext.register_filter_column('m8_filter_search_empty', 'tenant_id', 'tenant_id')",
    )
    .expect("tenant filter should be registered");

    let rows = table_search_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.search(
                'm8_filter_search_empty',
                '[0,0]'::vector,
                '{\"must\":[{\"key\":\"tenant_id\",\"match\":\"missing\"}]}',
                3
           )",
    );

    assert!(rows.is_empty());
}

#[pg_test]
fn table_search_filtered_without_hnsw_rechecks_exact_source_rows() {
    Spi::run(
        "CREATE TABLE public.m8_filtered_exact_docs (
             id bigint PRIMARY KEY,
             embedding vector NOT NULL,
             tenant text NOT NULL,
             status text NOT NULL
         )",
    )
    .expect("exact filtered source table should be created");
    Spi::run(
        "INSERT INTO public.m8_filtered_exact_docs (id, embedding, tenant, status)
         VALUES (10, '[3,0]'::vector, 'acme', 'open'),
                (20, '[1,0]'::vector, 'other', 'open'),
                (30, '[2,0]'::vector, 'acme', 'closed'),
                (40, '[0.5,0]'::vector, 'acme', 'open')",
    )
    .expect("exact filtered source rows should be inserted");
    Spi::run(
        "SELECT pgcontext.create_collection(
            'm8_filtered_exact_docs',
            'public.m8_filtered_exact_docs'
        )",
    )
    .expect("exact filtered collection should be created");
    Spi::run(
        "SELECT pgcontext.register_vector(
            'm8_filtered_exact_docs',
            'embedding',
            'embedding',
            2,
            'l2'
        )",
    )
    .expect("exact filtered vector should be registered");
    Spi::run(
        "SELECT pgcontext.register_filter_column('m8_filtered_exact_docs', 'tenant', 'tenant')",
    )
    .expect("exact tenant filter should be registered");
    Spi::run(
        "SELECT pgcontext.register_filter_column('m8_filtered_exact_docs', 'status', 'status')",
    )
    .expect("exact status filter should be registered");
    upsert_search_points("m8_filtered_exact_docs", &["10", "20", "30", "40"]);

    let rows = table_search_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.search(
                'm8_filtered_exact_docs',
                '[0,0]'::vector,
                '{\"must\":[
                    {\"key\":\"tenant\",\"match\":\"acme\"},
                    {\"key\":\"status\",\"match\":\"open\"}
                ]}',
                3
           )",
    );

    assert_eq!(
        rows.iter()
            .map(|(_, source_key, _)| source_key.as_str())
            .collect::<Vec<_>>(),
        vec!["40", "10"]
    );
    assert_eq!(rows[0].2, 0.5);
    assert_eq!(rows[1].2, 3.0);
}

#[pg_test]
fn table_search_rechecks_candidate_point_batch_before_scoring() {
    create_search_collection("m8_recheck_docs");
    upsert_search_points("m8_recheck_docs", &["10", "20", "30"]);

    let rows = table_search_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.search(
                'm8_recheck_docs',
                '[0,0]'::vector,
                (
                    SELECT array_agg(point_id ORDER BY point_id)
                      FROM pgcontext._collection_points
                     WHERE source_key IN ('10', '20')
                ),
                10
           )",
    );

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].1, "20");
    assert_eq!(rows[0].2, 1.0);
    assert_eq!(rows[1].1, "10");
    assert_eq!(rows[1].2, 3.0);
}

#[pg_test]
fn table_search_recheck_candidate_batch_excludes_deleted_points() {
    create_search_collection("m8_recheck_deleted");
    upsert_search_points("m8_recheck_deleted", &["10", "20", "30"]);
    Spi::run("SELECT pgcontext.delete_points('m8_recheck_deleted', ARRAY['20'])")
        .expect("candidate point should be deleted");

    let rows = table_search_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.search(
                'm8_recheck_deleted',
                '[0,0]'::vector,
                (
                    SELECT array_agg(point_id ORDER BY point_id)
                      FROM pgcontext._collection_points
                     WHERE source_key IN ('20', '30')
                ),
                10
           )",
    );

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].1, "30");
    assert_eq!(rows[0].2, 2.0);
}

#[pg_test]
fn table_search_recheck_candidate_batch_applies_shared_filter_plan() {
    create_filter_search_collection("m8_recheck_filter");
    upsert_search_points("m8_recheck_filter", &["10", "20", "30", "40"]);
    Spi::run(
        "SELECT pgcontext.register_filter_column(
            'm8_recheck_filter',
            'tenant_id',
            'tenant_id'
        )",
    )
    .expect("tenant filter should be registered");
    Spi::run("SELECT pgcontext.register_filter_column('m8_recheck_filter', 'status', 'status')")
        .expect("status filter should be registered");

    let rows = table_search_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.search(
                'm8_recheck_filter',
                '[0,0]'::vector,
                '{\"must\":[
                    {\"key\":\"tenant_id\",\"match\":\"acme\"},
                    {\"key\":\"status\",\"match\":\"open\"}
                ]}',
                (
                    SELECT array_agg(point_id ORDER BY point_id)
                      FROM pgcontext._collection_points
                     WHERE source_key IN ('10', '20', '30', '40')
                ),
                10
           )",
    );

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].1, "40");
    assert_eq!(rows[0].2, 0.5);
    assert_eq!(rows[1].1, "10");
    assert_eq!(rows[1].2, 3.0);
}

#[pg_test]
#[should_panic(expected = "unknown filter field: priority")]
fn table_search_recheck_candidate_batch_rejects_unregistered_filter_fields() {
    create_filter_search_collection("m8_recheck_unknown_filter");
    upsert_search_points("m8_recheck_unknown_filter", &["10"]);

    Spi::run(
        "SELECT pgcontext.search(
            'm8_recheck_unknown_filter',
            '[0,0]'::vector,
            '{\"must\":[{\"key\":\"priority\",\"match\":\"high\"}]}',
            (
                SELECT array_agg(point_id ORDER BY point_id)
                  FROM pgcontext._collection_points
                 WHERE source_key = '10'
            ),
            10
        )",
    )
    .expect("unregistered recheck filter should be rejected");
}

#[pg_test]
fn table_search_filter_first_supports_partitioned_source_tables() {
    Spi::run(
        "CREATE TABLE public.m8_partition_search_docs (
             id bigint NOT NULL,
             embedding vector NOT NULL,
             tenant_id text NOT NULL,
             PRIMARY KEY (tenant_id, id)
         ) PARTITION BY LIST (tenant_id)",
    )
    .expect("partitioned search source table should be created");
    Spi::run(
        "CREATE TABLE public.m8_partition_search_docs_acme
            PARTITION OF public.m8_partition_search_docs FOR VALUES IN ('acme')",
    )
    .expect("acme search partition should be created");
    Spi::run(
        "CREATE TABLE public.m8_partition_search_docs_other
            PARTITION OF public.m8_partition_search_docs FOR VALUES IN ('other')",
    )
    .expect("other search partition should be created");
    Spi::run(
        "INSERT INTO public.m8_partition_search_docs (id, embedding, tenant_id)
         VALUES (10, '[3,0]'::vector, 'acme'),
                (20, '[1,0]'::vector, 'other'),
                (30, '[2,0]'::vector, 'acme')",
    )
    .expect("partitioned search rows should be inserted");
    Spi::run(
        "SELECT pgcontext.create_collection(
            'm8_partition_search_docs',
            'public.m8_partition_search_docs'
        )",
    )
    .expect("partitioned search collection should be created");
    Spi::run(
        "SELECT pgcontext.register_vector(
            'm8_partition_search_docs',
            'embedding',
            'embedding',
            2,
            'l2'
        )",
    )
    .expect("partitioned search vector should be registered");
    Spi::run(
        "SELECT pgcontext.register_filter_column(
            'm8_partition_search_docs',
            'tenant_id',
            'tenant_id'
        )",
    )
    .expect("partition filter should be registered");
    upsert_search_points("m8_partition_search_docs", &["10", "20", "30"]);

    let rows = table_search_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.search(
                'm8_partition_search_docs',
                '[0,0]'::vector,
                '{\"must\":[{\"key\":\"tenant_id\",\"match\":\"acme\"}]}',
                10
           )",
    );

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].1, "30");
    assert_eq!(rows[0].2, 2.0);
    assert_eq!(rows[1].1, "10");
    assert_eq!(rows[1].2, 3.0);
}

#[pg_test]
fn table_search_medium_fixture_matches_exact_array_oracle_for_l2() {
    create_medium_search_collection("m2_medium_l2", "l2");

    assert_table_search_matches_exact_oracle("m2_medium_l2", "[0,0]", "l2", 5);
}

#[pg_test]
fn table_search_medium_fixture_matches_exact_array_oracle_for_inner_product() {
    create_medium_search_collection("m2_medium_inner_product", "inner_product");

    assert_table_search_matches_exact_oracle(
        "m2_medium_inner_product",
        "[1,1]",
        "inner_product",
        5,
    );
}

#[pg_test]
fn table_search_medium_fixture_matches_exact_array_oracle_for_l1() {
    create_medium_search_collection("m2_medium_l1", "l1");

    assert_table_search_matches_exact_oracle("m2_medium_l1", "[0,0]", "l1", 5);
}

#[pg_test]
fn table_search_cosine_fixture_matches_exact_array_oracle() {
    create_cosine_search_collection("m2_medium_cosine");

    let table_rows = table_search_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.search('m2_medium_cosine', '[1,0]'::vector, 5)",
    );
    let oracle_rows = exact_oracle_rows("[1,0]", "cosine", 5, &cosine_exact_candidates());

    assert_source_key_scores_match_exact_oracle(&table_rows, &oracle_rows);
}

#[pg_test]
fn table_search_edge_fixture_preserves_score_then_point_order_ties() {
    Spi::run(
        "CREATE TABLE public.m2_edge_ties (
             id bigint PRIMARY KEY,
             embedding vector NOT NULL
         )",
    )
    .expect("edge search source table should be created");
    Spi::run(
        "INSERT INTO public.m2_edge_ties (id, embedding)
         VALUES (30, '[1,0]'::vector),
                (10, '[1,0]'::vector),
                (20, '[1,0]'::vector),
                (40, '[2,0]'::vector)",
    )
    .expect("edge search rows should be inserted");
    Spi::run("SELECT pgcontext.create_collection('m2_edge_ties', 'public.m2_edge_ties')")
        .expect("edge search collection should be created");
    Spi::run("SELECT pgcontext.register_vector('m2_edge_ties', 'embedding', 'embedding', 2, 'l2')")
        .expect("edge search vector should be registered");
    upsert_search_points("m2_edge_ties", &["10", "20", "30", "40"]);

    let rows = table_search_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.search('m2_edge_ties', '[0,0]'::vector, 4)",
    );

    let source_keys = rows
        .iter()
        .map(|(_, source_key, _)| source_key.as_str())
        .collect::<Vec<_>>();
    assert_eq!(source_keys, vec!["10", "20", "30", "40"]);
    assert_eq!(rows[0].2, 1.0);
    assert_eq!(rows[1].2, 1.0);
    assert_eq!(rows[2].2, 1.0);
    assert_eq!(rows[3].2, 2.0);
}

#[pg_test]
fn table_search_selects_named_dense_vector() {
    create_named_vector_search_collection("m13_named_dense_docs");
    upsert_search_points("m13_named_dense_docs", &["10", "20", "30"]);

    let title_rows = table_search_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.search('m13_named_dense_docs', 'title', '[0,0]'::vector, 3)",
    );
    let body_rows = table_search_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.search('m13_named_dense_docs', 'body', '[0,0]'::vector, 3)",
    );

    assert_eq!(
        title_rows
            .iter()
            .map(|(_, source_key, _)| source_key.as_str())
            .collect::<Vec<_>>(),
        vec!["20", "30", "10"]
    );
    assert_eq!(
        body_rows
            .iter()
            .map(|(_, source_key, _)| source_key.as_str())
            .collect::<Vec<_>>(),
        vec!["30", "10", "20"]
    );
    assert_eq!(title_rows[0].2, 1.0);
    assert_eq!(body_rows[0].2, 1.0);

    Spi::run(
        "SELECT pgcontext.register_filter_column(
            'm13_named_dense_docs',
            'tenant_id',
            'tenant_id'
        )",
    )
    .expect("named vector tenant filter should be registered");

    let filtered_rows = table_search_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.search(
                'm13_named_dense_docs',
                'body',
                '[0,0]'::vector,
                '{\"must\":[{\"key\":\"tenant_id\",\"match\":\"acme\"}]}',
                3
           )",
    );
    assert_eq!(
        filtered_rows
            .iter()
            .map(|(_, source_key, _)| source_key.as_str())
            .collect::<Vec<_>>(),
        vec!["30", "10"]
    );

    let candidate_rows = table_search_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.search(
                'm13_named_dense_docs',
                'body',
                '[0,0]'::vector,
                (
                    SELECT array_agg(point_id ORDER BY point_id)
                      FROM pgcontext._collection_points
                     WHERE source_key IN ('10', '20')
                ),
                3
           )",
    );
    assert_eq!(
        candidate_rows
            .iter()
            .map(|(_, source_key, _)| source_key.as_str())
            .collect::<Vec<_>>(),
        vec!["10", "20"]
    );
}

#[pg_test]
#[should_panic(expected = "collection has multiple registered vectors; specify a vector name")]
fn table_search_rejects_ambiguous_dense_vector_selection() {
    create_named_vector_search_collection("m13_named_dense_ambiguous");
    upsert_search_points("m13_named_dense_ambiguous", &["10"]);

    Spi::run("SELECT pgcontext.search('m13_named_dense_ambiguous', '[0,0]'::vector, 1)")
        .expect("ambiguous named vector search should fail");
}

#[pg_test]
#[should_panic(expected = "registered vector does not exist for collection m13_named_dense_missing: missing")]
fn table_search_rejects_unknown_named_dense_vector() {
    create_named_vector_search_collection("m13_named_dense_missing");
    upsert_search_points("m13_named_dense_missing", &["10"]);

    Spi::run(
        "SELECT pgcontext.search('m13_named_dense_missing', 'missing', '[0,0]'::vector, 1)",
    )
    .expect("unknown named vector search should fail");
}

#[pg_test]
#[should_panic(expected = "registered vector column drifted: m2_search_drift.embedding")]
fn table_search_rejects_dropped_vector_column_drift() {
    create_search_collection("m2_search_drift");
    upsert_search_points("m2_search_drift", &["10"]);
    Spi::run("ALTER TABLE public.m2_search_drift DROP COLUMN embedding")
        .expect("vector column should be dropped");

    Spi::run("SELECT pgcontext.search('m2_search_drift', '[0,0]'::vector, 1)")
        .expect("search should reject drifted vector column");
}

#[pg_test]
#[should_panic(expected = "registered source table drifted: public.m2_search_table_drift")]
fn table_search_rejects_dropped_source_table_drift() {
    create_search_collection("m2_search_table_drift");
    upsert_search_points("m2_search_table_drift", &["10"]);
    Spi::run("DROP TABLE public.m2_search_table_drift").expect("source table should be dropped");

    Spi::run("SELECT pgcontext.search('m2_search_table_drift', '[0,0]'::vector, 1)")
        .expect("search should reject drifted source table");
}

#[pg_test]
#[should_panic(expected = "source key column does not exist on public.m2_search_key_drift: id")]
fn table_search_rejects_dropped_source_key_column_drift() {
    create_search_collection("m2_search_key_drift");
    upsert_search_points("m2_search_key_drift", &["10"]);
    Spi::run("ALTER TABLE public.m2_search_key_drift DROP COLUMN id")
        .expect("source key column should be dropped");

    Spi::run("SELECT pgcontext.search('m2_search_key_drift', '[0,0]'::vector, 1)")
        .expect("search should reject drifted source key column");
}

fn create_search_collection(collection_name: &str) {
    Spi::run(&format!(
        "CREATE TABLE public.{collection_name} (
             id bigint PRIMARY KEY,
             embedding pgcontext.vector NOT NULL
         )"
    ))
    .expect("search source table should be created");
    Spi::run(&format!(
        "INSERT INTO public.{collection_name} (id, embedding)
         VALUES (10, '[3,0]'::pgcontext.vector),
                (20, '[1,0]'::pgcontext.vector),
                (30, '[2,0]'::pgcontext.vector)"
    ))
    .expect("search source rows should be inserted");
    Spi::run(&format!(
        "SELECT pgcontext.create_collection('{collection_name}', 'public.{collection_name}')"
    ))
    .expect("search collection should be created");
    Spi::run(&format!(
        "SELECT pgcontext.register_vector('{collection_name}', 'embedding', 'embedding', 2, 'l2')"
    ))
    .expect("search vector should be registered");
}

fn create_filter_search_collection(collection_name: &str) {
    Spi::run(&format!(
        "CREATE TABLE public.{collection_name} (
             id bigint PRIMARY KEY,
             embedding vector NOT NULL,
             tenant_id text NOT NULL,
             status text NOT NULL
         )"
    ))
    .expect("filtered search source table should be created");
    Spi::run(&format!(
        "INSERT INTO public.{collection_name} (id, embedding, tenant_id, status)
         VALUES (10, '[3,0]'::vector, 'acme', 'open'),
                (20, '[1,0]'::vector, 'other', 'open'),
                (30, '[2,0]'::vector, 'acme', 'closed'),
                (40, '[0.5,0]'::vector, 'acme', 'open')"
    ))
    .expect("filtered search source rows should be inserted");
    Spi::run(&format!(
        "SELECT pgcontext.create_collection('{collection_name}', 'public.{collection_name}')"
    ))
    .expect("filtered search collection should be created");
    Spi::run(&format!(
        "SELECT pgcontext.register_vector('{collection_name}', 'embedding', 'embedding', 2, 'l2')"
    ))
    .expect("filtered search vector should be registered");
    Spi::run(&format!(
        "CREATE INDEX {collection_name}_hnsw_idx ON public.{collection_name} USING pgcontext_hnsw (embedding)"
    ))
    .expect("filtered search HNSW index should be created");
    Spi::run(&format!(
        "SELECT pgcontext.attach_hnsw_index('{collection_name}', 'embedding', 'public.{collection_name}_hnsw_idx')"
    ))
    .expect("filtered search HNSW index should attach");
}

fn create_named_vector_search_collection(collection_name: &str) {
    Spi::run(&format!(
        "CREATE TABLE public.{collection_name} (
             id bigint PRIMARY KEY,
             title_embedding vector NOT NULL,
             body_embedding vector NOT NULL,
             tenant_id text NOT NULL
         )"
    ))
    .expect("named vector search source table should be created");
    Spi::run(&format!(
        "INSERT INTO public.{collection_name} (id, title_embedding, body_embedding, tenant_id)
         VALUES (10, '[3,0]'::vector, '[2,0]'::vector, 'acme'),
                (20, '[1,0]'::vector, '[4,0]'::vector, 'other'),
                (30, '[2,0]'::vector, '[1,0]'::vector, 'acme')"
    ))
    .expect("named vector search source rows should be inserted");
    Spi::run(&format!(
        "SELECT pgcontext.create_collection('{collection_name}', 'public.{collection_name}')"
    ))
    .expect("named vector search collection should be created");
    Spi::run(&format!(
        "SELECT pgcontext.register_vector(
            '{collection_name}',
            'title',
            'title_embedding',
            2,
            'l2'
        )"
    ))
    .expect("title vector should be registered");
    Spi::run(&format!(
        "SELECT pgcontext.register_vector(
            '{collection_name}',
            'body',
            'body_embedding',
            2,
            'l2'
        )"
    ))
    .expect("body vector should be registered");
}

fn create_medium_search_collection(collection_name: &str, metric: &str) {
    Spi::run(&format!(
        "CREATE TABLE public.{collection_name} (
             id bigint PRIMARY KEY,
             embedding vector NOT NULL
         )"
    ))
    .expect("medium search source table should be created");
    Spi::run(&format!(
        "INSERT INTO public.{collection_name} (id, embedding)
         VALUES (1, '[0,0]'::vector),
                (2, '[1,0]'::vector),
                (3, '[0,1]'::vector),
                (4, '[2,0]'::vector),
                (5, '[-1,0]'::vector),
                (6, '[1,1]'::vector),
                (7, '[3,4]'::vector),
                (8, '[0,-2]'::vector)"
    ))
    .expect("medium search rows should be inserted");
    Spi::run(&format!(
        "SELECT pgcontext.create_collection('{collection_name}', 'public.{collection_name}')"
    ))
    .expect("medium search collection should be created");
    Spi::run(&format!(
        "SELECT pgcontext.register_vector(
            '{collection_name}',
            'embedding',
            'embedding',
            2,
            '{metric}'
        )"
    ))
    .expect("medium search vector should be registered");
    upsert_search_points(collection_name, &["1", "2", "3", "4", "5", "6", "7", "8"]);
}

fn create_cosine_search_collection(collection_name: &str) {
    Spi::run(&format!(
        "CREATE TABLE public.{collection_name} (
             id bigint PRIMARY KEY,
             embedding vector NOT NULL
         )"
    ))
    .expect("cosine search source table should be created");
    Spi::run(&format!(
        "INSERT INTO public.{collection_name} (id, embedding)
         VALUES (1, '[1,0]'::vector),
                (2, '[0,1]'::vector),
                (3, '[1,1]'::vector),
                (4, '[2,0]'::vector),
                (5, '[-1,0]'::vector),
                (6, '[3,4]'::vector)"
    ))
    .expect("cosine search rows should be inserted");
    Spi::run(&format!(
        "SELECT pgcontext.create_collection('{collection_name}', 'public.{collection_name}')"
    ))
    .expect("cosine search collection should be created");
    Spi::run(&format!(
        "SELECT pgcontext.register_vector(
            '{collection_name}',
            'embedding',
            'embedding',
            2,
            'cosine'
        )"
    ))
    .expect("cosine search vector should be registered");
    upsert_search_points(collection_name, &["1", "2", "3", "4", "5", "6"]);
}

fn assert_table_search_matches_exact_oracle(
    collection_name: &str,
    query: &str,
    metric: &str,
    limit: i32,
) {
    let table_rows = table_search_rows(&format!(
        "SELECT point_id, source_key, score
           FROM pgcontext.search('{collection_name}', '{query}'::vector, {limit})",
    ));
    let oracle_rows = exact_oracle_rows(query, metric, limit, &medium_exact_candidates());
    assert_source_key_scores_match_exact_oracle(&table_rows, &oracle_rows);
}

fn medium_exact_candidates() -> [(i64, &'static str); 8] {
    [
        (1, "[0,0]"),
        (2, "[1,0]"),
        (3, "[0,1]"),
        (4, "[2,0]"),
        (5, "[-1,0]"),
        (6, "[1,1]"),
        (7, "[3,4]"),
        (8, "[0,-2]"),
    ]
}

fn cosine_exact_candidates() -> [(i64, &'static str); 6] {
    [
        (1, "[1,0]"),
        (2, "[0,1]"),
        (3, "[1,1]"),
        (4, "[2,0]"),
        (5, "[-1,0]"),
        (6, "[3,4]"),
    ]
}

fn upsert_search_points(collection_name: &str, source_keys: &[&str]) {
    let source_keys = source_keys
        .iter()
        .map(|source_key| format!("'{source_key}'"))
        .collect::<Vec<_>>()
        .join(", ");
    Spi::run(&format!(
        "SELECT pgcontext.upsert_points('{collection_name}', ARRAY[{source_keys}])"
    ))
    .expect("search points should be upserted");
}

fn publish_mmap_hnsw_artifact(
    collection_name: &str,
    artifact_name: &str,
    records: &[(&str, &[f32], &[u32])],
) -> ArtifactFileRow {
    let payload_hex = hnsw_payload_hex(collection_name, records);
    let job_id = completed_artifact_build_job(collection_name, "mmap", artifact_name);
    artifact_file_rows(&format!(
        "SELECT artifact_id,
                collection_name,
                build_job_id,
                artifact_kind,
                artifact_name,
                target_name,
                segment_kind,
                format_version,
                payload_bytes,
                checksum,
                relative_path,
                lifecycle_state
           FROM pgcontext.publish_artifact_segment_file(
                {job_id},
                pgcontext.encode_artifact_segment(
                    'hnsw_graph',
                    decode('{payload_hex}', 'hex')
                )
           )"
    ))
    .into_iter()
    .next()
    .expect("published mmap HNSW artifact should return a row")
}

fn hnsw_payload_hex(collection_name: &str, records: &[(&str, &[f32], &[u32])]) -> String {
    let dimensions = records
        .first()
        .map(|(_, values, _)| values.len())
        .expect("test payload should contain records");
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"PGCTXHNS");
    bytes.extend_from_slice(&1_u32.to_le_bytes());
    bytes.extend_from_slice(
        &u32::try_from(records.len())
            .expect("test record count should fit u32")
            .to_le_bytes(),
    );
    bytes.extend_from_slice(
        &u32::try_from(dimensions)
            .expect("test dimensions should fit u32")
            .to_le_bytes(),
    );
    bytes.extend_from_slice(&0_u32.to_le_bytes());

    for (node_id, (source_key, values, neighbors)) in records.iter().enumerate() {
        assert_eq!(values.len(), dimensions);
        bytes.extend_from_slice(
            &u32::try_from(node_id)
                .expect("test node id should fit u32")
                .to_le_bytes(),
        );
        bytes.extend_from_slice(
            &u32::try_from(neighbors.len())
                .expect("test neighbor count should fit u32")
                .to_le_bytes(),
        );
        bytes.extend_from_slice(&point_id_for_source_key(collection_name, source_key).to_le_bytes());
        for value in *values {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        for neighbor in *neighbors {
            bytes.extend_from_slice(&neighbor.to_le_bytes());
        }
    }

    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join("")
}

fn point_id_for_source_key(collection_name: &str, source_key: &str) -> u64 {
    let point_id = Spi::get_one::<i64>(&format!(
        "SELECT point_id
           FROM pgcontext._visible_collection_points AS points
           JOIN pgcontext._collection_acl AS collections USING (collection_id)
          WHERE collections.collection_name = '{collection_name}'
            AND points.source_key = '{source_key}'
            AND points.deleted_at IS NULL"
    ))
    .expect("point id lookup should succeed")
    .expect("point id should exist for source key");
    u64::try_from(point_id).expect("point id should be positive")
}

fn table_search_rows(sql: &str) -> Vec<(i64, String, f32)> {
    Spi::connect(|client| {
        let rows = client.select(sql, None, &[])?;
        let mut output = Vec::new();
        for row in rows {
            output.push((
                row.get::<i64>(1)?.expect("point_id should not be null"),
                row.get::<String>(2)?.expect("source_key should not be null"),
                row.get::<f32>(3)?.expect("score should not be null"),
            ));
        }
        Ok::<_, spi::Error>(output)
    })
    .expect("table search query failed")
}
