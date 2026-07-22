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
