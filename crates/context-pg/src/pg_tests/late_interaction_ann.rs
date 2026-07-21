#[pg_test]
fn late_interaction_ann_search_dedupes_candidates_and_exact_reranks() {
    create_late_interaction_collection("m14_late_ann");
    create_late_interaction_token_table("m14_late_ann_tokens", true);
    upsert_hybrid_points("m14_late_ann", &["10", "20", "30", "40"]);

    let rows = hybrid_query_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.search_late_interaction_ann(
                'm14_late_ann',
                ARRAY['[1,0]'::vector, '[0,1]'::vector],
                'token_vectors',
                'public.m14_late_ann_tokens',
                'source_key',
                'token_embedding',
                3,
                3
           )",
    );

    assert_eq!(
        rows.into_iter()
            .map(|(_point_id, source_key, score)| (source_key, score))
            .collect::<Vec<_>>(),
        vec![
            ("10".to_owned(), 2.0),
            ("20".to_owned(), 1.5),
            ("30".to_owned(), 1.0),
        ]
    );
}

#[pg_test]
fn late_interaction_ann_search_rechecks_source_rows_for_final_scores() {
    create_late_interaction_collection("m14_late_ann_source_recheck");
    create_late_interaction_token_table_with_source_keys(
        "m14_late_ann_source_recheck_tokens",
        &["20"],
    );
    upsert_hybrid_points("m14_late_ann_source_recheck", &["10", "20", "30", "40"]);
    Spi::run(
        "UPDATE public.m14_late_ann_source_recheck
            SET token_vectors = ARRAY['[0,0]'::vector]
          WHERE id = 20",
    )
    .expect("late-interaction source row should be updated after token indexing");

    let rows = hybrid_query_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.search_late_interaction_ann(
                'm14_late_ann_source_recheck',
                ARRAY['[1,0]'::vector, '[0,1]'::vector],
                'token_vectors',
                'public.m14_late_ann_source_recheck_tokens',
                'source_key',
                'token_embedding',
                10,
                3
           )",
    );

    assert_eq!(
        rows.into_iter()
            .map(|(_point_id, source_key, score)| (source_key, score))
            .collect::<Vec<_>>(),
        vec![("20".to_owned(), 0.0)]
    );
}

#[pg_test]
fn late_interaction_ann_search_allows_projected_candidates_at_strict_collection_budget() {
    create_late_interaction_collection("m14_late_ann_candidate_budget_ok");
    create_late_interaction_token_table("m14_late_ann_candidate_budget_ok_tokens", true);
    upsert_hybrid_points("m14_late_ann_candidate_budget_ok", &["10", "20", "30", "40"]);
    configure_late_interaction_candidate_budget("m14_late_ann_candidate_budget_ok", 6);

    let rows = hybrid_query_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.search_late_interaction_ann(
                'm14_late_ann_candidate_budget_ok',
                ARRAY['[1,0]'::vector, '[0,1]'::vector],
                'token_vectors',
                'public.m14_late_ann_candidate_budget_ok_tokens',
                'source_key',
                'token_embedding',
                3,
                3
           )",
    );

    assert_eq!(
        rows.into_iter()
            .map(|(_point_id, source_key, score)| (source_key, score))
            .collect::<Vec<_>>(),
        vec![
            ("10".to_owned(), 2.0),
            ("20".to_owned(), 1.5),
            ("30".to_owned(), 1.0),
        ]
    );
}

#[pg_test]
fn late_interaction_ann_search_rejects_projected_candidates_above_strict_collection_budget() {
    create_late_interaction_collection("m14_late_ann_candidate_budget_bad");
    create_late_interaction_token_table("m14_late_ann_candidate_budget_bad_tokens", true);
    upsert_hybrid_points("m14_late_ann_candidate_budget_bad", &["10", "20"]);
    configure_late_interaction_candidate_budget("m14_late_ann_candidate_budget_bad", 3);

    shared_assert_sql_failure(
        "SELECT pgcontext.search_late_interaction_ann(
            'm14_late_ann_candidate_budget_bad',
            ARRAY['[1,0]'::vector, '[0,1]'::vector],
            'token_vectors',
            'public.m14_late_ann_candidate_budget_bad_tokens',
            'source_key',
            'token_embedding',
            2,
            2
        )",
        "54000",
        "collection m14_late_ann_candidate_budget_bad max_candidate_budget 3 exceeded: 4",
        "late-interaction ANN strict candidate budget",
    );
}

#[pg_test]
fn late_interaction_ann_explain_rejects_projected_candidates_above_strict_collection_budget() {
    create_late_interaction_collection("m14_late_ann_explain_candidate_budget_bad");
    create_late_interaction_token_table("m14_late_ann_explain_candidate_budget_bad_tokens", true);
    upsert_hybrid_points("m14_late_ann_explain_candidate_budget_bad", &["10", "20"]);
    configure_late_interaction_candidate_budget(
        "m14_late_ann_explain_candidate_budget_bad",
        3,
    );

    shared_assert_sql_failure(
        "SELECT pgcontext.explain_late_interaction_ann(
            'm14_late_ann_explain_candidate_budget_bad',
            ARRAY['[1,0]'::vector, '[0,1]'::vector],
            'token_vectors',
            'public.m14_late_ann_explain_candidate_budget_bad_tokens',
            'source_key',
            'token_embedding',
            2
        )",
        "54000",
        "collection m14_late_ann_explain_candidate_budget_bad max_candidate_budget 3 exceeded: 4",
        "late-interaction ANN explain strict candidate budget",
    );
}

#[pg_test]
fn late_interaction_ann_search_excludes_deleted_points_after_candidate_collection() {
    create_late_interaction_collection("m14_late_ann_deleted");
    create_late_interaction_token_table("m14_late_ann_deleted_tokens", true);
    upsert_hybrid_points("m14_late_ann_deleted", &["10", "20", "30", "40"]);
    Spi::run("SELECT pgcontext.delete_points('m14_late_ann_deleted', ARRAY['30'])")
        .expect("late-interaction ANN point should be deleted");

    let rows = hybrid_query_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.search_late_interaction_ann(
                'm14_late_ann_deleted',
                ARRAY['[1,0]'::vector, '[0,1]'::vector],
                'token_vectors',
                'public.m14_late_ann_deleted_tokens',
                'source_key',
                'token_embedding',
                10,
                4
           )",
    );

    assert_eq!(
        rows.into_iter()
            .map(|(_point_id, source_key, score)| (source_key, score))
            .collect::<Vec<_>>(),
        vec![
            ("10".to_owned(), 2.0),
            ("20".to_owned(), 1.5),
            ("40".to_owned(), 0.0),
        ]
    );
}

#[pg_test]
fn late_interaction_ann_search_excludes_missing_source_rows_after_candidate_collection() {
    create_late_interaction_collection("m14_late_ann_source_deleted");
    create_late_interaction_token_table("m14_late_ann_source_deleted_tokens", true);
    upsert_hybrid_points("m14_late_ann_source_deleted", &["10", "20", "30", "40"]);
    Spi::run("DELETE FROM public.m14_late_ann_source_deleted WHERE id = 30")
        .expect("late-interaction ANN source row should be deleted after token indexing");

    let rows = hybrid_query_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.search_late_interaction_ann(
                'm14_late_ann_source_deleted',
                ARRAY['[1,0]'::vector, '[0,1]'::vector],
                'token_vectors',
                'public.m14_late_ann_source_deleted_tokens',
                'source_key',
                'token_embedding',
                10,
                4
           )",
    );

    assert_eq!(
        rows.into_iter()
            .map(|(_point_id, source_key, score)| (source_key, score))
            .collect::<Vec<_>>(),
        vec![
            ("10".to_owned(), 2.0),
            ("20".to_owned(), 1.5),
            ("40".to_owned(), 0.0),
        ]
    );
}

#[pg_test]
fn late_interaction_ann_explain_reports_candidate_serving_strategy() {
    create_late_interaction_collection("m14_late_ann_explain");
    create_late_interaction_token_table("m14_late_ann_explain_tokens", true);
    upsert_hybrid_points("m14_late_ann_explain", &["10", "20", "30"]);

    let planner_detail = hybrid_explain_rows(
        "SELECT stage, detail
           FROM pgcontext.explain_late_interaction_ann(
                'm14_late_ann_explain',
                ARRAY['[1,0]'::vector, '[0,1]'::vector],
                'token_vectors',
                'public.m14_late_ann_explain_tokens',
                'source_key',
                'token_embedding',
                3
           )
          WHERE stage = 'ann_planner'",
    );

    assert_eq!(
        planner_detail,
        vec![(
            "ann_planner".to_owned(),
            "kind=ann_candidate_serving reasons=AnnCandidateServingReady projected_comparisons=12 comparison_budget=1000000"
                .to_owned(),
        )]
    );
}

#[pg_test]
fn late_interaction_ann_search_returns_empty_for_empty_collection() {
    create_late_interaction_collection("m14_late_ann_empty");
    create_late_interaction_token_table("m14_late_ann_empty_tokens", true);

    let rows = hybrid_query_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.search_late_interaction_ann(
                'm14_late_ann_empty',
                ARRAY['[1,0]'::vector],
                'token_vectors',
                'public.m14_late_ann_empty_tokens',
                'source_key',
                'token_embedding',
                2,
                5
           )",
    );

    assert!(rows.is_empty());
}

#[pg_test]
fn late_interaction_ann_explain_reports_comparison_budget_rejection() {
    create_late_interaction_budget_collection("m14_late_ann_budget_explain");
    create_late_interaction_token_table("m14_late_ann_budget_explain_tokens", true);
    upsert_hybrid_points("m14_late_ann_budget_explain", &["10"]);

    let rows = hybrid_explain_structured_rows(
        "SELECT stage,
                branch,
                strategy,
                status::text,
                estimated_candidates,
                candidate_budget
           FROM pgcontext.explain_late_interaction_ann(
                'm14_late_ann_budget_explain',
                array_fill('[1,0]'::vector, ARRAY[1001]),
                'token_vectors',
                'public.m14_late_ann_budget_explain_tokens',
                'source_key',
                'token_embedding',
                1000
           )
          WHERE stage = 'ann_planner'",
    );

    assert_eq!(
        rows,
        vec![(
            "ann_planner".to_owned(),
            Some("multi_vector".to_owned()),
            "rejected".to_owned(),
            "Policy".to_owned(),
            Some(1_001_000),
            Some(1_000_000),
        )]
    );
}

#[pg_test]
fn late_interaction_ann_search_rejects_projected_comparison_budget() {
    create_late_interaction_budget_collection("m14_late_ann_budget_search");
    create_late_interaction_token_table("m14_late_ann_budget_search_tokens", true);
    upsert_hybrid_points("m14_late_ann_budget_search", &["10"]);

    shared_assert_sql_failure(
        "SELECT pgcontext.search_late_interaction_ann(
            'm14_late_ann_budget_search',
            array_fill('[1,0]'::vector, ARRAY[1001]),
            'token_vectors',
            'public.m14_late_ann_budget_search_tokens',
            'source_key',
            'token_embedding',
            1000,
            1
        )",
        "54000",
        "late interaction comparison budget exceeded: 1001000 > 1000000",
        "late-interaction ANN search comparison budget",
    );
}

#[pg_test]
fn late_interaction_ann_search_rejects_hydrated_comparison_budget() {
    create_late_interaction_budget_collection_with_vectors("m14_late_ann_hydrated_budget", 1001);
    create_late_interaction_token_table_with_source_keys(
        "m14_late_ann_hydrated_budget_tokens",
        &["10"],
    );
    upsert_hybrid_points("m14_late_ann_hydrated_budget", &["10"]);

    shared_assert_sql_failure(
        "SELECT pgcontext.search_late_interaction_ann(
            'm14_late_ann_hydrated_budget',
            array_fill('[1,0]'::vector, ARRAY[1000]),
            'token_vectors',
            'public.m14_late_ann_hydrated_budget_tokens',
            'source_key',
            'token_embedding',
            1,
            1
        )",
        "54000",
        "late interaction comparison budget exceeded: 1001000 > 1000000",
        "late-interaction ANN hydrated comparison budget",
    );
}

#[pg_test]
fn late_interaction_ann_search_rejects_missing_hnsw_index_with_sqlstate() {
    create_late_interaction_collection("m14_late_ann_no_index");
    create_late_interaction_token_table("m14_late_ann_no_index_tokens", false);
    upsert_hybrid_points("m14_late_ann_no_index", &["10", "20"]);

    shared_assert_sql_failure(
        "SELECT pgcontext.search_late_interaction_ann(
            'm14_late_ann_no_index',
            ARRAY['[1,0]'::vector],
            'token_vectors',
            'public.m14_late_ann_no_index_tokens',
            'source_key',
            'token_embedding',
            2,
            2
        )",
        "55000",
        "late-interaction ANN token table requires a pgcontext_hnsw index on public.m14_late_ann_no_index_tokens.token_embedding",
        "late-interaction missing ANN token index",
    );
}

#[pg_test]
fn late_interaction_ann_search_rejects_partial_hnsw_index_with_sqlstate() {
    create_late_interaction_collection("m14_late_ann_partial_index");
    create_late_interaction_token_table_with_partial_hnsw_index(
        "m14_late_ann_partial_index_tokens",
    );
    upsert_hybrid_points("m14_late_ann_partial_index", &["10", "20"]);

    shared_assert_sql_failure(
        "SELECT pgcontext.search_late_interaction_ann(
            'm14_late_ann_partial_index',
            ARRAY['[1,0]'::vector],
            'token_vectors',
            'public.m14_late_ann_partial_index_tokens',
            'source_key',
            'token_embedding',
            2,
            2
        )",
        "55000",
        "late-interaction ANN token table requires a pgcontext_hnsw index on public.m14_late_ann_partial_index_tokens.token_embedding",
        "late-interaction partial ANN token index",
    );
}

#[pg_test]
fn late_interaction_ann_search_rejects_missing_token_table() {
    create_late_interaction_collection("m14_late_ann_missing_token_table");
    upsert_hybrid_points("m14_late_ann_missing_token_table", &["10"]);

    shared_assert_sql_failure(
        "SELECT pgcontext.search_late_interaction_ann(
            'm14_late_ann_missing_token_table',
            ARRAY['[1,0]'::vector],
            'token_vectors',
            'public.m14_late_ann_missing_token_table_tokens',
            'source_key',
            'token_embedding',
            2,
            2
        )",
        "42P01",
        "late-interaction ANN token table does not exist: public.m14_late_ann_missing_token_table_tokens",
        "late-interaction ANN missing token table",
    );
}

#[pg_test]
fn late_interaction_ann_search_rejects_missing_token_source_key_column() {
    create_late_interaction_collection("m14_late_ann_missing_source_key");
    create_late_interaction_token_table_without_source_key(
        "m14_late_ann_missing_source_key_tokens",
    );
    upsert_hybrid_points("m14_late_ann_missing_source_key", &["10"]);

    shared_assert_sql_failure(
        "SELECT pgcontext.search_late_interaction_ann(
            'm14_late_ann_missing_source_key',
            ARRAY['[1,0]'::vector],
            'token_vectors',
            'public.m14_late_ann_missing_source_key_tokens',
            'source_key',
            'token_embedding',
            2,
            2
        )",
        "42703",
        "late-interaction ANN source key column does not exist: public.m14_late_ann_missing_source_key_tokens.source_key",
        "late-interaction ANN missing token source key",
    );
}

#[pg_test]
fn late_interaction_ann_search_rejects_nullable_token_source_key_column() {
    create_late_interaction_collection("m14_late_ann_nullable_source_key");
    create_late_interaction_token_table_with_nullable_source_key(
        "m14_late_ann_nullable_source_key_tokens",
    );
    upsert_hybrid_points("m14_late_ann_nullable_source_key", &["10"]);

    shared_assert_sql_failure(
        "SELECT pgcontext.search_late_interaction_ann(
            'm14_late_ann_nullable_source_key',
            ARRAY['[1,0]'::vector],
            'token_vectors',
            'public.m14_late_ann_nullable_source_key_tokens',
            'source_key',
            'token_embedding',
            2,
            2
        )",
        "55000",
        "late-interaction ANN source key column must be NOT NULL: public.m14_late_ann_nullable_source_key_tokens.source_key",
        "late-interaction ANN nullable token source key",
    );
}

#[pg_test]
fn late_interaction_ann_search_rejects_wrong_token_vector_type() {
    create_late_interaction_collection("m14_late_ann_wrong_token_type");
    create_late_interaction_token_table_with_wrong_vector_type(
        "m14_late_ann_wrong_token_type_tokens",
    );
    upsert_hybrid_points("m14_late_ann_wrong_token_type", &["10"]);

    shared_assert_sql_failure(
        "SELECT pgcontext.search_late_interaction_ann(
            'm14_late_ann_wrong_token_type',
            ARRAY['[1,0]'::vector],
            'token_vectors',
            'public.m14_late_ann_wrong_token_type_tokens',
            'source_key',
            'token_embedding',
            2,
            2
        )",
        "42804",
        "late-interaction ANN vector column must have type vector: public.m14_late_ann_wrong_token_type_tokens.token_embedding",
        "late-interaction ANN wrong token vector type",
    );
}

#[pg_test]
fn late_interaction_ann_search_rejects_token_vector_dimension_mismatch() {
    create_late_interaction_collection("m14_late_ann_token_dims");
    create_late_interaction_token_table_with_dimension("m14_late_ann_token_dims_tokens", 3);
    upsert_hybrid_points("m14_late_ann_token_dims", &["10"]);

    shared_assert_sql_failure(
        "SELECT pgcontext.search_late_interaction_ann(
            'm14_late_ann_token_dims',
            ARRAY['[1,0]'::vector],
            'token_vectors',
            'public.m14_late_ann_token_dims_tokens',
            'source_key',
            'token_embedding',
            2,
            2
        )",
        "22023",
        "dimension mismatch: left has 2 dimensions, right has 3",
        "late-interaction ANN token vector dimension mismatch",
    );
}

#[pg_test]
fn late_interaction_ann_search_rejects_untyped_token_vector_column() {
    create_late_interaction_collection("m14_late_ann_untyped_token");
    create_untyped_late_interaction_token_table("m14_late_ann_untyped_token_tokens");
    upsert_hybrid_points("m14_late_ann_untyped_token", &["10"]);

    shared_assert_sql_failure(
        "SELECT pgcontext.search_late_interaction_ann(
            'm14_late_ann_untyped_token',
            ARRAY['[1,0]'::vector],
            'token_vectors',
            'public.m14_late_ann_untyped_token_tokens',
            'source_key',
            'token_embedding',
            2,
            2
        )",
        "55000",
        "late-interaction ANN vector column must declare dimensions with vector(n): public.m14_late_ann_untyped_token_tokens.token_embedding",
        "late-interaction ANN untyped token vector column",
    );
}

#[pg_test]
fn late_interaction_ann_search_rejects_mixed_query_vector_dimensions() {
    create_late_interaction_collection("m14_late_ann_mixed_query_dims");
    create_late_interaction_token_table("m14_late_ann_mixed_query_dims_tokens", true);
    upsert_hybrid_points("m14_late_ann_mixed_query_dims", &["10"]);

    shared_assert_sql_failure(
        "SELECT pgcontext.search_late_interaction_ann(
            'm14_late_ann_mixed_query_dims',
            ARRAY['[1,0]'::vector, '[1,0,0]'::vector],
            'token_vectors',
            'public.m14_late_ann_mixed_query_dims_tokens',
            'source_key',
            'token_embedding',
            2,
            2
        )",
        "22023",
        "dimension mismatch: left has 2 dimensions, right has 3",
        "late-interaction ANN mixed query vector dimensions",
    );
}

#[pg_test]
fn late_interaction_ann_search_rejects_missing_token_vector_column() {
    create_late_interaction_collection("m14_late_ann_missing_vector_column");
    create_late_interaction_token_table_without_vector_column(
        "m14_late_ann_missing_vector_column_tokens",
    );
    upsert_hybrid_points("m14_late_ann_missing_vector_column", &["10"]);

    shared_assert_sql_failure(
        "SELECT pgcontext.search_late_interaction_ann(
            'm14_late_ann_missing_vector_column',
            ARRAY['[1,0]'::vector],
            'token_vectors',
            'public.m14_late_ann_missing_vector_column_tokens',
            'source_key',
            'token_embedding',
            2,
            2
        )",
        "42703",
        "late-interaction ANN vector column does not exist: public.m14_late_ann_missing_vector_column_tokens.token_embedding",
        "late-interaction ANN missing token vector",
    );
}

#[pg_test]
fn late_interaction_ann_search_rejects_nullable_token_vector_column() {
    create_late_interaction_collection("m14_late_ann_nullable_token_vector");
    create_late_interaction_token_table_with_nullable_vector(
        "m14_late_ann_nullable_token_vector_tokens",
    );
    upsert_hybrid_points("m14_late_ann_nullable_token_vector", &["10"]);

    shared_assert_sql_failure(
        "SELECT pgcontext.search_late_interaction_ann(
            'm14_late_ann_nullable_token_vector',
            ARRAY['[1,0]'::vector],
            'token_vectors',
            'public.m14_late_ann_nullable_token_vector_tokens',
            'source_key',
            'token_embedding',
            2,
            2
        )",
        "55000",
        "late-interaction ANN vector column must be NOT NULL: public.m14_late_ann_nullable_token_vector_tokens.token_embedding",
        "late-interaction ANN nullable token vector",
    );
}

#[pg_test]
fn late_interaction_ann_search_rejects_source_table_drift_with_sqlstate() {
    create_late_interaction_collection("m14_late_ann_source_drift");
    create_late_interaction_token_table("m14_late_ann_source_drift_tokens", true);
    upsert_hybrid_points("m14_late_ann_source_drift", &["10"]);
    Spi::run(
        "ALTER TABLE public.m14_late_ann_source_drift
            RENAME TO m14_late_ann_source_drift_old",
    )
    .expect("late-interaction ANN source table should be renamed away");

    shared_assert_sql_failure(
        "SELECT pgcontext.search_late_interaction_ann(
            'm14_late_ann_source_drift',
            ARRAY['[1,0]'::vector],
            'token_vectors',
            'public.m14_late_ann_source_drift_tokens',
            'source_key',
            'token_embedding',
            2,
            2
        )",
        "42P01",
        "registered source table drifted: public.m14_late_ann_source_drift",
        "late-interaction ANN source table drift",
    );
}

#[pg_test]
fn late_interaction_ann_explain_rejects_source_table_drift_with_sqlstate() {
    create_late_interaction_collection("m14_late_ann_explain_source_drift");
    create_late_interaction_token_table("m14_late_ann_explain_source_drift_tokens", true);
    upsert_hybrid_points("m14_late_ann_explain_source_drift", &["10"]);
    Spi::run(
        "ALTER TABLE public.m14_late_ann_explain_source_drift
            RENAME TO m14_late_ann_explain_source_drift_old",
    )
    .expect("late-interaction ANN explain source table should be renamed away");

    shared_assert_sql_failure(
        "SELECT pgcontext.explain_late_interaction_ann(
            'm14_late_ann_explain_source_drift',
            ARRAY['[1,0]'::vector],
            'token_vectors',
            'public.m14_late_ann_explain_source_drift_tokens',
            'source_key',
            'token_embedding',
            2
        )",
        "42P01",
        "registered source table drifted: public.m14_late_ann_explain_source_drift",
        "late-interaction ANN explain source table drift",
    );
}

#[pg_test]
fn late_interaction_ann_search_rejects_missing_source_vector_column_with_sqlstate() {
    create_late_interaction_collection("m14_late_ann_missing_source_vector");
    create_late_interaction_token_table("m14_late_ann_missing_source_vector_tokens", true);
    upsert_hybrid_points("m14_late_ann_missing_source_vector", &["10"]);
    Spi::run("ALTER TABLE public.m14_late_ann_missing_source_vector DROP COLUMN token_vectors")
        .expect("late-interaction ANN source vector column should be dropped");

    shared_assert_sql_failure(
        "SELECT pgcontext.search_late_interaction_ann(
            'm14_late_ann_missing_source_vector',
            ARRAY['[1,0]'::vector],
            'token_vectors',
            'public.m14_late_ann_missing_source_vector_tokens',
            'source_key',
            'token_embedding',
            2,
            2
        )",
        "42703",
        "late-interaction vector column does not exist on public.m14_late_ann_missing_source_vector: token_vectors",
        "late-interaction ANN missing source vector column",
    );
}

#[pg_test]
fn late_interaction_ann_explain_rejects_missing_source_vector_column_with_sqlstate() {
    create_late_interaction_collection("m14_late_ann_explain_missing_source_vector");
    create_late_interaction_token_table("m14_late_ann_explain_missing_source_vector_tokens", true);
    upsert_hybrid_points("m14_late_ann_explain_missing_source_vector", &["10"]);
    Spi::run(
        "ALTER TABLE public.m14_late_ann_explain_missing_source_vector DROP COLUMN token_vectors",
    )
    .expect("late-interaction ANN explain source vector column should be dropped");

    shared_assert_sql_failure(
        "SELECT pgcontext.explain_late_interaction_ann(
            'm14_late_ann_explain_missing_source_vector',
            ARRAY['[1,0]'::vector],
            'token_vectors',
            'public.m14_late_ann_explain_missing_source_vector_tokens',
            'source_key',
            'token_embedding',
            2
        )",
        "42703",
        "late-interaction vector column does not exist on public.m14_late_ann_explain_missing_source_vector: token_vectors",
        "late-interaction ANN explain missing source vector column",
    );
}

#[pg_test]
fn late_interaction_ann_search_rejects_wrong_source_vector_column_type_with_sqlstate() {
    create_wrong_type_late_interaction_collection("m14_late_ann_wrong_source_vector");
    create_late_interaction_token_table("m14_late_ann_wrong_source_vector_tokens", true);
    upsert_hybrid_points("m14_late_ann_wrong_source_vector", &["10"]);

    shared_assert_sql_failure(
        "SELECT pgcontext.search_late_interaction_ann(
            'm14_late_ann_wrong_source_vector',
            ARRAY['[1,0]'::vector],
            'token_vectors',
            'public.m14_late_ann_wrong_source_vector_tokens',
            'source_key',
            'token_embedding',
            2,
            2
        )",
        "42804",
        "late-interaction vector column must have type vector[]: public.m14_late_ann_wrong_source_vector.token_vectors",
        "late-interaction ANN wrong source vector column type",
    );
}

#[pg_test]
fn late_interaction_ann_explain_rejects_wrong_source_vector_column_type_with_sqlstate() {
    create_wrong_type_late_interaction_collection("m14_late_ann_explain_wrong_source_vector");
    create_late_interaction_token_table("m14_late_ann_explain_wrong_source_vector_tokens", true);
    upsert_hybrid_points("m14_late_ann_explain_wrong_source_vector", &["10"]);

    shared_assert_sql_failure(
        "SELECT pgcontext.explain_late_interaction_ann(
            'm14_late_ann_explain_wrong_source_vector',
            ARRAY['[1,0]'::vector],
            'token_vectors',
            'public.m14_late_ann_explain_wrong_source_vector_tokens',
            'source_key',
            'token_embedding',
            2
        )",
        "42804",
        "late-interaction vector column must have type vector[]: public.m14_late_ann_explain_wrong_source_vector.token_vectors",
        "late-interaction ANN explain wrong source vector column type",
    );
}

#[pg_test]
fn late_interaction_ann_search_rejects_missing_source_key_column_with_sqlstate() {
    create_late_interaction_collection("m14_late_ann_missing_source_id");
    create_late_interaction_token_table("m14_late_ann_missing_source_id_tokens", true);
    upsert_hybrid_points("m14_late_ann_missing_source_id", &["10"]);
    Spi::run("ALTER TABLE public.m14_late_ann_missing_source_id DROP COLUMN id")
        .expect("late-interaction ANN source key column should be dropped");

    shared_assert_sql_failure(
        "SELECT pgcontext.search_late_interaction_ann(
            'm14_late_ann_missing_source_id',
            ARRAY['[1,0]'::vector],
            'token_vectors',
            'public.m14_late_ann_missing_source_id_tokens',
            'source_key',
            'token_embedding',
            2,
            2
        )",
        "42703",
        "source key column does not exist on public.m14_late_ann_missing_source_id: id",
        "late-interaction ANN missing source key column",
    );
}

#[pg_test]
fn late_interaction_ann_explain_rejects_missing_source_key_column_with_sqlstate() {
    create_late_interaction_collection("m14_late_ann_explain_missing_source_id");
    create_late_interaction_token_table("m14_late_ann_explain_missing_source_id_tokens", true);
    upsert_hybrid_points("m14_late_ann_explain_missing_source_id", &["10"]);
    Spi::run("ALTER TABLE public.m14_late_ann_explain_missing_source_id DROP COLUMN id")
        .expect("late-interaction ANN explain source key column should be dropped");

    shared_assert_sql_failure(
        "SELECT pgcontext.explain_late_interaction_ann(
            'm14_late_ann_explain_missing_source_id',
            ARRAY['[1,0]'::vector],
            'token_vectors',
            'public.m14_late_ann_explain_missing_source_id_tokens',
            'source_key',
            'token_embedding',
            2
        )",
        "42703",
        "source key column does not exist on public.m14_late_ann_explain_missing_source_id: id",
        "late-interaction ANN explain missing source key column",
    );
}

#[pg_test]
fn late_interaction_ann_explain_rejects_missing_hnsw_index_with_sqlstate() {
    create_late_interaction_collection("m14_late_ann_explain_no_index");
    create_late_interaction_token_table("m14_late_ann_explain_no_index_tokens", false);
    upsert_hybrid_points("m14_late_ann_explain_no_index", &["10", "20"]);

    shared_assert_sql_failure(
        "SELECT pgcontext.explain_late_interaction_ann(
            'm14_late_ann_explain_no_index',
            ARRAY['[1,0]'::vector],
            'token_vectors',
            'public.m14_late_ann_explain_no_index_tokens',
            'source_key',
            'token_embedding',
            2
        )",
        "55000",
        "late-interaction ANN token table requires a pgcontext_hnsw index on public.m14_late_ann_explain_no_index_tokens.token_embedding",
        "late-interaction ANN explain missing token index",
    );
}

#[pg_test]
fn late_interaction_ann_explain_rejects_missing_token_table() {
    create_late_interaction_collection("m14_late_ann_explain_missing_token_table");
    upsert_hybrid_points("m14_late_ann_explain_missing_token_table", &["10"]);

    shared_assert_sql_failure(
        "SELECT pgcontext.explain_late_interaction_ann(
            'm14_late_ann_explain_missing_token_table',
            ARRAY['[1,0]'::vector],
            'token_vectors',
            'public.m14_late_ann_explain_missing_token_table_tokens',
            'source_key',
            'token_embedding',
            2
        )",
        "42P01",
        "late-interaction ANN token table does not exist: public.m14_late_ann_explain_missing_token_table_tokens",
        "late-interaction ANN explain missing token table",
    );
}

#[pg_test]
fn late_interaction_ann_explain_rejects_missing_token_source_key_column() {
    create_late_interaction_collection("m14_late_ann_explain_missing_source_key");
    create_late_interaction_token_table_without_source_key(
        "m14_late_ann_explain_missing_source_key_tokens",
    );
    upsert_hybrid_points("m14_late_ann_explain_missing_source_key", &["10"]);

    shared_assert_sql_failure(
        "SELECT pgcontext.explain_late_interaction_ann(
            'm14_late_ann_explain_missing_source_key',
            ARRAY['[1,0]'::vector],
            'token_vectors',
            'public.m14_late_ann_explain_missing_source_key_tokens',
            'source_key',
            'token_embedding',
            2
        )",
        "42703",
        "late-interaction ANN source key column does not exist: public.m14_late_ann_explain_missing_source_key_tokens.source_key",
        "late-interaction ANN explain missing token source key",
    );
}

#[pg_test]
fn late_interaction_ann_explain_rejects_nullable_token_source_key_column() {
    create_late_interaction_collection("m14_late_ann_explain_nullable_source_key");
    create_late_interaction_token_table_with_nullable_source_key(
        "m14_late_ann_explain_nullable_source_key_tokens",
    );
    upsert_hybrid_points("m14_late_ann_explain_nullable_source_key", &["10"]);

    shared_assert_sql_failure(
        "SELECT pgcontext.explain_late_interaction_ann(
            'm14_late_ann_explain_nullable_source_key',
            ARRAY['[1,0]'::vector],
            'token_vectors',
            'public.m14_late_ann_explain_nullable_source_key_tokens',
            'source_key',
            'token_embedding',
            2
        )",
        "55000",
        "late-interaction ANN source key column must be NOT NULL: public.m14_late_ann_explain_nullable_source_key_tokens.source_key",
        "late-interaction ANN explain nullable token source key",
    );
}

#[pg_test]
fn late_interaction_ann_explain_rejects_wrong_token_vector_type() {
    create_late_interaction_collection("m14_late_ann_explain_wrong_token_type");
    create_late_interaction_token_table_with_wrong_vector_type(
        "m14_late_ann_explain_wrong_token_type_tokens",
    );
    upsert_hybrid_points("m14_late_ann_explain_wrong_token_type", &["10"]);

    shared_assert_sql_failure(
        "SELECT pgcontext.explain_late_interaction_ann(
            'm14_late_ann_explain_wrong_token_type',
            ARRAY['[1,0]'::vector],
            'token_vectors',
            'public.m14_late_ann_explain_wrong_token_type_tokens',
            'source_key',
            'token_embedding',
            2
        )",
        "42804",
        "late-interaction ANN vector column must have type vector: public.m14_late_ann_explain_wrong_token_type_tokens.token_embedding",
        "late-interaction ANN explain wrong token vector type",
    );
}

#[pg_test]
fn late_interaction_ann_explain_rejects_token_vector_dimension_mismatch() {
    create_late_interaction_collection("m14_late_ann_explain_token_dims");
    create_late_interaction_token_table_with_dimension("m14_late_ann_explain_token_dims_tokens", 3);
    upsert_hybrid_points("m14_late_ann_explain_token_dims", &["10"]);

    shared_assert_sql_failure(
        "SELECT pgcontext.explain_late_interaction_ann(
            'm14_late_ann_explain_token_dims',
            ARRAY['[1,0]'::vector],
            'token_vectors',
            'public.m14_late_ann_explain_token_dims_tokens',
            'source_key',
            'token_embedding',
            2
        )",
        "22023",
        "dimension mismatch: left has 2 dimensions, right has 3",
        "late-interaction ANN explain token vector dimension mismatch",
    );
}

#[pg_test]
fn late_interaction_ann_explain_rejects_untyped_token_vector_column() {
    create_late_interaction_collection("m14_late_ann_explain_untyped_token");
    create_untyped_late_interaction_token_table("m14_late_ann_explain_untyped_token_tokens");
    upsert_hybrid_points("m14_late_ann_explain_untyped_token", &["10"]);

    shared_assert_sql_failure(
        "SELECT pgcontext.explain_late_interaction_ann(
            'm14_late_ann_explain_untyped_token',
            ARRAY['[1,0]'::vector],
            'token_vectors',
            'public.m14_late_ann_explain_untyped_token_tokens',
            'source_key',
            'token_embedding',
            2
        )",
        "55000",
        "late-interaction ANN vector column must declare dimensions with vector(n): public.m14_late_ann_explain_untyped_token_tokens.token_embedding",
        "late-interaction ANN explain untyped token vector column",
    );
}

#[pg_test]
fn late_interaction_ann_explain_rejects_mixed_query_vector_dimensions() {
    create_late_interaction_collection("m14_late_ann_explain_mixed_query_dims");
    create_late_interaction_token_table("m14_late_ann_explain_mixed_query_dims_tokens", true);
    upsert_hybrid_points("m14_late_ann_explain_mixed_query_dims", &["10"]);

    shared_assert_sql_failure(
        "SELECT pgcontext.explain_late_interaction_ann(
            'm14_late_ann_explain_mixed_query_dims',
            ARRAY['[1,0]'::vector, '[1,0,0]'::vector],
            'token_vectors',
            'public.m14_late_ann_explain_mixed_query_dims_tokens',
            'source_key',
            'token_embedding',
            2
        )",
        "22023",
        "dimension mismatch: left has 2 dimensions, right has 3",
        "late-interaction ANN explain mixed query vector dimensions",
    );
}

#[pg_test]
fn late_interaction_ann_explain_rejects_missing_token_vector_column() {
    create_late_interaction_collection("m14_late_ann_explain_missing_vector");
    create_late_interaction_token_table_without_vector_column(
        "m14_late_ann_explain_missing_vector_tokens",
    );
    upsert_hybrid_points("m14_late_ann_explain_missing_vector", &["10"]);

    shared_assert_sql_failure(
        "SELECT pgcontext.explain_late_interaction_ann(
            'm14_late_ann_explain_missing_vector',
            ARRAY['[1,0]'::vector],
            'token_vectors',
            'public.m14_late_ann_explain_missing_vector_tokens',
            'source_key',
            'token_embedding',
            2
        )",
        "42703",
        "late-interaction ANN vector column does not exist: public.m14_late_ann_explain_missing_vector_tokens.token_embedding",
        "late-interaction ANN explain missing token vector",
    );
}

#[pg_test]
fn late_interaction_ann_explain_rejects_nullable_token_vector_column() {
    create_late_interaction_collection("m14_late_ann_explain_nullable_token");
    create_late_interaction_token_table_with_nullable_vector(
        "m14_late_ann_explain_nullable_token_tokens",
    );
    upsert_hybrid_points("m14_late_ann_explain_nullable_token", &["10"]);

    shared_assert_sql_failure(
        "SELECT pgcontext.explain_late_interaction_ann(
            'm14_late_ann_explain_nullable_token',
            ARRAY['[1,0]'::vector],
            'token_vectors',
            'public.m14_late_ann_explain_nullable_token_tokens',
            'source_key',
            'token_embedding',
            2
        )",
        "55000",
        "late-interaction ANN vector column must be NOT NULL: public.m14_late_ann_explain_nullable_token_tokens.token_embedding",
        "late-interaction ANN explain nullable token vector",
    );
}

#[pg_test]
fn late_interaction_ann_explain_rejects_partial_hnsw_index_with_sqlstate() {
    create_late_interaction_collection("m14_late_ann_explain_partial_index");
    create_late_interaction_token_table_with_partial_hnsw_index(
        "m14_late_ann_explain_partial_index_tokens",
    );
    upsert_hybrid_points("m14_late_ann_explain_partial_index", &["10", "20"]);

    shared_assert_sql_failure(
        "SELECT pgcontext.explain_late_interaction_ann(
            'm14_late_ann_explain_partial_index',
            ARRAY['[1,0]'::vector],
            'token_vectors',
            'public.m14_late_ann_explain_partial_index_tokens',
            'source_key',
            'token_embedding',
            2
        )",
        "55000",
        "late-interaction ANN token table requires a pgcontext_hnsw index on public.m14_late_ann_explain_partial_index_tokens.token_embedding",
        "late-interaction explain partial ANN token index",
    );
}

#[pg_test]
fn late_interaction_ann_explain_rejects_token_table_select_denial() {
    create_late_interaction_collection("m14_late_ann_explain_token_acl");
    create_late_interaction_token_table("m14_late_ann_explain_token_acl_tokens", true);
    upsert_hybrid_points("m14_late_ann_explain_token_acl", &["10"]);
    sql_test_create_role("m14_late_ann_explain_token_acl_denied");
    sql_test_grant_api_access("m14_late_ann_explain_token_acl_denied");
    Spi::run(
        "UPDATE pgcontext._collections
            SET owner_role = 'm14_late_ann_explain_token_acl_denied'::regrole
          WHERE collection_name = 'm14_late_ann_explain_token_acl';
         GRANT SELECT ON TABLE public.m14_late_ann_explain_token_acl
           TO m14_late_ann_explain_token_acl_denied;
         REVOKE ALL ON TABLE public.m14_late_ann_explain_token_acl_tokens FROM PUBLIC;
         REVOKE ALL ON TABLE public.m14_late_ann_explain_token_acl_tokens
           FROM m14_late_ann_explain_token_acl_denied",
    )
    .expect("late-interaction ANN explain token table privileges should be configured");

    with_session_user_reset("m14_late_ann_explain_token_acl_denied", || {
        shared_assert_sql_failure(
            "SELECT pgcontext.explain_late_interaction_ann(
                'm14_late_ann_explain_token_acl',
                ARRAY['[1,0]'::vector],
                'token_vectors',
                'public.m14_late_ann_explain_token_acl_tokens',
                'source_key',
                'token_embedding',
                2
            )",
            "42501",
            "permission denied for ANN token table: public.m14_late_ann_explain_token_acl_tokens",
            "late-interaction ANN explain token table privilege",
        );
    });
}

#[pg_test]
fn late_interaction_ann_search_rejects_token_table_select_denial() {
    create_late_interaction_collection("m14_late_ann_token_acl");
    create_late_interaction_token_table("m14_late_ann_token_acl_tokens", true);
    upsert_hybrid_points("m14_late_ann_token_acl", &["10"]);
    sql_test_create_role("m14_late_ann_token_acl_denied");
    sql_test_grant_api_access("m14_late_ann_token_acl_denied");
    Spi::run(
        "UPDATE pgcontext._collections
            SET owner_role = 'm14_late_ann_token_acl_denied'::regrole
          WHERE collection_name = 'm14_late_ann_token_acl';
         GRANT SELECT ON TABLE public.m14_late_ann_token_acl
           TO m14_late_ann_token_acl_denied;
         REVOKE ALL ON TABLE public.m14_late_ann_token_acl_tokens FROM PUBLIC;
         REVOKE ALL ON TABLE public.m14_late_ann_token_acl_tokens
           FROM m14_late_ann_token_acl_denied",
    )
    .expect("late-interaction ANN token table privileges should be configured");

    with_session_user_reset("m14_late_ann_token_acl_denied", || {
        shared_assert_sql_failure(
            "SELECT pgcontext.search_late_interaction_ann(
                'm14_late_ann_token_acl',
                ARRAY['[1,0]'::vector],
                'token_vectors',
                'public.m14_late_ann_token_acl_tokens',
                'source_key',
                'token_embedding',
                2,
                2
            )",
            "42501",
            "permission denied for ANN token table: public.m14_late_ann_token_acl_tokens",
            "late-interaction ANN token table privilege",
        );
    });
}

#[pg_test]
fn late_interaction_ann_search_rejects_source_table_select_denial() {
    create_late_interaction_collection("m14_late_ann_source_acl");
    create_late_interaction_token_table("m14_late_ann_source_acl_tokens", true);
    upsert_hybrid_points("m14_late_ann_source_acl", &["10"]);
    sql_test_create_role("m14_late_ann_source_acl_denied");
    sql_test_grant_api_access("m14_late_ann_source_acl_denied");
    Spi::run(
        "UPDATE pgcontext._collections
            SET owner_role = 'm14_late_ann_source_acl_denied'::regrole
          WHERE collection_name = 'm14_late_ann_source_acl';
         GRANT SELECT ON TABLE public.m14_late_ann_source_acl_tokens
           TO m14_late_ann_source_acl_denied;
         REVOKE ALL ON TABLE public.m14_late_ann_source_acl FROM PUBLIC;
         REVOKE ALL ON TABLE public.m14_late_ann_source_acl
           FROM m14_late_ann_source_acl_denied",
    )
    .expect("late-interaction ANN source table privileges should be configured");

    with_session_user_reset("m14_late_ann_source_acl_denied", || {
        shared_assert_sql_failure(
            "SELECT pgcontext.search_late_interaction_ann(
                'm14_late_ann_source_acl',
                ARRAY['[1,0]'::vector],
                'token_vectors',
                'public.m14_late_ann_source_acl_tokens',
                'source_key',
                'token_embedding',
                2,
                2
            )",
            "42501",
            "permission denied for source table: public.m14_late_ann_source_acl",
            "late-interaction ANN source table privilege",
        );
    });
}

#[pg_test]
fn late_interaction_ann_explain_rejects_source_table_select_denial() {
    create_late_interaction_collection("m14_late_ann_explain_source_acl");
    create_late_interaction_token_table("m14_late_ann_explain_source_acl_tokens", true);
    upsert_hybrid_points("m14_late_ann_explain_source_acl", &["10"]);
    sql_test_create_role("m14_late_ann_explain_source_acl_denied");
    sql_test_grant_api_access("m14_late_ann_explain_source_acl_denied");
    Spi::run(
        "UPDATE pgcontext._collections
            SET owner_role = 'm14_late_ann_explain_source_acl_denied'::regrole
          WHERE collection_name = 'm14_late_ann_explain_source_acl';
         GRANT SELECT ON TABLE public.m14_late_ann_explain_source_acl_tokens
           TO m14_late_ann_explain_source_acl_denied;
         REVOKE ALL ON TABLE public.m14_late_ann_explain_source_acl FROM PUBLIC;
         REVOKE ALL ON TABLE public.m14_late_ann_explain_source_acl
           FROM m14_late_ann_explain_source_acl_denied",
    )
    .expect("late-interaction ANN explain source table privileges should be configured");

    with_session_user_reset("m14_late_ann_explain_source_acl_denied", || {
        shared_assert_sql_failure(
            "SELECT pgcontext.explain_late_interaction_ann(
                'm14_late_ann_explain_source_acl',
                ARRAY['[1,0]'::vector],
                'token_vectors',
                'public.m14_late_ann_explain_source_acl_tokens',
                'source_key',
                'token_embedding',
                2
            )",
            "42501",
            "permission denied for source table: public.m14_late_ann_explain_source_acl",
            "late-interaction ANN explain source table privilege",
        );
    });
}

fn with_session_user_reset(role_name: &str, assertion: impl FnOnce()) {
    sql_test_set_session_user(role_name);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(assertion));
    sql_test_reset_session_user();
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}

fn create_late_interaction_budget_collection(collection_name: &str) {
    create_late_interaction_budget_collection_with_vectors(collection_name, 1000);
}

fn create_late_interaction_budget_collection_with_vectors(
    collection_name: &str,
    vector_count: usize,
) {
    Spi::run(&format!(
        "CREATE TABLE public.{collection_name} (
             id bigint PRIMARY KEY,
             token_vectors vector[] NOT NULL
         )"
    ))
    .expect("late-interaction ANN budget source table should be created");
    Spi::run(&format!(
        "INSERT INTO public.{collection_name} (id, token_vectors)
         VALUES (10, array_fill('[1,0]'::vector, ARRAY[{vector_count}]))"
    ))
    .expect("late-interaction ANN budget source row should be inserted");
    Spi::run(&format!(
        "SELECT pgcontext.create_collection('{collection_name}', 'public.{collection_name}')"
    ))
    .expect("late-interaction ANN budget collection should be created");
}

fn create_wrong_type_late_interaction_collection(collection_name: &str) {
    Spi::run(&format!(
        "CREATE TABLE public.{collection_name} (
             id bigint PRIMARY KEY,
             token_vectors text NOT NULL
         )"
    ))
    .expect("wrong-type late-interaction ANN source table should be created");
    Spi::run(&format!(
        "INSERT INTO public.{collection_name} (id, token_vectors)
         VALUES (10, 'bad')"
    ))
    .expect("wrong-type late-interaction ANN source row should be inserted");
    Spi::run(&format!(
        "SELECT pgcontext.create_collection('{collection_name}', 'public.{collection_name}')"
    ))
    .expect("wrong-type late-interaction ANN collection should be created");
}

fn configure_late_interaction_candidate_budget(collection_name: &str, candidate_budget: i32) {
    Spi::run(&format!(
        "SELECT pgcontext.configure_collection_limits(
            '{collection_name}',
            true,
            NULL,
            NULL,
            NULL,
            NULL,
            NULL,
            {candidate_budget},
            NULL,
            NULL
        )"
    ))
    .expect("late-interaction ANN candidate budget should be configured");
}

fn create_late_interaction_token_table(table_name: &str, with_hnsw_index: bool) {
    create_late_interaction_token_table_rows(
        table_name,
        &["10", "10", "20", "20", "30", "30", "40"],
    );
    if with_hnsw_index {
        Spi::run(&format!(
            "CREATE INDEX {table_name}_token_embedding_idx
                ON public.{table_name} USING pgcontext_hnsw (token_embedding)"
        ))
        .expect("late-interaction token HNSW index should be created");
    }
}

fn create_late_interaction_token_table_with_source_keys(table_name: &str, source_keys: &[&str]) {
    create_late_interaction_token_table_rows(table_name, source_keys);
    Spi::run(&format!(
        "CREATE INDEX {table_name}_token_embedding_idx
            ON public.{table_name} USING pgcontext_hnsw (token_embedding)"
    ))
    .expect("late-interaction token HNSW index should be created");
}

fn create_late_interaction_token_table_with_dimension(table_name: &str, dimensions: usize) {
    Spi::run(&format!(
        "CREATE TABLE public.{table_name} (
             source_key text NOT NULL,
             token_embedding vector({dimensions}) NOT NULL
         )"
    ))
    .expect("late-interaction dimension-mismatch token table should be created");
    Spi::run(&format!(
        "CREATE INDEX {table_name}_token_embedding_idx
            ON public.{table_name} USING pgcontext_hnsw (token_embedding)"
    ))
    .expect("late-interaction dimension-mismatch token HNSW index should be created");
}

fn create_untyped_late_interaction_token_table(table_name: &str) {
    Spi::run(&format!(
        "CREATE TABLE public.{table_name} (
             source_key text NOT NULL,
             token_embedding vector NOT NULL
         )"
    ))
    .expect("untyped late-interaction token table should be created");
    Spi::run(&format!(
        "CREATE INDEX {table_name}_token_embedding_idx
            ON public.{table_name} USING pgcontext_hnsw (token_embedding)"
    ))
    .expect("untyped late-interaction token HNSW index should be created");
}

fn create_late_interaction_token_table_with_partial_hnsw_index(table_name: &str) {
    create_late_interaction_token_table_rows(table_name, &["10", "20"]);
    Spi::run(&format!(
        "CREATE INDEX {table_name}_token_embedding_idx
            ON public.{table_name} USING pgcontext_hnsw (token_embedding)
            WHERE source_key <> '20'"
    ))
    .expect("late-interaction partial token HNSW index should be created");
}

fn create_late_interaction_token_table_rows(table_name: &str, source_keys: &[&str]) {
    Spi::run(&format!(
        "CREATE TABLE public.{table_name} (
             source_key text NOT NULL,
             token_embedding vector(2) NOT NULL
         )"
    ))
    .expect("late-interaction token table should be created");
    let values = source_keys
        .iter()
        .enumerate()
        .map(|(index, source_key)| {
            let token = if index % 2 == 0 { "[1,0]" } else { "[0,1]" };
            format!("('{source_key}', '{token}'::vector)")
        })
        .collect::<Vec<_>>()
        .join(", ");
    Spi::run(&format!(
        "INSERT INTO public.{table_name} (source_key, token_embedding)
         VALUES {values}"
    ))
    .expect("late-interaction token rows should be inserted");
}

fn create_late_interaction_token_table_without_source_key(table_name: &str) {
    Spi::run(&format!(
        "CREATE TABLE public.{table_name} (
             token_embedding vector NOT NULL
         )"
    ))
    .expect("late-interaction token table without source key should be created");
    Spi::run(&format!(
        "INSERT INTO public.{table_name} (token_embedding)
         VALUES ('[1,0]'::vector)"
    ))
    .expect("late-interaction token row without source key should be inserted");
    Spi::run(&format!(
        "CREATE INDEX {table_name}_token_embedding_idx
            ON public.{table_name} USING pgcontext_hnsw (token_embedding)"
    ))
    .expect("late-interaction token HNSW index should be created");
}

fn create_late_interaction_token_table_without_vector_column(table_name: &str) {
    Spi::run(&format!(
        "CREATE TABLE public.{table_name} (
             source_key text NOT NULL
         )"
    ))
    .expect("late-interaction token table without vector column should be created");
    Spi::run(&format!(
        "INSERT INTO public.{table_name} (source_key)
         VALUES ('10')"
    ))
    .expect("late-interaction token row without vector column should be inserted");
}

fn create_late_interaction_token_table_with_nullable_source_key(table_name: &str) {
    Spi::run(&format!(
        "CREATE TABLE public.{table_name} (
             source_key text,
             token_embedding vector NOT NULL
         )"
    ))
    .expect("late-interaction token table with nullable source key should be created");
    Spi::run(&format!(
        "INSERT INTO public.{table_name} (source_key, token_embedding)
         VALUES (NULL, '[1,0]'::vector),
                ('10', '[1,0]'::vector)"
    ))
    .expect("late-interaction token rows with nullable source key should be inserted");
    Spi::run(&format!(
        "CREATE INDEX {table_name}_token_embedding_idx
            ON public.{table_name} USING pgcontext_hnsw (token_embedding)"
    ))
    .expect("late-interaction token HNSW index should be created");
}

fn create_late_interaction_token_table_with_wrong_vector_type(table_name: &str) {
    Spi::run(&format!(
        "CREATE TABLE public.{table_name} (
             source_key text NOT NULL,
             token_embedding text NOT NULL
         )"
    ))
    .expect("late-interaction token table with wrong vector type should be created");
    Spi::run(&format!(
        "INSERT INTO public.{table_name} (source_key, token_embedding)
         VALUES ('10', '[1,0]')"
    ))
    .expect("late-interaction token row with wrong vector type should be inserted");
}

fn create_late_interaction_token_table_with_nullable_vector(table_name: &str) {
    Spi::run(&format!(
        "CREATE TABLE public.{table_name} (
             source_key text NOT NULL,
             token_embedding vector
         )"
    ))
    .expect("late-interaction token table with nullable vector should be created");
    Spi::run(&format!(
        "INSERT INTO public.{table_name} (source_key, token_embedding)
         VALUES ('10', NULL),
                ('10', '[1,0]'::vector)"
    ))
    .expect("late-interaction token rows with nullable vector should be inserted");
    Spi::run(&format!(
        "CREATE INDEX {table_name}_token_embedding_idx
            ON public.{table_name} USING pgcontext_hnsw (token_embedding)"
    ))
    .expect("late-interaction nullable token vector HNSW index should be created");
}
