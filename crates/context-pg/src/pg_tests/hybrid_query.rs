#[pg_test]
fn hybrid_query_fuses_dense_and_full_text_branches() {
    create_hybrid_collection("m5_hybrid_docs");
    upsert_search_points("m5_hybrid_docs", &["10", "20", "30", "40"]);

    let rows = hybrid_query_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.query(
                'm5_hybrid_docs',
                '[0,0]'::vector,
                'database',
                'body',
                4
           )",
    );

    assert_eq!(
        rows.into_iter()
            .map(|(_point_id, source_key, _score)| source_key)
            .collect::<Vec<_>>(),
        vec![
            "20".to_owned(),
            "10".to_owned(),
            "30".to_owned(),
            "40".to_owned()
        ]
    );
}

#[pg_test]
fn hybrid_query_excludes_deleted_points_from_both_branches() {
    create_hybrid_collection("m5_hybrid_deleted");
    upsert_search_points("m5_hybrid_deleted", &["10", "20", "30", "40"]);
    Spi::run("SELECT pgcontext.delete_points('m5_hybrid_deleted', ARRAY['20'])")
        .expect("point should be deleted");

    let rows = hybrid_query_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.query(
                'm5_hybrid_deleted',
                '[0,0]'::vector,
                'database',
                'body',
                4
           )",
    );

    assert_eq!(
        rows.into_iter()
            .map(|(_point_id, source_key, _score)| source_key)
            .collect::<Vec<_>>(),
        vec!["10".to_owned(), "30".to_owned(), "40".to_owned()]
    );
}

#[pg_test]
fn hybrid_query_returns_dense_results_when_full_text_branch_is_empty() {
    create_hybrid_collection("m5_hybrid_dense_only");
    upsert_search_points("m5_hybrid_dense_only", &["10", "20"]);

    let rows = hybrid_query_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.query(
                'm5_hybrid_dense_only',
                '[0,0]'::vector,
                'notpresent',
                'body',
                4
           )",
    );

    assert_eq!(rows.len(), 2, "dense-only hybrid query should return the two active points");
    let source_keys = rows
        .into_iter()
        .map(|(_point_id, source_key, _score)| source_key)
        .collect::<Vec<_>>();
    let oracle_rows = exact_oracle_rows("[0,0]", "l2", 2, &[(10, "[1,0]"), (20, "[0,0]")]);
    assert_source_keys_match_exact_oracle(&source_keys, &oracle_rows);
}

#[pg_test]
fn hybrid_query_returns_empty_result_when_all_branches_are_empty() {
    create_hybrid_collection("m5_hybrid_empty");

    let rows = hybrid_query_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.query(
                'm5_hybrid_empty',
                '[0,0]'::vector,
                'database',
                'body',
                4
           )",
    );

    assert_eq!(rows, Vec::new());
}

#[pg_test]
fn hybrid_query_breaks_fused_score_ties_by_point_id() {
    create_hybrid_tie_collection("m5_hybrid_ties");
    upsert_search_points("m5_hybrid_ties", &["10", "20"]);

    let rows = hybrid_query_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.query(
                'm5_hybrid_ties',
                '[0,0]'::vector,
                'database',
                'body',
                2
           )",
    );

    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows.iter()
            .map(|(_point_id, source_key, _score)| source_key.clone())
            .collect::<Vec<_>>(),
        vec!["10".to_owned(), "20".to_owned()]
    );
    assert!(rows[0].0 < rows[1].0);
    assert_eq!(rows[0].2, rows[1].2);
}

#[pg_test]
fn dense_sparse_query_fuses_registered_dense_and_sparse_branches() {
    create_dense_sparse_collection("m14_dense_sparse_docs");
    upsert_hybrid_points("m14_dense_sparse_docs", &["10", "20", "30"]);

    let rows = hybrid_query_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.query(
                'm14_dense_sparse_docs',
                '[0,0]'::vector,
                'lexical',
                pgcontext.sparsevec('{1:1}/4'),
                3
           )",
    );

    assert_eq!(
        rows.into_iter()
            .map(|(_point_id, source_key, _score)| source_key)
            .collect::<Vec<_>>(),
        vec!["10".to_owned(), "20".to_owned(), "30".to_owned()]
    );
}

#[pg_test]
fn dense_sparse_query_excludes_deleted_points_from_both_branches() {
    create_dense_sparse_collection("m14_dense_sparse_deleted");
    upsert_hybrid_points("m14_dense_sparse_deleted", &["10", "20", "30"]);
    Spi::run("SELECT pgcontext.delete_points('m14_dense_sparse_deleted', ARRAY['20'])")
        .expect("dense+sparse point should be deleted");

    let rows = hybrid_query_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.query(
                'm14_dense_sparse_deleted',
                '[0,0]'::vector,
                'lexical',
                pgcontext.sparsevec('{1:1}/4'),
                3
           )",
    );

    assert_eq!(
        rows.into_iter()
            .map(|(_point_id, source_key, _score)| source_key)
            .collect::<Vec<_>>(),
        vec!["10".to_owned(), "30".to_owned()]
    );
}

#[pg_test]
fn dense_sparse_query_rejects_missing_sparse_registration() {
    create_hybrid_collection("m14_dense_sparse_missing");

    shared_assert_sql_failure(
        "SELECT pgcontext.query(
            'm14_dense_sparse_missing',
            '[0,0]'::vector,
            'lexical',
            pgcontext.sparsevec('{1:1}/4'),
            3
        )",
        "42704",
        "sparse vector registration does not exist for collection m14_dense_sparse_missing: lexical",
        "missing dense+sparse registration",
    );
}

#[pg_test]
fn dense_sparse_query_rejects_sparse_dimension_mismatch() {
    create_dense_sparse_collection("m14_dense_sparse_dim_mismatch");
    upsert_hybrid_points("m14_dense_sparse_dim_mismatch", &["10"]);

    shared_assert_sql_failure(
        "SELECT pgcontext.query(
            'm14_dense_sparse_dim_mismatch',
            '[0,0]'::vector,
            'lexical',
            pgcontext.sparsevec('{1:1}/2'),
            3
        )",
        "22023",
        "dimension mismatch: left has 4 dimensions, right has 2",
        "dense+sparse dimension mismatch",
    );
}

#[pg_test]
fn hybrid_explain_returns_query_stage_diagnostics() {
    create_hybrid_collection("m5_hybrid_explain");

    let rows = hybrid_explain_rows(
        "SELECT stage, detail
           FROM pgcontext.explain('m5_hybrid_explain', 'body')",
    );

    assert_eq!(
        rows,
        vec![
            (
                "collection".to_owned(),
                "source_table=public.m5_hybrid_explain".to_owned()
            ),
            ("dense".to_owned(), "vector_column=embedding metric=l2".to_owned()),
            ("full_text".to_owned(), "text_column=body config=simple".to_owned()),
            (
                "fusion".to_owned(),
                "algorithm=rrf k=60 tie_break=point_id".to_owned()
            ),
            (
                "recall_budget".to_owned(),
                "max_recall_check_point_ids=10000 hnsw_candidate_budget=32 hnsw_iterative_expansion_limit=10000 hnsw_recall_threshold=0.95".to_owned()
            ),
        ]
    );
}

#[pg_test]
fn hybrid_explain_returns_structured_stage_diagnostics() {
    create_hybrid_collection("m5_hybrid_explain_structured");
    upsert_search_points("m5_hybrid_explain_structured", &["10", "20", "30"]);
    Spi::run("SELECT pgcontext.delete_points('m5_hybrid_explain_structured', ARRAY['30'])")
        .expect("one structured explain point should be deleted");

    let rows = hybrid_explain_structured_rows(
        "SELECT stage,
                branch,
                strategy,
                status::text,
                estimated_candidates,
                candidate_budget
           FROM pgcontext.explain('m5_hybrid_explain_structured', 'body')",
    );

    assert_eq!(
        rows,
        vec![
            (
                "collection".to_owned(),
                None,
                "source_table".to_owned(),
                "Ready".to_owned(),
                Some(2),
                None,
            ),
            (
                "dense".to_owned(),
                Some("dense".to_owned()),
                "exact_table_scan".to_owned(),
                "Fallback".to_owned(),
                Some(2),
                Some(10_000),
            ),
            (
                "full_text".to_owned(),
                Some("full_text".to_owned()),
                "postgres_full_text".to_owned(),
                "Ready".to_owned(),
                Some(2),
                Some(10_000),
            ),
            (
                "fusion".to_owned(),
                Some("hybrid".to_owned()),
                "reciprocal_rank_fusion".to_owned(),
                "Ready".to_owned(),
                None,
                Some(10_000),
            ),
            (
                "recall_budget".to_owned(),
                None,
                "policy".to_owned(),
                "Policy".to_owned(),
                None,
                Some(32),
            ),
        ]
    );
}

#[pg_test]
fn late_interaction_query_scores_table_backed_vector_arrays() {
    create_late_interaction_collection("m14_late_table");
    upsert_hybrid_points("m14_late_table", &["10", "20", "30", "40"]);

    let rows = hybrid_query_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.search_late_interaction(
                'm14_late_table',
                ARRAY['[1,0]'::vector, '[0,1]'::vector],
                'token_vectors',
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
fn late_interaction_explain_reports_exact_planner_and_budget_gates() {
    create_late_interaction_collection("m14_late_explain");
    upsert_hybrid_points("m14_late_explain", &["10", "20", "30"]);
    Spi::run("SELECT pgcontext.delete_points('m14_late_explain', ARRAY['30'])")
        .expect("one late-interaction explain point should be deleted");

    let rows = hybrid_explain_structured_rows(
        "SELECT stage,
                branch,
                strategy,
                status::text,
                estimated_candidates,
                candidate_budget
           FROM pgcontext.explain_late_interaction(
                'm14_late_explain',
                ARRAY['[1,0]'::vector, '[0,1]'::vector],
                'token_vectors'
           )",
    );

    assert_eq!(
        rows,
        vec![
            (
                "collection".to_owned(),
                None,
                "source_table".to_owned(),
                "Ready".to_owned(),
                Some(2),
                None,
            ),
            (
                "late_interaction".to_owned(),
                Some("multi_vector".to_owned()),
                "exact_table_scan".to_owned(),
                "Fallback".to_owned(),
                Some(4),
                Some(1_000_000),
            ),
            (
                "maxsim".to_owned(),
                Some("multi_vector".to_owned()),
                "exact_maxsim".to_owned(),
                "Ready".to_owned(),
                Some(8),
                Some(1_000_000),
            ),
            (
                "ann_planner".to_owned(),
                Some("multi_vector".to_owned()),
                "exact_table_scan".to_owned(),
                "Fallback".to_owned(),
                Some(8),
                Some(1_000_000),
            ),
        ]
    );

    let planner_detail = hybrid_explain_rows(
        "SELECT stage, detail
           FROM pgcontext.explain_late_interaction(
                'm14_late_explain',
                ARRAY['[1,0]'::vector, '[0,1]'::vector],
                'token_vectors'
           )
          WHERE stage = 'ann_planner'",
    );
    assert_eq!(
        planner_detail,
        vec![(
            "ann_planner".to_owned(),
            "kind=exact_table_scan reasons=NoAnnServingPath projected_comparisons=8 comparison_budget=1000000"
                .to_owned(),
        )]
    );
}

#[pg_test]
fn late_interaction_explain_rejects_empty_query_vectors() {
    create_late_interaction_collection("m14_late_explain_empty_query");
    shared_assert_sql_failure(
        "SELECT pgcontext.explain_late_interaction(
            'm14_late_explain_empty_query',
            ARRAY[]::vector[],
            'token_vectors'
        )",
        "22023",
        "late interaction query_vectors must not be empty",
        "late-interaction explain empty query vectors",
    );
}

#[pg_test]
fn late_interaction_explain_rejects_missing_vector_column_with_sqlstate() {
    create_late_interaction_collection("m14_late_explain_missing_column");
    Spi::run("ALTER TABLE public.m14_late_explain_missing_column DROP COLUMN token_vectors")
        .expect("late-interaction explain vector column should be dropped");

    shared_assert_sql_failure(
        "SELECT pgcontext.explain_late_interaction(
            'm14_late_explain_missing_column',
            ARRAY['[1,0]'::vector],
            'token_vectors'
        )",
        "42703",
        "late-interaction vector column does not exist on public.m14_late_explain_missing_column: token_vectors",
        "late-interaction explain missing vector column",
    );
}

#[pg_test]
fn late_interaction_explain_rejects_wrong_vector_column_type_with_sqlstate() {
    Spi::run(
        "CREATE TABLE public.m14_late_explain_wrong_type (
             id bigint PRIMARY KEY,
             token_vectors text NOT NULL
         )",
    )
    .expect("wrong-type late-interaction explain table should be created");
    Spi::run(
        "INSERT INTO public.m14_late_explain_wrong_type (id, token_vectors) VALUES (10, 'bad')",
    )
    .expect("wrong-type late-interaction explain row should be inserted");
    Spi::run(
        "SELECT pgcontext.create_collection(
            'm14_late_explain_wrong_type',
            'public.m14_late_explain_wrong_type'
        )",
    )
    .expect("wrong-type late-interaction explain collection should be created");

    shared_assert_sql_failure(
        "SELECT pgcontext.explain_late_interaction(
            'm14_late_explain_wrong_type',
            ARRAY['[1,0]'::vector],
            'token_vectors'
        )",
        "42804",
        "late-interaction vector column must have type vector[]: public.m14_late_explain_wrong_type.token_vectors",
        "late-interaction explain wrong vector column type",
    );
}

#[pg_test]
fn late_interaction_explain_rejects_missing_source_key_column_with_sqlstate() {
    create_late_interaction_collection("m14_late_explain_missing_id");
    Spi::run("ALTER TABLE public.m14_late_explain_missing_id DROP COLUMN id")
        .expect("late-interaction explain source key column should be dropped");

    shared_assert_sql_failure(
        "SELECT pgcontext.explain_late_interaction(
            'm14_late_explain_missing_id',
            ARRAY['[1,0]'::vector],
            'token_vectors'
        )",
        "42703",
        "source key column does not exist on public.m14_late_explain_missing_id: id",
        "late-interaction explain missing source key column",
    );
}

#[pg_test]
fn late_interaction_explain_rejects_source_table_drift_with_sqlstate() {
    create_late_interaction_collection("m14_late_explain_drift");
    Spi::run("ALTER TABLE public.m14_late_explain_drift RENAME TO m14_late_explain_drift_old")
        .expect("late-interaction explain source table should be renamed away");

    shared_assert_sql_failure(
        "SELECT pgcontext.explain_late_interaction(
            'm14_late_explain_drift',
            ARRAY['[1,0]'::vector],
            'token_vectors'
        )",
        "42P01",
        "registered source table drifted: public.m14_late_explain_drift",
        "late-interaction explain source table drift",
    );
}

#[pg_test]
fn late_interaction_explain_rejects_source_table_select_denial_with_sqlstate() {
    create_late_interaction_collection("m14_late_explain_source_acl");
    upsert_hybrid_points("m14_late_explain_source_acl", &["10"]);
    sql_test_create_role("m14_late_explain_source_acl_denied");
    sql_test_grant_api_access("m14_late_explain_source_acl_denied");
    Spi::run(
        "UPDATE pgcontext._collections
            SET owner_role = 'm14_late_explain_source_acl_denied'::regrole
          WHERE collection_name = 'm14_late_explain_source_acl'",
    )
    .expect("late-interaction explain collection owner should be reassigned for ACL test");
    Spi::run(
        "REVOKE ALL ON TABLE public.m14_late_explain_source_acl FROM PUBLIC;
         REVOKE ALL ON TABLE public.m14_late_explain_source_acl
           FROM m14_late_explain_source_acl_denied",
    )
    .expect("late-interaction explain source table privileges should be revoked");

    sql_test_set_session_user("m14_late_explain_source_acl_denied");
    shared_assert_sql_failure(
        "SELECT pgcontext.explain_late_interaction(
            'm14_late_explain_source_acl',
            ARRAY['[1,0]'::vector],
            'token_vectors'
        )",
        "42501",
        "permission denied for source table: public.m14_late_explain_source_acl",
        "late-interaction explain source table privilege",
    );
    sql_test_reset_session_user();
}

#[pg_test]
fn late_interaction_query_excludes_deleted_points_and_breaks_ties() {
    create_late_interaction_tie_collection("m14_late_table_ties");
    upsert_hybrid_points("m14_late_table_ties", &["10", "20", "30"]);
    Spi::run("SELECT pgcontext.delete_points('m14_late_table_ties', ARRAY['20'])")
        .expect("late-interaction point should be deleted");

    let rows = hybrid_query_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.search_late_interaction(
                'm14_late_table_ties',
                ARRAY['[1,0]'::vector],
                'token_vectors',
                3
           )",
    );

    assert_eq!(
        rows.into_iter()
            .map(|(_point_id, source_key, score)| (source_key, score))
            .collect::<Vec<_>>(),
        vec![("10".to_owned(), 1.0), ("30".to_owned(), 1.0)]
    );
}

#[pg_test]
fn late_interaction_query_rejects_empty_query_vectors() {
    create_late_interaction_collection("m14_late_empty_query");
    shared_assert_sql_failure(
        "SELECT pgcontext.search_late_interaction(
            'm14_late_empty_query',
            ARRAY[]::vector[],
            'token_vectors',
            3
        )",
        "22023",
        "late interaction query_vectors must not be empty",
        "late-interaction empty query vectors",
    );
}

#[pg_test]
fn late_interaction_query_rejects_missing_vector_column() {
    create_late_interaction_collection("m14_late_missing_column");
    Spi::run("ALTER TABLE public.m14_late_missing_column DROP COLUMN token_vectors")
        .expect("late-interaction vector column should be dropped");

    shared_assert_sql_failure(
        "SELECT pgcontext.search_late_interaction(
            'm14_late_missing_column',
            ARRAY['[1,0]'::vector],
            'token_vectors',
            3
        )",
        "42703",
        "late-interaction vector column does not exist on public.m14_late_missing_column: token_vectors",
        "late-interaction missing vector column",
    );
}

#[pg_test]
fn late_interaction_query_rejects_wrong_vector_column_type() {
    Spi::run(
        "CREATE TABLE public.m14_late_wrong_type (
             id bigint PRIMARY KEY,
             token_vectors text NOT NULL
         )",
    )
    .expect("wrong-type late-interaction table should be created");
    Spi::run("INSERT INTO public.m14_late_wrong_type (id, token_vectors) VALUES (10, 'bad')")
        .expect("wrong-type late-interaction row should be inserted");
    Spi::run("SELECT pgcontext.create_collection('m14_late_wrong_type', 'public.m14_late_wrong_type')")
        .expect("wrong-type late-interaction collection should be created");

    shared_assert_sql_failure(
        "SELECT pgcontext.search_late_interaction(
            'm14_late_wrong_type',
            ARRAY['[1,0]'::vector],
            'token_vectors',
            3
        )",
        "42804",
        "late-interaction vector column must have type vector[]: public.m14_late_wrong_type.token_vectors",
        "late-interaction wrong vector column type",
    );
}

#[pg_test]
fn late_interaction_query_rejects_missing_source_key_column_with_sqlstate() {
    create_late_interaction_collection("m14_late_missing_id");
    Spi::run("ALTER TABLE public.m14_late_missing_id DROP COLUMN id")
        .expect("late-interaction source key column should be dropped");

    shared_assert_sql_failure(
        "SELECT pgcontext.search_late_interaction(
            'm14_late_missing_id',
            ARRAY['[1,0]'::vector],
            'token_vectors',
            3
        )",
        "42703",
        "source key column does not exist on public.m14_late_missing_id: id",
        "late-interaction missing source key column",
    );
}

#[pg_test]
fn late_interaction_query_rejects_source_table_drift_with_sqlstate() {
    create_late_interaction_collection("m14_late_drift");
    Spi::run("ALTER TABLE public.m14_late_drift RENAME TO m14_late_drift_old")
        .expect("late-interaction source table should be renamed away");

    shared_assert_sql_failure(
        "SELECT pgcontext.search_late_interaction(
            'm14_late_drift',
            ARRAY['[1,0]'::vector],
            'token_vectors',
            3
        )",
        "42P01",
        "registered source table drifted: public.m14_late_drift",
        "late-interaction source table drift",
    );
}

#[pg_test]
fn late_interaction_query_rejects_empty_candidate_vectors() {
    create_late_interaction_collection("m14_late_empty_candidate");
    Spi::run("UPDATE public.m14_late_empty_candidate SET token_vectors = ARRAY[]::vector[] WHERE id = 10")
        .expect("late-interaction candidate vectors should be emptied");
    upsert_hybrid_points("m14_late_empty_candidate", &["10"]);

    shared_assert_sql_failure(
        "SELECT pgcontext.search_late_interaction(
            'm14_late_empty_candidate',
            ARRAY['[1,0]'::vector],
            'token_vectors',
            3
        )",
        "22023",
        "each late-interaction candidate point must have at least one vector",
        "late-interaction empty candidate vectors",
    );
}

#[pg_test]
fn late_interaction_query_rejects_dimension_mismatch() {
    create_late_interaction_collection("m14_late_dim_mismatch");
    upsert_hybrid_points("m14_late_dim_mismatch", &["10"]);

    shared_assert_sql_failure(
        "SELECT pgcontext.search_late_interaction(
            'm14_late_dim_mismatch',
            ARRAY['[1]'::vector],
            'token_vectors',
            3
        )",
        "22P02",
        "invalid vector: dimension mismatch: left has 1 dimensions, right has 2",
        "late-interaction dimension mismatch",
    );
}

#[pg_test]
fn late_interaction_query_rejects_source_table_select_denial_with_sqlstate() {
    create_late_interaction_collection("m14_late_source_acl");
    upsert_hybrid_points("m14_late_source_acl", &["10"]);
    sql_test_create_role("m14_late_source_acl_denied");
    sql_test_grant_api_access("m14_late_source_acl_denied");
    Spi::run(
        "UPDATE pgcontext._collections
            SET owner_role = 'm14_late_source_acl_denied'::regrole
          WHERE collection_name = 'm14_late_source_acl'",
    )
    .expect("late-interaction collection owner should be reassigned for ACL test");
    Spi::run(
        "REVOKE ALL ON TABLE public.m14_late_source_acl FROM PUBLIC;
         REVOKE ALL ON TABLE public.m14_late_source_acl FROM m14_late_source_acl_denied",
    )
    .expect("late-interaction source table privileges should be revoked");

    sql_test_set_session_user("m14_late_source_acl_denied");
    shared_assert_sql_failure(
        "SELECT pgcontext.search_late_interaction(
            'm14_late_source_acl',
            ARRAY['[1,0]'::vector],
            'token_vectors',
            3
        )",
        "42501",
        "permission denied for source table: public.m14_late_source_acl",
        "late-interaction source table privilege",
    );
    sql_test_reset_session_user();
}

#[pg_test]
fn late_interaction_query_rejects_excessive_comparison_budget_with_sqlstate() {
    Spi::run(
        "CREATE TABLE public.m14_late_budget (
             id bigint PRIMARY KEY,
             token_vectors vector[] NOT NULL
         )",
    )
    .expect("late-interaction budget table should be created");
    Spi::run(
        "INSERT INTO public.m14_late_budget (id, token_vectors)
         VALUES (10, array_fill('[1,0]'::vector, ARRAY[1000]));",
    )
    .expect("late-interaction budget row should be inserted");
    Spi::run("SELECT pgcontext.create_collection('m14_late_budget', 'public.m14_late_budget')")
        .expect("late-interaction budget collection should be created");
    upsert_hybrid_points("m14_late_budget", &["10"]);

    shared_assert_sql_failure(
        "SELECT pgcontext.search_late_interaction(
            'm14_late_budget',
            array_fill('[1,0]'::vector, ARRAY[1001]),
            'token_vectors',
            3
        )",
        "54000",
        "late interaction comparison budget exceeded: 1001000 > 1000000",
        "late-interaction table-backed comparison budget",
    );
}

#[pg_test]
#[should_panic(expected = "query text column does not exist on public.m5_hybrid_missing_text: body")]
fn hybrid_query_rejects_missing_text_column() {
    create_hybrid_collection("m5_hybrid_missing_text");
    Spi::run("ALTER TABLE public.m5_hybrid_missing_text DROP COLUMN body")
        .expect("text column should be dropped");

    Spi::run(
        "SELECT pgcontext.query(
            'm5_hybrid_missing_text',
            '[0,0]'::vector,
            'database',
            'body',
            4
        )",
    )
    .expect("missing text column should be rejected");
}

fn create_hybrid_collection(collection_name: &str) {
    Spi::run(&format!(
        "CREATE TABLE public.{collection_name} (
             id bigint PRIMARY KEY,
             embedding vector NOT NULL,
             body text NOT NULL
         )"
    ))
    .expect("hybrid source table should be created");
    Spi::run(&format!(
        "INSERT INTO public.{collection_name} (id, embedding, body)
         VALUES (10, '[1,0]'::vector, 'database internals'),
                (20, '[0,0]'::vector, 'database database'),
                (30, '[2,0]'::vector, 'storage database'),
                (40, '[3,0]'::vector, 'unrelated')"
    ))
    .expect("hybrid source rows should be inserted");
    Spi::run(&format!(
        "SELECT pgcontext.create_collection('{collection_name}', 'public.{collection_name}')"
    ))
    .expect("hybrid collection should be created");
    Spi::run(&format!(
        "SELECT pgcontext.register_vector('{collection_name}', 'embedding', 'embedding', 2, 'l2')"
    ))
    .expect("hybrid vector should be registered");
}

fn create_hybrid_tie_collection(collection_name: &str) {
    Spi::run(&format!(
        "CREATE TABLE public.{collection_name} (
             id bigint PRIMARY KEY,
             embedding vector NOT NULL,
             body text NOT NULL
         )"
    ))
    .expect("hybrid tie source table should be created");
    Spi::run(&format!(
        "INSERT INTO public.{collection_name} (id, embedding, body)
         VALUES (10, '[0,0]'::vector, 'database'),
                (20, '[1,0]'::vector, 'database database database')"
    ))
    .expect("hybrid tie source rows should be inserted");
    Spi::run(&format!(
        "SELECT pgcontext.create_collection('{collection_name}', 'public.{collection_name}')"
    ))
    .expect("hybrid tie collection should be created");
    Spi::run(&format!(
        "SELECT pgcontext.register_vector('{collection_name}', 'embedding', 'embedding', 2, 'l2')"
    ))
    .expect("hybrid tie vector should be registered");
}

fn create_dense_sparse_collection(collection_name: &str) {
    create_dense_sparse_collection_with_metric(collection_name, "inner_product");
}

fn create_dense_sparse_collection_with_metric(collection_name: &str, sparse_metric: &str) {
    Spi::run(&format!(
        "CREATE TABLE public.{collection_name} (
             id bigint PRIMARY KEY,
             embedding vector NOT NULL,
             lexical sparsevec NOT NULL,
             body text NOT NULL
         )"
    ))
    .expect("dense+sparse source table should be created");
    Spi::run(&format!(
        "INSERT INTO public.{collection_name} (id, embedding, lexical, body)
         VALUES (10, '[0,0]'::vector, pgcontext.sparsevec('{{1:1}}/4'), 'first'),
                (20, '[3,0]'::vector, pgcontext.sparsevec('{{1:3}}/4'), 'second'),
                (30, '[2,0]'::vector, pgcontext.sparsevec('{{1:2}}/4'), 'third')"
    ))
    .expect("dense+sparse rows should be inserted");
    Spi::run(&format!(
        "SELECT pgcontext.create_collection('{collection_name}', 'public.{collection_name}')"
    ))
    .expect("dense+sparse collection should be created");
    Spi::run(&format!(
        "SELECT pgcontext.register_vector('{collection_name}', 'embedding', 'embedding', 2, 'l2')"
    ))
    .expect("dense vector should be registered");
    Spi::run(&format!(
        "SELECT pgcontext.register_sparse_vector(
            '{collection_name}',
            'lexical',
            'lexical',
            4,
            '{sparse_metric}'
        )"
    ))
    .expect("sparse vector should be registered");
}

fn create_late_interaction_collection(collection_name: &str) {
    Spi::run(&format!(
        "CREATE TABLE public.{collection_name} (
             id bigint PRIMARY KEY,
             token_vectors vector[] NOT NULL
         )"
    ))
    .expect("late-interaction source table should be created");
    Spi::run(&format!(
        "INSERT INTO public.{collection_name} (id, token_vectors)
         VALUES (10, ARRAY['[1,0]'::vector, '[0,1]'::vector]),
                (20, ARRAY['[0.8,0.1]'::vector, '[0.1,0.7]'::vector]),
                (30, ARRAY['[1,0]'::vector, '[1,0]'::vector]),
                (40, ARRAY['[0,0]'::vector])"
    ))
    .expect("late-interaction source rows should be inserted");
    Spi::run(&format!(
        "SELECT pgcontext.create_collection('{collection_name}', 'public.{collection_name}')"
    ))
    .expect("late-interaction collection should be created");
}

fn create_late_interaction_tie_collection(collection_name: &str) {
    Spi::run(&format!(
        "CREATE TABLE public.{collection_name} (
             id bigint PRIMARY KEY,
             token_vectors vector[] NOT NULL
         )"
    ))
    .expect("late-interaction tie source table should be created");
    Spi::run(&format!(
        "INSERT INTO public.{collection_name} (id, token_vectors)
         VALUES (10, ARRAY['[1,0]'::vector]),
                (20, ARRAY['[1,0]'::vector]),
                (30, ARRAY['[1,0]'::vector])"
    ))
    .expect("late-interaction tie source rows should be inserted");
    Spi::run(&format!(
        "SELECT pgcontext.create_collection('{collection_name}', 'public.{collection_name}')"
    ))
    .expect("late-interaction tie collection should be created");
}

fn upsert_hybrid_points(collection_name: &str, source_keys: &[&str]) {
    let source_keys = source_keys
        .iter()
        .map(|source_key| format!("'{source_key}'"))
        .collect::<Vec<_>>()
        .join(", ");
    Spi::run(&format!(
        "SELECT pgcontext.upsert_points('{collection_name}', ARRAY[{source_keys}])"
    ))
    .expect("hybrid points should be upserted");
}

fn hybrid_query_rows(sql: &str) -> Vec<(i64, String, f64)> {
    Spi::connect(|client| {
        let rows = client.select(sql, None, &[])?;
        let mut output = Vec::new();
        for row in rows {
            output.push((
                row.get::<i64>(1)?.expect("point_id should not be null"),
                row.get::<String>(2)?.expect("source_key should not be null"),
                row.get::<f64>(3)?.expect("score should not be null"),
            ));
        }
        Ok::<_, spi::Error>(output)
    })
    .expect("hybrid query failed")
}

fn hybrid_explain_rows(sql: &str) -> Vec<(String, String)> {
    Spi::connect(|client| {
        let rows = client.select(sql, None, &[])?;
        let mut output = Vec::new();
        for row in rows {
            output.push((
                row.get::<String>(1)?.expect("stage should not be null"),
                row.get::<String>(2)?.expect("detail should not be null"),
            ));
        }
        Ok::<_, spi::Error>(output)
    })
    .expect("hybrid explain failed")
}

type StructuredExplainRow = (
    String,
    Option<String>,
    String,
    String,
    Option<i64>,
    Option<i64>,
);

fn hybrid_explain_structured_rows(sql: &str) -> Vec<StructuredExplainRow> {
    Spi::connect(|client| {
        let rows = client.select(sql, None, &[])?;
        let mut output = Vec::new();
        for row in rows {
            output.push((
                row.get::<String>(1)?.expect("stage should not be null"),
                row.get::<String>(2)?,
                row.get::<String>(3)?.expect("strategy should not be null"),
                row.get::<String>(4)?.expect("status should not be null"),
                row.get::<i64>(5)?,
                row.get::<i64>(6)?,
            ));
        }
        Ok::<_, spi::Error>(output)
    })
    .expect("hybrid structured explain failed")
}
