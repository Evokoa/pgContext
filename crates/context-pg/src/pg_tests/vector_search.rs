#[pg_test]
fn exact_search_returns_ranked_rows_with_stable_tie_breaks() {
    let rows = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT point_id, score
                   FROM pgcontext.search(
                        '[0,0]'::vector,
                        ARRAY[30, 10, 20]::bigint[],
                        ARRAY[
                            '[2,0]'::vector,
                            '[1,0]'::vector,
                            '[0,1]'::vector
                        ],
                        'l2',
                        3
                   )",
                None,
                &[],
            )
            .expect("exact search query failed");

        let mut rows = Vec::new();
        for row in result {
            rows.push((
                row.get::<i64>(1)?.unwrap_or_default(),
                row.get::<f32>(2)?.unwrap_or_default(),
            ));
        }
        Ok::<_, spi::Error>(rows)
    })
    .expect("exact search rows failed");

    assert_eq!(rows, vec![(10, 1.0), (20, 1.0), (30, 2.0)]);
}

#[pg_test]
fn exact_search_supports_inner_product_ordering() {
    let rows = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT point_id, score
                   FROM pgcontext.search(
                        '[1,0]'::vector,
                        ARRAY[1, 2, 3]::bigint[],
                        ARRAY[
                            '[1,0]'::vector,
                            '[3,0]'::vector,
                            '[2,0]'::vector
                        ],
                        'inner_product',
                        2
                   )",
                None,
                &[],
            )
            .expect("inner product search query failed");

        let mut rows = Vec::new();
        for row in result {
            rows.push((
                row.get::<i64>(1)?.unwrap_or_default(),
                row.get::<f32>(2)?.unwrap_or_default(),
            ));
        }
        Ok::<_, spi::Error>(rows)
    })
    .expect("inner product rows failed");

    assert_eq!(rows, vec![(2, -3.0), (3, -2.0)]);
}

#[pg_test]
fn sparse_exact_search_returns_ranked_rows_with_stable_tie_breaks() {
    let rows = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT point_id, score
                   FROM pgcontext.search_sparse(
                        pgcontext.sparsevec('{}/4'),
                        ARRAY[30, 10, 20]::bigint[],
                        ARRAY[
                            pgcontext.sparsevec('{1:2}/4'),
                            pgcontext.sparsevec('{1:1}/4'),
                            pgcontext.sparsevec('{2:1}/4')
                        ],
                        'l2',
                        3
                   )",
                None,
                &[],
            )
            .expect("sparse exact search query failed");

        let mut rows = Vec::new();
        for row in result {
            rows.push((
                row.get::<i64>(1)?.unwrap_or_default(),
                row.get::<f32>(2)?.unwrap_or_default(),
            ));
        }
        Ok::<_, spi::Error>(rows)
    })
    .expect("sparse exact search rows failed");

    assert_eq!(rows, vec![(10, 1.0), (20, 1.0), (30, 2.0)]);
}

#[pg_test]
fn sparse_exact_search_supports_inner_product_and_l1_ordering() {
    let rows = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT 'ip' AS metric, point_id, score
                   FROM pgcontext.search_sparse(
                        pgcontext.sparsevec('{1:1}/3'),
                        ARRAY[1, 2, 3]::bigint[],
                        ARRAY[
                            pgcontext.sparsevec('{1:1}/3'),
                            pgcontext.sparsevec('{1:3}/3'),
                            pgcontext.sparsevec('{1:2}/3')
                        ],
                        'inner_product',
                        2
                   )
                 UNION ALL
                 SELECT 'l1' AS metric, point_id, score
                   FROM pgcontext.search_sparse(
                        pgcontext.sparsevec('{1:2,3:1}/4'),
                        ARRAY[7, 8, 9]::bigint[],
                        ARRAY[
                            pgcontext.sparsevec('{1:4}/4'),
                            pgcontext.sparsevec('{1:2,3:2}/4'),
                            pgcontext.sparsevec('{2:9}/4')
                        ],
                        'l1',
                        2
                   )
                  ORDER BY metric DESC, score, point_id",
                None,
                &[],
            )
            .expect("sparse exact metric search query failed");

        let mut rows = Vec::new();
        for row in result {
            rows.push((
                row.get::<String>(1)?.unwrap_or_default(),
                row.get::<i64>(2)?.unwrap_or_default(),
                row.get::<f32>(3)?.unwrap_or_default(),
            ));
        }
        Ok::<_, spi::Error>(rows)
    })
    .expect("sparse exact metric rows failed");

    assert_eq!(
        rows,
        vec![
            ("l1".to_owned(), 8, 1.0),
            ("l1".to_owned(), 7, 3.0),
            ("ip".to_owned(), 2, -3.0),
            ("ip".to_owned(), 3, -2.0),
        ]
    );
}

#[pg_test]
fn sparse_exact_search_supports_cosine_ordering() {
    let rows = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT point_id, score
                   FROM pgcontext.search_sparse(
                        pgcontext.sparsevec('{1:1}/3'),
                        ARRAY[1, 2, 3]::bigint[],
                        ARRAY[
                            pgcontext.sparsevec('{1:1}/3'),
                            pgcontext.sparsevec('{2:1}/3'),
                            pgcontext.sparsevec('{1:1,2:1}/3')
                        ],
                        'cosine',
                        3
                   )",
                None,
                &[],
            )
            .expect("sparse exact cosine search query failed");

        let mut rows = Vec::new();
        for row in result {
            rows.push((
                row.get::<i64>(1)?.unwrap_or_default(),
                row.get::<f32>(2)?.unwrap_or_default(),
            ));
        }
        Ok::<_, spi::Error>(rows)
    })
    .expect("sparse exact cosine rows failed");

    assert_eq!(rows[0], (1, 0.0));
    assert!((rows[1].1 - 0.292_893_23).abs() < 0.000_001);
    assert_eq!(rows[1].0, 3);
    assert_eq!(rows[2], (2, 1.0));
}

#[pg_test]
fn sparse_exact_search_rejects_mismatched_candidate_arrays_with_sqlstate() {
    assert_vector_search_sql_failure(
        "SELECT pgcontext.search_sparse(
                pgcontext.sparsevec('{}/1'),
                ARRAY[1, 2]::bigint[],
                ARRAY[pgcontext.sparsevec('{}/1')],
                'l2',
                1
         )",
        "22023",
        "point_ids and sparse vectors must have the same length: got 2 ids and 1 vectors",
        "sparse exact candidate length mismatch",
    );
}

#[pg_test]
fn sparse_exact_search_rejects_negative_point_ids_with_sqlstate() {
    assert_vector_search_sql_failure(
        "SELECT pgcontext.search_sparse(
                pgcontext.sparsevec('{}/1'),
                ARRAY[-1]::bigint[],
                ARRAY[pgcontext.sparsevec('{}/1')],
                'l2',
                1
         )",
        "22023",
        "point id must be non-negative: -1",
        "sparse exact negative point id",
    );
}

#[pg_test]
fn sparse_exact_search_rejects_dimension_mismatch_with_sqlstate() {
    assert_vector_search_sql_failure(
        "SELECT pgcontext.search_sparse(
                pgcontext.sparsevec('{}/1'),
                ARRAY[1]::bigint[],
                ARRAY[pgcontext.sparsevec('{}/2')],
                'l2',
                1
         )",
        "22023",
        "dimension mismatch: left has 1 dimensions, right has 2",
        "sparse exact dimension mismatch",
    );
}

#[pg_test]
fn sparse_exact_search_rejects_unknown_metric_with_sqlstate() {
    assert_vector_search_sql_failure(
        "SELECT pgcontext.search_sparse(
                pgcontext.sparsevec('{}/1'),
                ARRAY[1]::bigint[],
                ARRAY[pgcontext.sparsevec('{}/1')],
                'bad_metric',
                1
         )",
        "22023",
        "unsupported sparse distance metric: bad_metric",
        "sparse exact unknown metric",
    );
}

#[pg_test]
fn sparse_exact_search_rejects_bit_metric_with_sqlstate() {
    assert_vector_search_sql_failure(
        "SELECT pgcontext.search_sparse(
                pgcontext.sparsevec('{}/1'),
                ARRAY[1]::bigint[],
                ARRAY[pgcontext.sparsevec('{}/1')],
                'hamming',
                1
         )",
        "22023",
        "unsupported sparse distance metric: hamming",
        "sparse exact bit metric",
    );
}

#[pg_test]
fn sparse_exact_search_rejects_cosine_zero_vector_with_sqlstate() {
    assert_vector_search_sql_failure(
        "SELECT pgcontext.search_sparse(
                pgcontext.sparsevec('{}/3'),
                ARRAY[1]::bigint[],
                ARRAY[pgcontext.sparsevec('{1:1}/3')],
                'cosine',
                1
         )",
        "22P02",
        "invalid vector: sparse cosine distance is undefined for zero vectors",
        "sparse exact cosine zero query vector",
    );
}

#[pg_test]
fn sparse_exact_search_rejects_invalid_limit_with_sqlstate() {
    assert_vector_search_sql_failure(
        "SELECT pgcontext.search_sparse(
                pgcontext.sparsevec('{}/1'),
                ARRAY[1]::bigint[],
                ARRAY[pgcontext.sparsevec('{}/1')],
                'l2',
                0
         )",
        "22023",
        "invalid search limit: 0",
        "sparse exact invalid limit",
    );
}

#[pg_test]
fn sparse_table_search_uses_registered_sparse_vector_metric() {
    create_sparse_search_collection("m14_sparse_table", "inner_product");
    upsert_sparse_search_points("m14_sparse_table", &["10", "20", "30"]);

    let rows = sparse_table_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.search_sparse(
                'm14_sparse_table',
                'lexical',
                pgcontext.sparsevec('{1:1}/4'),
                3
           )",
    );

    assert_eq!(
        rows.into_iter()
            .map(|(_point_id, source_key, score)| (source_key, score))
            .collect::<Vec<_>>(),
        vec![
            ("20".to_owned(), -3.0),
            ("30".to_owned(), -2.0),
            ("10".to_owned(), -1.0),
        ]
    );
}

#[pg_test]
fn sparse_table_search_supports_registered_cosine_metric() {
    create_sparse_search_collection("m14_sparse_table_cosine", "cosine");
    upsert_sparse_search_points("m14_sparse_table_cosine", &["10", "20", "30"]);

    let rows = sparse_table_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.search_sparse(
                'm14_sparse_table_cosine',
                'lexical',
                pgcontext.sparsevec('{1:1}/4'),
                3
           )",
    );

    let rows = rows
        .into_iter()
        .map(|(_point_id, source_key, score)| (source_key, score))
        .collect::<Vec<_>>();
    assert_eq!(rows[0], ("10".to_owned(), 0.0));
    assert_eq!(rows[1], ("20".to_owned(), 0.0));
    assert_eq!(rows[2], ("30".to_owned(), 0.0));
}

#[pg_test]
fn sparse_table_search_rejects_registered_cosine_zero_vector_with_sqlstate() {
    create_sparse_search_collection("m14_sparse_table_cosine_zero", "cosine");
    upsert_sparse_search_points("m14_sparse_table_cosine_zero", &["10"]);

    assert_vector_search_sql_failure(
        "SELECT point_id, source_key, score
           FROM pgcontext.search_sparse(
                'm14_sparse_table_cosine_zero',
                'lexical',
                pgcontext.sparsevec('{}/4'),
                3
           )",
        "22P02",
        "invalid vector: sparse cosine distance is undefined for zero vectors",
        "sparse table cosine zero query vector",
    );
}

#[pg_test]
fn sparse_table_search_excludes_deleted_points_and_breaks_ties() {
    create_sparse_search_collection("m14_sparse_table_ties", "l2");
    upsert_sparse_search_points("m14_sparse_table_ties", &["10", "20", "30"]);
    Spi::run("SELECT pgcontext.delete_points('m14_sparse_table_ties', ARRAY['20'])")
        .expect("sparse point should be deleted");

    let rows = sparse_table_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.search_sparse(
                'm14_sparse_table_ties',
                'lexical',
                pgcontext.sparsevec('{}/4'),
                3
           )",
    );

    assert_eq!(
        rows.into_iter()
            .map(|(_point_id, source_key, score)| (source_key, score))
            .collect::<Vec<_>>(),
        vec![("10".to_owned(), 1.0), ("30".to_owned(), 2.0)]
    );
}

#[pg_test]
fn sparse_table_search_rejects_missing_sparse_registration_with_sqlstate() {
    create_sparse_search_source_table("m14_sparse_missing_reg");
    Spi::run(
        "SELECT pgcontext.create_collection(
            'm14_sparse_missing_reg',
            'public.m14_sparse_missing_reg'
        )",
    )
    .expect("sparse collection should be created");

    assert_vector_search_sql_failure(
        "SELECT pgcontext.search_sparse(
            'm14_sparse_missing_reg',
            'lexical',
            pgcontext.sparsevec('{}/4'),
            3
        )",
        "42704",
        "sparse vector registration does not exist for collection m14_sparse_missing_reg: lexical",
        "sparse table missing registration",
    );
}

#[pg_test]
fn sparse_table_search_rejects_sparse_column_drift_with_sqlstate() {
    create_sparse_search_collection("m14_sparse_drift", "l2");
    Spi::run("ALTER TABLE public.m14_sparse_drift DROP COLUMN lexical")
        .expect("sparse vector column should be dropped");

    assert_vector_search_sql_failure(
        "SELECT pgcontext.search_sparse(
            'm14_sparse_drift',
            'lexical',
            pgcontext.sparsevec('{}/4'),
            3
        )",
        "42703",
        "registered sparse vector column drifted: m14_sparse_drift.lexical",
        "sparse table column drift",
    );
}

#[pg_test]
fn sparse_table_search_rejects_dimension_mismatch_with_sqlstate() {
    create_sparse_search_collection("m14_sparse_dim_mismatch", "l2");
    upsert_sparse_search_points("m14_sparse_dim_mismatch", &["10"]);

    assert_vector_search_sql_failure(
        "SELECT pgcontext.search_sparse(
            'm14_sparse_dim_mismatch',
            'lexical',
            pgcontext.sparsevec('{}/2'),
            3
        )",
        "22023",
        "dimension mismatch: left has 4 dimensions, right has 2",
        "sparse table dimension mismatch",
    );
}

#[pg_test]
fn named_sparse_ann_exactly_rechecks_bounded_hnsw_candidates() {
    Spi::run(
        "CREATE TABLE public.m14_sparse_ann (
             id bigint PRIMARY KEY,
             lexical sparsevec NOT NULL
         );
         INSERT INTO public.m14_sparse_ann (id, lexical)
         SELECT value,
                pg_catalog.format('{1:%s,2:%s}/4', value, value % 7)::sparsevec
           FROM generate_series(1, 256) AS value;
         SELECT pgcontext.create_collection('m14_sparse_ann', 'public.m14_sparse_ann');
         SELECT pgcontext.register_sparse_vector(
             'm14_sparse_ann', 'lexical', 'lexical', 4, 'l2'
         );
         SELECT pgcontext.upsert_points(
             'm14_sparse_ann',
             ARRAY(SELECT value::text FROM generate_series(1, 256) AS value)
         );",
    )
    .expect("sparse ANN fixture should be created");

    let exact = sparse_table_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.search_sparse(
                'm14_sparse_ann', 'lexical',
                pgcontext.sparsevec('{1:128,2:2}/4'), 5
           )",
    );
    let exact_strategy = Spi::get_one::<String>(
        "SELECT strategy FROM pgcontext.explain_sparse(
             'm14_sparse_ann', 'lexical',
             pgcontext.sparsevec('{1:128,2:2}/4'), 5
         )",
    )
    .expect("exact sparse explain should execute")
    .expect("exact sparse explain should return a row");
    assert_eq!(exact_strategy, "exact");

    Spi::run(
        "CREATE INDEX m14_sparse_ann_hnsw
             ON public.m14_sparse_ann USING pgcontext_hnsw
             (lexical pgcontext.sparsevec_hnsw_ops);
         SELECT pgcontext.attach_sparse_hnsw_index(
             'm14_sparse_ann', 'lexical', 'public.m14_sparse_ann_hnsw'
         );",
    )
    .expect("sparse HNSW index should attach");

    let ann = sparse_table_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.search_sparse(
                'm14_sparse_ann', 'lexical',
                pgcontext.sparsevec('{1:128,2:2}/4'), 5
           )",
    );
    assert_eq!(ann, exact, "ANN candidates must be exactly reranked");

    let explain = Spi::connect(|client| {
        let row = client
            .select(
                "SELECT strategy, active_points, scored_count, candidate_count, recheck_count
                   FROM pgcontext.explain_sparse(
                        'm14_sparse_ann', 'lexical',
                        pgcontext.sparsevec('{1:128,2:2}/4'), 5
                   )",
                Some(1),
                &[],
            )?
            .first();
        Ok::<_, spi::Error>((
            row.get::<String>(1)?.expect("strategy should exist"),
            row.get::<i64>(2)?.expect("active points should exist"),
            row.get::<i64>(3)?.expect("scored count should exist"),
            row.get::<i64>(4)?.expect("candidate count should exist"),
            row.get::<i64>(5)?.expect("recheck count should exist"),
        ))
    })
    .expect("sparse ANN explain should execute");
    assert_eq!(explain.0, "hnsw");
    assert!(explain.2 < explain.1, "HNSW should score less than the collection");
    assert!(explain.3 < explain.1, "HNSW should produce bounded candidates");
    assert_eq!(explain.3, explain.4, "every candidate must be exactly rechecked");
}

#[pg_test]
fn named_sparse_ann_reports_exact_delta_scoring_work() {
    Spi::run(
        "CREATE TABLE public.m14_sparse_delta_work (
             id bigint PRIMARY KEY,
             lexical sparsevec NOT NULL
         );
         SELECT pgcontext.create_collection(
             'm14_sparse_delta_work', 'public.m14_sparse_delta_work'
         );
         SELECT pgcontext.register_sparse_vector(
             'm14_sparse_delta_work', 'lexical', 'lexical', 4, 'l2'
         );
         CREATE INDEX m14_sparse_delta_work_hnsw
             ON public.m14_sparse_delta_work USING pgcontext_hnsw
             (lexical pgcontext.sparsevec_hnsw_ops);
         SELECT pgcontext.attach_sparse_hnsw_index(
             'm14_sparse_delta_work', 'lexical',
             'public.m14_sparse_delta_work_hnsw'
         );
         INSERT INTO public.m14_sparse_delta_work (id, lexical)
         SELECT value, pg_catalog.format('{1:%s,2:%s}/4', value, value % 9)::sparsevec
           FROM generate_series(1, 128) AS value;
         SELECT pgcontext.upsert_points(
             'm14_sparse_delta_work',
             ARRAY(SELECT value::text FROM generate_series(1, 128) AS value)
         );",
    )
    .expect("sparse delta-work fixture should build");

    let before = sparse_explain_work(
        "m14_sparse_delta_work",
        "pgcontext.sparsevec('{1:64,2:1}/4')",
    );
    assert_eq!(before.0, 128);
    assert_eq!(
        before.1, before.0,
        "an empty-built index must report every exactly scored live delta vector"
    );

    Spi::run("REINDEX INDEX public.m14_sparse_delta_work_hnsw")
        .expect("sparse delta-work index should reindex");
    let after = sparse_explain_work(
        "m14_sparse_delta_work",
        "pgcontext.sparsevec('{1:64,2:1}/4')",
    );
    assert_eq!(after.0, before.0);
    assert!(
        after.1 < after.0,
        "after REINDEX publishes the rows into the base graph, reported scoring work must be bounded"
    );
}

fn sparse_explain_work(collection: &str, query: &str) -> (i64, i64) {
    Spi::connect(|client| {
        let sql = format!(
            "SELECT active_points, scored_count
               FROM pgcontext.explain_sparse(
                    '{collection}', 'lexical', {query}, 5
               )"
        );
        let row = client.select(&sql, Some(1), &[])?.first();
        Ok::<_, spi::Error>((
            row.get::<i64>(1)?.expect("active point count should exist"),
            row.get::<i64>(2)?.expect("scored count should exist"),
        ))
    })
    .expect("sparse explain work should load")
}

#[pg_test]
fn sparse_hnsw_internal_candidate_helpers_reject_direct_sql_calls() {
    Spi::run(
        "CREATE TABLE public.m14_sparse_helper_acl (
             id bigint PRIMARY KEY,
             lexical sparsevec NOT NULL
         );
         INSERT INTO public.m14_sparse_helper_acl VALUES (1, '{1:1}/1');
         CREATE INDEX m14_sparse_helper_acl_hnsw
             ON public.m14_sparse_helper_acl USING pgcontext_hnsw
             (lexical pgcontext.sparsevec_hnsw_ops);",
    )
    .expect("sparse helper ACL fixture should build");
    for helper_call in [
        "SELECT * FROM pgcontext._hnsw_sparse_candidates(
             'public.m14_sparse_helper_acl_hnsw'::regclass,
             pgcontext.sparsevec('{1:1}/1'),
             1
         )",
        "SELECT * FROM pgcontext._hnsw_sparse_masked_candidates(
             'public.m14_sparse_helper_acl_hnsw'::regclass,
             pgcontext.sparsevec('{1:1}/1'),
             ARRAY[]::tid[],
             1
         )",
        "SELECT * FROM pgcontext._hnsw_candidates(
             'public.m14_sparse_helper_acl_hnsw'::regclass,
             '[1]'::vector,
             1
         )",
        "SELECT * FROM pgcontext._hnsw_masked_candidates(
             'public.m14_sparse_helper_acl_hnsw'::regclass,
             '[1]'::vector,
             ARRAY[]::tid[],
             1
         )",
    ] {
        assert_vector_search_sql_failure(
            helper_call,
            "42501",
            "pgcontext internal HNSW candidate helper cannot be called directly",
            "direct HNSW candidate helper call",
        );
    }
}

#[pg_test]
fn hnsw_candidate_helper_capability_is_bound_to_one_index() {
    Spi::run(
        "CREATE TABLE public.m14_helper_capability (
             id bigint PRIMARY KEY,
             lexical sparsevec NOT NULL
         );
         INSERT INTO public.m14_helper_capability VALUES (1, '{1:1}/1');
         CREATE INDEX m14_helper_capability_hnsw
             ON public.m14_helper_capability USING pgcontext_hnsw
             (lexical pgcontext.sparsevec_hnsw_ops);",
    )
    .expect("candidate-helper capability fixture should build");
    let wrong_index_oid = Spi::get_one::<pg_sys::Oid>(
        "SELECT 'public.m14_helper_capability_pkey'::regclass::oid",
    )
    .expect("wrong capability index should load")
    .expect("wrong capability index should exist");

    crate::hnsw_am::with_hnsw_candidate_helper_capability(wrong_index_oid, || {
        assert_vector_search_sql_failure(
            "SELECT * FROM pgcontext._hnsw_candidates(
                 'public.m14_helper_capability_hnsw'::regclass,
                 '[1]'::vector,
                 1
             )",
            "42501",
            "pgcontext internal HNSW candidate helper cannot be called directly",
            "candidate helper capability bound to another index",
        );
    });
}

#[pg_test]
fn named_sparse_ann_follows_hot_updated_source_rows() {
    Spi::run(
        "CREATE TABLE public.m14_sparse_hot (
             id bigint PRIMARY KEY,
             lexical sparsevec NOT NULL,
             note text NOT NULL
         ) WITH (fillfactor = 50);
         INSERT INTO public.m14_sparse_hot (id, lexical, note)
         SELECT value, pg_catalog.format('{1:%s}/4', value)::sparsevec, 'before'
           FROM generate_series(1, 96) AS value;
         SELECT pgcontext.create_collection('m14_sparse_hot', 'public.m14_sparse_hot');
         SELECT pgcontext.register_sparse_vector(
             'm14_sparse_hot', 'lexical', 'lexical', 4, 'l2'
         );
         SELECT pgcontext.upsert_points(
             'm14_sparse_hot', ARRAY(SELECT value::text FROM generate_series(1, 96) AS value)
         );
         CREATE INDEX m14_sparse_hot_hnsw
             ON public.m14_sparse_hot USING pgcontext_hnsw
             (lexical pgcontext.sparsevec_hnsw_ops);
         SELECT pgcontext.attach_sparse_hnsw_index(
             'm14_sparse_hot', 'lexical', 'public.m14_sparse_hot_hnsw'
         );",
    )
    .expect("sparse HOT fixture should build");
    let original_tid = Spi::get_one::<String>(
        "SELECT ctid::text FROM public.m14_sparse_hot WHERE id = 48",
    )
    .expect("original source TID should load")
    .expect("source row should exist");
    Spi::run("UPDATE public.m14_sparse_hot SET note = 'after' WHERE id = 48")
        .expect("non-indexed HOT update should succeed");
    let updated_tid = Spi::get_one::<String>(
        "SELECT ctid::text FROM public.m14_sparse_hot WHERE id = 48",
    )
    .expect("updated source TID should load")
    .expect("updated source row should exist");
    assert_ne!(original_tid, updated_tid, "fixture must create a new heap tuple");

    let rows = sparse_table_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.search_sparse(
                'm14_sparse_hot', 'lexical',
                pgcontext.sparsevec('{1:48}/4'), 1
           )",
    );
    assert_eq!(
        rows.first().map(|row| row.1.as_str()),
        Some("48"),
        "the index root TID must resolve to the visible HOT successor"
    );
}

#[pg_test]
fn named_sparse_ann_uses_registered_filter_masks() {
    Spi::run(
        "CREATE TABLE public.m14_sparse_ann_filter (
             id bigint PRIMARY KEY,
             lexical sparsevec NOT NULL,
             tenant text NOT NULL
         );
         INSERT INTO public.m14_sparse_ann_filter (id, lexical, tenant)
         SELECT value,
                pg_catalog.format('{1:%s,2:%s}/4', value, value % 11)::sparsevec,
                CASE WHEN value % 2 = 0 THEN 'even' ELSE 'odd' END
           FROM generate_series(1, 256) AS value;
         SELECT pgcontext.create_collection(
             'm14_sparse_ann_filter', 'public.m14_sparse_ann_filter'
         );
         SELECT pgcontext.register_sparse_vector(
             'm14_sparse_ann_filter', 'lexical', 'lexical', 4, 'l2'
         );
         SELECT pgcontext.register_filter_column(
             'm14_sparse_ann_filter', 'tenant', 'tenant'
         );
         SELECT pgcontext.upsert_points(
             'm14_sparse_ann_filter',
             ARRAY(SELECT value::text FROM generate_series(1, 256) AS value)
         );",
    )
    .expect("filtered sparse ANN fixture should be created");

    let search_sql = "SELECT point_id, source_key, score
           FROM pgcontext.search_sparse(
                'm14_sparse_ann_filter', 'lexical',
                pgcontext.sparsevec('{1:128,2:7}/4'),
                '{\"must\":[{\"key\":\"tenant\",\"match\":\"even\"}]}',
                5
           )";
    let exact = sparse_table_rows(search_sql);

    Spi::run(
        "CREATE INDEX m14_sparse_ann_filter_hnsw
             ON public.m14_sparse_ann_filter USING pgcontext_hnsw
             (lexical pgcontext.sparsevec_hnsw_ops);
         SELECT pgcontext.attach_sparse_hnsw_index(
             'm14_sparse_ann_filter', 'lexical',
             'public.m14_sparse_ann_filter_hnsw'
         );",
    )
    .expect("filtered sparse HNSW index should attach");

    let ann = sparse_table_rows(search_sql);
    assert_eq!(ann, exact, "masked sparse ANN must retain exact reranking");
    assert!(
        ann.iter()
            .all(|(_, source_key, _)| source_key.parse::<i64>().unwrap_or_default() % 2 == 0),
        "the sparse candidate mask must exclude nonmatching source rows"
    );
    let exact_strategy = Spi::get_one::<bool>(
        "SELECT exact_strategy FROM pgcontext.hnsw_last_scan_work()",
    )
    .expect("masked sparse work should load")
    .expect("masked sparse work should be present");
    assert!(!exact_strategy, "the filtered path should traverse sparse HNSW");

    Spi::run("SET LOCAL pgcontext.hnsw_mask_candidate_limit = 0")
        .expect("zero mask budget should be accepted");
    Spi::run(
        "SELECT pgcontext.configure_collection_limits(
             'm14_sparse_ann_filter', true,
             NULL, NULL, NULL, NULL, NULL, 5, NULL, NULL
         )",
    )
    .expect("strict fallback candidate budget should configure");
    let fallback = sparse_table_rows(search_sql);
    assert_eq!(fallback, exact, "zero mask budget must retain exact results");
}

#[pg_test]
fn named_sparse_ann_applies_filter_mask_to_post_build_delta() {
    Spi::run(
        "CREATE TABLE public.m14_sparse_masked_delta (
             id bigint PRIMARY KEY,
             lexical sparsevec NOT NULL,
             tenant text NOT NULL
         );
         INSERT INTO public.m14_sparse_masked_delta (id, lexical, tenant)
         SELECT value,
                pg_catalog.format('{1:%s}/4', value)::sparsevec,
                'matching'
           FROM generate_series(100, 107) AS value;
         SELECT pgcontext.create_collection(
             'm14_sparse_masked_delta', 'public.m14_sparse_masked_delta'
         );
         SELECT pgcontext.register_sparse_vector(
             'm14_sparse_masked_delta', 'lexical', 'lexical', 4, 'l2'
         );
         SELECT pgcontext.register_filter_column(
             'm14_sparse_masked_delta', 'tenant', 'tenant'
         );
         SELECT pgcontext.upsert_points(
             'm14_sparse_masked_delta',
             ARRAY(SELECT value::text FROM generate_series(100, 107) AS value)
         );
         CREATE INDEX m14_sparse_masked_delta_hnsw
             ON public.m14_sparse_masked_delta USING pgcontext_hnsw
             (lexical pgcontext.sparsevec_hnsw_ops);
         SELECT pgcontext.attach_sparse_hnsw_index(
             'm14_sparse_masked_delta', 'lexical',
             'public.m14_sparse_masked_delta_hnsw'
         );

         -- These closer rows are written after the graph build, so the AM
         -- serves them from its exact-scanned delta rather than the base graph.
         INSERT INTO public.m14_sparse_masked_delta (id, lexical, tenant)
         SELECT value,
                pg_catalog.format('{1:%s}/4', value)::sparsevec,
                'nonmatching'
           FROM generate_series(1, 64) AS value;
         SELECT pgcontext.upsert_points(
             'm14_sparse_masked_delta',
             ARRAY(SELECT value::text FROM generate_series(1, 64) AS value)
         );
         SET LOCAL pgcontext.hnsw_candidate_budget = 8;",
    )
    .expect("masked sparse delta fixture should build");

    let rows = sparse_table_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.search_sparse(
                'm14_sparse_masked_delta', 'lexical',
                pgcontext.sparsevec('{}/4'),
                '{\"must\":[{\"key\":\"tenant\",\"match\":\"matching\"}]}',
                3
           )",
    );
    assert_eq!(rows.len(), 3, "masked delta rows must not consume base candidates");
    assert!(
        rows.iter().all(|(_, source_key, _)| {
            source_key.parse::<i64>().is_ok_and(|value| value >= 100)
        }),
        "only matching base-graph rows should survive exact recheck"
    );
}

#[pg_test]
fn named_sparse_ann_honors_raised_filter_mask_budget() {
    Spi::run(
        "CREATE TABLE public.m14_sparse_large_mask (
             id bigint PRIMARY KEY,
             lexical sparsevec NOT NULL,
             tenant text NOT NULL
         );
         INSERT INTO public.m14_sparse_large_mask (id, lexical, tenant)
         SELECT value,
                pg_catalog.format('{1:%s,2:%s}/4', value, value % 17)::sparsevec,
                'all'
           FROM generate_series(1, 10001) AS value;
         SELECT pgcontext.create_collection(
             'm14_sparse_large_mask', 'public.m14_sparse_large_mask'
         );
         SELECT pgcontext.register_sparse_vector(
             'm14_sparse_large_mask', 'lexical', 'lexical', 4, 'l2'
         );
         SELECT pgcontext.register_filter_column(
             'm14_sparse_large_mask', 'tenant', 'tenant'
         );
         SELECT pgcontext.upsert_points(
             'm14_sparse_large_mask',
             ARRAY(SELECT value::text FROM generate_series(1, 10000) AS value)
         );
         SELECT pgcontext.upsert_points('m14_sparse_large_mask', ARRAY['10001']);
         CREATE INDEX m14_sparse_large_mask_hnsw
             ON public.m14_sparse_large_mask USING pgcontext_hnsw
             (lexical pgcontext.sparsevec_hnsw_ops);
         SELECT pgcontext.attach_sparse_hnsw_index(
             'm14_sparse_large_mask', 'lexical',
             'public.m14_sparse_large_mask_hnsw'
         );
         SET LOCAL pgcontext.hnsw_mask_candidate_limit = 20000;",
    )
    .expect("large sparse mask fixture should build");

    let rows = sparse_table_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.search_sparse(
                'm14_sparse_large_mask', 'lexical',
                pgcontext.sparsevec('{1:5000,2:2}/4'),
                '{\"must\":[{\"key\":\"tenant\",\"match\":\"all\"}]}',
                5
           )",
    );
    assert_eq!(rows.len(), 5);
    let exact_strategy = Spi::get_one::<bool>(
        "SELECT exact_strategy FROM pgcontext.hnsw_last_scan_work()",
    )
    .expect("large sparse mask work should load")
    .expect("large sparse mask work should be present");
    assert!(
        !exact_strategy,
        "a raised mask budget must reach the sparse HNSW candidate source"
    );
}

#[pg_test]
fn named_sparse_ann_binds_and_serves_every_sparse_metric() {
    for (suffix, metric, opclass) in [
        ("l2", "l2", "sparsevec_hnsw_ops"),
        ("ip", "inner_product", "sparsevec_hnsw_ip_ops"),
        ("cos", "cosine", "sparsevec_hnsw_cosine_ops"),
        ("l1", "l1", "sparsevec_hnsw_l1_ops"),
    ] {
        let table = format!("m14_sparse_metric_{suffix}");
        Spi::run(&format!(
            "CREATE TABLE public.{table} (
                 id bigint PRIMARY KEY,
                 lexical sparsevec NOT NULL
             );
             INSERT INTO public.{table} (id, lexical)
             SELECT value,
                    pg_catalog.format('{{1:%s,2:%s}}/4', value, value % 13)::sparsevec
               FROM generate_series(1, 96) AS value;
             SELECT pgcontext.create_collection('{table}', 'public.{table}');
             SELECT pgcontext.register_sparse_vector(
                 '{table}', 'lexical', 'lexical', 4, '{metric}'
             );
             SELECT pgcontext.upsert_points(
                 '{table}', ARRAY(SELECT value::text FROM generate_series(1, 96) AS value)
             );"
        ))
        .expect("sparse metric fixture should be created");
        let search_sql = format!(
            "SELECT point_id, source_key, score
               FROM pgcontext.search_sparse(
                    '{table}', 'lexical',
                    pgcontext.sparsevec('{{1:48,2:9}}/4'), 5
               )"
        );
        let exact = sparse_table_rows(&search_sql);
        Spi::run(&format!(
            "CREATE INDEX {table}_hnsw
                 ON public.{table} USING pgcontext_hnsw
                 (lexical pgcontext.{opclass});
             SELECT pgcontext.attach_sparse_hnsw_index(
                 '{table}', 'lexical', 'public.{table}_hnsw'
             );"
        ))
        .expect("metric-matched sparse HNSW index should attach");
        let ann = sparse_table_rows(&search_sql);
        assert_eq!(ann, exact, "named sparse {metric} rerank must match exact");
    }
}

#[pg_test]
fn sparse_hnsw_attachment_rejects_partial_and_wrong_metric_indexes() {
    create_sparse_search_collection("m14_sparse_bad_attach", "l2");
    Spi::run(
        "CREATE INDEX m14_sparse_bad_attach_partial
             ON public.m14_sparse_bad_attach USING pgcontext_hnsw
             (lexical pgcontext.sparsevec_hnsw_ops)
             WHERE id > 0;
         CREATE INDEX m14_sparse_bad_attach_cosine
             ON public.m14_sparse_bad_attach USING pgcontext_hnsw
             (lexical pgcontext.sparsevec_hnsw_cosine_ops);",
    )
    .expect("invalid attachment probes should build");

    for index_name in [
        "public.m14_sparse_bad_attach_partial",
        "public.m14_sparse_bad_attach_cosine",
    ] {
        assert_vector_search_sql_failure(
            &format!(
                "SELECT pgcontext.attach_sparse_hnsw_index(
                     'm14_sparse_bad_attach', 'lexical', '{index_name}'
                 )"
            ),
            "22023",
            "HNSW index does not match the registered sparse collection vector",
            "invalid sparse HNSW attachment",
        );
    }
}

#[pg_test]
fn sparse_hnsw_attachment_rejects_partitioned_parent_index() {
    Spi::run(
        "CREATE TABLE public.m14_sparse_partitioned (
             id bigint NOT NULL,
             lexical sparsevec NOT NULL
         ) PARTITION BY RANGE (id);
         CREATE TABLE public.m14_sparse_partitioned_p1
             PARTITION OF public.m14_sparse_partitioned
             FOR VALUES FROM (0) TO (100);
         SELECT pgcontext.create_collection(
             'm14_sparse_partitioned', 'public.m14_sparse_partitioned'
         );
         SELECT pgcontext.register_sparse_vector(
             'm14_sparse_partitioned', 'lexical', 'lexical', 4, 'l2'
         );
         CREATE INDEX m14_sparse_partitioned_hnsw
             ON public.m14_sparse_partitioned USING pgcontext_hnsw
             (lexical pgcontext.sparsevec_hnsw_ops);",
    )
    .expect("partitioned sparse fixture should build");

    assert_vector_search_sql_failure(
        "SELECT pgcontext.attach_sparse_hnsw_index(
             'm14_sparse_partitioned', 'lexical',
             'public.m14_sparse_partitioned_hnsw'
         )",
        "22023",
        "HNSW index does not match the registered sparse collection vector",
        "partitioned parent sparse HNSW attachment",
    );
}

#[pg_test]
fn named_sparse_ann_masks_logically_deleted_nearer_rows() {
    Spi::run(
        "CREATE TABLE public.m14_sparse_active_mask (
             id bigint PRIMARY KEY,
             lexical sparsevec NOT NULL
         );
         INSERT INTO public.m14_sparse_active_mask (id, lexical)
         SELECT value, pg_catalog.format('{1:%s}/4', value)::sparsevec
           FROM generate_series(1, 64) AS value;
         INSERT INTO public.m14_sparse_active_mask (id, lexical)
         VALUES (100, pgcontext.sparsevec('{1:100}/4'));
         SELECT pgcontext.create_collection(
             'm14_sparse_active_mask', 'public.m14_sparse_active_mask'
         );
         SELECT pgcontext.register_sparse_vector(
             'm14_sparse_active_mask', 'lexical', 'lexical', 4, 'l2'
         );
         SELECT pgcontext.upsert_points(
             'm14_sparse_active_mask',
             ARRAY(
                 SELECT value::text FROM generate_series(1, 64) AS value
                 UNION ALL SELECT '100'
             )
         );
         CREATE INDEX m14_sparse_active_mask_hnsw
             ON public.m14_sparse_active_mask USING pgcontext_hnsw
             (lexical pgcontext.sparsevec_hnsw_ops);
         SELECT pgcontext.attach_sparse_hnsw_index(
             'm14_sparse_active_mask', 'lexical',
             'public.m14_sparse_active_mask_hnsw'
         );
         SELECT pgcontext.delete_points(
             'm14_sparse_active_mask',
             ARRAY(SELECT value::text FROM generate_series(1, 64) AS value)
         );
         SET LOCAL pgcontext.hnsw_candidate_budget = 8;",
    )
    .expect("active sparse mask fixture should build");

    let rows = sparse_table_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.search_sparse(
                'm14_sparse_active_mask', 'lexical',
                pgcontext.sparsevec('{}/4'), 1
           )",
    );
    assert_eq!(rows.first().map(|row| row.1.as_str()), Some("100"));

    Spi::run("SELECT pgcontext.delete_points('m14_sparse_active_mask', ARRAY['100'])")
        .expect("last active sparse point should delete");
    let (strategy, scored_count) = Spi::get_two::<String, i64>(
        "SELECT strategy, scored_count
           FROM pgcontext.explain_sparse(
                'm14_sparse_active_mask', 'lexical',
                pgcontext.sparsevec('{}/4'), 1
           )",
    )
    .expect("empty sparse explain should execute");
    assert_eq!(strategy.as_deref(), Some("exact"));
    assert_eq!(scored_count, Some(0));
}

#[pg_test]
fn named_sparse_ann_survives_dml_reindex_and_falls_back_after_drop() {
    Spi::run(
        "CREATE TABLE public.m14_sparse_ann_lifecycle (
             id bigint PRIMARY KEY,
             lexical sparsevec NOT NULL
         );
         INSERT INTO public.m14_sparse_ann_lifecycle (id, lexical)
         SELECT value, pg_catalog.format('{1:%s}/4', value)::sparsevec
           FROM generate_series(1, 96) AS value;
         SELECT pgcontext.create_collection(
             'm14_sparse_ann_lifecycle', 'public.m14_sparse_ann_lifecycle'
         );
         SELECT pgcontext.register_sparse_vector(
             'm14_sparse_ann_lifecycle', 'lexical', 'lexical', 4, 'l2'
         );
         SELECT pgcontext.upsert_points(
             'm14_sparse_ann_lifecycle',
             ARRAY(SELECT value::text FROM generate_series(1, 96) AS value)
         );
         CREATE INDEX m14_sparse_ann_lifecycle_hnsw
             ON public.m14_sparse_ann_lifecycle USING pgcontext_hnsw
             (lexical pgcontext.sparsevec_hnsw_ops);
         SELECT pgcontext.attach_sparse_hnsw_index(
             'm14_sparse_ann_lifecycle', 'lexical',
             'public.m14_sparse_ann_lifecycle_hnsw'
         );
         UPDATE public.m14_sparse_ann_lifecycle
            SET lexical = pgcontext.sparsevec('{1:48}/4')
          WHERE id = 96;
         DELETE FROM public.m14_sparse_ann_lifecycle WHERE id = 48;
         SELECT pgcontext.delete_points(
             'm14_sparse_ann_lifecycle', ARRAY['47']
         );",
    )
    .expect("sparse HNSW lifecycle operations should complete");

    let search_sql = "SELECT point_id, source_key, score
           FROM pgcontext.search_sparse(
                'm14_sparse_ann_lifecycle', 'lexical',
                pgcontext.sparsevec('{1:48}/4'), 5
           )";
    let exact_sql = "SELECT points.point_id,
                            points.source_key,
                            pgcontext.sparsevec_l2_distance(
                                source.lexical,
                                pgcontext.sparsevec('{1:48}/4')
                            ) AS score
                       FROM pgcontext._visible_collection_points AS points
                       JOIN public.m14_sparse_ann_lifecycle AS source
                         ON source.id::text = points.source_key
                      WHERE points.collection_id = (
                                SELECT collection_id
                                  FROM pgcontext._collection_acl
                                 WHERE collection_name = 'm14_sparse_ann_lifecycle'
                            )
                        AND points.deleted_at IS NULL
                      ORDER BY score, points.point_id
                      LIMIT 5";
    let delta_ann = sparse_table_rows(search_sql);
    let delta_exact = sparse_table_rows(exact_sql);
    assert_eq!(
        delta_ann, delta_exact,
        "live delta update/delete state must match the exact sparse oracle"
    );

    Spi::run("REINDEX INDEX public.m14_sparse_ann_lifecycle_hnsw")
        .expect("sparse lifecycle index should rebuild");
    let ann = sparse_table_rows(search_sql);
    assert_eq!(ann, delta_exact, "rebuilt sparse ANN must match the exact oracle");
    assert_eq!(ann.first().map(|row| row.1.as_str()), Some("96"));
    assert!(!ann.iter().any(|row| row.1 == "47" || row.1 == "48"));

    Spi::run(
        "SELECT pgcontext.configure_sparse_vector(
             'm14_sparse_ann_lifecycle', 'lexical',
             '{}'::jsonb, '{}'::jsonb, 'ready'
         )",
    )
    .expect("sparse configuration change should clear the old index binding");
    let configured_fallback = sparse_table_rows(search_sql);
    assert_eq!(configured_fallback, ann);
    let configured_strategy = Spi::get_one::<String>(
        "SELECT strategy FROM pgcontext.explain_sparse(
             'm14_sparse_ann_lifecycle', 'lexical',
             pgcontext.sparsevec('{1:48}/4'), 5
         )",
    )
    .expect("configured fallback explain should execute")
    .expect("configured fallback explain should return a row");
    assert_eq!(configured_strategy, "exact");
    Spi::run(
        "SELECT pgcontext.attach_sparse_hnsw_index(
             'm14_sparse_ann_lifecycle', 'lexical',
             'public.m14_sparse_ann_lifecycle_hnsw'
         )",
    )
    .expect("sparse HNSW index should reattach after configuration change");

    Spi::run("DROP INDEX public.m14_sparse_ann_lifecycle_hnsw")
        .expect("attached sparse HNSW index should drop");
    let fallback = sparse_table_rows(search_sql);
    assert_eq!(fallback, ann, "dropped sparse index must fall back to exact");
    let strategy = Spi::get_one::<String>(
        "SELECT strategy FROM pgcontext.explain_sparse(
             'm14_sparse_ann_lifecycle', 'lexical',
             pgcontext.sparsevec('{1:48}/4'), 5
         )",
    )
    .expect("fallback explain should execute")
    .expect("fallback explain should return a row");
    assert_eq!(strategy, "exact");
}

#[pg_test]
fn quantized_candidate_rerank_uses_original_vectors_for_final_order() {
    let rows = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT point_id, score
                   FROM pgcontext.rerank_quantized_candidates(
                        '[0,0]'::vector,
                        ARRAY[30, 10, 20]::bigint[],
                        ARRAY[
                            '[2,0]'::vector,
                            '[1,0]'::vector,
                            '[0,1]'::vector
                        ],
                        'l2',
                        2
                   )",
                None,
                &[],
            )
            .expect("quantized candidate rerank query failed");

        let mut rows = Vec::new();
        for row in result {
            rows.push((
                row.get::<i64>(1)?.unwrap_or_default(),
                row.get::<f32>(2)?.unwrap_or_default(),
            ));
        }
        Ok::<_, spi::Error>(rows)
    })
    .expect("quantized candidate rerank rows failed");

    assert_eq!(rows, vec![(10, 1.0), (20, 1.0)]);
}

#[pg_test]
fn quantized_candidate_rerank_supports_inner_product_ordering() {
    let rows = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT point_id, score
                   FROM pgcontext.rerank_quantized_candidates(
                        '[1,0]'::vector,
                        ARRAY[1, 2, 3]::bigint[],
                        ARRAY[
                            '[1,0]'::vector,
                            '[3,0]'::vector,
                            '[2,0]'::vector
                        ],
                        'inner_product',
                        2
                   )",
                None,
                &[],
            )
            .expect("inner product quantized rerank query failed");

        let mut rows = Vec::new();
        for row in result {
            rows.push((
                row.get::<i64>(1)?.unwrap_or_default(),
                row.get::<f32>(2)?.unwrap_or_default(),
            ));
        }
        Ok::<_, spi::Error>(rows)
    })
    .expect("inner product quantized rerank rows failed");

    assert_eq!(rows, vec![(2, -3.0), (3, -2.0)]);
}

#[pg_test]
fn late_interaction_rerank_uses_maxsim_and_stable_tie_breaks() {
    let rows = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT point_id, score
                   FROM pgcontext.rerank_late_interaction(
                        ARRAY['[1,0]'::vector, '[0,1]'::vector],
                        ARRAY[30, 10, 20]::bigint[],
                        ARRAY[
                            '[0.9,0.1]'::vector,
                            '[0.1,0.8]'::vector,
                            '[1,0]'::vector,
                            '[0,1]'::vector,
                            '[1,0]'::vector,
                            '[0,1]'::vector
                        ],
                        ARRAY[0, 2, 4, 6]::integer[],
                        3
                   )",
                None,
                &[],
            )
            .expect("late interaction rerank query failed");

        let mut rows = Vec::new();
        for row in result {
            rows.push((
                row.get::<i64>(1)?.unwrap_or_default(),
                row.get::<f32>(2)?.unwrap_or_default(),
            ));
        }
        Ok::<_, spi::Error>(rows)
    })
    .expect("late interaction rerank rows failed");

    assert_eq!(rows, vec![(10, 2.0), (20, 2.0), (30, 1.7)]);
}

#[pg_test]
fn late_interaction_rerank_applies_limit_after_exact_scores() {
    let rows = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT point_id, score
                   FROM pgcontext.rerank_late_interaction(
                        ARRAY['[1,0]'::vector],
                        ARRAY[1, 2, 3]::bigint[],
                        ARRAY[
                            '[0.2,0]'::vector,
                            '[0.9,0]'::vector,
                            '[0.5,0]'::vector
                        ],
                        ARRAY[0, 1, 2, 3]::integer[],
                        2
                   )",
                None,
                &[],
            )
            .expect("limited late interaction query failed");

        let mut rows = Vec::new();
        for row in result {
            rows.push((
                row.get::<i64>(1)?.unwrap_or_default(),
                row.get::<f32>(2)?.unwrap_or_default(),
            ));
        }
        Ok::<_, spi::Error>(rows)
    })
    .expect("limited late interaction rows failed");

    assert_eq!(rows, vec![(2, 0.9), (3, 0.5)]);
}

#[pg_test]
#[should_panic(
    expected = "quantized rerank requires one original vector per candidate point: got 2 ids and 1 vectors"
)]
fn quantized_candidate_rerank_rejects_missing_original_vectors() {
    Spi::run(
        "SELECT pgcontext.rerank_quantized_candidates(
                '[0]'::vector,
                ARRAY[1, 2]::bigint[],
                ARRAY['[0]'::vector],
                'l2',
                1
         )",
    )
    .expect("missing original vectors should fail");
}

#[pg_test]
#[should_panic(expected = "point id must be non-negative: -1")]
fn quantized_candidate_rerank_rejects_negative_point_ids() {
    Spi::run(
        "SELECT pgcontext.rerank_quantized_candidates(
                '[0]'::vector,
                ARRAY[-1]::bigint[],
                ARRAY['[0]'::vector],
                'l2',
                1
         )",
    )
    .expect("negative point id should fail");
}

#[pg_test]
#[should_panic(expected = "dimension mismatch: left has 1 dimensions, right has 2")]
fn quantized_candidate_rerank_rejects_dimension_mismatch() {
    Spi::run(
        "SELECT pgcontext.rerank_quantized_candidates(
                '[0]'::vector,
                ARRAY[1]::bigint[],
                ARRAY['[0,0]'::vector],
                'l2',
                1
         )",
    )
    .expect("dimension mismatch should fail");
}

#[pg_test]
#[should_panic(expected = "unsupported distance metric: bad_metric")]
fn quantized_candidate_rerank_rejects_unknown_metric() {
    Spi::run(
        "SELECT pgcontext.rerank_quantized_candidates(
                '[0]'::vector,
                ARRAY[1]::bigint[],
                ARRAY['[0]'::vector],
                'bad_metric',
                1
         )",
    )
    .expect("unknown metric should fail");
}

#[pg_test]
fn late_interaction_rerank_rejects_mismatched_offsets_with_sqlstate() {
    assert_vector_search_sql_failure(
        "SELECT pgcontext.rerank_late_interaction(
                ARRAY['[1,0]'::vector],
                ARRAY[1, 2]::bigint[],
                ARRAY['[1,0]'::vector, '[0,1]'::vector],
                ARRAY[0, 2]::integer[],
                1
         )",
        "22023",
        "candidate_offsets must have one more entry than point_ids: got 2 offsets and 2 point ids",
        "late-interaction offset length mismatch",
    );
}

#[pg_test]
fn late_interaction_rerank_rejects_empty_candidate_vector_ranges_with_sqlstate() {
    assert_vector_search_sql_failure(
        "SELECT pgcontext.rerank_late_interaction(
                ARRAY['[1,0]'::vector],
                ARRAY[1, 2]::bigint[],
                ARRAY['[1,0]'::vector],
                ARRAY[0, 0, 1]::integer[],
                1
         )",
        "22023",
        "each late-interaction candidate point must have at least one vector",
        "late-interaction empty candidate vector range",
    );
}

#[pg_test]
fn late_interaction_rerank_rejects_dimension_mismatch_with_sqlstate() {
    assert_vector_search_sql_failure(
        "SELECT pgcontext.rerank_late_interaction(
                ARRAY['[1]'::vector],
                ARRAY[1]::bigint[],
                ARRAY['[1,0]'::vector],
                ARRAY[0, 1]::integer[],
                1
         )",
        "22P02",
        "invalid vector: dimension mismatch: left has 1 dimensions, right has 2",
        "late-interaction dimension mismatch",
    );
}

#[pg_test]
fn late_interaction_rerank_rejects_excessive_comparison_budget_with_sqlstate() {
    assert_vector_search_sql_failure(
        "SELECT pgcontext.rerank_late_interaction(
                array_fill('[1,0]'::vector, ARRAY[1001]),
                ARRAY[1]::bigint[],
                array_fill('[1,0]'::vector, ARRAY[1000]),
                ARRAY[0, 1000]::integer[],
                1
         )",
        "54000",
        "late interaction comparison budget exceeded: 1001000 > 1000000",
        "late-interaction comparison budget",
    );
}

#[pg_test]
#[should_panic(expected = "point_ids and vectors must have the same length: got 2 ids and 1 vectors")]
fn exact_search_rejects_mismatched_candidate_arrays() {
    Spi::run(
        "SELECT pgcontext.search(
                '[0]'::vector,
                ARRAY[1, 2]::bigint[],
                ARRAY['[0]'::vector],
                'l2',
                1
         )",
    )
    .expect("mismatched candidate arrays should fail");
}

#[pg_test]
#[should_panic(expected = "unsupported distance metric: bad_metric")]
fn exact_search_rejects_unknown_metric() {
    Spi::run(
        "SELECT pgcontext.search(
                '[0]'::vector,
                ARRAY[1]::bigint[],
                ARRAY['[0]'::vector],
                'bad_metric',
                1
         )",
    )
    .expect("unknown metric should fail");
}

#[pg_test]
#[should_panic(expected = "unsupported distance metric: hamming")]
fn exact_search_rejects_bit_metric() {
    Spi::run(
        "SELECT pgcontext.search(
                '[0]'::vector,
                ARRAY[1]::bigint[],
                ARRAY['[0]'::vector],
                'hamming',
                1
         )",
    )
    .expect("bit metric should fail for numeric exact search");
}

#[pg_test]
#[should_panic(expected = "invalid search limit: 0")]
fn exact_search_rejects_invalid_limit() {
    Spi::run(
        "SELECT pgcontext.search(
                '[0]'::vector,
                ARRAY[1]::bigint[],
                ARRAY['[0]'::vector],
                'l2',
                0
         )",
    )
    .expect("invalid limit should fail");
}

#[pg_test]
#[should_panic(expected = "dimension mismatch: left has 1 dimensions, right has 2")]
fn exact_search_rejects_dimension_mismatch() {
    Spi::run(
        "SELECT pgcontext.search(
                '[0]'::vector,
                ARRAY[1]::bigint[],
                ARRAY['[0,0]'::vector],
                'l2',
                1
         )",
    )
    .expect("dimension mismatch should fail");
}

#[pg_test]
#[should_panic(expected = "invalid vector: value at dimension 0 is not finite: NaN")]
fn vector_input_rejects_non_finite_values() {
    Spi::run("SELECT '[NaN]'::vector").expect("non-finite vector input should fail");
}

fn create_sparse_search_collection(collection_name: &str, metric: &str) {
    create_sparse_search_source_table(collection_name);
    Spi::run(&format!(
        "SELECT pgcontext.create_collection('{collection_name}', 'public.{collection_name}')"
    ))
    .expect("sparse search collection should be created");
    Spi::run(&format!(
        "SELECT pgcontext.register_sparse_vector(
            '{collection_name}',
            'lexical',
            'lexical',
            4,
            '{metric}'
        )"
    ))
    .expect("sparse search vector should be registered");
}

fn create_sparse_search_source_table(collection_name: &str) {
    Spi::run(&format!(
        "CREATE TABLE public.{collection_name} (
             id bigint PRIMARY KEY,
             lexical sparsevec NOT NULL
         )"
    ))
    .expect("sparse search table should be created");
    Spi::run(&format!(
        "INSERT INTO public.{collection_name} (id, lexical)
         VALUES (10, pgcontext.sparsevec('{{1:1}}/4')),
                (20, pgcontext.sparsevec('{{1:3}}/4')),
                (30, pgcontext.sparsevec('{{1:2}}/4'))"
    ))
    .expect("sparse search rows should be inserted");
}

fn upsert_sparse_search_points(collection_name: &str, source_keys: &[&str]) {
    let source_keys = source_keys
        .iter()
        .map(|source_key| format!("'{source_key}'"))
        .collect::<Vec<_>>()
        .join(", ");
    Spi::run(&format!(
        "SELECT pgcontext.upsert_points('{collection_name}', ARRAY[{source_keys}])"
    ))
    .expect("sparse search points should be upserted");
}

fn sparse_table_rows(sql: &str) -> Vec<(i64, String, f32)> {
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
    .expect("sparse table search failed")
}

fn assert_vector_search_sql_failure(sql: &str, sqlstate: &str, message: &str, context: &str) {
    let message = message.replace('\'', "''");
    Spi::run(&format!(
        r#"
        DO $$
        DECLARE
            actual_sqlstate text;
        BEGIN
            BEGIN
                PERFORM * FROM ({sql}) AS invalid_call;
                RAISE EXCEPTION 'expected {context} failure';
            EXCEPTION WHEN OTHERS THEN
                GET STACKED DIAGNOSTICS actual_sqlstate = RETURNED_SQLSTATE;
                IF actual_sqlstate <> '{sqlstate}' THEN
                    RAISE EXCEPTION 'unexpected {context} SQLSTATE: %', actual_sqlstate;
                END IF;
                IF SQLERRM <> '{message}' THEN
                    RAISE EXCEPTION 'unexpected {context} error: %', SQLERRM;
                END IF;
            END;
        END $$;
        "#
    ))
    .expect("invalid vector search call should raise expected error");
}
